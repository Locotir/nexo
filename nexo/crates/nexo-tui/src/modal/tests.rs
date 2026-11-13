use super::*;
use nexo_core::tool::ToolKind;

#[test]
fn the_permission_title_names_a_sub_agent_within_the_room_and_keeps_the_main_agents_as_it_was() {
    assert_eq!(permission_title(None, "bash", 52), " permission · bash ");
    assert_eq!(
        permission_title(None, "mcp__a_very_long_server__a_very_long_tool_name", 20),
        " permission · mcp__a_very_long_server__a_very_long_tool_name "
    );
    assert_eq!(permission_title(Some("general"), "bash", 52), " permission · general · bash ");
    // a long name is cut with an ellipsis, never past the room
    let t = permission_title(Some("a-very-long-agent-name-that-goes-on"), "bash", 52);
    assert_eq!(t, " permission · a-very-long-age… · bash ");
    let t = permission_title(Some("a-very-long-agent-name-that-goes-on"), "bash", 30);
    assert!(crate::render::wrap::width(&t) <= 30 && t.contains('…'), "{t:?}");
    // no room for a name: the short title
    assert_eq!(permission_title(Some("general"), "bash", 20), " permission · bash ");
}

fn req() -> PermissionRequest {
    PermissionRequest {
        agent: None,
        id: "p1".into(),
        tool: "bash".into(),
        kind: ToolKind::Exec,
        summary: "git status".into(),
        detail: ToolDisplay::Shell {
            command: "git status".into(),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
        },
        suggested_rules: vec!["bash(git *)".into(), "bash".into()],
    }
}

#[test]
fn help_files_and_todos_in_the_dialog_show_no_marks() {
    let th = Theme::dark();
    let help = Modal::help_lines(&th, 80).iter().map(crate::render::markdown::text_of).collect::<Vec<_>>().join("\n");
    assert!(!help.contains('`'), "{help}");
    let mut r = req();
    r.detail = ToolDisplay::File { path: "notes.md".into(), text: "# Title\nsome **bold**".into(), start_line: 1 };
    let t = Modal::permission_lines(&r, 60, &th)
        .iter()
        .map(crate::render::markdown::text_of)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(t.contains("Title") && !t.contains("# Title") && !t.contains("**"), "{t}");
    r.detail = ToolDisplay::File { path: "a.txt".into(), text: "# Title".into(), start_line: 1 };
    let t = Modal::permission_lines(&r, 60, &th)
        .iter()
        .map(crate::render::markdown::text_of)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(t.contains("# Title"), "{t}");
}

#[test]
fn permission_keys_answer() {
    let mut m = Modal::permission(req());
    assert_eq!(
        m.key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
        ModalOut::Permission { id: "p1".into(), decision: PermissionDecision::Allow }
    );
    assert_eq!(
        m.key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE)),
        ModalOut::Permission {
            id: "p1".into(),
            decision: PermissionDecision::AllowAlways { rule: "bash".into(), persist: false }
        }
    );
    assert_eq!(
        m.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        ModalOut::Permission { id: "p1".into(), decision: PermissionDecision::Deny { reason: None } }
    );
    let lines = Modal::permission_lines(&req(), 40, &Theme::dark());
    assert!(lines.iter().any(|l| crate::render::markdown::text_of(l).contains("bash(git *)")));
}

#[test]
fn a_long_command_is_shown_whole_in_the_permission_dialog() {
    let mut r = req();
    let cmd = "rm -rf build && cargo build --release 2>&1 | tail -n 5 && echo the-very-end";
    r.detail =
        ToolDisplay::Shell { command: cmd.into(), stdout: String::new(), stderr: String::new(), exit_code: None };
    let lines = Modal::permission_lines(&r, 30, &Theme::dark());
    let text: Vec<String> = lines.iter().map(crate::render::markdown::text_of).collect();
    assert!(text.iter().all(|l| crate::render::wrap::width(l) <= 30));
    let joined = text.join(" ");
    assert!(joined.contains("tail -n 5") && joined.contains("the-very-end"));
    assert_eq!(text.iter().filter(|l| l.starts_with("$ ")).count(), 1);
}

#[test]
fn a_dragged_scrollbar_sets_the_body_or_moves_a_lists_selection() {
    let mut m = Modal::Help { scroll: 0 };
    m.set_scroll(7, 10, 0);
    assert!(matches!(m, Modal::Help { scroll: 7 }));
    let items: Vec<PickItem> =
        (0..40).map(|i| PickItem { label: format!("item {i}"), value: i.to_string(), hint: String::new() }).collect();
    let mut m = Modal::Picker(Picker::new(PickKind::Model, "Model", items, None));
    // a body of 10 rows, the list from row 2: an offset of 5 needs the selection on row 14,
    // which is the list's 12th item
    m.set_scroll(5, 10, 2);
    let Modal::Picker(p) = &m else { unreachable!() };
    assert_eq!(p.selected, 12);
    let row = p.selected + 2;
    assert_eq!(row + 1 - 10, 5, "the draw's own rule puts the view at the offset");
    // back to the top: the selection comes into the first page
    m.set_scroll(0, 10, 2);
    let Modal::Picker(p) = &m else { unreachable!() };
    assert!(p.selected + 2 < 10);
    // far past the end: the last item
    m.set_scroll(500, 10, 2);
    let Modal::Picker(p) = &m else { unreachable!() };
    assert_eq!(p.selected, 39);
}

#[test]
fn the_permission_buttons_shrink_to_fit_and_always_keep_three_keys() {
    for avail in [60usize, 33, 32, 31, 25, 12, 11, 5] {
        let (b, gap) = permission_buttons(avail);
        let used: usize = b.iter().map(|(l, _)| l.chars().count()).sum::<usize>() + gap * (b.len() - 1);
        assert_eq!(b.iter().map(|(_, k)| *k).collect::<String>(), "yan");
        if avail >= 11 {
            assert!(used <= avail, "{avail}: {used}");
        }
    }
    assert_eq!(permission_buttons(60).0[0].0, "[y] allow");
    assert_eq!(permission_buttons(11).0[1].0, "[a]");
}

#[test]
fn a_diff_permission_says_how_many_lines_it_adds_and_removes() {
    let mut r = req();
    r.detail =
        ToolDisplay::Diff { path: "src/a.rs".into(), old: "a\nb\n".into(), new: "a\nc\nd\n".into(), start_line: 1 };
    let text: Vec<String> =
        Modal::permission_lines(&r, 40, &Theme::dark()).iter().map(crate::render::markdown::text_of).collect();
    assert!(text.iter().any(|l| l.starts_with("src/a.rs") && l.contains("+2 -1")), "{text:?}");
    // a long path gives way to the numbers
    r.detail = ToolDisplay::Diff { path: "x/".repeat(40), old: "a\n".into(), new: "b\n".into(), start_line: 1 };
    let text: Vec<String> =
        Modal::permission_lines(&r, 30, &Theme::dark()).iter().map(crate::render::markdown::text_of).collect();
    assert!(text.iter().all(|l| crate::render::wrap::width(l) <= 30), "{text:?}");
    assert!(text.iter().any(|l| l.contains("+1 -1")), "{text:?}");
}

#[test]
fn a_diff_permission_numbers_its_lines_from_where_the_text_sits_in_the_file() {
    let mut r = req();
    r.detail = ToolDisplay::Diff { path: "src/a.rs".into(), old: "b\n".into(), new: "c\n".into(), start_line: 40 };
    let text: Vec<String> =
        Modal::permission_lines(&r, 40, &Theme::dark()).iter().map(crate::render::markdown::text_of).collect();
    assert!(text.iter().any(|l| l.contains("@@ -40,1 +40,1 @@")), "{text:?}");
    assert!(text.iter().any(|l| l.trim_start().starts_with("40 - b")), "{text:?}");
    assert!(text.iter().any(|l| l.trim_start().starts_with("40 + c")), "{text:?}");
}

#[test]
fn picker_filters_and_picks() {
    let items = vec![
        PickItem {
            label: "deepseek/deepseek-chat".into(),
            value: "deepseek/deepseek-chat".into(),
            hint: String::new(),
        },
        PickItem { label: "openai/gpt".into(), value: "openai/gpt".into(), hint: String::new() },
    ];
    let mut m = Modal::Picker(Picker::new(PickKind::Model, "Model", items, Some("openai/gpt")));
    if let Modal::Picker(p) = &m {
        assert_eq!(p.current().unwrap().value, "openai/gpt");
    }
    for c in "deep".chars() {
        m.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert_eq!(
        m.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ModalOut::Pick { kind: PickKind::Model, value: "deepseek/deepseek-chat".into() }
    );
}
