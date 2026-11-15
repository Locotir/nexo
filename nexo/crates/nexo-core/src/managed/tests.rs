use super::*;

fn policy(text: &str) -> ManagedPolicy {
    let mut p = ManagedPolicy::parse(text).unwrap();
    p.file = PathBuf::from("/etc/nexo/managed.toml");
    p
}

#[test]
fn globs() {
    assert!(glob_match("claude-*", "Claude-opus-4"));
    assert!(glob_match("*/gpt-?", "openai/gpt-5"));
    assert!(!glob_match("gpt-?", "gpt-55"));
    assert!(glob_match("*", ""));
    assert!(glob_match("npx *", "npx -y server"));
    assert!(!glob_match("a*b", "acd"));
}

#[test]
fn unknown_keys_and_bad_hooks_are_errors() {
    assert!(ManagedPolicy::parse("disable_webb = true").is_err());
    assert!(ManagedPolicy::parse("[permissions]\nsandbox = \"loose\"").is_err());
    assert!(ManagedPolicy::parse("[[forced_hooks]]\nevent = \"nope\"\ncommand = \"x\"").is_err());
    assert!(ManagedPolicy::parse("[[forced_hooks]]\nevent = \"stop\"").is_err());
}

#[test]
fn deny_beats_allow_in_the_merged_rules() {
    let p = policy("[permissions]\ndeny = [\"bash(rm *)\"]\nallow = [\"read\"]");
    let mut c = Config::default();
    c.permissions.allow.push("bash(rm *)".into());
    p.enforce(&mut c);
    assert!(c.permissions.deny.contains(&"bash(rm *)".to_string()));
    assert!(c.permissions.allow.contains(&"read".to_string()));
    p.enforce(&mut c);
    assert_eq!(c.permissions.deny.len(), 1, "twice changes nothing more");
}

#[test]
fn the_sandbox_only_tightens() {
    let p = policy("[permissions]\nsandbox = \"workspace-write\"\nsandbox_network = false");
    assert_eq!(p.clamp_sandbox(SandboxMode::Off), SandboxMode::WorkspaceWrite);
    assert_eq!(p.clamp_sandbox(SandboxMode::ReadOnly), SandboxMode::ReadOnly);
    assert!(p.check_sandbox(SandboxMode::Off).is_err());
    assert!(p.check_sandbox_network(true).is_err());
    let mut c = Config::default();
    c.permissions.sandbox_network = true;
    p.enforce(&mut c);
    assert_eq!((c.permissions.sandbox, c.permissions.sandbox_network), (SandboxMode::WorkspaceWrite, false));
    let forced = policy("[permissions]\nsandbox = \"workspace-write\"\nsandbox_forced = true");
    assert_eq!(forced.clamp_sandbox(SandboxMode::ReadOnly), SandboxMode::WorkspaceWrite);
}

#[test]
fn models_and_providers() {
    let p = policy(
        "[providers]\ndeny = [\"xai\"]\n[models]\nallow = [\"claude-*\", \"openai/gpt-5*\"]\ndeny = [\"*-preview\"]",
    );
    assert!(p.model_allowed("anthropic", "claude-opus"));
    assert!(p.model_allowed("openai", "gpt-5-mini"));
    assert!(!p.model_allowed("other", "gpt-5-mini"));
    assert!(!p.model_allowed("anthropic", "claude-x-preview"));
    assert!(!p.model_allowed("xai", "claude-opus"));
    let e = p.check_model("xai", "grok").unwrap_err();
    assert!(e.contains("blocked by your organization's policy (/etc/nexo/managed.toml)"), "{e}");
}

#[test]
fn mcp_allowlist_and_denylist() {
    let p = policy("[mcp]\nallow = [\"files\"]\nallow_commands = [\"npx -y @acme/*\"]\ndeny_commands = [\"*curl*\"]");
    let local = |c: &str, a: &[&str]| McpServerConfig {
        command: c.into(),
        args: a.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    assert!(p.check_mcp("files", &local("x", &[])).is_ok());
    assert!(p.check_mcp("other", &local("npx", &["-y", "@acme/tool"])).is_ok());
    assert!(p.check_mcp("other", &local("evil", &[])).is_err());
    assert!(p.check_mcp("files", &local("curl", &["x"])).is_err(), "a deny beats an allow");
    let mut c = Config::default();
    c.mcp.insert("files".into(), local("x", &[]));
    c.mcp.insert("rogue".into(), local("rogue", &[]));
    p.enforce(&mut c);
    assert_eq!(c.mcp.keys().collect::<Vec<_>>(), vec!["files"]);
}

#[test]
fn hooks_are_filtered_by_origin_and_forced_ones_stay() {
    let p = policy(
        "allow_user_hooks = false\nallow_project_hooks = false\n[[forced_hooks]]\nevent = \"pre_tool\"\ncommand = \"audit\"",
    );
    let hook = |source: &str| HookConfig {
        event: "stop".into(),
        command: "x".into(),
        source: source.into(),
        ..Default::default()
    };
    let mut c = Config {
        hooks: vec![hook(""), hook("project .claude/settings.json"), hook("project .nexo/config.toml")],
        ..Default::default()
    };
    p.enforce(&mut c);
    p.enforce(&mut c);
    assert_eq!(c.hooks.len(), 1);
    assert_eq!((c.hooks[0].command.as_str(), c.hooks[0].source.as_str()), ("audit", SOURCE));
}

#[test]
fn bypass_web_and_the_status_lines() {
    let p = policy("disable_bypass_mode = true\ndisable_web = true");
    assert!(p.check_mode(PermissionMode::Auto).is_err());
    assert!(p.check_mode(PermissionMode::AcceptEdits).is_ok());
    let mut c = Config::default();
    c.permissions.mode = PermissionMode::Auto;
    p.enforce(&mut c);
    assert_eq!(c.permissions.mode, PermissionMode::Ask);
    assert!(!c.web.enabled);
    let lines = p.lines().join("\n");
    assert!(lines.contains("disable_bypass_mode = true  (source: managed)"), "{lines}");
    assert!(lines.contains("disable_web"), "{lines}");
}

#[test]
fn extensions_default_to_allowed() {
    let p = policy("[extensions]\nuser_skills = false");
    assert!(!p.extension_allowed("user", "skills"));
    assert!(p.extension_allowed("project", "skills"));
    assert!(p.extension_allowed("user", "commands"));
}

#[test]
fn a_missing_file_is_no_policy_and_a_bad_one_is_an_error() {
    let d = tempfile::tempdir().unwrap();
    assert!(load_from(&d.path().join("none.toml")).unwrap().is_none());
    let f = d.path().join("managed.toml");
    std::fs::write(&f, "this is = = not toml").unwrap();
    let e = load_from(&f).unwrap_err().to_string();
    assert!(e.contains(&f.display().to_string()) && e.contains("does not start"), "{e}");
    std::fs::write(&f, "disable_web = true").unwrap();
    assert!(load_from(&f).unwrap().unwrap().disable_web);
}
