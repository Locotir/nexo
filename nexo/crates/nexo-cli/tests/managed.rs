//! The binary against a managed policy file (`NEXO_MANAGED_FILE`, honoured by debug builds only).

use std::path::Path;
use std::process::{Command, Output};

const MOCK: &str =
    "provider = \"mock\"\nmodel = \"mock\"\n[providers.mock]\nprotocol = \"mock\"\nmodels = [\"mock\", \"other\"]\n[context]\npolicy = \"plain\"\n";

/// Runs nexo in `dir` with the user config `user` and, when given, the managed file `managed`.
fn nexo(dir: &Path, user: &str, managed: Option<&str>, args: &[&str]) -> Output {
    std::fs::write(dir.join("config.toml"), user).unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_nexo"));
    c.args(args)
        .current_dir(dir)
        .env("NEXO_CONFIG_DIR", dir)
        .env("NEXO_DATA_DIR", dir.join("data"))
        .env("NEXO_CACHE_DIR", dir.join("cache"))
        .env_remove("NEXO_PERMISSION_MODE");
    match managed {
        Some(text) => {
            let f = dir.join("managed.toml");
            std::fs::write(&f, text).unwrap();
            c.env("NEXO_MANAGED_FILE", f);
        }
        None => {
            c.env("NEXO_MANAGED_FILE", dir.join("no-such-managed.toml"));
        }
    }
    c.output().expect("nexo runs")
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn a_managed_deny_rule_beats_a_user_allow_rule() {
    let d = tempfile::tempdir().unwrap();
    let target = d.path().join("w.txt");
    let call = format!("tool:write {}", serde_json::json!({"path": target, "content": "hello"}));
    let user = format!("{MOCK}[permissions]\nallow = [\"write\"]\n");
    let o = nexo(d.path(), &user, None, &["-p", &call]);
    assert_eq!(o.status.code(), Some(0), "control: the user's allow rule lets the write through: {}", err(&o));
    assert!(target.exists());
    std::fs::remove_file(&target).unwrap();
    let o = nexo(d.path(), &user, Some("[permissions]\ndeny = [\"write\"]\n"), &["-p", &call]);
    assert_ne!(o.status.code(), Some(0), "{}", out(&o));
    assert!(!target.exists(), "the managed deny rule held");
}

#[test]
fn the_sandbox_cannot_be_loosened() {
    let d = tempfile::tempdir().unwrap();
    let managed = "[permissions]\nsandbox = \"workspace-write\"\nsandbox_network = false\n";
    let o = nexo(d.path(), MOCK, Some(managed), &["-p", "hi", "--sandbox", "off"]);
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    assert!(err(&o).contains("blocked by your organization's policy"), "{}", err(&o));
    // a stricter choice is the user's to make
    let o = nexo(d.path(), MOCK, Some(managed), &["-p", "hi", "--sandbox", "read-only"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    // the user's own file asking for less is raised, and doctor says so
    let user = format!("{MOCK}[permissions]\nsandbox = \"off\"\nsandbox_network = true\n");
    let o = nexo(d.path(), &user, Some(managed), &["doctor", "--offline"]);
    let text = out(&o);
    assert!(text.contains("sandbox off changed to workspace-write"), "{text}");
    assert!(text.contains("permissions.sandbox = workspace-write (at least)  (source: managed)"), "{text}");
}

#[test]
fn a_denied_model_is_refused_with_dash_p_model() {
    let d = tempfile::tempdir().unwrap();
    let managed = "[models]\ndeny = [\"other\"]\n";
    let o = nexo(d.path(), MOCK, Some(managed), &["-p", "hi", "--model", "other"]);
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    let e = err(&o);
    assert!(e.contains("blocked by your organization's policy (") && e.contains("managed.toml"), "{e}");
    let o = nexo(d.path(), MOCK, Some(managed), &["-p", "hi", "--model", "mock"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
}

#[cfg(unix)]
#[test]
fn an_mcp_server_not_on_the_allowlist_is_not_started() {
    let d = tempfile::tempdir().unwrap();
    let mark = d.path().join("started");
    let user = format!("{MOCK}[mcp.rogue]\ncommand = \"sh\"\nargs = [\"-c\", \"touch {}; sleep 1\"]\n", mark.display());
    nexo(d.path(), &user, None, &["-p", "hi"]);
    assert!(mark.exists(), "control: without a policy the server is started");
    std::fs::remove_file(&mark).unwrap();
    nexo(d.path(), &user, Some("[mcp]\nallow = [\"files\"]\n"), &["-p", "hi"]);
    assert!(!mark.exists(), "the server is not on the allowlist");
    let o = nexo(d.path(), &user, Some("[mcp]\nallow = [\"files\"]\n"), &["doctor", "--offline"]);
    assert!(out(&o).contains("MCP server \"rogue\" not started"), "{}", out(&o));
    // by command pattern
    nexo(d.path(), &user, Some("[mcp]\ndeny_commands = [\"sh *\"]\n"), &["-p", "hi"]);
    assert!(!mark.exists(), "the command is on the denylist");
}

#[cfg(unix)]
#[test]
fn user_hooks_are_off_when_disallowed_and_forced_hooks_run() {
    let d = tempfile::tempdir().unwrap();
    let (user_mark, forced_mark) = (d.path().join("user"), d.path().join("forced"));
    let user =
        format!("{MOCK}[[hooks]]\nevent = \"user_prompt_submit\"\ncommand = \"touch {}\"\n", user_mark.display());
    nexo(d.path(), &user, None, &["-p", "hi"]);
    assert!(user_mark.exists(), "control: the user's hook runs");
    std::fs::remove_file(&user_mark).unwrap();
    let managed = format!(
        "allow_user_hooks = false\n[[forced_hooks]]\nevent = \"user_prompt_submit\"\ncommand = \"touch {}\"\n",
        forced_mark.display()
    );
    let o = nexo(d.path(), &user, Some(&managed), &["-p", "hi"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(!user_mark.exists(), "the user's hook is off");
    assert!(forced_mark.exists(), "the forced hook ran");
}

#[test]
fn bypass_mode_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let managed = "disable_bypass_mode = true\n";
    for flags in [&["--yolo"][..], &["--permission-mode", "auto"]] {
        let mut args = vec!["-p", "hi"];
        args.extend_from_slice(flags);
        let o = nexo(d.path(), MOCK, Some(managed), &args);
        assert_eq!(o.status.code(), Some(1), "{flags:?}: {}", out(&o));
        assert!(err(&o).contains("blocked by your organization's policy"), "{}", err(&o));
    }
    let o = nexo(d.path(), MOCK, Some(managed), &["-p", "hi", "--permission-mode", "accept-edits"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
}

#[test]
fn a_malformed_managed_file_stops_startup() {
    let d = tempfile::tempdir().unwrap();
    for bad in ["this is = = not toml", "disable_webb = true\n", "[permissions]\nsandbox = \"loose\"\n"] {
        let o = nexo(d.path(), MOCK, Some(bad), &["-p", "hi"]);
        assert_eq!(o.status.code(), Some(1), "{bad}: {}", out(&o));
        let e = err(&o);
        assert!(e.contains("managed.toml") && e.contains("does not start"), "{e}");
        assert!(out(&o).is_empty(), "nothing ran: {}", out(&o));
    }
}

/// A folder in the home is outside every writable root of the sandbox (the temp directory is not).
#[cfg(target_os = "linux")]
#[test]
fn add_dir_reaches_the_sandbox_write_dirs() {
    let d = tempfile::tempdir().unwrap();
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else { return };
    let other = tempfile::Builder::new().prefix(".nexo-adddir-test-").tempdir_in(home).unwrap();
    let target = other.path().join("b.txt");
    let cmd = format!("echo x > {}", target.display());
    let call = format!("tool:bash {}", serde_json::json!({"command": cmd}));
    let base = ["-p", call.as_str(), "--permission-mode", "auto", "--sandbox", "workspace-write"];
    nexo(d.path(), MOCK, None, &base);
    if target.exists() {
        eprintln!("skipped: no OS sandbox here");
        return;
    }
    let dir = other.path().to_str().unwrap();
    let mut args = base.to_vec();
    args.extend(["--add-dir", dir]);
    let o = nexo(d.path(), MOCK, None, &args);
    assert!(target.exists(), "the folder is writable for a sandboxed command: {} {}", out(&o), err(&o));
}

/// A plugin folder whose only content is a hook that touches `mark` when a prompt is sent.
fn plugin_source(dir: &Path, mark: &Path) -> std::path::PathBuf {
    let src = dir.join("acme-src");
    std::fs::create_dir_all(src.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(src.join("hooks")).unwrap();
    std::fs::write(src.join(".claude-plugin/plugin.json"), r#"{"name":"acme","version":"1.0.0"}"#).unwrap();
    let hooks = serde_json::json!({"hooks": {"UserPromptSubmit": [{"hooks": [{"type": "command", "command": format!("touch {}", mark.display())}]}]}});
    std::fs::write(src.join("hooks/hooks.json"), hooks.to_string()).unwrap();
    src
}

#[test]
fn plugin_install_is_refused_for_a_scope_the_policy_forbids() {
    let d = tempfile::tempdir().unwrap();
    let src = plugin_source(d.path(), &d.path().join("mark"));
    let s = src.to_str().unwrap();
    let managed = "[extensions]\nuser_plugins = false\n";
    let o = nexo(d.path(), MOCK, Some(managed), &["plugin", "install", s, "--yes"]);
    assert_ne!(o.status.code(), Some(0), "{}", out(&o));
    assert!(err(&o).contains("blocked by your organization's policy"), "{}", err(&o));
    assert!(!d.path().join("plugins/acme").exists(), "nothing was installed");
    // the other scope is still allowed
    let o = nexo(d.path(), MOCK, Some(managed), &["plugin", "install", s, "--yes", "--scope", "project"]);
    assert_eq!(o.status.code(), Some(0), "{} {}", out(&o), err(&o));
    let o = nexo(
        d.path(),
        MOCK,
        Some("[extensions]\nproject_plugins = false\n"),
        &["plugin", "install", s, "--yes", "--scope", "project"],
    );
    assert_ne!(o.status.code(), Some(0), "{}", out(&o));
    assert!(err(&o).contains("blocked by your organization's policy"), "{}", err(&o));
}

#[cfg(unix)]
#[test]
fn plugins_of_a_forbidden_scope_are_not_loaded() {
    for (scope, key) in [("user", "user_plugins"), ("project", "project_plugins")] {
        let d = tempfile::tempdir().unwrap();
        let mark = d.path().join("mark");
        let src = plugin_source(d.path(), &mark);
        let o = nexo(d.path(), MOCK, None, &["plugin", "install", src.to_str().unwrap(), "--yes", "--scope", scope]);
        assert_eq!(o.status.code(), Some(0), "{} {}", out(&o), err(&o));
        nexo(d.path(), MOCK, None, &["-p", "hi"]);
        assert!(mark.exists(), "control: the {scope} plugin's hook runs");
        std::fs::remove_file(&mark).unwrap();
        nexo(d.path(), MOCK, Some(&format!("[extensions]\n{key} = false\n")), &["-p", "hi"]);
        assert!(!mark.exists(), "the {scope} plugin is not loaded");
        let other = if scope == "user" { "project_plugins" } else { "user_plugins" };
        nexo(d.path(), MOCK, Some(&format!("[extensions]\n{other} = false\n")), &["-p", "hi"]);
        assert!(mark.exists(), "the {scope} plugin still loads when only {other} is off");
    }
}

#[test]
fn the_audit_log_is_forced_on_at_the_pinned_path() {
    let d = tempfile::tempdir().unwrap();
    let pinned = d.path().join("pinned-audit.jsonl");
    let elsewhere = d.path().join("mine.jsonl");
    let user = format!("{MOCK}[audit]\nenabled = false\npath = \"{}\"\n", elsewhere.display());
    nexo(d.path(), &user, None, &["-p", "hi"]);
    assert!(!pinned.exists() && !elsewhere.exists(), "control: the log is off");
    let managed = format!("[audit]\nenabled = true\npath = \"{}\"\n", pinned.display());
    let o = nexo(d.path(), &user, Some(&managed), &["-p", "hi"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let log = std::fs::read_to_string(&pinned).expect("the pinned log is written");
    assert!(!log.trim().is_empty());
    assert!(!elsewhere.exists(), "the user's path is replaced");
    let o = nexo(d.path(), &user, Some(&managed), &["doctor", "--offline"]);
    let text = out(&o);
    assert!(text.contains("audit.enabled = true  (source: managed)"), "{text}");
    assert!(text.contains(&format!("audit.path = {}  (source: managed)", pinned.display())), "{text}");
}
