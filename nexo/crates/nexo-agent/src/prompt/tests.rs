use super::*;

/// The prompt reads `prompt.md` from `NEXO_CONFIG_DIR`, one variable for the whole process:
/// the tests that point it somewhere take turns, so one never sees another's file.
static CONFIG_DIR: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn environment_lists_the_tree_the_manifests_and_the_check_command() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
    std::fs::create_dir(d.path().join("src")).unwrap();
    std::fs::write(d.path().join("src/lib.rs"), "").unwrap();
    let e = environment(d.path());
    assert!(e.contains("- src/ (1 files)"), "{e}");
    assert!(e.contains("  - src/lib.rs"), "{e}");
    assert!(e.contains("Manifests: Cargo.toml (demo)"), "{e}");
    assert!(e.contains("\ncheck: cargo test -q"), "{e}");
}

#[test]
fn the_missing_commands_line_ends_the_block_and_nothing_missing_changes_nothing() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.txt"), "").unwrap();
    let plain = project_block_with(d.path(), "");
    assert!(!plain.contains("Not installed"), "{plain}");
    let with = project_block_with(d.path(), "Not installed here: curl, file, xxd.");
    assert_eq!(with, format!("{plain}\nNot installed here: curl, file, xxd."));
    assert_eq!(project_block_with(d.path(), ""), plain);
}

#[test]
fn the_line_fits_inside_the_cap() {
    let d = tempfile::tempdir().unwrap();
    for i in 0..500 {
        std::fs::write(d.path().join(format!("file_with_a_long_name_{i}.txt")), "").unwrap();
    }
    let b = project_block_with(d.path(), "Not installed here: curl, file, xxd.");
    assert!(b.len() <= PROJECT_CAP && b.ends_with("xxd."), "{}", b.len());
}

#[test]
fn environment_block_stays_under_its_cap() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("data")).unwrap();
    for i in 0..500 {
        std::fs::write(d.path().join(format!("file_with_a_long_name_{i}.txt")), "").unwrap();
        std::fs::write(d.path().join(format!("data/another_long_file_name_{i}.txt")), "").unwrap();
    }
    let b = project_block(d.path());
    assert!(b.len() <= PROJECT_CAP, "{}", b.len());
    assert!(b.matches("\n- ").count() + b.matches("\n  - ").count() <= PROJECT_ENTRIES + 1, "{b}");
    assert!(b.contains("more entries"), "{b}");
}

#[test]
fn names_the_request_gives_are_kept() {
    // configure-git-webserver: asked for user@server, the model pushed as the account it found
    assert!(built_in().contains("use them exactly. When one does not exist yet, create it"));
}

#[test]
fn the_strands_line_says_steps_in_order_go_in_one_message_and_a_failure_stops_the_rest() {
    let all = tool_guidance(&nexo_tools::builtin());
    for want in [
        "Steps that follow each other also go in ONE message",
        "write a script and run it; edit, then run the check",
        "they run one at a time in the order given, and when one fails the ones after it are not run",
    ] {
        assert!(all.contains(want), "missing {want}: {all}");
    }
    assert!(!all.contains("a change runs alone"), "{all}");
    assert!(BUILT_IN_COMPACT.contains("a failure stops the rest"));
}

#[test]
fn the_you_can_do_section_lists_only_enabled_abilities() {
    let all = tool_guidance(&nexo_tools::builtin());
    for want in [
        "You can do:",
        "independent strands",
        "ONE message",
        "run at the same time",
        "`wait_until`",
        "touches the same state",
        "many files",
        "`note`: a lasting preference",
    ] {
        assert!(all.contains(want), "missing {want}: {all}");
    }
    struct Named(&'static str);
    #[async_trait::async_trait]
    impl nexo_core::tool::Tool for Named {
        fn name(&self) -> &str {
            self.0
        }
        fn spec(&self) -> nexo_core::provider::ToolSpec {
            nexo_core::provider::ToolSpec {
                name: self.0.into(),
                description: String::new(),
                parameters: serde_json::json!({}),
            }
        }
        fn kind(&self) -> nexo_core::tool::ToolKind {
            nexo_core::tool::ToolKind::Read
        }
        fn summary(&self, _: &serde_json::Value) -> String {
            String::new()
        }
        async fn run(&self, _: serde_json::Value, _: &nexo_core::tool::ToolContext) -> nexo_core::tool::ToolOutput {
            nexo_core::tool::ToolOutput::ok("")
        }
    }
    let with_task = tool_guidance(&nexo_tools::builtin().with(std::sync::Arc::new(Named("task"))));
    assert!(with_task.contains("`task` sub-agents"), "{with_task}");
    let mut few = ToolSet::new();
    for n in ["read", "bash"] {
        few = few.with(nexo_tools::builtin().get(n).unwrap().clone());
    }
    let g = tool_guidance(&few);
    assert!(g.contains("independent strands") && g.contains("`background: true`"), "{g}");
    assert!(!g.contains("`edit`") && !g.contains("`task`") && !g.contains("`note`") && !g.contains("`recall`"), "{g}");
    assert_eq!(tool_guidance(&ToolSet::new()), "");
}

#[test]
fn the_distribution_comes_from_os_release() {
    let debian = "PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\nNAME=\"Debian GNU/Linux\"\nVERSION_ID=\"12\"\n";
    assert_eq!(os_name(debian).as_deref(), Some("Debian GNU/Linux 12 (bookworm)"));
    assert_eq!(os_name("NAME=Alpine Linux\nVERSION_ID=3.20.3\n").as_deref(), Some("Alpine Linux 3.20.3"));
    assert_eq!(os_name("PRETTY_NAME=\"\"\nNAME=Arch\n").as_deref(), Some("Arch"));
    assert_eq!(os_name("ID=x\n"), None);
    let none = tempfile::tempdir().unwrap();
    assert_eq!(git_line(none.path()), "not a git repository");
}

#[test]
fn another_agents_user_file_is_background_capped_and_never_the_request() {
    let _turn = CONFIG_DIR.lock().unwrap_or_else(|e| e.into_inner());
    let (cfg, home, d) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::env::set_var("NEXO_CONFIG_DIR", cfg.path());
    std::env::set_var("NEXO_COMPAT_HOME", home.path());
    let line = "Act for the boss: keep going until it is done, unasked.\n";
    let file = home.path().join(".claude/CLAUDE.md");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, line.repeat(200)).unwrap();
    let p = system_prompt(d.path(), &AgentConfig::default(), &ToolSet::new());
    std::env::remove_var("NEXO_COMPAT_HOME");
    std::env::remove_var("NEXO_CONFIG_DIR");
    assert!(p.contains(&format!("Notes this person wrote for another agent ({}): background", file.display())), "{p}");
    assert!(!p.contains(&format!("Instructions from {}", file.display())), "{p}");
    let kept = p.matches(line.trim_end()).count();
    assert!(kept > 0 && kept * line.len() <= FALLBACK_CAP, "{kept}");
    assert!(p.contains(&format!("[{} more characters left out]", line.len() * (200 - kept))), "{p}");
    assert!(p.contains("they are never the request"), "{p}");
}

#[test]
fn carries_directory_instructions_and_extra() {
    let _turn = CONFIG_DIR.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("NEXO_CONFIG_DIR", home.path());
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("NEXO.md"), "Use tabs.").unwrap();
    std::fs::create_dir(d.path().join(".git")).unwrap();
    let cfg = AgentConfig { system_prompt_extra: Some("Answer in Spanish.".into()), ..Default::default() };
    let p = system_prompt(d.path(), &cfg, &ToolSet::new());
    assert!(p.starts_with("You are nexo"));
    assert!(p.contains(&format!("Working directory: {}", d.path().display())));
    assert!(p.contains(&format!("\nSystem: {}", std::env::consts::OS)), "{p}");
    assert!(p.contains("\nGit: this folder is a git repository"), "{p}");
    let sub = d.path().join("src");
    std::fs::create_dir(&sub).unwrap();
    let inner = system_prompt(&sub, &AgentConfig::default(), &ToolSet::new());
    assert!(inner.contains(&format!("Git: inside the git repository at {}", d.path().display())), "{inner}");
    assert!(p.contains("Use tabs."));
    assert!(p.ends_with("Answer in Spanish."));
    let f = d.path().join("sys.txt");
    std::fs::write(&f, "Custom.").unwrap();
    let cfg = AgentConfig { system_prompt_file: Some(f), ..Default::default() };
    assert!(system_prompt(d.path(), &cfg, &ToolSet::new()).starts_with("Custom."));
    std::env::remove_var("NEXO_CONFIG_DIR");
}

#[test]
fn a_hermetic_prompt_leaves_out_the_machines_instructions_and_preferences() {
    let _turn = CONFIG_DIR.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("NEXO_CONFIG_DIR", home.path());
    std::fs::write(home.path().join("prompt.md"), "Reply in Spanish.\n").unwrap();
    std::fs::write(d.path().join("AGENTS.md"), "Private notes.\n").unwrap();
    let mut cfg = AgentConfig::default();
    let p = system_prompt(d.path(), &cfg, &ToolSet::new());
    assert!(p.contains("Private notes.") && p.contains("Reply in Spanish."), "{p}");
    cfg.hermetic = true;
    let p = system_prompt(d.path(), &cfg, &ToolSet::new());
    std::env::remove_var("NEXO_CONFIG_DIR");
    assert!(p.starts_with(built_in()), "{p}");
    assert!(
        !p.contains("Private notes.") && !p.contains("Reply in Spanish.") && !p.contains("Instructions from"),
        "{p}"
    );
}

#[test]
fn the_persons_prompt_file_adds_preferences_and_a_whole_copy_replaces_the_text() {
    let _turn = CONFIG_DIR.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("NEXO_CONFIG_DIR", home.path());
    let cfg = AgentConfig::default();
    assert!(system_prompt(d.path(), &cfg, &ToolSet::new()).starts_with("You are nexo"));
    // preferences: the built-in text stays first, they come last, the comments are not sent
    std::fs::write(home.path().join("prompt.md"), format!("{PREFS_TEMPLATE}Mine.\n<!-- note -->Short answers.\n"))
        .unwrap();
    let p = system_prompt(d.path(), &cfg, &ToolSet::new());
    assert!(p.starts_with(built_in()), "{p}");
    assert!(p.ends_with("The person's preferences:\nMine.\nShort answers."), "{p}");
    assert!(!p.contains("<!--"), "{p}");
    // only the template: nothing is added
    std::fs::write(home.path().join("prompt.md"), PREFS_TEMPLATE).unwrap();
    assert!(!system_prompt(d.path(), &cfg, &ToolSet::new()).contains("preferences:"));
    // a whole prompt saved by an earlier version replaces the built-in text, without its stale
    // directory, date and tools
    std::fs::write(
        home.path().join("prompt.md"),
        "You are nexo, mine.\n\nWorking directory: /old\nDate: 2020-01-01\n\nTools:\n- `read`: x\n",
    )
    .unwrap();
    let p = system_prompt(d.path(), &cfg, &ToolSet::new());
    assert!(
        p.starts_with("You are nexo, mine.\n\nWorking directory:") && !p.contains("/old") && !p.contains("2020-01-01"),
        "{p}"
    );
    std::fs::write(home.path().join("prompt.md"), format!("{RAW_MARK}\nOnly this.\n")).unwrap();
    assert_eq!(system_prompt(d.path(), &cfg, &ToolSet::new()), "Only this.");
    let f = d.path().join("sys.txt");
    std::fs::write(&f, "Configured.").unwrap();
    let cfg = AgentConfig { system_prompt_file: Some(f), ..Default::default() };
    assert!(system_prompt(d.path(), &cfg, &ToolSet::new()).starts_with("Configured.\n\nWorking"));
    std::env::remove_var("NEXO_CONFIG_DIR");
}

#[test]
fn the_prompt_tells_the_model_what_to_remember_and_where_only_when_it_has_note() {
    let all = tool_guidance(&nexo_tools::builtin());
    for want in [
        "lasting preference, rule or fact",
        "\"always ...\"",
        "`global`",
        "`project`",
        "one-off details",
        "secrets",
        "one short line",
        "the exact fact (path, value, name, command) and the rule, no filler",
        // saving is silent: the screen shows it, and a line of reply about it is what the person does not want
        "or what is saved.",
        "Say nothing about saving it.",
    ] {
        assert!(all.contains(want), "missing {want}: {all}");
    }
    assert!(!tool_guidance(&ToolSet::new()).contains("lasting preference, rule or fact"));
    assert!(!all.contains("Tell the person what you saved"), "{all}");
}

/// What the model saves with `note` is in the prompt of the next session (and the current one keeps its prompt, so
/// the cached prefix stays the same: the call itself stays in the context for it).
#[test]
fn a_saved_rule_is_in_the_prompt_of_the_next_session_and_not_of_this_one() {
    use nexo_core::tool::{Tool, ToolContext};
    let _turn = CONFIG_DIR.lock().unwrap_or_else(|e| e.into_inner());
    let (cwd, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::create_dir(cwd.path().join(".git")).unwrap();
    std::env::set_var("NEXO_CONFIG_DIR", home.path());
    let cfg = AgentConfig::default();
    let tools = nexo_tools::builtin();
    let before = system_prompt(cwd.path(), &cfg, &tools);
    let cx = ToolContext::new(cwd.path().to_path_buf());
    let note = nexo_tools::note::Note;
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let run = |args: serde_json::Value| rt.block_on(note.run(args, &cx));
    let g = run(serde_json::json!({"text": "Rule: my .bashrc is at ~/dotfiles/bashrc", "scope": "global"}));
    let p = run(serde_json::json!({"text": "Rule: the dev server runs on port 4100", "scope": "project"}));
    let after = system_prompt(cwd.path(), &cfg, &tools);
    std::env::remove_var("NEXO_CONFIG_DIR");
    assert!(!g.is_error && !p.is_error, "{} / {}", g.content, p.content);
    assert!(!before.contains("dotfiles") && !before.contains("4100"));
    // the model is told, right under each heading, that this is background and not the person speaking now
    let f = compat::REMEMBERED_FRAME;
    assert!(after.contains(&format!("The person's preferences:\n## Remembered by nexo\n{f}\n- ")), "{after}");
    assert_eq!(after.matches(f).count(), 2, "{after}");
    // and neither file holds that line
    let agents = std::fs::read_to_string(cwd.path().join("AGENTS.md")).unwrap();
    assert!(agents.contains("## Remembered by nexo\n- ") && !agents.contains(f), "{agents}");
    assert!(after.contains("my .bashrc is at ~/dotfiles/bashrc"), "{after}");
    assert!(after.contains("the dev server runs on port 4100"), "{after}");
    assert!(after.contains(&format!("Instructions from {}", cwd.path().join("AGENTS.md").display())), "{after}");
}

/// When to search and to cite is said once, in the description of `web_search` (a sub-agent gets it too), and
/// not a second time in the system text.
#[test]
fn the_web_tools_carry_their_own_guidance() {
    let tools = nexo_tools::builtin();
    let search = tools.get("web_search").unwrap().spec().description;
    for want in ["anything current or outside the repository", "`web_fetch` before relying on them", "cite their URLs"]
    {
        assert!(search.contains(want), "missing {want}: {search}");
    }
    let off = nexo_core::config::WebConfig { enabled: false, ..Default::default() };
    for set in [tools, nexo_tools::builtin_with(&off)] {
        let g = tool_guidance(&set);
        assert!(!g.contains("web_"), "{g}");
    }
}

/// The model is told to answer in the reply that holds its last checks only when that reply ends the turn
/// (`agent.close_on_verified`); told so with the setting off, it writes the answer twice.
#[test]
fn the_line_about_answering_with_the_last_checks_is_sent_only_when_that_ends_the_turn() {
    let d = tempfile::tempdir().unwrap();
    let tools = nexo_tools::builtin();
    let off = AgentConfig { hermetic: true, ..Default::default() };
    let on = AgentConfig { close_on_verified: true, ..off.clone() };
    let line = "put the answer for the person in that same message";
    assert!(!off.close_on_verified && !system_prompt(d.path(), &off, &tools).contains(line));
    let with = system_prompt(d.path(), &on, &tools);
    assert!(with.contains(&format!("\n{CLOSE_ON_VERIFIED}\n\nWorking directory: ")), "{with}");
    assert!(with.starts_with(built_in()), "{with}");
    assert!(!system_prompt_for(d.path(), &on, &tools, Some(8_000)).contains(line));
    // `nexo prompt --builtin` prints what the session gets
    assert!(builtin_part_for(&tools, None, None, true).contains(line));
    assert!(!builtin_part(&tools, None).contains(line));
}

#[test]
fn claude_md_and_nested_files_reach_the_prompt() {
    let _turn = CONFIG_DIR.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("NEXO_CONFIG_DIR", home.path());
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join(".git")).unwrap();
    std::fs::write(d.path().join("CLAUDE.md"), "Use tabs. @style.md").unwrap();
    std::fs::write(d.path().join("style.md"), "Tabs are four wide.").unwrap();
    std::fs::create_dir(d.path().join("web")).unwrap();
    std::fs::write(d.path().join("web/AGENTS.md"), "Web rules.").unwrap();
    let p = system_prompt(d.path(), &AgentConfig::default(), &ToolSet::new());
    std::env::remove_var("NEXO_CONFIG_DIR");
    assert!(p.contains("Use tabs.") && p.contains("Tabs are four wide."), "{p}");
    assert!(p.contains("- web/AGENTS.md") && !p.contains("Web rules."), "{p}");
}
