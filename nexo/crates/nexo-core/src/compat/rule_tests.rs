use super::*;

const D: &str = "2026-11-09";

#[test]
fn the_text_sent_says_under_the_heading_that_saved_rules_are_background() {
    let f = REMEMBERED_FRAME;
    assert_eq!(framed("# P\n\nBuild with make.\n"), "# P\n\nBuild with make.\n");
    assert!(matches!(framed("no heading"), std::borrow::Cow::Borrowed(_)));
    assert_eq!(
        framed("# P\n\n## Remembered by nexo\n- 2026-11-01: A.\n\n## Other\nx\n"),
        format!("# P\n\n## Remembered by nexo\n{f}\n- 2026-11-01: A.\n\n## Other\nx\n")
    );
    // the heading as the last line, with and without its line end, and Windows line ends
    assert_eq!(framed("## Remembered by nexo"), format!("## Remembered by nexo\n{f}\n"));
    assert_eq!(framed("## Remembered by nexo\n"), format!("## Remembered by nexo\n{f}\n"));
    assert_eq!(
        framed("A\r\n## remembered by nexo\r\n- x\r\n"),
        format!("A\r\n## remembered by nexo\r\n{f}\r\n- x\r\n")
    );
    // the frame is one line and the file never gets it: a rule saved into framed text is not the way in
    assert!(!f.contains('\n') && f.starts_with('(') && f.ends_with(')'));
    assert_eq!(with_rule("## Remembered by nexo\n- 2026-11-01: B.", "A.", "2026-11-09").unwrap().matches(f).count(), 0);
}

#[test]
fn a_rule_goes_under_the_remembered_heading_and_nothing_else_moves() {
    let before = "# Project\n\nBuild with make.\n\n## Rules\n- Use tabs.\n\n## Remembered by nexo\n- 2026-11-01: Old one.\n\n## Other\ntext\n";
    let t = with_rule(before, "Run `make fmt` first.", D).unwrap();
    assert_eq!(
        t,
        "# Project\n\nBuild with make.\n\n## Rules\n- Use tabs.\n\n## Remembered by nexo\n- 2026-11-01: Old one.\n- 2026-11-09: Run `make fmt` first.\n\n## Other\ntext\n"
    );
    // near-exact copies (case, spacing, punctuation, the date) and lines of the person's own are skipped
    assert!(with_rule(&t, "run `make fmt`  first", "2030-01-01").is_none());
    assert!(with_rule(&t, "old one", D).is_none());
    assert!(with_rule(&t, "Use tabs!", D).is_none());
    // a file without the heading gets it at the end, with a blank line before it
    assert_eq!(with_rule("Mine.", "A.", D).unwrap(), "Mine.\n\n## Remembered by nexo\n- 2026-11-09: A.\n");
    assert_eq!(with_rule("Mine.\n", "A.", D).unwrap(), "Mine.\n\n## Remembered by nexo\n- 2026-11-09: A.\n");
    assert_eq!(with_rule("", "A.", D).unwrap(), "## Remembered by nexo\n- 2026-11-09: A.\n");
    // an empty section, at the very end or without a final newline
    assert_eq!(with_rule("## Remembered by nexo\n", "A.", D).unwrap(), "## Remembered by nexo\n- 2026-11-09: A.\n");
    assert_eq!(
        with_rule("## Remembered by nexo\n- 2026-11-01: B.", "A.", D).unwrap(),
        "## Remembered by nexo\n- 2026-11-01: B.\n- 2026-11-09: A.\n"
    );
    // Windows line endings stay Windows line endings
    assert_eq!(
        with_rule("Mine.\r\nMore.\r\n", "A.", D).unwrap(),
        "Mine.\r\nMore.\r\n\r\n## Remembered by nexo\r\n- 2026-11-09: A.\r\n"
    );
}

#[test]
fn save_rule_appends_dated_and_dedupes() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("AGENTS.md");
    std::fs::write(&f, "# Project\n\nBuild with make.\n").unwrap();
    assert_eq!(save_rule(&f, None, "Run `make fmt` first.", false).unwrap(), Saved::New);
    assert_eq!(save_rule(&f, None, "run `make fmt`  first.", false).unwrap(), Saved::There);
    let today = time::OffsetDateTime::now_utc().date();
    let one = format!("# Project\n\nBuild with make.\n\n## Remembered by nexo\n- {today}: Run `make fmt` first.\n");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), one);
    // nearly the same words: nothing is written and the line that is there comes back
    let near = save_rule(&f, None, "Always run `make fmt` first, before a commit.", false).unwrap();
    assert_eq!(near, Saved::Near(format!("{today}: Run `make fmt` first.")));
    assert_eq!(std::fs::read_to_string(&f).unwrap(), one);
    // the caller can insist; the very same words stay once all the same
    assert_eq!(save_rule(&f, None, "Always run `make fmt` first, before a commit.", true).unwrap(), Saved::New);
    assert_eq!(save_rule(&f, None, "Run `make fmt` first.", true).unwrap(), Saved::There);
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        format!("{one}- {today}: Always run `make fmt` first, before a commit.\n")
    );
}

#[test]
fn a_rule_the_remembered_section_nearly_says_is_found_and_the_persons_own_text_is_not_compared() {
    let a = r"Acme install folder is always %LOCALAPPDATA%\Acme; if it already exists, only the config file is overwritten (Launch.c:50 APP_DIRECTORY, :1349 installed flag, :1519 returns TRUE). No per-game names.";
    let b = r"Acme install folder always %LOCALAPPDATA%\Acme; if it already exists only config.json is rewritten (Launch.c:50 APP_DIRECTORY, :1519 installed branch), confirmed 2026-11-10.";
    let text = format!(
        "# Project\n\nThe wallet binary lives in /opt/acme/bin and is owned by root.\n\n## Remembered by nexo\n- 2026-11-11: {a}\n- 2026-11-11: tests run with `make test`\n\n## Other\n- deploys always go through the ship script on fridays\n"
    );
    assert_eq!(near_rule(&text, b), Some(format!("2026-11-11: {a}")));
    // a short rule the section holds whole
    assert_eq!(near_rule(&text, "Tests run with make").as_deref(), Some("2026-11-11: tests run with `make test`"));
    // another fact, a rule too short to judge, and lines outside nexo's own section
    assert_eq!(near_rule(&text, "the relay keeps events for seven days"), None);
    assert_eq!(near_rule(&text, "run make"), None);
    assert_eq!(near_rule(&text, "the wallet binary lives in /opt/acme/bin, owned by root"), None);
    assert_eq!(near_rule(&text, "deploys always go through the ship script"), None);
    assert_eq!(near_rule("", b), None);
    // two facts that share their joining words and one noun are two facts; the same one in more words is not
    let ports = "## Remembered by nexo\n- 2026-11-11: the production database is on port 5432\n";
    assert_eq!(near_rule(ports, "the staging database is on port 5433"), None);
    assert_eq!(
        near_rule(ports, "production database listens on port 5432 (not the default)").as_deref(),
        Some("2026-11-11: the production database is on port 5432")
    );
}

#[test]
fn a_saved_line_is_corrected_in_place_and_nothing_else_in_the_file_changes() {
    const D: &str = "2026-11-12";
    let text = "# Project\r\n\r\n- the staging database is on port 5433\r\n\r\n## Remembered by nexo\r\n- 2026-11-01: tests run with `make test`\r\n* 2026-11-02: the staging database is on port 5433 of db-stage\r\n- 2026-11-03: never touch vendor/\r\n\r\n## Other\r\n- 2026-11-02: the staging database is on port 5433 of db-stage\r\n";
    let fixed = "the staging database is on port 5434 of db-stage";
    let want = text.replacen(
        "* 2026-11-02: the staging database is on port 5433 of db-stage",
        "- 2026-11-12: the staging database is on port 5434 of db-stage",
        1,
    );
    let was = "2026-11-02: the staging database is on port 5433 of db-stage".to_string();
    // named by its words, with or without the date and the marker, or nearly
    for old in [
        "the staging database is on port 5433 of db-stage",
        "- 2026-11-02: The staging database is on port 5433 of db-stage.",
        "staging database, port 5433 on db-stage",
    ] {
        assert_eq!(with_replaced(text, old, fixed, D), Some((want.clone(), was.clone())), "{old}");
    }
    // no such line in nexo's section, nothing to name, the same words outside any such section, and a rule
    // the file already has (the same line takes another rule)
    let old = "the staging database is on port 5433 of db-stage";
    assert_eq!(with_replaced(text, "the relay keeps events for seven days", fixed, D), None);
    assert_eq!(with_replaced(text, "", fixed, D), None);
    assert_eq!(
        with_replaced("# P\n- 2026-11-02: the staging database is on port 5433 of db-stage\n", old, fixed, D),
        None
    );
    assert!(with_replaced(text, "tests run with make test", "tests run with make check", D).is_some());
    assert_eq!(with_replaced(text, "tests run with make test", "Never touch vendor", D), None);
    // a line the person wrote in the section without a list marker is theirs; a vague quote names no line,
    // and neither does one that fits two
    let mixed = "## Remembered by nexo\nBackups: nightly at 03:00 to the big disk\n- 2026-11-01: the staging database is on port 5433 of db-stage, user shop\n- 2026-11-02: the staging replica is on port 5434 of db-stage-ro, user shop\n";
    assert_eq!(with_replaced(mixed, "Backups: nightly at 03:00 to the big disk", fixed, D), None);
    assert_eq!(with_replaced(mixed, "staging database port", fixed, D), None);
    assert_eq!(with_replaced(mixed, "staging on port of db-stage, user shop", fixed, D), None);
    let (now, was) =
        with_replaced(mixed, "the staging database is on port 5433 of db-stage (user shop)", fixed, D).unwrap();
    assert_eq!(was, "2026-11-01: the staging database is on port 5433 of db-stage, user shop");
    assert_eq!(
        now,
        mixed.replace(
            "- 2026-11-01: the staging database is on port 5433 of db-stage, user shop",
            "- 2026-11-12: the staging database is on port 5434 of db-stage"
        )
    );
    // the last line of a file without a final line end
    assert_eq!(
        with_replaced("## Remembered by nexo\n- 2026-11-01: port is 5433", "port is 5433", "port is 5434", D),
        Some(("## Remembered by nexo\n- 2026-11-12: port is 5434".to_string(), "2026-11-01: port is 5433".to_string()))
    );
    // on disk: the file changes only when a line is named, and a missing file is not made
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("AGENTS.md");
    assert_eq!(replace_rule(&f, "port is 5433", "port is 5434").unwrap(), Replaced::NoLine);
    assert!(!f.exists());
    std::fs::write(&f, "## Remembered by nexo\n- 2026-11-01: port is 5433\n").unwrap();
    assert_eq!(replace_rule(&f, "something else entirely", "port is 5434").unwrap(), Replaced::NoLine);
    assert_eq!(replace_rule(&f, "something else entirely", "Port is 5433.").unwrap(), Replaced::There);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "## Remembered by nexo\n- 2026-11-01: port is 5433\n");
    assert_eq!(
        replace_rule(&f, "port is 5433", "port is 5434").unwrap(),
        Replaced::Done("2026-11-01: port is 5433".to_string())
    );
    let now = std::fs::read_to_string(&f).unwrap();
    assert!(
        now.starts_with("## Remembered by nexo\n- ") && now.ends_with(": port is 5434\n") && !now.contains("5433"),
        "{now}"
    );
}

#[test]
fn each_scope_has_its_file_and_the_next_session_reads_it() {
    let d = tempfile::tempdir().unwrap();
    let (cfg, proj) = (d.path().join("cfg"), d.path().join("proj"));
    std::fs::create_dir_all(proj.join(".git")).unwrap();
    std::fs::create_dir_all(proj.join("src/deep")).unwrap();
    let (f, seed) = rules_file("global", &proj, &cfg, None).unwrap();
    assert_eq!(f, cfg.join("prompt.md"));
    assert_eq!(save_rule(&f, seed.as_deref(), "Answer in English.", false).unwrap(), Saved::New);
    let text = std::fs::read_to_string(&f).unwrap();
    assert!(text.starts_with(crate::paths::PREFS_TEMPLATE), "{text}");
    assert!(text.contains("## Remembered by nexo\n- "), "{text}");
    // from a sub-folder the project file is at the repository root, created when missing
    let (p, _) = rules_file("project", &proj.join("src/deep"), &cfg, None).unwrap();
    assert_eq!(p, proj.join("AGENTS.md"));
    assert_eq!(save_rule(&p, None, "Never edit vendor/.", false).unwrap(), Saved::New);
    let all = instructions(&proj, &cfg, None);
    assert!(all.iter().any(|i| i.text.contains("Never edit vendor/.")), "{all:?}");
    // an instructions file nexo already reads is the one that grows
    std::fs::remove_file(&p).unwrap();
    std::fs::write(proj.join("CLAUDE.md"), "Mine.\n").unwrap();
    assert_eq!(rules_file("project", &proj, &cfg, None).unwrap().0, proj.join("CLAUDE.md"));
    assert_eq!(rules_file("user", &proj, &cfg, None).unwrap().0, cfg.join("AGENTS.md"));
    assert!(rules_file("session", &proj, &cfg, None).is_none());
}
