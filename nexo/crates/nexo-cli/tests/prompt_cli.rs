//! `nexo prompt` run as the binary: what it prints, `--tools`, `--builtin`, and `[compat] claude_code`.

use std::path::Path;
use std::process::{Command, Stdio};

fn nexo(config: &Path, claude_home: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_nexo"))
        .args(args)
        .current_dir(cwd)
        .env("NEXO_CONFIG_DIR", config)
        .env("NEXO_DATA_DIR", config.join("data"))
        .env("NEXO_CACHE_DIR", config.join("cache"))
        .env("NEXO_COMPAT_HOME", claude_home)
        .stdin(Stdio::null())
        .output()
        .expect("nexo runs")
}

fn out(o: &std::process::Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// A configuration folder with `config`, a home with a `~/.claude/CLAUDE.md` and a project with a `CLAUDE.md`.
fn setup(config: &str) -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
    let (cfg, home, proj) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(cfg.path().join("config.toml"), config).unwrap();
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    std::fs::write(home.path().join(".claude/CLAUDE.md"), "USER-CLAUDE-RULE").unwrap();
    std::fs::write(proj.path().join("CLAUDE.md"), "PROJECT-CLAUDE-RULE").unwrap();
    (cfg, home, proj)
}

#[test]
fn claude_code_files_are_read_unless_the_config_turns_them_off() {
    let (cfg, home, proj) = setup("");
    let on = out(&nexo(cfg.path(), home.path(), proj.path(), &["prompt"]));
    assert!(on.contains("USER-CLAUDE-RULE") && on.contains("PROJECT-CLAUDE-RULE"), "{on}");
    let (cfg, home, proj) = setup("[compat]\nclaude_code = false\n");
    let off = out(&nexo(cfg.path(), home.path(), proj.path(), &["prompt"]));
    assert!(!off.contains("CLAUDE-RULE") && !off.contains("Instructions from"), "{off}");
    assert!(off.contains("Working directory"), "the rest of the prompt stays");
}

#[test]
fn builtin_is_only_nexos_own_part_of_the_prompt() {
    let (cfg, home, proj) = setup("");
    std::fs::write(proj.path().join("AGENTS.md"), "PROJECT-AGENTS-RULE").unwrap();
    let b = out(&nexo(cfg.path(), home.path(), proj.path(), &["prompt", "--builtin"]));
    assert!(b.starts_with("You are nexo,") && b.contains("You can do:"), "{b}");
    for not in ["CLAUDE-RULE", "AGENTS-RULE", "Working directory", "Instructions from", "preferences"] {
        assert!(!b.contains(not), "{not}: {b}");
    }
}

#[test]
fn tools_lists_each_definition_the_model_gets() {
    let (cfg, home, proj) = setup("");
    let t = out(&nexo(cfg.path(), home.path(), proj.path(), &["prompt", "--tools"]));
    for name in ["bash", "read", "edit", "task", "ask_user", "web_search"] {
        assert!(t.contains(&format!("=== {name} ===\ndescription:\n")), "{name}: {t}");
    }
    // not offered in a normal session: plan mode and goals only
    assert!(!t.contains("=== exit_plan ===") && !t.contains("=== goal_brief ==="), "{t}");
    assert!(t.contains("\nparameters:\n{\n") && t.contains("\"type\": \"object\""), "{t}");
    assert!(!t.contains("You are nexo,"), "tools only, not the prompt");
}

#[test]
fn tools_and_builtin_do_not_go_together() {
    let (cfg, home, proj) = setup("");
    let o = nexo(cfg.path(), home.path(), proj.path(), &["prompt", "--tools", "--builtin"]);
    assert!(!o.status.success());
}

#[test]
fn window_selects_the_compact_text_for_a_small_model() {
    let (cfg, home, proj) = setup("");
    let run = |args: &[&str]| out(&nexo(cfg.path(), home.path(), proj.path(), args));
    let small = run(&["prompt", "--builtin", "--window", "8000"]);
    assert!(
        small.starts_with("You are nexo,") && !small.contains("How to write code") && small.len() <= 2_600,
        "{small}"
    );
    let tools = run(&["prompt", "--tools", "--window", "8000"]);
    // tools that bring their own text (skills, servers) keep it, so the size is checked on the built-in ones
    assert!(tools.contains("=== bash ===\nRun a shell command") && !tools.contains("description:\n"), "{tools}");
    assert!(tools.len() < run(&["prompt", "--tools"]).len() / 2, "{tools}");
    // above 32k, or without the flag: today's text
    assert_eq!(run(&["prompt", "--builtin", "--window", "200000"]), run(&["prompt", "--builtin"]));
    assert_eq!(run(&["prompt", "--tools", "--window", "32001"]), run(&["prompt", "--tools"]));
    assert!(run(&["prompt", "--builtin"]).contains("How to write code"));
}
