//! Ctrl+C, list rows, MCP prompts and window, unknown commands.

use super::*;

#[test]
fn an_unknown_tool_shows_key_value_pairs_not_json() {
    let args = serde_json::json!({ "action": "output", "id": 3, "wait": 5 });
    assert_eq!(summarise("jobs", &args), "action output, id 3, wait 5");
}

#[test]
fn a_replayed_message_with_a_note_memory_and_text_becomes_cards_and_a_bubble() {
    let (mut a, _rx) = app();
    let base = a.blocks.len();
    let note = "[background jobs ended]\n- job 3 `make` done (exit 0) after 4s, 2 lines of output\nRead the output with the `jobs` tool (action \"output\", the job's id).";
    let mem = "<memory from=\"s-1\" title=\"parser\">\nShared.\n</memory>";
    a.history(0, Message::user(format!("{mem}\n\n{note}\n\nfix it")));
    a.history(1, Message::user("[summary] The conversation before this point was replaced\n[files]"));
    assert!(matches!(&a.blocks[base], Block::User { text, .. } if crate::render::blocks::is_job_note(text)));
    assert!(matches!(&a.blocks[base + 1], Block::User { text, .. } if text == "fix it"));
    // the memory is a folded line under the words, not a card of its own
    assert!(
        matches!(&a.blocks[base + 2], Block::Added { parts, folded: true } if parts.len() == 1 && parts[0].label == "memory from parser")
    );
    assert!(
        matches!(&a.blocks[base + 3], Block::Notice { text, .. } if text.starts_with("earlier conversation summarised"))
    );
}

#[test]
fn a_long_call_summary_cuts_non_ascii_json_on_a_character_boundary() {
    // byte 79 falls inside a two-byte "ñ"
    let args = serde_json::json!({ "x": format!("{}{}", "a".repeat(72), "ñ".repeat(10)) });
    let s = summarise("other", &args);
    assert!(s.ends_with('…'), "{s}");
    assert!(crate::render::wrap::width(&s) <= 80, "{s}");
}

#[test]
fn ctrl_c_with_the_palette_open_clears_the_draft_and_the_palette() {
    let (mut a, _rx) = app();
    for c in "/m".chars() {
        crate::event::key(&mut a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert!(a.palette.is_some());
    crate::event::key(&mut a, KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(a.input.is_empty());
    assert!(a.palette.is_none(), "no list left over an empty prompt");
}

#[test]
fn ctrl_c_in_a_permission_dialog_denies_it_and_stops_the_turn() {
    let (mut a, mut rx) = app();
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(AgentEvent::PermissionAsk {
        request: PermissionRequest {
            agent: None,
            id: "p1".into(),
            tool: "bash".into(),
            kind: nexo_core::tool::ToolKind::Exec,
            summary: "x".into(),
            detail: ToolDisplay::Plain,
            suggested_rules: vec![],
        },
    });
    while rx.try_recv().is_ok() {}
    crate::event::key(&mut a, KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(a.modal.is_none());
    let sent: Vec<AgentCommand> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        matches!(&sent[..], [AgentCommand::Permission { id, decision: PermissionDecision::Deny { .. } }, AgentCommand::Interrupt] if id == "p1"),
        "{sent:?}"
    );
    // a second press quits, as at the prompt
    crate::event::key(&mut a, KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(a.should_quit);
}

#[test]
fn a_list_at_rest_shows_the_row_the_arrow_reached_without_another_key() {
    let (mut a, _rx) = app();
    a.opts.animations = true;
    // the stars of the empty session behind the list rest later (see `bursts.rs`)
    a.opts.home_stars = false;
    a.opts.sessions = (0..5).map(|i| (format!("s{i}"), format!("chat {i}"))).collect();
    a.run_command(Command::Sessions);
    let t0 = Instant::now();
    fx_at(&mut a, t0, 0);
    frame(&mut a, 70, 16);
    fx_at(&mut a, t0, 20_000);
    a.tick_frame(false);
    assert_eq!(a.load(), crate::frame::Load::Idle, "the open list rests");
    press(&mut a, KeyCode::Down);
    frame(&mut a, 70, 16);
    assert_eq!(a.load(), crate::frame::Load::Motion, "the glide begun in the frame asks for the next ones");
    fx_at(&mut a, t0, 20_000 + crate::motion::GLIDE_MS + 50);
    a.tick_frame(false);
    assert!(a.dirty, "a frame comes after the glide ends");
    frame(&mut a, 70, 16);
    let Some(Modal::Picker(p)) = &a.modal else { panic!() };
    assert_eq!(a.fx.pick_sel.level(p.selected, a.now()), 1.0, "the row reached is fully lit");
}

#[test]
fn mcp_prompts_become_commands_and_send_the_server_prompt_and_arguments() {
    let (mut a, mut rx) = app();
    a.handle_agent(AgentEvent::Mcp { servers: vec![docs_server()] });
    a.input.set_text("/mcp__docs__rev");
    a.refresh_palette();
    assert_eq!(a.palette.as_ref().and_then(|p| p.current()), Some("mcp__docs__review"));
    a.input.set_text("/mcp__docs__review src/lib.rs focus=speed");
    a.submit();
    match rx.try_recv() {
        Ok(AgentCommand::McpPrompt { server, prompt, args }) => {
            assert_eq!((server.as_str(), prompt.as_str(), args.as_str()), ("docs", "review", "src/lib.rs focus=speed"));
        }
        other => panic!("{other:?}"),
    }
    // the server goes away: the command goes with it
    a.handle_agent(AgentEvent::Mcp { servers: vec![] });
    assert!(a.custom_commands.iter().all(|c| c.origin != "mcp"));
}

#[test]
fn at_server_colon_completes_resource_uris() {
    let (mut a, _rx) = app();
    a.handle_agent(AgentEvent::Mcp { servers: vec![docs_server()] });
    a.input.set_text("read @docs:db");
    a.refresh_palette();
    assert_eq!(a.palette.as_ref().and_then(|p| p.current()), Some("docs:db://users"));
    a.palette_accept(false);
    assert!(a.input.text().starts_with("read @docs:db://users"), "{}", a.input.text());
}

#[test]
fn the_mcp_window_lists_servers_and_its_keys_send_commands() {
    use crate::modal::Modal;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use nexo_core::agent::{McpServerInfo, McpStatus};
    let down = McpServerInfo {
        name: "broken".into(),
        transport: "stdio".into(),
        status: McpStatus::Failed("spawn failed: no such file".into()),
        tools: 0,
        resources: vec![],
        templates: vec![],
        prompts: vec![],
    };
    let login = McpServerInfo { name: "cloud".into(), status: McpStatus::NeedsLogin, ..down.clone() };
    let (mut a, mut rx) = app();
    a.handle_agent(AgentEvent::Mcp { servers: vec![docs_server(), login, down] });
    a.run_command(crate::commands::Command::Mcp);
    assert!(matches!(a.modal, Some(Modal::Mcp(_))));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShowMcp)));
    a.opts.animations = false;
    frame(&mut a, 100, 44);
    let text = frame(&mut a, 100, 44);
    let all = text.join("\n");
    for want in [
        "1/3 connected",
        "docs",
        "http",
        "4 tools · 2 resources · 1 prompt",
        "/mcp__docs__review",
        "needs login",
        "press l to sign in",
        "failed",
        "spawn failed: no such file",
    ] {
        assert!(all.contains(want), "{want} missing:\n{all}");
    }
    let press = |a: &mut App, c: char| crate::event::key(a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    press(&mut a, ' ');
    assert!(
        matches!(rx.try_recv(), Ok(AgentCommand::McpToggle { server, on: false, persist: false }) if server == "docs")
    );
    press(&mut a, 'j');
    press(&mut a, 'l');
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::McpLogin { server }) if server == "cloud"));
    press(&mut a, 'j');
    press(&mut a, 'r');
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::McpReconnect { server }) if server == "broken"));
    press(&mut a, 'p');
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::McpToggle { on: false, persist: true, .. })));
    // a server that is off is turned on by the same key
    let mut off = docs_server();
    off.status = McpStatus::Disabled;
    a.handle_agent(AgentEvent::Mcp { servers: vec![off] });
    let all = frame(&mut a, 100, 44).join("\n");
    assert!(all.contains("off") && all.contains("Space turns it on"), "{all}");
    press(&mut a, ' ');
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::McpToggle { on: true, .. })));
    press(&mut a, 'q');
    assert!(a.modal.is_none());
}

#[test]
fn a_message_starting_with_a_path_is_sent_not_taken_for_a_command() {
    for line in ["/home/u/foo.rs has a bug", "/tmp/x.log", "/etc/hosts.d", "/foo.rs is broken", "/c\\temp\\x"] {
        let (mut a, mut rx) = app();
        a.input.set_text(line);
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. }) if text == line), "{line}");
        assert!(a.input.is_empty(), "{line}");
    }
}

#[test]
fn an_unknown_command_warns_and_keeps_the_draft() {
    let (mut a, mut rx) = app();
    a.input.set_text("/foo bar");
    a.submit();
    assert!(rx.try_recv().is_err());
    assert_eq!(a.input.full_text(), "/foo bar");
    assert!(matches!(a.blocks.last(), Some(Block::Notice { .. })));
}

#[test]
fn enter_on_an_empty_draft_with_a_queued_request_sends_it_now() {
    let (mut a, mut rx) = app();
    a.busy = true;
    a.queued = 1;
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::InterruptRun)));
    assert!(rx.try_recv().is_err(), "one command");
    let said = a.status.as_ref().map(|(t, _)| t.clone()).unwrap_or_default();
    assert!(said.contains("queued request now"), "{said}");
}

#[test]
fn the_branch_keeps_its_slashes_and_a_windows_cwd_shows_its_last_folder() {
    let dir = std::env::temp_dir().join(format!("nexo-branch-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/feature/login\n").unwrap();
    assert_eq!(git_branch(&dir).as_deref(), Some("feature/login"));
    std::fs::remove_dir_all(&dir).ok();

    let (mut a, _rx) = app();
    a.peers = vec![nexo_core::mail::PeerInfo {
        id: "bbbb2222".into(),
        name: "api".into(),
        cwd: "C:\\Users\\dev\\api".into(),
        pid: 1,
        external: false,
        bypass: false,
        sendable: true,
        me: false,
    }];
    assert_eq!(a.peer_values(), vec![("api".to_string(), "api".to_string())]);
}
