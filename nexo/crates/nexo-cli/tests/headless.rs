//! The binary itself, run headless against the built-in mock provider.

use std::process::Command;

const MOCK: &str =
    "provider = \"mock\"\nmodel = \"mock\"\n[providers.mock]\nprotocol = \"mock\"\nmodels = [\"mock\"]\n";

/// With the plain policy: the mock echoes the request word for word (the curator
/// would send it with its filler words out).
fn nexo(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    nexo_with(dir, &format!("{MOCK}[context]\npolicy = \"plain\"\n"), args)
}

fn nexo_with(dir: &std::path::Path, config: &str, args: &[&str]) -> std::process::Output {
    std::fs::write(dir.join("config.toml"), config).unwrap();
    Command::new(env!("CARGO_BIN_EXE_nexo"))
        .args(args)
        .current_dir(dir)
        .env("NEXO_CONFIG_DIR", dir)
        .env("NEXO_DATA_DIR", dir.join("data"))
        .env("NEXO_CACHE_DIR", dir.join("cache"))
        .output()
        .expect("nexo runs")
}

#[test]
fn a_headless_request_prints_the_reply_and_saves_the_session() {
    let d = tempfile::tempdir().unwrap();
    let out = nexo(d.path(), &["-p", "hello there"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "echo: hello there");
    let sessions = std::fs::read_dir(d.path().join("data/sessions")).unwrap().count();
    assert_eq!(sessions, 1, "the run is kept as a session");
}

#[test]
fn json_mode_streams_one_event_a_line() {
    let d = tempfile::tempdir().unwrap();
    let out = nexo(d.path(), &["-p", "hi", "--json"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    let events: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).expect("a JSON line")).collect();
    assert_eq!(events[0]["event"], "ready");
    assert!(events.iter().any(|e| e["event"] == "turn_end"), "{text}");
}

/// The default policy runs the curator from a folder with no curator checkout in
/// it: its package is built into the binary, and the title bar is told what it does.
#[test]
fn auto_runs_the_built_in_curator_anywhere() {
    let d = tempfile::tempdir().unwrap();
    let out = nexo_with(d.path(), MOCK, &["-p", "hello there", "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let ctx: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|e| e["event"] == "context")
        .collect();
    assert!(!ctx.is_empty(), "{text}");
    for e in &ctx {
        assert_eq!(e["policy"], "curator", "{e}");
        assert!(e["label"].as_str().unwrap().starts_with("curator"), "{e}");
        assert!(!e["label"].as_str().unwrap().contains("off"), "{e}");
    }
    assert!(text.contains("echo: hello there"), "the curator sent the request as written: {text}");
    // the curator runs in process: nothing is written out for it
    assert!(!d.path().join("cache/curator").exists(), "a curator package was written out");
}

#[test]
fn the_config_command_shows_where_things_live() {
    let d = tempfile::tempdir().unwrap();
    let out = nexo(d.path(), &["config"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("config.toml") && text.contains("sessions"), "{text}");
}

/// Run as root with a caller's `$HOME` and `$XDG_*` (what `sudo` often keeps), nexo still
/// lives in root's home; run as anyone else, it follows `$HOME`.
#[cfg(unix)]
#[test]
fn as_root_the_paths_are_roots_even_with_a_callers_home() {
    let d = tempfile::tempdir().unwrap();
    let fake = d.path().join("home/someone");
    std::fs::create_dir_all(&fake).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_nexo"))
        .arg("config")
        .current_dir(d.path())
        .env_remove("NEXO_CONFIG_DIR")
        .env_remove("NEXO_DATA_DIR")
        .env_remove("NEXO_CACHE_DIR")
        .env("HOME", &fake)
        .env("XDG_CONFIG_HOME", fake.join(".config"))
        .env("XDG_DATA_HOME", fake.join(".local/share"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let uid = Command::new("id").arg("-u").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if uid.ok().as_deref() == Some("0") {
        assert!(text.contains("/root/.nexo/config.toml"), "{text}");
        assert!(text.contains("/root/.nexo/sessions"), "{text}");
        assert!(!text.contains("someone"), "nothing in the caller's home: {text}");
    } else {
        // anyone else: their own home
        assert!(text.contains("someone/.nexo/config.toml"), "{text}");
    }
}

#[test]
fn a_key_kept_with_auth_login_sets_up_a_directory_provider() {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str], stdin: Option<&str>| {
        use std::io::Write;
        let mut c = Command::new(env!("CARGO_BIN_EXE_nexo"));
        c.args(args)
            .current_dir(dir.path())
            .env("NEXO_CONFIG_DIR", dir.path().join("config"))
            .env("NEXO_DATA_DIR", dir.path().join("data"))
            .env("NEXO_CACHE_DIR", dir.path().join("cache"))
            .env_remove("NEXO_PROVIDER")
            .env_remove("NEXO_MODEL")
            .env_remove("CHUTES_API_KEY")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = c.spawn().unwrap();
        child.stdin.take().unwrap().write_all(stdin.unwrap_or("").as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (out.status.code(), String::from_utf8_lossy(&out.stdout).to_string())
    };
    let (code, out) = run(&["auth", "login", "chutes", "--key", "-"], Some("test-key-123\n"));
    assert_eq!(code, Some(0), "{out}");
    let (_, out) = run(&["auth", "list"], None);
    assert!(out.contains("chutes") && out.contains("api key") && !out.contains("test-key-123"), "{out}");
    let (code, _) = run(&["use", "chutes"], None);
    assert_eq!(code, Some(0));
    let cfg = std::fs::read_to_string(dir.path().join("config/config.toml")).unwrap();
    assert!(cfg.contains("provider = \"chutes\""), "{cfg}");
    let (code, out) = run(&["doctor", "--offline"], None);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("stored api key"), "{out}");
    let (_, out) = run(&["providers", "--all", "--json"], None);
    let list: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(list.as_array().unwrap().len() > 150);
    let (code, _) = run(&["auth", "logout", "chutes"], None);
    assert_eq!(code, Some(0));
    let (code, out) = run(&["doctor", "--offline"], None);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("nexo auth login chutes"), "{out}");
    let (code, out) = run(&["guide"], None);
    assert_eq!(code, Some(0));
    assert!(out.contains("nexo auth login"));
}

/// A project's own config may not run commands until the person trusts it: the run says what it
/// left out, `nexo config trust` lists the project, and the next run applies all of it.
#[test]
fn an_untrusted_project_is_told_and_trusted_on_request() {
    let d = tempfile::tempdir().unwrap();
    let proj = d.path().join("proj");
    std::fs::create_dir_all(proj.join(".nexo")).unwrap();
    std::fs::write(proj.join(".nexo/config.toml"), "[ui]\nstatus_command = \"true\"\n").unwrap();
    let cwd = proj.to_string_lossy().to_string();
    let out = nexo(d.path(), &["--cwd", &cwd, "-p", "hi"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("is not trusted") && err.contains("ui.status_command"), "{err}");
    let out = nexo(d.path(), &["--cwd", &cwd, "config", "trust"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let list = std::fs::read_to_string(d.path().join("trusted-projects")).unwrap();
    assert_eq!(list.trim(), proj.canonicalize().unwrap().to_string_lossy());
    let out = nexo(d.path(), &["--cwd", &cwd, "-p", "hi"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("is not trusted"), "{err}");
}

#[test]
fn the_curator_spec_can_be_given_for_one_run() {
    let d = tempfile::tempdir().unwrap();
    let cfg = format!("{MOCK}[context]\npolicy = \"curator\"\ncurator_spec = \"eoq=24000\"\n");
    // a setting the curator cannot run, given on the command line, is named: the flag reached the curator
    let out = nexo_with(d.path(), &cfg, &["-p", "hi", "--json", "--curator-spec", "eoq=24000;rule=lru"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let all = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(all.contains("rule=lru"), "{all}");
    let out = nexo_with(d.path(), &cfg, &["-p", "hi", "--json"]);
    let all = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!all.contains("rule=lru"), "without the flag the file's spec holds: {all}");
}
