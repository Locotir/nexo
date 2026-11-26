use std::path::PathBuf;

use nexo_core::agent::AgentEvent;
use nexo_core::message::Message;
use nexo_core::tool::ToolOutput;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn app() -> App {
    // the captures must not depend on the other tools' folders in this machine's home
    std::env::set_var("NEXO_COMPAT_HOME", "");
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![("deepseek".into(), "deepseek-chat".into())],
        providers: vec![],
        theme: "dark".into(),
        mouse: true,
        show_reasoning: true,
        timestamps: false,
        fold_after: 4,
        group_tools: false,
        complete_words: true,
        suggest: false,
        hints: true,
        keys: Default::default(),
        notify_after: 15,
        animations: false,
        detail: nexo_core::config::Detail::Full,
        figure: nexo_core::config::Figure::Galaxy,
        home_stars: true,
        bursts: true,
        todo_strip: false,
        status_line: vec![],
        status_command: String::new(),
        vim: false,
        prompt: None,
        session_id: Some("20261102-100000-ef56".into()),
        sessions: vec![],
        skills: vec![],
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a.handle_agent(AgentEvent::Ready {
        session_id: "20261102-100000-ef56".into(),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        cwd: PathBuf::from("/tmp/project"),
        permission_mode: nexo_core::permission::PermissionMode::Ask,
    });
    a
}

fn draw(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer())
}

fn tool(a: &mut App, id: &str, name: &str, summary: &str, ok: bool) {
    a.handle_agent(AgentEvent::ToolCall {
        id: id.into(),
        name: name.into(),
        args: serde_json::json!({ "command": summary }),
        summary: summary.into(),
    });
    a.handle_agent(AgentEvent::ToolStart { id: id.into() });
    let o = if ok {
        ToolOutput::ok("out")
    } else {
        ToolOutput::error("error: something failed with a long message that goes past the narrow width")
    };
    a.handle_agent(AgentEvent::ToolDone { id: id.into(), output: o, elapsed_ms: 1200 });
}

fn working() -> App {
    let mut a = app();
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("fix the failing test") });
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    tool(&mut a, "c3", "bash", "false", false);
    a
}

/// The size of a long draft sits at the right of the rule above the box. The working line on that
/// rule used to be laid out for the whole width, so the size was drawn over its last words and left
/// `Esc inter` (or a lone `·`) behind.
#[test]
fn the_draft_size_on_the_rule_never_cuts_the_working_line() {
    for (w, h) in [(60u16, 20u16), (40, 15)] {
        let mut a = working();
        a.input.set_text(&"word ".repeat(80));
        let rows = draw(&mut a, w, h);
        let rule = rows.iter().find(|r| r.contains("Thinking")).expect("working line").clone();
        assert!(rule.contains("chars"), "{w}x{h}: the size tag is gone\n{}", rows.join("\n"));
        let esc = rule.matches("Esc").count();
        assert_eq!(esc, rule.matches("Esc interrupt").count(), "{w}x{h}: a cut hint {rule:?}");
        assert!(!rule.contains("·  "), "{w}x{h}: a dangling dot {rule:?}");
        assert!(rule.chars().count() <= w as usize, "{w}x{h}: {rule:?}");
    }
}

/// Quiet has no other interrupt hint: keep it whole even when the clock no longer fits.
#[test]
fn activity_prefers_interrupt_to_clock_when_narrow() {
    let mut a = app();
    a.opts.detail = nexo_core::config::Detail::Quiet;
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    let rows = draw(&mut a, 30, 15);
    let rule = rows.iter().find(|r| r.contains("Thinking…")).expect("working line");
    assert!(rule.contains("Esc interrupt"), "{rule:?}");
    assert!(!rule.contains(" · "), "clock or dangling separator: {rule:?}");
}

/// A long activity is clipped at grapheme boundaries before the overlaid draft and effort tag.
#[test]
fn long_unicode_activity_stays_clear_of_the_tag() {
    let mut a = app();
    a.opts.detail = nexo_core::config::Detail::Quiet;
    a.effort = Some("high".into());
    a.input.set_text(&"draft ".repeat(80));
    a.handle_agent(AgentEvent::Preparing { what: format!("preparing e\u{301}👩‍💻 {}", "界".repeat(80)) });
    let rows = draw(&mut a, 60, 20);
    let rule = rows.iter().find(|r| r.contains("Preparing")).expect("working line");
    assert!(rule.contains("e\u{301}") && rule.contains('👩') && rule.contains('💻'), "{rule:?}");
    assert!(!rule.contains('\u{200d}'), "the terminal must not receive a joiner: {rule:?}");
    assert!(rule.contains('…') && rule.contains("◆ high") && rule.contains("chars"), "{rule:?}");
    assert!(!rule.contains("Esc") && !rule.contains(" ·  "), "{rule:?}");
}

/// With space, retain the full activity and the original clock, tokens, hint order.
#[test]
fn generous_activity_keeps_all_segments() {
    let mut a = app();
    a.handle_agent(AgentEvent::Preparing { what: "preparing the complete request".into() });
    a.streamed_chars = 400;
    let line = nexo_tui::ui::activity_line(&a, &a.activity().unwrap(), 100);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("Preparing the complete request…"), "{text:?}");
    let clock = text.find("0s").expect("clock");
    let tokens = text.find("↓ 100 tok").expect("tokens");
    let esc = text.find("Esc interrupt").expect("hint");
    assert!(clock < tokens && tokens < esc, "{text:?}");
}

/// Tiny widths show the activity, not a broken spinner or an invented shortcut.
#[test]
fn tiny_activity_does_not_leave_shortcut_fragments() {
    let a = app();
    for w in 1..=20 {
        let line = nexo_tui::ui::activity_line(&a, "Thinking", w);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains("Esc") && !text.contains('·'), "{w}: {text:?}");
        if (2..6).contains(&w) {
            assert!(text.starts_with('T'), "{w}: {text:?}");
        }
    }
}

/// A folded result that hides a single row said `1 more lines`.
#[test]
fn a_fold_hiding_one_row_says_line_not_lines() {
    let mut a = working();
    let all = draw(&mut a, 24, 10).join("\n");
    assert!(all.contains("1 more line") && !all.contains("1 more lines"), "{all}");
}

/// At 24x10 the scrollbar column overwrote a corner of the welcome card.
#[test]
fn the_scrollbar_never_overwrites_the_welcome_card() {
    let mut a = app();
    let rows = draw(&mut a, 24, 10);
    let all = rows.join("\n");
    let top: Vec<&String> =
        rows.iter().filter(|r| r.trim_start().starts_with('│') || r.trim_start().starts_with('╰')).collect();
    assert!(top.len() >= 3, "no card\n{all}");
    for r in top {
        // the last column is the scrollbar; the card's edge is the one just left of it
        let edge = r.chars().nth(22).unwrap();
        assert!("│╮╯".contains(edge), "card edge hit: {r:?}\n{all}");
    }
}
