//! Themes, own commands and skills, history search and the cost meter.

use super::*;

#[test]
fn the_theme_picker_previews_and_restores() {
    let (mut a, _rx) = app();
    assert_eq!(a.theme.name, "dark");
    a.run_command(Command::Theme(None));
    if let Some(Modal::Picker(p)) = &mut a.modal {
        p.selected = crate::theme::NAMES.iter().position(|n| *n == "nord").unwrap();
    }
    a.modal_out(ModalOut::Nothing);
    assert_eq!(a.theme.name, "nord", "the hovered theme shows");
    a.modal_out(ModalOut::Close);
    assert_eq!(a.theme.name, "dark", "Esc restores");
    a.set_theme("gruvbox-dark", false);
    assert_eq!(a.theme.name, "gruvbox");
    a.set_theme("nope", false);
    assert!(matches!(a.blocks.last(), Some(Block::Notice { .. })));
}

#[test]
fn undo_init_status_tools_and_the_editor_key() {
    let (mut a, mut rx) = app();
    a.input.set_text("/undo");
    a.submit();
    assert!(rx.try_recv().is_err(), "nothing to undo yet");
    a.handle_agent(AgentEvent::History { index: 0, message: nexo_core::message::Message::user("make it blue") });
    a.input.set_text("/undo");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Rewind { index: 0 })));
    assert_eq!(a.input.text(), "make it blue", "the request comes back to the editor");
    a.input.set_text("/init");
    a.submit();
    // the session is named after the command first
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Rename { .. })));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. }) if text.contains("AGENTS.md")));
    a.input.set_text("/status");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShowStatus)));
    a.input.set_text("/tools");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShowTools)));
    crate::event::key(
        &mut a,
        crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('g'), crossterm::event::KeyModifiers::CONTROL),
    );
    assert!(a.draft_edit_request);
}

#[test]
fn a_command_of_ones_own_is_offered_and_sends_its_text() {
    let (mut a, mut rx) = app();
    a.command_loader = Box::new(|_| {
        vec![crate::custom::CustomCommand {
            name: "triage".into(),
            description: "review against our rules".into(),
            body: "Review $ARGUMENTS against CONTRIBUTING.md.".into(),
            path: PathBuf::from("triage.md"),
            hint: String::new(),
            allowed_tools: String::new(),
            origin: String::new(),
        }]
    });
    a.input.set_text("/tri");
    a.refresh_palette();
    let p = a.palette.as_ref().expect("the palette");
    let (rows, _) = p.rows(20);
    let row = rows.iter().find(|r| r.label == "triage").expect("the command is offered");
    assert!(row.hint.contains("review against our rules") && row.hint.ends_with("· custom"));
    a.input.set_text("/triage src/lib.rs");
    a.submit();
    assert!(
        matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. }) if text == "Review src/lib.rs against CONTRIBUTING.md.")
    );
}

#[test]
fn a_skill_is_a_command_that_sends_its_instructions_and_is_listed() {
    let (mut a, mut rx) = app();
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("lint-fix");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), "---\nname: lint-fix\ndescription: fix lint\n---\nRun the linter.\n").unwrap();
    let skills = vec![nexo_core::skills::Skill {
        name: "lint-fix".into(),
        description: "fix lint".into(),
        dir,
        allowed_tools: vec![],
        source: ".claude/skills".into(),
    }];
    a.set_skills(skills);
    a.input.set_text("/lint");
    a.refresh_palette();
    let (rows, _) = a.palette.as_ref().expect("the palette").rows(20);
    assert!(rows.iter().any(|r| r.label == "lint-fix"));
    a.input.set_text("/lint-fix src");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. })
        if text.contains("Run the linter.") && text.ends_with("Arguments: src")));
    a.run_command(Command::Skills);
    let shown = format!("{:?}", a.blocks.last());
    assert!(shown.contains("/lint-fix") && shown.contains(".claude/skills"), "{shown}");
}

#[test]
fn ctrl_r_finds_an_earlier_request_and_the_history_outlives_the_run() {
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("prompt-history.jsonl");
    let (mut a, _rx) = app();
    a.input.persist_history(file.clone());
    for t in ["fix the parser", "add a test\nfor the parser", "run the suite"] {
        a.input.set_text(t);
        a.submit();
    }
    crate::event::key(
        &mut a,
        crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('r'), crossterm::event::KeyModifiers::CONTROL),
    );
    let Some(Modal::Picker(p)) = &a.modal else { panic!("a history picker") };
    assert_eq!(p.items[0].label, "run the suite", "newest first");
    assert_eq!(p.items[1].hint, "+1 lines");
    a.modal_out(ModalOut::Pick { kind: PickKind::History, value: "fix the parser".into() });
    assert_eq!(a.input.text(), "fix the parser");
    // a new run starts from the file
    let (mut b, _rx2) = app();
    b.input.persist_history(file);
    assert_eq!(b.input.history().len(), 3);
    assert_eq!(b.input.history()[1], "add a test\nfor the parser");
}

#[test]
fn the_cost_rolls_up_like_a_meter_and_settles_on_the_spend() {
    let (mut a, _rx) = app();
    a.opts.animations = true;
    let t0 = Instant::now();
    a.fx.clock = Some(t0);
    a.tick();
    a.cost = 0.0123;
    let first = frame(&mut a, 120, 10).last().unwrap().clone();
    assert!(first.contains("$0.0000"), "starts where it was: {first}");
    let mut seen = vec![];
    for ms in (0..=500u64).step_by(25) {
        a.fx.clock = Some(t0 + std::time::Duration::from_millis(ms));
        a.tick();
        seen.push(a.cost_shown);
    }
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "never runs backwards");
    assert!(seen.iter().any(|c| *c > 0.0 && *c < 0.0123), "passes through the figures between");
    let last = frame(&mut a, 120, 10).last().unwrap().clone();
    assert!(last.contains("$0.0123"), "{last}");
    // without animations the figure is the spend at once
    a.opts.animations = false;
    a.cost = 0.5;
    assert!(frame(&mut a, 120, 10).last().unwrap().contains("$0.5000"));
}

#[test]
fn budget_commands_and_the_spend_in_the_title() {
    let (mut a, mut rx) = app();
    for (typed, want) in [
        ("/budget", None),
        ("/budget 1.5", Some(Some(1.5))),
        ("/budget $2", Some(Some(2.0))),
        ("/budget off", Some(None)),
    ] {
        a.input.set_text(typed);
        a.submit();
        match (rx.try_recv(), want) {
            (Ok(AgentCommand::ShowBudget), None) => {}
            (Ok(AgentCommand::SetBudget { limit }), Some(l)) => assert_eq!(limit, l, "{typed}"),
            (other, _) => panic!("{typed}: {other:?}"),
        }
    }
    a.input.set_text("/budget lots");
    a.submit();
    assert!(rx.try_recv().is_err(), "not an amount: nothing sent");
    a.cost = 0.9;
    a.cost_shown = 0.9;
    a.handle_agent(AgentEvent::Budget { session_cost: 0.9, day_cost: 1.2, session_limit: Some(1.0), day_limit: None });
    let backend = ratatui::backend::TestBackend::new(120, 10);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let top = crate::ui::rows_of(term.backend().buffer()).last().unwrap().clone();
    assert!(top.contains("$0.9000/1.00"), "{top}");
}

#[test]
fn a_pasted_image_is_kept_and_goes_with_the_request() {
    let d = tempfile::tempdir().unwrap();
    std::env::set_var("NEXO_CACHE_DIR", d.path());
    // a 2×1 picture: one red pixel, one transparent
    let png = crate::clipboard::encode_png(2, 1, &[255, 0, 0, 255, 0, 0, 0, 0]).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    let back = image::load_from_memory(&png).unwrap().to_rgba8();
    assert_eq!((back.width(), back.height(), back.get_pixel(0, 0).0), (2, 1, [255, 0, 0, 255]));
    assert!(crate::clipboard::encode_png(3, 3, &[0; 4]).is_err(), "pixels that do not fill the size");
    let (mut a, mut rx) = app();
    a.input.set_text("what is wrong here");
    a.attach_png(&png);
    std::env::remove_var("NEXO_CACHE_DIR");
    let draft = a.input.text();
    let path = draft.split_whitespace().find_map(|w| w.strip_prefix('@')).unwrap().to_string();
    assert!(path.starts_with(&d.path().join("pastes").display().to_string()) && path.ends_with(".png"), "{draft}");
    assert_eq!(std::fs::read(&path).unwrap(), png);
    a.submit();
    match rx.try_recv() {
        Ok(AgentCommand::Submit { attachments, .. }) => assert_eq!(attachments[0].path, PathBuf::from(&path)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn every_listed_theme_changes_what_is_drawn_and_an_unknown_name_lists_the_known_ones() {
    for name in crate::theme::NAMES.iter().filter(|n| **n != "dark") {
        let (mut a, _rx) = app();
        a.opts.animations = false;
        assert_eq!(a.theme.name, "dark");
        let colours = |a: &mut App| -> Vec<_> { buf_of(a, 80, 24).content().iter().map(|c| (c.fg, c.bg)).collect() };
        let before = colours(&mut a);
        // Not persisted: the persisting path writes the real user config file.
        a.set_theme(name, false);
        assert_eq!(a.theme.name, *name);
        let after = colours(&mut a);
        assert!(before != after, "/theme {name} drew the same colours as dark");
    }
    let (mut a, _rx) = app();
    a.run_command(Command::Theme(Some("neon".into())));
    let said = a.blocks.iter().find_map(|b| if let Block::Notice { text, .. } = b { Some(text.clone()) } else { None });
    let said = said.expect("a notice");
    for name in crate::theme::NAMES {
        assert!(said.contains(name), "{said}");
    }
    assert!(said.contains("neon"), "{said}");
    assert_eq!(a.theme.name, "dark", "an unknown name changes nothing");
}

#[test]
fn a_command_that_expands_into_a_prompt_records_only_what_was_typed() {
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("prompt-history.jsonl");
    let (mut a, mut rx) = app();
    a.input.persist_history(file.clone());
    a.command_loader = Box::new(|_| {
        vec![crate::custom::CustomCommand {
            name: "triage".into(),
            description: String::new(),
            body: "Review $ARGUMENTS against CONTRIBUTING.md.".into(),
            path: PathBuf::from("triage.md"),
            hint: String::new(),
            allowed_tools: String::new(),
            origin: String::new(),
        }]
    });
    a.input.set_text("/init");
    a.submit();
    a.input.set_text("/triage src/lib.rs");
    a.submit();
    let sent: Vec<_> =
        std::iter::from_fn(|| rx.try_recv().ok()).filter(|c| matches!(c, AgentCommand::Submit { .. })).collect();
    assert_eq!(sent.len(), 2, "both expansions reach the agent");
    assert_eq!(a.input.history(), ["/init", "/triage src/lib.rs"]);
    let stored = std::fs::read_to_string(&file).unwrap();
    assert_eq!(stored.lines().collect::<Vec<_>>(), ["\"/init\"", "\"/triage src/lib.rs\""]);
}

#[test]
fn a_command_file_written_mid_session_is_offered_and_run_while_skills_and_mcp_stay() {
    let (mut a, mut rx) = app();
    let (cfg, proj) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::create_dir_all(proj.path().join(".git")).unwrap();
    let trust = cfg.path().join("trusted-projects");
    nexo_core::config::trust_project(proj.path(), &trust).unwrap();
    let (cfg_dir, proj_dir) = (cfg.path().to_path_buf(), proj.path().to_path_buf());
    a.command_loader = Box::new(move |_| crate::custom::load_from(&proj_dir, &cfg_dir, &trust, None));
    let entry = |name: &str, origin: &str| crate::custom::CustomCommand {
        name: name.into(),
        description: "pushed by an event".into(),
        body: String::new(),
        path: PathBuf::new(),
        hint: String::new(),
        allowed_tools: String::new(),
        origin: origin.into(),
    };
    a.custom_commands = vec![entry("lint-fix", "skill"), entry("mcp__srv__ask", "mcp"), entry("stale", "")];
    let dir = proj.path().join(".nexo/commands");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("fresh.md"), "Do the fresh thing: $ARGUMENTS").unwrap();
    // a skill with the name of a new file is shadowed by the file, and stays behind it
    std::fs::write(dir.join("lint-fix.md"), "File version").unwrap();
    a.input.set_text("/");
    a.refresh_palette();
    let (rows, _) = a.palette.as_ref().expect("the palette").rows(1000);
    assert!(rows.iter().any(|r| r.label == "fresh"), "the new file is offered");
    let names: Vec<&str> = a.custom_commands.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["fresh", "lint-fix", "lint-fix", "mcp__srv__ask"], "{names:?}");
    assert_eq!(a.custom_commands[1].origin, "project .nexo/commands");
    // an edit is what runs, without opening the palette
    std::fs::write(dir.join("fresh.md"), "Edited: $ARGUMENTS").unwrap();
    a.input.set_text("/fresh now");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. }) if text == "Edited: now"));
}

#[test]
fn a_skill_shadowed_by_a_command_file_comes_back_when_the_file_is_deleted() {
    let (mut a, mut rx) = app();
    let (cfg, proj) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::create_dir_all(proj.path().join(".git")).unwrap();
    let trust = cfg.path().join("trusted-projects");
    nexo_core::config::trust_project(proj.path(), &trust).unwrap();
    let (cfg_dir, proj_dir) = (cfg.path().to_path_buf(), proj.path().to_path_buf());
    a.command_loader = Box::new(move |_| crate::custom::load_from(&proj_dir, &cfg_dir, &trust, None));
    let skill_dir = proj.path().join("skills/lint-fix");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "---\nname: lint-fix\ndescription: fix lint\n---\nRun the linter.\n")
        .unwrap();
    a.set_skills(vec![nexo_core::skills::Skill {
        name: "lint-fix".into(),
        description: "fix lint".into(),
        dir: skill_dir,
        allowed_tools: vec![],
        source: ".claude/skills".into(),
    }]);
    let cmds = proj.path().join(".nexo/commands");
    std::fs::create_dir_all(&cmds).unwrap();
    std::fs::write(cmds.join("lint-fix.md"), "File version $ARGUMENTS").unwrap();
    a.input.set_text("/lint-fix src");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. }) if text == "File version src"));
    // the palette offers the name once
    a.input.set_text("/");
    a.refresh_palette();
    let (rows, _) = a.palette.as_ref().expect("the palette").rows(1000);
    assert_eq!(rows.iter().filter(|r| r.label == "lint-fix").count(), 1);
    // the file goes: the skill is what runs again
    std::fs::remove_file(cmds.join("lint-fix.md")).unwrap();
    a.input.set_text("/lint-fix src");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. })
        if text.contains("Run the linter.") && text.ends_with("Arguments: src")));
}
