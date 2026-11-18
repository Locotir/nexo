use super::*;

/// `<config_dir>/team.toml`: the lanes `nexo team` runs on their own, each a long-lived session in a folder of
/// its own that does a routine on a clock and answers mail between rounds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TeamConfig {
    pub lane: Vec<Lane>,
}

/// One `[[lane]]` of `team.toml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lane {
    /// Unique among the lanes; the session's title and the name of its state files.
    pub name: String,
    /// The folder the session works in (`~` is the home folder); its instruction files apply as usual.
    pub dir: String,
    /// `provider/model`; none: the person's default.
    pub model: Option<String>,
    /// The permission mode; none: the person's default.
    pub mode: Option<String>,
    /// How often the routine runs (`30s`, `15m`, `2h`), start to start; none: once at start, then only mail.
    pub every: Option<String>,
    /// The routine's text.
    pub routine: Option<String>,
    /// A file (relative to `dir`) that holds the routine under the heading line `<routine_section>:`.
    pub routine_file: Option<String>,
    pub routine_section: Option<String>,
    /// Dollars the lane may spend a day.
    pub max_spend_day: Option<f64>,
    /// The OS sandbox for the lane's bash tool: `off`, `workspace-write` or `read-only`; none: the setting of
    /// the lane's folder and of the person, as for any session. A lane runs unattended, so this is where a
    /// sandbox is set for it alone, without touching the person's own sessions.
    pub sandbox: Option<String>,
    /// Whether sandboxed commands of the lane may use the network; none: the setting as it is.
    pub sandbox_network: Option<bool>,
    /// Folders besides `dir` that sandboxed commands of the lane may write (`~` is the home folder, a relative
    /// name is taken from `dir`); only those that exist count.
    pub sandbox_write_dirs: Vec<String>,
}

/// `team.toml` as read: the lanes, and the keys nexo does not read said in one line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TeamFile {
    pub team: TeamConfig,
    pub unread: Option<String>,
}

/// Where the lanes are set: `<config_dir>/team.toml`.
pub fn team_file() -> PathBuf {
    paths::config_dir().join("team.toml")
}

impl TeamConfig {
    /// Reads and checks `text`: every lane has a name of its own, a folder, a routine (text or file and
    /// section, not both), a clock that parses and a mode nexo knows.
    pub fn parse(text: &str) -> Result<TeamFile, String> {
        let team: TeamConfig = toml::from_str(text).map_err(|e| e.message().to_string())?;
        let mut names: Vec<&str> = Vec::new();
        for l in &team.lane {
            let n = l.name.trim();
            if n.is_empty() || !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return Err(format!(
                    "lane name {:?}: use letters, digits, `-` and `_` (it names the lane's files)",
                    l.name
                ));
            }
            if names.iter().any(|m| m.eq_ignore_ascii_case(n)) {
                return Err(format!("lane {n}: the name is used twice"));
            }
            names.push(n);
            if l.dir.trim().is_empty() {
                return Err(format!("lane {n}: `dir` is missing"));
            }
            match (&l.routine, &l.routine_file, &l.routine_section) {
                (Some(_), None, None) | (None, Some(_), Some(_)) => {}
                (Some(_), _, _) => return Err(format!("lane {n}: give `routine` or `routine_file`, not both")),
                (None, Some(_), None) | (None, None, Some(_)) => {
                    return Err(format!("lane {n}: `routine_file` and `routine_section` go together"))
                }
                (None, None, None) => {
                    return Err(format!("lane {n}: no `routine` (or `routine_file` + `routine_section`)"))
                }
            }
            if let Some(e) = &l.every {
                parse_every(e).map_err(|why| format!("lane {n}: every = {e:?}: {why}"))?;
            }
            if let Some(m) = &l.mode {
                if crate::permission::PermissionMode::parse(m).is_none() {
                    return Err(format!(
                        "lane {n}: unknown mode {m:?}; use one of: ask, accept-edits, plan, yolo, read-only"
                    ));
                }
            }
            if let Some(s) = &l.sandbox {
                if crate::sandbox::SandboxMode::parse(s).is_none() {
                    return Err(format!(
                        "lane {n}: unknown sandbox {s:?}; use one of: off, workspace-write, read-only"
                    ));
                }
            }
            if l.max_spend_day.is_some_and(|d| !d.is_finite() || d <= 0.0) {
                return Err(format!("lane {n}: max_spend_day must be a number more than 0"));
            }
        }
        // every key a lane can have, so a slip in one it left out is named with the key it was meant to be
        let all = TeamConfig { lane: team.lane.iter().map(Lane::with_every_key).collect() };
        let unread = unread_keys(text, &all);
        Ok(TeamFile { team, unread })
    }

    /// The lane called `name` (case does not matter).
    pub fn get(&self, name: &str) -> Option<&Lane> {
        self.lane.iter().find(|l| l.name.eq_ignore_ascii_case(name))
    }
}

impl Lane {
    /// The lane with each key it left out given a value: an absent key is not written out, and only the
    /// keys written out count as read.
    fn with_every_key(&self) -> Lane {
        let mut l = self.clone();
        for o in [&mut l.model, &mut l.mode, &mut l.every, &mut l.routine, &mut l.routine_file, &mut l.routine_section]
        {
            o.get_or_insert_with(String::new);
        }
        l.max_spend_day.get_or_insert(0.0);
        l.sandbox.get_or_insert_with(String::new);
        l.sandbox_network.get_or_insert(false);
        l
    }

    /// Puts the lane's sandbox settings on `config`, over what the folder's and the person's files said. A
    /// managed policy still decides: a mode looser than its floor, network it shuts and write folders it fixes
    /// are refused or left as they are, as for the command line.
    pub fn apply_sandbox(&self, config: &mut Config) -> Result<(), String> {
        if let Some(s) = &self.sandbox {
            let mode = crate::sandbox::SandboxMode::parse(s).ok_or_else(|| format!("unknown sandbox mode {s:?}"))?;
            if let Some(p) = &config.managed {
                p.check_sandbox(mode)?;
            }
            config.permissions.sandbox = mode;
        }
        if let Some(network) = self.sandbox_network {
            if let Some(p) = &config.managed {
                p.check_sandbox_network(network)?;
            }
            config.permissions.sandbox_network = network;
        }
        let fixed = config.managed.as_ref().is_some_and(|m| m.permissions.sandbox_write_dirs.is_some());
        if !fixed {
            let base = crate::team::lane_dir(self);
            for d in &self.sandbox_write_dirs {
                // a name that is not a folder is left out, as the global setting leaves it out
                let Ok(real) = base.join(crate::team::expand_home(d)).canonicalize() else { continue };
                if real.is_dir() && !config.permissions.sandbox_write_dirs.contains(&real) {
                    config.permissions.sandbox_write_dirs.push(real);
                }
            }
        }
        Ok(())
    }
}

/// `30s`, `15m`, `2h`: a whole number and one unit, more than zero.
pub fn parse_every(s: &str) -> Result<std::time::Duration, String> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.len().saturating_sub(1));
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => return Err("use a number and s, m or h (`15m`)".into()),
    };
    match num.parse::<u64>() {
        Ok(n) if n > 0 => Ok(std::time::Duration::from_secs(n * mult)),
        _ => Err("use a number more than 0 and s, m or h (`15m`)".into()),
    }
}

/// The routine under the heading line `<section>:` of `text`: the lines of the first code block after it, as
/// a team script's lanes file is read. None when there is no such heading or block.
pub fn routine_section(text: &str, section: &str) -> Option<String> {
    let head = format!("{section}:");
    let mut lines = text.lines().skip_while(|l| *l != head).skip(1);
    lines.find(|l| l.starts_with("```"))?;
    let mut out = Vec::new();
    for l in lines {
        if l.starts_with("```") {
            let body = out.join("\n");
            return (!body.trim().is_empty()).then_some(body);
        }
        out.push(l);
    }
    None
}

#[cfg(test)]
mod team_tests {
    use super::*;

    #[test]
    fn a_lane_takes_a_routine_as_text_or_as_a_file_and_section() {
        let t = r#"
[[lane]]
name = "WINDOWS"
dir = "~/work"
model = "xai/grok-4.7"
mode = "accept-edits"
every = "15m"
routine = "Do the next order."
max_spend_day = 2.0

[[lane]]
name = "ios"
dir = "/w/ios"
routine_file = "docs/LANES.md"
routine_section = "IOS"
"#;
        let f = TeamConfig::parse(t).unwrap();
        assert_eq!(f.unread, None);
        assert_eq!(f.team.lane.len(), 2);
        let w = f.team.get("windows").unwrap();
        assert_eq!(w.routine.as_deref(), Some("Do the next order."));
        assert_eq!(w.max_spend_day, Some(2.0));
        let i = f.team.get("IOS").unwrap();
        assert_eq!((i.routine_file.as_deref(), i.routine_section.as_deref()), (Some("docs/LANES.md"), Some("IOS")));
        assert_eq!(i.every, None);
        let both =
            "[[lane]]\nname = \"a\"\ndir = \"/w\"\nroutine = \"x\"\nroutine_file = \"f\"\nroutine_section = \"A\"\n";
        assert!(TeamConfig::parse(both).unwrap_err().contains("not both"));
        let half = "[[lane]]\nname = \"a\"\ndir = \"/w\"\nroutine_file = \"f\"\n";
        assert!(TeamConfig::parse(half).unwrap_err().contains("go together"));
        let none = "[[lane]]\nname = \"a\"\ndir = \"/w\"\n";
        assert!(TeamConfig::parse(none).unwrap_err().contains("no `routine`"));
        let twice = "[[lane]]\nname = \"a\"\ndir = \"/w\"\nroutine = \"x\"\n[[lane]]\nname = \"A\"\ndir = \"/v\"\nroutine = \"y\"\n";
        assert!(TeamConfig::parse(twice).unwrap_err().contains("used twice"));
        let bad_mode = "[[lane]]\nname = \"a\"\ndir = \"/w\"\nroutine = \"x\"\nmode = \"wild\"\n";
        assert!(TeamConfig::parse(bad_mode).unwrap_err().contains("unknown mode"));
        let bad_name = "[[lane]]\nname = \"a b\"\ndir = \"/w\"\nroutine = \"x\"\n";
        assert!(TeamConfig::parse(bad_name).unwrap_err().contains("letters, digits"));
    }

    #[test]
    fn a_daily_cap_that_is_not_a_finite_number_above_zero_is_refused() {
        for bad in ["inf", "-inf", "nan", "0.0", "-1.0"] {
            let t = format!("[[lane]]\nname = \"a\"\ndir = \"/w\"\nroutine = \"x\"\nmax_spend_day = {bad}\n");
            let e = TeamConfig::parse(&t).unwrap_err();
            assert!(e.contains("max_spend_day must be a number more than 0"), "{bad}: {e}");
        }
    }

    #[test]
    fn durations_are_a_whole_number_and_one_unit() {
        use std::time::Duration;
        assert_eq!(parse_every("1s"), Ok(Duration::from_secs(1)));
        assert_eq!(parse_every("15m"), Ok(Duration::from_secs(900)));
        assert_eq!(parse_every(" 2h "), Ok(Duration::from_secs(7200)));
        for bad in ["0m", "m", "15", "1d", "1.5h", "-1s", ""] {
            assert!(parse_every(bad).is_err(), "{bad}");
        }
        let t = "[[lane]]\nname = \"a\"\ndir = \"/w\"\nroutine = \"x\"\nevery = \"soon\"\n";
        assert!(TeamConfig::parse(t).unwrap_err().contains("every = \"soon\""));
    }

    #[test]
    fn keys_nexo_does_not_read_are_named_and_the_file_still_loads() {
        let t = "[[lane]]\nname = \"a\"\ndir = \"/w\"\nroutine = \"x\"\nevry = \"1m\"\n[extra]\nk = 1\n";
        let f = TeamConfig::parse(t).unwrap();
        assert_eq!(f.team.lane.len(), 1);
        let u = f.unread.unwrap();
        assert!(u.contains("lane[0].evry (did you mean every?)"), "{u}");
        assert!(u.contains("extra"), "{u}");
    }

    #[test]
    fn the_routine_section_is_the_first_code_block_under_its_heading() {
        let text = "# Lanes\n\nIOS:\n```\nnot this one\n```\n\nWINDOWS:\nSome words.\n```text\nCheck the orders.\nReport there.\n```\n```\nlater block\n```\n";
        assert_eq!(routine_section(text, "WINDOWS").as_deref(), Some("Check the orders.\nReport there."));
        assert_eq!(routine_section(text, "IOS").as_deref(), Some("not this one"));
        assert_eq!(routine_section(text, "MAC"), None);
        assert_eq!(routine_section("WINDOWS:\n```\nnever closed\n", "WINDOWS"), None);
    }

    fn lane_with(extra: &str) -> String {
        format!("[[lane]]\nname = \"a\"\ndir = \"/w\"\nroutine = \"x\"\n{extra}")
    }

    #[test]
    fn a_lane_reads_the_three_sandbox_modes_and_refuses_any_other() {
        use crate::sandbox::SandboxMode;
        for (text, mode) in [
            ("off", SandboxMode::Off),
            ("workspace-write", SandboxMode::WorkspaceWrite),
            ("read-only", SandboxMode::ReadOnly),
        ] {
            let f = TeamConfig::parse(&lane_with(&format!("sandbox = \"{text}\"\nsandbox_network = true\n"))).unwrap();
            assert_eq!(f.unread, None, "{text}: the keys are read");
            let lane = &f.team.lane[0];
            assert_eq!(lane.sandbox.as_deref().and_then(SandboxMode::parse), Some(mode), "{text}");
            assert_eq!(lane.sandbox_network, Some(true));
        }
        let e = TeamConfig::parse(&lane_with("sandbox = \"sideways\"\n")).unwrap_err();
        assert!(e.contains("unknown sandbox"), "{e}");
        let none = TeamConfig::parse(&lane_with("")).unwrap().team;
        assert_eq!((none.lane[0].sandbox.clone(), none.lane[0].sandbox_network), (None, None), "default: unchanged");
    }

    #[test]
    fn a_lane_with_a_sandbox_sets_it_on_its_config_and_keeps_only_write_folders_that_exist() {
        use crate::sandbox::SandboxMode;
        let d = tempfile::tempdir().unwrap();
        let shared = d.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        let real = shared.canonicalize().unwrap();
        let lane = Lane {
            dir: d.path().display().to_string(),
            sandbox: Some("workspace-write".into()),
            sandbox_network: Some(false),
            sandbox_write_dirs: vec!["shared".into(), "missing".into(), "shared".into()],
            ..Lane::default()
        };
        let mut config = crate::Config::default();
        config.permissions.sandbox_network = true;
        lane.apply_sandbox(&mut config).unwrap();
        assert_eq!(config.permissions.sandbox, SandboxMode::WorkspaceWrite);
        assert!(!config.permissions.sandbox_network);
        assert_eq!(config.permissions.sandbox_write_dirs, vec![real]);
        let mut untouched = crate::Config::default();
        Lane::default().apply_sandbox(&mut untouched).unwrap();
        assert_eq!(untouched, crate::Config::default(), "a lane that says nothing changes nothing");
    }

    #[test]
    fn a_managed_policy_still_bounds_the_sandbox_of_a_lane() {
        let policy = "[permissions]\nsandbox = \"workspace-write\"\nsandbox_network = false\n";
        let managed = crate::managed::ManagedPolicy::parse(policy).unwrap();
        let mut config = crate::Config { managed: Some(std::sync::Arc::new(managed)), ..Default::default() };
        let lane = |sandbox: &str, network: bool| Lane {
            sandbox: Some(sandbox.into()),
            sandbox_network: Some(network),
            ..Lane::default()
        };
        assert!(lane("off", false).apply_sandbox(&mut config).is_err());
        assert!(lane("workspace-write", true).apply_sandbox(&mut config).is_err());
        assert!(lane("read-only", false).apply_sandbox(&mut config).is_ok());
    }
}
