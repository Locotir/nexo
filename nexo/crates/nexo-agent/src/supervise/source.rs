//! Sources: where observations come from. A source only reads; it never writes to a session, a log or a
//! person's configuration. The supervisor polls every source and folds what they return into one track
//! per session.

use serde_json::Value;

use super::{Observation, Secs};

/// Where a session runs, for the card's icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceKind {
    /// A nexo session (this one, or a sub-agent of it).
    Nexo,
    /// A session of another program, seen through its log and its process list.
    External,
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            SourceKind::Nexo => "nexo",
            SourceKind::External => "external",
        }
    }
}

/// The program name of an external session whose source cannot tell it.
pub const EXTERNAL: &str = "external";

pub trait Source: Send {
    fn id(&self) -> &str;
    fn kind(&self) -> SourceKind;
    /// The name of the program the sessions belong to, for the panel's section header. A source that does
    /// not know it leaves the generic name.
    fn program(&self) -> String {
        match self.kind() {
            SourceKind::Nexo => "nexo".to_string(),
            SourceKind::External => EXTERNAL.to_string(),
        }
    }
    /// What happened since the last call.
    fn poll(&mut self, now: Secs) -> Vec<Observation>;
}

/// The longest step line kept.
pub const STEP_CHARS: usize = 120;

/// The first `n` characters of the first non-empty line of `text`, redacted, with `…` when cut.
pub fn one_line(text: &str, n: usize) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let line = nexo_core::redact::redact(line);
    let line: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= n {
        return line;
    }
    let mut out: String = line.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// `name  gist` for a tool call: the first argument that says what it works on, redacted and short.
pub fn tool_step(name: &str, input: &Value) -> String {
    const KEYS: [&str; 9] =
        ["command", "file_path", "path", "pattern", "query", "url", "description", "prompt", "skill"];
    let gist = KEYS
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .map(|s| one_line(s, STEP_CHARS))
        .unwrap_or_default();
    if gist.is_empty() {
        name.to_string()
    } else {
        format!("{name}  {gist}")
    }
}

/// The last `n` characters of `text`.
pub fn tail(text: &str, n: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(n)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_step_names_the_tool_and_what_it_works_on() {
        assert_eq!(tool_step("Bash", &json!({"command": "cargo test\nmore"})), "Bash  cargo test");
        assert_eq!(tool_step("Read", &json!({"file_path": "/a/b.rs"})), "Read  /a/b.rs");
        assert_eq!(tool_step("Idle", &json!({})), "Idle");
    }

    #[test]
    fn a_step_never_carries_a_secret_and_is_cut() {
        let key = format!("{}{}", "s", "k-abcdefghijklmnopqrstuvwx");
        let s = tool_step("Bash", &json!({ "command": format!("curl -H 'x: {key}' https://example.test") }));
        assert!(!s.contains(&key), "{s}");
        let long = one_line(&"word ".repeat(100), 20);
        assert_eq!(long.chars().count(), 20);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn tail_keeps_the_end() {
        assert_eq!(tail("héllo wörld", 5), "wörld");
        assert_eq!(tail("ab", 5), "ab");
    }
}
