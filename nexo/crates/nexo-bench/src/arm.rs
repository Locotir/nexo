//! An arm: one way of running the agent (a context policy and its parameters) the tasks are
//! measured with. Written `[NAME@]POLICY[:k=v;k=v...]`: for `curator` the pairs are its spec, for
//! `compact` the one pair `at=TOKENS` (the request size that triggers a summary). Any policy also takes
//! `reader=TOKENS`: a tool output longer than that is read by a separate request (`[context] reader_at`).

/// The context policies an arm can use.
pub const POLICIES: [&str; 3] = ["plain", "compact", "curator"];

/// The arms a run uses when none are given.
pub const DEFAULT_ARMS: &str = "plain,compact,curator";

/// The `compact` threshold when the arm sets none: the agent's own default.
pub const COMPACT_AT: u64 = 100_000;

fn compact_at(spec: &str) -> Result<u64, String> {
    if spec.is_empty() {
        return Ok(COMPACT_AT);
    }
    match spec.split_once('=') {
        Some((k, v)) if k.trim() == "at" => match v.trim().parse::<u64>() {
            Ok(n) if n > 0 => Ok(n),
            _ => Err(format!("at={v:?}: a positive number of tokens")),
        },
        _ => Err(format!("{spec:?}: compact takes only at=TOKENS")),
    }
}

/// `spec` without its `reader=TOKENS` pair, and that number (0 when there is none).
fn split_reader(spec: &str) -> Result<(String, u64), String> {
    let mut reader = 0;
    let mut rest = Vec::new();
    for pair in spec.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        match pair.split_once('=') {
            Some((k, v)) if k.trim() == "reader" => {
                reader = v.trim().parse::<u64>().map_err(|_| format!("reader={v:?}: a number of tokens"))?;
            }
            _ => rest.push(pair),
        }
    }
    Ok((rest.join(";"), reader))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm {
    /// Names the arm in file names and tables.
    pub name: String,
    pub policy: String,
    /// The curator spec (`k=v;k=v`) or compact's `at=TOKENS`, empty for the defaults.
    pub spec: String,
}

impl Arm {
    pub fn parse(text: &str) -> Result<Arm, String> {
        let text = text.trim();
        let (name, rest) = match text.split_once('@') {
            Some((n, r)) => (Some(n.trim()), r.trim()),
            None => (None, text),
        };
        let (policy, spec) = match rest.split_once(':') {
            Some((p, s)) => (p.trim(), s.trim()),
            None => (rest, ""),
        };
        if !POLICIES.contains(&policy) {
            return Err(format!("arm {text:?}: the policy must be one of {}", POLICIES.join(", ")));
        }
        let name = name.unwrap_or(policy).to_string();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)) {
            return Err(format!("arm {text:?}: a name uses only letters, digits, '_', '.' and '-'"));
        }
        let (own, _) = split_reader(spec).map_err(|e| format!("arm {text:?}: {e}"))?;
        if !own.is_empty() {
            let spec = own.as_str();
            match policy {
                "curator" => {
                    nexo_curator::Curator::new(spec).map_err(|e| format!("arm {text:?}: {e}"))?;
                }
                "compact" => {
                    compact_at(spec).map_err(|e| format!("arm {text:?}: {e}"))?;
                }
                _ => return Err(format!("arm {text:?}: plain takes no parameters")),
            }
        }
        Ok(Arm { name, policy: policy.to_string(), spec: spec.to_string() })
    }

    /// The request size, in input tokens, at which a `compact` arm summarises.
    pub fn compact_at(&self) -> u64 {
        compact_at(&self.own_spec()).unwrap_or(COMPACT_AT)
    }

    /// The policy's own spec: `spec` without `reader=`.
    fn own_spec(&self) -> String {
        split_reader(&self.spec).map(|(s, _)| s).unwrap_or_default()
    }

    /// The curator spec, empty for any other policy.
    pub fn curator_spec(&self) -> String {
        if self.policy == "curator" {
            self.own_spec()
        } else {
            String::new()
        }
    }

    /// The tokens over which a tool output goes to a separate reader request (0: never).
    pub fn reader_at(&self) -> u64 {
        split_reader(&self.spec).map(|(_, r)| r).unwrap_or(0)
    }

    /// Every arm in `items` (each a comma list); names must be unique.
    pub fn parse_all<S: AsRef<str>>(items: &[S]) -> Result<Vec<Arm>, String> {
        let mut arms: Vec<Arm> = Vec::new();
        for part in items.iter().flat_map(|s| s.as_ref().split(',')).filter(|s| !s.trim().is_empty()) {
            let arm = Arm::parse(part)?;
            if arms.iter().any(|a| a.name == arm.name) {
                return Err(format!("two arms are named {:?}; name them with NAME@", arm.name));
            }
            arms.push(arm);
        }
        if arms.is_empty() {
            return Arm::parse_all(&[DEFAULT_ARMS]);
        }
        Ok(arms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_policy_names_the_arm_after_itself() {
        assert_eq!(
            Arm::parse("compact").unwrap(),
            Arm { name: "compact".into(), policy: "compact".into(), spec: "".into() }
        );
    }

    #[test]
    fn a_named_curator_arm_keeps_its_spec() {
        let a = Arm::parse("tight@curator:eoq=4000").unwrap();
        assert_eq!((a.name.as_str(), a.policy.as_str(), a.spec.as_str()), ("tight", "curator", "eoq=4000"));
    }

    #[test]
    fn a_compact_arm_takes_its_threshold() {
        assert_eq!(Arm::parse("compact").unwrap().compact_at(), COMPACT_AT);
        let a = Arm::parse("early@compact:at=40000").unwrap();
        assert_eq!((a.name.as_str(), a.compact_at(), a.curator_spec().as_str()), ("early", 40_000, ""));
        assert_eq!(nexo_core::config::ContextConfig::default().compact_at, COMPACT_AT, "the same default");
    }

    #[test]
    fn no_arms_means_the_three_default_ones() {
        let names: Vec<String> = Arm::parse_all::<&str>(&[]).unwrap().into_iter().map(|a| a.name).collect();
        assert_eq!(names, ["plain", "compact", "curator"]);
    }

    #[test]
    fn comma_lists_and_repeated_flags_combine() {
        let arms = Arm::parse_all(&["plain,compact", "x@curator"]).unwrap();
        assert_eq!(arms.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["plain", "compact", "x"]);
    }

    #[test]
    fn any_arm_may_take_a_reader() {
        let a = Arm::parse("lector@curator:directives=0;reader=2000").unwrap();
        assert_eq!((a.curator_spec().as_str(), a.reader_at()), ("directives=0", 2000));
        let p = Arm::parse("lp@plain:reader=3000").unwrap();
        assert_eq!((p.curator_spec().as_str(), p.reader_at()), ("", 3000));
        assert_eq!(Arm::parse("plain").unwrap().reader_at(), 0);
        assert!(Arm::parse("plain:reader=x").is_err());
    }

    #[test]
    fn bad_arms_are_refused() {
        assert!(Arm::parse("summary").is_err());
        assert!(Arm::parse("compact:eoq=4000").is_err());
        assert!(Arm::parse("compact:at=0").is_err());
        assert!(Arm::parse("compact:at=lots").is_err());
        assert!(Arm::parse("plain:at=1").is_err());
        assert!(Arm::parse("a b@plain").is_err());
        assert!(Arm::parse("curator:no_such_knob=1").is_err());
        assert!(Arm::parse_all(&["plain,plain"]).is_err());
    }
}
