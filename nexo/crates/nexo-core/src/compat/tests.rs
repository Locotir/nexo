use super::*;

fn w(p: PathBuf, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join(".git")).unwrap();
    d
}

const CLAUDE_HOOKS: &str = r#"{"hooks": {"PreToolUse": [{"matcher": "Bash|Edit", "hooks": [{"type": "command", "command": "check.sh", "timeout": 5}, {"type": "prompt", "command": "x"}]}], "Stop": [{"hooks": [{"command": "done.sh"}]}], "Whatever": [{"hooks": [{"command": "no.sh"}]}]}}"#;

#[test]
fn claude_settings_hooks_load_only_in_a_trusted_project_with_nexo_tool_names() {
    let (p, t) = (repo(), tempfile::tempdir().unwrap());
    w(p.path().join(".claude/settings.json"), CLAUDE_HOOKS);
    let trust = t.path().join("trusted");
    let (none, note) = claude_hooks(p.path(), &trust);
    assert!(none.is_empty() && note.unwrap().contains("nexo config trust"));
    crate::config::trust_project(p.path(), &trust).unwrap();
    let (got, note) = claude_hooks(p.path(), &trust);
    assert!(note.is_none());
    let seen: Vec<(String, Option<String>, String, Option<u64>)> =
        got.iter().map(|h| (h.event.clone(), h.matcher.clone(), h.command.clone(), h.timeout)).collect();
    assert_eq!(
        seen,
        [
            ("pre_tool".into(), Some("bash|edit".into()), "check.sh".into(), Some(5)),
            ("stop".into(), None, "done.sh".into(), None),
        ]
    );
    assert_eq!(got[0].source, "project .claude/settings.json");
}

#[test]
fn a_project_at_the_home_folder_brings_no_claude_hooks() {
    // there `.claude/settings.json` is Claude Code's own user file, trusted or not
    let (p, t) = (repo(), tempfile::tempdir().unwrap());
    w(p.path().join(".claude/settings.json"), CLAUDE_HOOKS);
    let trust = t.path().join("trusted");
    crate::config::trust_project(p.path(), &trust).unwrap();
    let below = p.path().join("work/dir");
    std::fs::create_dir_all(&below).unwrap();
    for cwd in [p.path(), below.as_path()] {
        assert_eq!(claude_hooks_in(cwd, &trust, Some(p.path())), (Vec::new(), None));
        assert_eq!(claude_hooks_in(cwd, &trust, Some(t.path())).0.len(), 2);
    }
}

#[test]
fn claude_matchers_become_nexo_tool_names() {
    assert_eq!(map_matcher("Bash").as_deref(), Some("bash"));
    assert_eq!(map_matcher("Edit|Write").as_deref(), Some("edit|write"));
    assert_eq!(map_matcher("*"), None);
    assert_eq!(map_matcher(""), None);
}

#[test]
fn agents_md_wins_over_claude_md_and_the_local_file_adds() {
    let (p, cfg, home) = (repo(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    w(p.path().join("AGENTS.md"), "agents");
    w(p.path().join("CLAUDE.md"), "claude");
    w(p.path().join("CLAUDE.local.md"), "mine");
    let got: Vec<String> = instructions(p.path(), cfg.path(), Some(home.path())).into_iter().map(|i| i.text).collect();
    assert_eq!(got, ["agents", "mine"]);
    std::fs::remove_file(p.path().join("AGENTS.md")).unwrap();
    let got: Vec<String> = instructions(p.path(), cfg.path(), Some(home.path())).into_iter().map(|i| i.text).collect();
    assert_eq!(got, ["claude", "mine"]);
}

#[test]
fn user_level_fallbacks_only_when_nexo_has_none() {
    let (p, cfg, home) = (repo(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    w(home.path().join(".claude/CLAUDE.md"), "c-user");
    w(home.path().join(".codex/AGENTS.md"), "x-user");
    let got: Vec<String> = instructions(p.path(), cfg.path(), Some(home.path())).into_iter().map(|i| i.text).collect();
    assert_eq!(got, ["x-user", "c-user"]);
    w(cfg.path().join("AGENTS.md"), "own");
    let got = instructions(p.path(), cfg.path(), Some(home.path()));
    assert_eq!(got.len(), 1);
    assert_eq!((got[0].text.as_str(), got[0].origin), ("own", "user"));
}

#[test]
fn the_persons_home_is_no_project_seen_from_below_and_one_only_as_a_real_repository() {
    // a home holding another tool's state notes, and a folder below it that is no repository
    let (home, cfg, other) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let h = home.path();
    w(h.join("AGENTS.md"), "Read this first: the lanes and their state.");
    w(h.join("CLAUDE.md"), "Act for the boss, unasked.");
    w(h.join("work/AGENTS.md"), "Work rules.");
    w(other.path().join(".claude/CLAUDE.md"), "Another agent's mandate.");
    let sub = h.join("Downloads");
    std::fs::create_dir_all(&sub).unwrap();
    let none_from = |cwd: &Path| {
        let got = instructions_with(cwd, cfg.path(), None, true, Some(h));
        assert!(got.is_empty(), "{cwd:?}: {got:?}");
        assert!(nested_instructions_with(cwd, true, Some(h)).is_empty(), "{cwd:?}");
    };
    // no `.git`, then a stray `.git` folder that is no repository: the same
    for stray in [false, true] {
        if stray {
            std::fs::create_dir_all(h.join(".git/info")).unwrap();
        }
        none_from(h);
        none_from(&sub);
        assert!(project_dirs(h, Some(h)).is_empty() && project_dirs(&sub, Some(h)) == [sub.as_path()]);
        assert_eq!(project_root_with(&sub, Some(h)), sub);
        // what /memory opens and where a project rule goes: nexo's own user file in the home (never seeded
        // from another agent's), a new file below it
        assert_eq!(project_file_with(h, true, Some(h), cfg.path()), cfg.path().join("AGENTS.md"));
        assert_eq!(project_file_with(&sub, true, Some(h), cfg.path()), sub.join("AGENTS.md"));
        let (f, seed) = rules_file_with("project", h, cfg.path(), Some(other.path()), Some(h)).unwrap();
        assert_eq!((f, seed), (cfg.path().join("AGENTS.md"), None));
        assert_eq!(rules_file_with("project", &sub, cfg.path(), None, Some(h)).unwrap().0, sub.join("AGENTS.md"));
    }
    // a repository below the home is a project
    let work = h.join("work");
    std::fs::create_dir_all(work.join(".git")).unwrap();
    let got: Vec<String> =
        instructions_with(&work, cfg.path(), None, true, Some(h)).into_iter().map(|i| i.text).collect();
    assert_eq!(got, ["Work rules."]);
    // a home that is a real repository (a container whose home is the project) is one in the home itself,
    // never from a folder below it
    w(h.join(".git/HEAD"), "ref: refs/heads/main\n");
    let got: Vec<String> = instructions_with(h, cfg.path(), None, true, Some(h)).into_iter().map(|i| i.text).collect();
    assert_eq!(got, ["Read this first: the lanes and their state."]);
    none_from(&sub);
}

#[test]
fn memory_opens_the_nearest_project_file_and_starts_the_users_own_from_the_fallback() {
    let (p, cfg) = (repo(), tempfile::tempdir().unwrap());
    let sub = p.path().join("web");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(project_file(&sub), sub.join("AGENTS.md"), "none yet: a new one where the person is");
    w(p.path().join("CLAUDE.md"), "Root rules.");
    assert_eq!(project_file(&sub), p.path().join("CLAUDE.md"));
    w(sub.join("NEXO.md"), "Web rules.");
    assert_eq!(project_file(&sub), sub.join("NEXO.md"));

    let home = tempfile::tempdir().unwrap();
    assert_eq!(user_file(cfg.path(), Some(home.path())), (cfg.path().join("AGENTS.md"), None));
    w(home.path().join(".claude/CLAUDE.md"), "Answer in Spanish.");
    let (path, seed) = user_file(cfg.path(), Some(home.path()));
    assert_eq!(path, cfg.path().join("AGENTS.md"));
    assert_eq!(seed.as_deref(), Some("Answer in Spanish.\n"), "what nexo read stays");
    w(cfg.path().join("NEXO.md"), "Mine.");
    assert_eq!(user_file(cfg.path(), Some(home.path())), (cfg.path().join("NEXO.md"), None));
}
#[test]
fn with_claude_code_off_no_file_of_it_is_read() {
    let (p, cfg, home) = (repo(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    w(home.path().join(".claude/CLAUDE.md"), "c-user");
    w(home.path().join(".codex/AGENTS.md"), "x-user");
    w(p.path().join("CLAUDE.md"), "claude");
    w(p.path().join("CLAUDE.local.md"), "mine");
    w(p.path().join("web/CLAUDE.md"), "web");
    w(p.path().join("web/AGENTS.md"), "web agents");
    let texts = |claude| -> Vec<String> {
        instructions_with(p.path(), cfg.path(), Some(home.path()), claude, None).into_iter().map(|i| i.text).collect()
    };
    assert_eq!(texts(true), ["x-user", "c-user", "claude", "mine"]);
    assert_eq!(texts(false), ["x-user"], "the codex file and nexo's own stay");
    w(p.path().join("AGENTS.md"), "agents");
    assert_eq!(texts(false), ["x-user", "agents"]);
    assert_eq!(
        nested_instructions_with(p.path(), true, None),
        [PathBuf::from("web/AGENTS.md"), PathBuf::from("web/CLAUDE.md")]
    );
    assert_eq!(nested_instructions_with(p.path(), false, None), [PathBuf::from("web/AGENTS.md")]);

    let bare = repo();
    w(bare.path().join("CLAUDE.md"), "Root rules.");
    assert_eq!(project_file_with(bare.path(), true, None, bare.path()), bare.path().join("CLAUDE.md"));
    assert_eq!(project_file_with(bare.path(), false, None, bare.path()), bare.path().join("AGENTS.md"));
    let (path, seed) = user_file_with(cfg.path(), Some(home.path()), false);
    assert_eq!((path, seed.as_deref()), (cfg.path().join("AGENTS.md"), Some("x-user\n")));
    let (_, seed) = user_file_with(cfg.path(), Some(home.path()), true);
    assert_eq!(seed.as_deref(), Some("x-user\n\nc-user\n"));
}

#[test]
fn with_claude_code_off_its_command_and_agent_folders_are_not_sources() {
    let (p, cfg, home) = (repo(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let trust = cfg.path().join("trusted");
    std::fs::write(&trust, format!("{}\n", p.path().canonicalize().unwrap().display())).unwrap();
    let dirs = |claude_sub| -> Vec<String> {
        sources_with(p.path(), cfg.path(), &trust, Some(home.path()), "commands", claude_sub, Some("prompts"))
            .into_iter()
            .map(|s| s.origin)
            .collect()
    };
    assert!(dirs(Some("commands")).iter().any(|o| o.contains(".claude")));
    let off = dirs(None);
    assert!(off.iter().all(|o| !o.contains(".claude")), "{off:?}");
    assert!(off.iter().any(|o| o.contains(".codex")), "{off:?}");
}

#[test]
fn imports_are_relative_limited_and_never_cycle() {
    let (p, cfg) = (repo(), tempfile::tempdir().unwrap());
    w(
        p.path().join("AGENTS.md"),
        "Top\nSee @docs/a.md for more.\n```\n@docs/a.md\n```\nmail me@docs/a.md and @types/node",
    );
    w(p.path().join("docs/a.md"), "A text\n@b.md\n@../AGENTS.md");
    w(p.path().join("docs/b.md"), "B text");
    let t = &instructions(p.path(), cfg.path(), None)[0].text;
    assert!(t.contains("A text") && t.contains("B text"), "{t}");
    assert_eq!(t.matches("A text").count(), 1, "the fenced mention and the mid-word one do not import: {t}");
    assert_eq!(t.matches("Top").count(), 1, "the cycle back to the top file is skipped: {t}");
    assert!(t.contains("@types/node"));
    // depth: a chain of files stops after 5 hops
    for i in 0..8 {
        w(p.path().join(format!("c{i}.md")), &format!("level{i}\n@c{}.md", i + 1));
    }
    w(p.path().join("AGENTS.md"), "@c0.md");
    let t = &instructions(p.path(), cfg.path(), None)[0].text;
    assert!(t.contains("level4") && !t.contains("level5"), "{t}");
}

#[test]
fn a_key_file_is_never_imported() {
    let (p, cfg) = (repo(), tempfile::tempdir().unwrap());
    w(p.path().join(".env"), "SECRET=1");
    w(p.path().join("AGENTS.md"), "x\n@.env");
    assert!(!instructions(p.path(), cfg.path(), None)[0].text.contains("SECRET"));
}

#[test]
fn nested_files_are_listed_but_those_on_the_way_are_loaded() {
    let (p, cfg) = (repo(), tempfile::tempdir().unwrap());
    w(p.path().join("AGENTS.md"), "root");
    w(p.path().join("web/CLAUDE.md"), "web");
    w(p.path().join("api/AGENTS.md"), "api");
    w(p.path().join("api/src/CLAUDE.md"), "deep");
    let cwd = p.path().join("api");
    let got: Vec<String> = instructions(&cwd, cfg.path(), None).into_iter().map(|i| i.text).collect();
    assert_eq!(got, ["api", "root"]);
    let nested = nested_instructions(&cwd);
    assert_eq!(nested, [PathBuf::from("api/src/CLAUDE.md"), PathBuf::from("web/CLAUDE.md")]);
}

#[test]
fn commands_nest_with_colons_and_nexo_wins() {
    let (p, cfg, home) = (repo(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let trust = cfg.path().join("trusted-projects");
    w(home.path().join(".codex/prompts/ship.md"), "codex ship");
    w(
        p.path().join(".claude/commands/ship.md"),
        "---\ndescription: Ship it\nargument-hint: [env]\nallowed-tools: Bash(git:*)\n---\nclaude ship",
    );
    w(p.path().join(".claude/commands/git/sync.md"), "# Sync\nsync");
    w(cfg.path().join("commands/ship.md"), "nexo ship");
    let none = |_: &str| false;
    let before = commands(p.path(), cfg.path(), &trust, Some(home.path()), &none);
    let names: Vec<&str> = before.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["ship"], "an untrusted project's commands are left out");
    assert_eq!(before[0].body, "nexo ship");
    crate::config::trust_project(p.path(), &trust).unwrap();
    let all = commands(p.path(), cfg.path(), &trust, Some(home.path()), &none);
    let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["git:sync", "ship"]);
    assert_eq!(all[1].origin, "user .nexo/commands");
    assert_eq!(all[0].origin, "project .claude/commands");
    assert_eq!(all[0].description, "Sync");
    std::fs::remove_file(cfg.path().join("commands/ship.md")).unwrap();
    let all = commands(p.path(), cfg.path(), &trust, Some(home.path()), &none);
    let ship = all.iter().find(|c| c.name == "ship").unwrap();
    assert_eq!(
        (ship.description.as_str(), ship.hint.as_str(), ship.allowed_tools.as_str()),
        ("Ship it", "[env]", "Bash(git:*)")
    );
    assert_eq!(ship.body, "claude ship", "the project's own folder beats the user's");
}

#[test]
fn a_home_kept_in_git_reads_its_dot_folders_once_as_the_persons_own() {
    let (home, cfg) = (repo(), tempfile::tempdir().unwrap());
    w(home.path().join(".claude/CLAUDE.md"), "c-user");
    w(home.path().join(".claude/commands/mine.md"), "Mine.");
    let sub = home.path().join("work");
    std::fs::create_dir_all(&sub).unwrap();
    let got = instructions(&sub, cfg.path(), Some(home.path()));
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].origin, "user fallback");
    let trust = cfg.path().join("trusted-projects");
    crate::config::trust_project(home.path(), &trust).unwrap();
    let src = sources(&sub, cfg.path(), &trust, Some(home.path()), "commands", Some("commands"), None);
    let claude = home.path().join(".claude/commands");
    assert_eq!(src.iter().filter(|s| s.dir == claude).count(), 1, "{src:?}");
    assert!(src.iter().all(|s| !s.origin.starts_with("project .claude")), "{src:?}");
    // another repository still has its own project folders
    let other = repo();
    crate::config::trust_project(other.path(), &trust).unwrap();
    let src = sources(other.path(), cfg.path(), &trust, Some(home.path()), "commands", Some("commands"), None);
    assert!(src.iter().any(|s| s.origin.starts_with("project")), "{src:?}");
}
