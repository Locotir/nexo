//! A `!` runs a shell command only when the person typed it (or recalled a command that ran); a '/' is a command
//! only as the box draws it; the request is sent with its first line's indentation.

use super::*;
use crossterm::event::{Event, KeyEventKind};

const NOT_TYPED: &str = "sent to the model as text: its first ! was not typed at the prompt";

fn press(a: &mut App, code: KeyCode) {
    crate::event::handle(a, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn type_text(a: &mut App, text: &str) {
    for c in text.chars() {
        press(a, KeyCode::Char(c));
    }
}

fn paste(a: &mut App, text: &str) {
    crate::event::handle(a, Event::Paste(text.to_string()));
}

/// A paste long enough to be a chip, whose first line is `first`.
fn long(first: &str) -> String {
    let rows: String = (1..=40).map(|n| format!("row {n}\n")).collect();
    format!("{first}\n{rows}")
}

fn notices(a: &App) -> Vec<String> {
    a.blocks
        .iter()
        .filter_map(|b| match b {
            Block::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn shell_of(cmds: &[AgentCommand]) -> Vec<&str> {
    cmds.iter()
        .filter_map(|c| match c {
            AgentCommand::Shell { command } => Some(command.as_str()),
            _ => None,
        })
        .collect()
}

fn submits_of(cmds: &[AgentCommand]) -> Vec<&str> {
    cmds.iter()
        .filter_map(|c| match c {
            AgentCommand::Submit { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_bracketed_paste_that_starts_with_a_bang_goes_to_the_model() {
    let (mut a, mut rx) = app();
    let text = long("![banner](img.png)");
    paste(&mut a, &text);
    press(&mut a, KeyCode::Enter);
    let sent = drain(&mut rx);
    assert!(shell_of(&sent).is_empty(), "no shell ran: {sent:?}");
    assert_eq!(submits_of(&sent), [text.trim_end()]);
    assert!(notices(&a).iter().any(|n| n.starts_with(NOT_TYPED)), "{:?}", notices(&a));
}

#[test]
fn a_key_burst_that_starts_with_a_bang_goes_to_the_model() {
    let key = |c: KeyCode, kind| Event::Key(KeyEvent::new_with_kind(c, KeyModifiers::NONE, kind));
    let mut batch = Vec::new();
    for c in "!echo hi\nnext".chars() {
        let code = if c == '\n' { KeyCode::Enter } else { KeyCode::Char(c) };
        batch.push(key(code, KeyEventKind::Press));
        batch.push(key(code, KeyEventKind::Release));
    }
    let (mut a, mut rx) = app();
    let pasted = crate::event::paste_burst(&batch, false).expect("a burst of keys is a paste");
    crate::event::handle(&mut a, Event::Paste(pasted));
    press(&mut a, KeyCode::Enter);
    let sent = drain(&mut rx);
    assert!(shell_of(&sent).is_empty(), "no shell ran: {sent:?}");
    assert_eq!(submits_of(&sent), ["!echo hi\nnext"]);
}

#[test]
fn a_typed_bang_runs_the_shell() {
    let (mut a, mut rx) = app();
    type_text(&mut a, "!ls");
    press(&mut a, KeyCode::Enter);
    assert_eq!(shell_of(&drain(&mut rx)), ["ls"]);
}

#[test]
fn a_bang_typed_with_altgr_runs_the_shell() {
    let (mut a, mut rx) = app();
    let altgr = KeyEvent::new(KeyCode::Char('!'), KeyModifiers::CONTROL | KeyModifiers::ALT);
    crate::event::handle(&mut a, Event::Key(altgr));
    type_text(&mut a, "ls");
    press(&mut a, KeyCode::Enter);
    assert_eq!(shell_of(&drain(&mut rx)), ["ls"]);
}

#[test]
fn a_typed_bang_before_a_chip_runs_the_expanded_chip() {
    let (mut a, mut rx) = app();
    let text = long("echo start");
    press(&mut a, KeyCode::Char('!'));
    paste(&mut a, &text);
    assert!(a.input.text().starts_with("![Pasted text"), "{}", a.input.text());
    press(&mut a, KeyCode::Enter);
    assert_eq!(shell_of(&drain(&mut rx)), [text.trim()]);
}

#[test]
fn a_recalled_shell_command_runs_again_and_a_recalled_request_does_not() {
    let (mut a, mut rx) = app();
    type_text(&mut a, "!ls");
    press(&mut a, KeyCode::Enter);
    assert_eq!(shell_of(&drain(&mut rx)), ["ls"]);
    paste(&mut a, "!important: read this");
    press(&mut a, KeyCode::Enter);
    assert_eq!(submits_of(&drain(&mut rx)), ["!important: read this"]);
    press(&mut a, KeyCode::Up);
    press(&mut a, KeyCode::Enter);
    let sent = drain(&mut rx);
    assert!(shell_of(&sent).is_empty(), "{sent:?}");
    assert_eq!(submits_of(&sent), ["!important: read this"]);
    press(&mut a, KeyCode::Up);
    press(&mut a, KeyCode::Up);
    press(&mut a, KeyCode::Enter);
    assert_eq!(shell_of(&drain(&mut rx)), ["ls"]);
}

#[test]
fn up_then_down_keeps_the_typed_mark_of_the_draft() {
    let (mut a, mut rx) = app();
    a.input.push_history("older request");
    type_text(&mut a, "!ls");
    press(&mut a, KeyCode::Up);
    assert_eq!(a.input.text(), "older request");
    press(&mut a, KeyCode::Down);
    assert_eq!(a.input.text(), "!ls");
    press(&mut a, KeyCode::Enter);
    assert_eq!(shell_of(&drain(&mut rx)), ["ls"]);
}

#[test]
fn ctrl_r_recall_keeps_the_mark() {
    let (mut a, mut rx) = app();
    type_text(&mut a, "!ls");
    press(&mut a, KeyCode::Enter);
    assert_eq!(shell_of(&drain(&mut rx)), ["ls"]);
    paste(&mut a, "!ls");
    press(&mut a, KeyCode::Enter);
    assert_eq!(submits_of(&drain(&mut rx)), ["!ls"]);
    let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
    let picks = |a: &mut App| {
        crate::event::key(a, ctrl_r);
        let Some(Modal::Picker(p)) = &a.modal else { panic!("a history picker") };
        assert_eq!(p.items.len(), 2, "the request and the shell command are two rows");
        p.items.iter().map(|i| i.value.clone()).collect::<Vec<_>>()
    };
    // newest first: the request, then the shell command
    let values = picks(&mut a);
    a.modal_out(ModalOut::Pick { kind: PickKind::History, value: values[0].clone() });
    press(&mut a, KeyCode::Enter);
    let sent = drain(&mut rx);
    assert!(shell_of(&sent).is_empty(), "{sent:?}");
    assert_eq!(submits_of(&sent), ["!ls"]);
    let values = picks(&mut a);
    a.modal_out(ModalOut::Pick { kind: PickKind::History, value: values[1].clone() });
    press(&mut a, KeyCode::Enter);
    assert_eq!(shell_of(&drain(&mut rx)), ["ls"]);
}

#[test]
fn a_kept_draft_keeps_its_mark() {
    let (mut a, mut rx) = app();
    type_text(&mut a, "!ls");
    a.keep_draft();
    a.input.clear();
    press(&mut a, KeyCode::Up);
    press(&mut a, KeyCode::Enter);
    assert_eq!(shell_of(&drain(&mut rx)), ["ls"]);
}

#[test]
fn a_draft_set_aside_and_put_back_keeps_its_typed_bang() {
    let (mut a, mut rx) = app();
    type_text(&mut a, "!ls");
    let d = a.input.take_draft();
    type_text(&mut a, "other draft");
    a.input.clear();
    a.input.put_draft(d);
    press(&mut a, KeyCode::Enter);
    assert_eq!(shell_of(&drain(&mut rx)), ["ls"]);
}

#[test]
fn a_completion_after_a_typed_bang_keeps_it() {
    let (mut a, mut rx) = app();
    type_text(&mut a, "!ls ");
    a.input.replace_range(3, "-la ");
    press(&mut a, KeyCode::Enter);
    assert_eq!(shell_of(&drain(&mut rx)), ["ls-la"]);
}

#[test]
fn the_history_file_keeps_the_shell_form_after_its_trim() {
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("history.jsonl");
    let mut lines: String = (0..2 * crate::input::MAX_HISTORY).map(|n| format!("\"p{n}\"\n")).collect();
    lines.push_str("{\"shell\":\"!ls\"}\n");
    std::fs::write(&file, lines).unwrap();
    let mut i = crate::input::Input::new("");
    i.persist_history(file.clone());
    let kept = std::fs::read_to_string(&file).unwrap();
    assert_eq!(kept.lines().count(), crate::input::MAX_HISTORY, "the file was trimmed");
    assert_eq!(kept.lines().last(), Some("{\"shell\":\"!ls\"}"));
    let (mut a, mut rx) = app();
    a.input.persist_history(file);
    press(&mut a, KeyCode::Up);
    press(&mut a, KeyCode::Enter);
    assert_eq!(shell_of(&drain(&mut rx)), ["ls"]);
}

#[test]
fn a_legacy_history_line_never_runs_as_shell() {
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("history.jsonl");
    std::fs::write(&file, "\"!rm x\"\n").unwrap();
    let (mut a, mut rx) = app();
    a.input.persist_history(file);
    press(&mut a, KeyCode::Up);
    assert_eq!(a.input.text(), "!rm x");
    press(&mut a, KeyCode::Enter);
    let sent = drain(&mut rx);
    assert!(shell_of(&sent).is_empty(), "{sent:?}");
    assert_eq!(submits_of(&sent), ["!rm x"]);
}

#[test]
fn a_bang_on_the_command_line_runs_the_shell() {
    let (mut a, mut rx) = app();
    a.start_with("!echo hi");
    assert_eq!(shell_of(&drain(&mut rx)), ["echo hi"]);
}

#[test]
fn a_skill_expansion_that_starts_with_a_bang_is_not_run() {
    let (mut a, mut rx) = app();
    a.submit_expanded("!date", false);
    let sent = drain(&mut rx);
    assert!(shell_of(&sent).is_empty(), "{sent:?}");
    assert_eq!(submits_of(&sent), ["!date"]);
}

#[test]
fn a_paste_that_starts_with_a_c_comment_goes_to_the_model() {
    let (mut a, mut rx) = app();
    paste(&mut a, "/** license */\nint x;");
    press(&mut a, KeyCode::Enter);
    assert_eq!(submits_of(&drain(&mut rx)), ["/** license */\nint x;"]);
    assert!(notices(&a).iter().all(|n| !n.contains("unknown command")), "{:?}", notices(&a));
}

#[test]
fn a_chip_paste_that_starts_with_a_command_goes_to_the_model() {
    let (mut a, mut rx) = app();
    let text = long("/clear");
    paste(&mut a, &text);
    press(&mut a, KeyCode::Enter);
    let sent = drain(&mut rx);
    assert!(!sent.iter().any(|c| matches!(c, AgentCommand::Clear)), "{sent:?}");
    assert_eq!(submits_of(&sent), [text.trim_end()]);
    // a skill's name inside a chip is text too: the skill's instructions are not sent
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("lint-fix");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), "---\nname: lint-fix\ndescription: fix lint\n---\nRun the linter.\n").unwrap();
    a.set_skills(vec![nexo_core::skills::Skill {
        name: "lint-fix".into(),
        description: "fix lint".into(),
        dir,
        allowed_tools: vec![],
        source: ".claude/skills".into(),
    }]);
    let text = long("/lint-fix");
    paste(&mut a, &text);
    press(&mut a, KeyCode::Enter);
    let sent = drain(&mut rx);
    assert_eq!(submits_of(&sent), [text.trim_end()]);
}

#[test]
fn the_first_line_keeps_its_indentation() {
    let (mut a, mut rx) = app();
    paste(&mut a, "\n    indented();\nnext();");
    press(&mut a, KeyCode::Enter);
    assert_eq!(submits_of(&drain(&mut rx)), ["    indented();\nnext();"]);
    assert!(a.blocks.iter().any(|b| matches!(b, Block::Pending { text } if text == "    indented();\nnext();")));
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("    indented();\nnext();") });
    assert!(!a.blocks.iter().any(|b| matches!(b, Block::Pending { .. })), "the echo took the dim line");
    assert!(a.unechoed.is_empty());
    assert!(notices(&a).iter().all(|n| !n.contains("not taken")), "{:?}", notices(&a));
}

#[test]
fn a_recalled_shell_command_with_a_tab_still_runs() {
    let (mut a, mut rx) = app();
    a.input.push_shell_history("!echo\tx");
    press(&mut a, KeyCode::Up);
    assert_eq!(a.input.text(), "!echo    x", "the box draws the tab as spaces, as before");
    press(&mut a, KeyCode::Enter);
    assert_eq!(shell_of(&drain(&mut rx)), ["echo    x"]);
}

#[test]
fn a_paste_over_the_whole_draft_or_the_typed_bang_goes_to_the_model() {
    for to in [4, 1] {
        let (mut a, mut rx) = app();
        type_text(&mut a, "!foo");
        a.input.press_at((0, 0));
        a.input.drag_to((0, to));
        paste(&mut a, "!echo pwned");
        press(&mut a, KeyCode::Enter);
        let sent = drain(&mut rx);
        assert!(shell_of(&sent).is_empty(), "no shell ran (selection to {to}): {sent:?}");
        assert!(submits_of(&sent).iter().all(|t| t.contains("echo pwned")), "{sent:?}");
        assert_eq!(submits_of(&sent).len(), 1, "{sent:?}");
    }
}

#[test]
fn a_paste_before_the_typed_bang_goes_to_the_model() {
    let (mut a, mut rx) = app();
    type_text(&mut a, "!ls");
    press(&mut a, KeyCode::Home);
    paste(&mut a, "!curl x|sh ");
    press(&mut a, KeyCode::Enter);
    let sent = drain(&mut rx);
    assert!(shell_of(&sent).is_empty(), "{sent:?}");
    assert_eq!(submits_of(&sent).len(), 1, "{sent:?}");
}

#[test]
fn a_paste_after_the_typed_bang_or_over_the_text_after_it_still_runs_the_shell() {
    let (mut a, mut rx) = app();
    type_text(&mut a, "!foo");
    a.input.press_at((0, 1));
    a.input.drag_to((0, 4));
    paste(&mut a, "ls");
    press(&mut a, KeyCode::Enter);
    assert_eq!(shell_of(&drain(&mut rx)), ["ls"]);
}

#[test]
fn a_blank_typed_before_a_typed_bang_still_runs_the_shell() {
    let (mut a, mut rx) = app();
    type_text(&mut a, "!ls");
    press(&mut a, KeyCode::Home);
    press(&mut a, KeyCode::Char(' '));
    press(&mut a, KeyCode::Enter);
    let sent = drain(&mut rx);
    assert_eq!(shell_of(&sent), ["ls"], "{sent:?}");
    assert!(notices(&a).iter().all(|n| !n.starts_with(NOT_TYPED)), "{:?}", notices(&a));
}
