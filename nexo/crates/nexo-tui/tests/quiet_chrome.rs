//! `[ui] detail = "quiet"` (the default): the screen under the draft is the rule and the status bar, no row of
//! key hints; the status bar loses the billed-token total and the curator figures and ends with `? help`; the
//! permission dialog opens as the question and three keys, its list of rules waits behind `a`; the welcome box
//! has name, model and folder. `detail = "full"` keeps every one of them as before.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_core::agent::AgentCommand;
use nexo_core::config::Detail;
use nexo_core::permission::{PermissionDecision, PermissionRequest};
use nexo_core::tool::{ToolDisplay, ToolKind};
use nexo_tui::app::HintHit;
use nexo_tui::modal::Modal;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, event, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn app(detail: Detail) -> (App, mpsc::UnboundedReceiver<AgentCommand>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![("deepseek".into(), "deepseek-chat".into())],
        theme: "dark".into(),
        timestamps: false,
        suggest: false,
        animations: false,
        detail,
        session_id: Some("20261027-120000-ab12".into()),
        ..TuiOptions::default()
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    (a, rx)
}

fn render(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer())
}

fn busy(a: &mut App) {
    a.busy = true;
    a.turn_started = Some(Instant::now() - Duration::from_secs(10));
}

fn key(a: &mut App, c: char) {
    event::key(a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
}

fn ask(rules: bool) -> PermissionRequest {
    PermissionRequest {
        agent: None,
        id: "p1".into(),
        tool: "bash".into(),
        kind: ToolKind::Exec,
        summary: "cargo test".into(),
        detail: ToolDisplay::Shell {
            command: "cargo test".into(),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
        },
        suggested_rules: if rules {
            vec!["bash(cargo test*)".into(), "bash(cargo *)".into(), "bash".into()]
        } else {
            Vec::new()
        },
    }
}

/// The rows of the dialog drawn in `rows`: from its top edge to its bottom edge.
fn dialog_height(rows: &[String]) -> usize {
    let top = rows.iter().position(|r| r.contains("permission")).expect("the dialog is drawn");
    let bottom = rows.iter().skip(top).position(|r| r.contains('╰')).expect("its bottom edge");
    bottom + 1
}

const KEY_WORDS: [&str; 4] = ["Enter send", "Ctrl+J newline", "Esc interrupt", "Enter add a message"];

#[test]
fn the_idle_screen_has_two_chrome_rows_at_both_sizes() {
    for (w, h) in [(120, 36), (80, 24)] {
        let (mut a, _rx) = app(Detail::Quiet);
        let rows = render(&mut a, w, h);
        let n = rows.len();
        // the rule, the draft, the status bar: the draft is the row right above the bar
        assert!(rows[n - 3].starts_with('─'), "{w}x{h} rule: {:?}", rows[n - 3]);
        assert!(rows[n - 2].starts_with('›'), "{w}x{h} draft: {:?}", rows[n - 2]);
        assert!(rows[n - 1].contains("nexo"), "{w}x{h} bar: {:?}", rows[n - 1]);
        let text = rows.join("\n");
        for k in KEY_WORDS {
            assert!(!text.contains(k), "{w}x{h}: {k}\n{text}");
        }
    }
}

#[test]
fn the_busy_screen_has_two_chrome_rows_and_a_short_placeholder() {
    for (w, h) in [(120, 36), (80, 24)] {
        let (mut a, _rx) = app(Detail::Quiet);
        busy(&mut a);
        let rows = render(&mut a, w, h);
        let n = rows.len();
        assert!(rows[n - 3].starts_with('─') && rows[n - 3].contains("Esc interrupt"), "{:?}", rows[n - 3]);
        assert!(rows[n - 2].starts_with('›') && rows[n - 2].contains("add a message"), "{:?}", rows[n - 2]);
        assert!(!rows[n - 2].contains("joins the turn"));
        assert!(rows[n - 1].contains("nexo"));
        // the interrupt key is said once, on the rule
        assert_eq!(rows.join("\n").matches("Esc interrupt").count(), 1);
    }
}

#[test]
fn full_keeps_the_keys_row_and_the_long_placeholder() {
    for (w, h) in [(120, 36), (80, 24)] {
        let (mut a, _rx) = app(Detail::Full);
        let rows = render(&mut a, w, h);
        let n = rows.len();
        assert!(rows[n - 2].contains("Enter send") && rows[n - 2].contains("? help"), "{:?}", rows[n - 2]);
        assert!(rows[n - 3].starts_with('›'), "the draft sits above the keys row");
        busy(&mut a);
        let rows = render(&mut a, w, h);
        assert!(rows[n - 2].contains("Esc interrupt"), "{:?}", rows[n - 2]);
        assert!(rows[n - 3].contains("type a message: it joins the turn"), "{:?}", rows[n - 3]);
        assert!(!rows[n - 1].contains("? help"), "full has no help chip in the bar");
    }
}

#[test]
fn the_bar_ends_with_help_and_drops_it_first() {
    let (mut a, _rx) = app(Detail::Quiet);
    let bar = |a: &mut App, w, h| render(a, w, h).pop().unwrap();
    let wide = bar(&mut a, 120, 36);
    assert!(wide.trim_end().ends_with("? help"), "{wide:?}");
    let (mut b, _rx) = app(Detail::Quiet);
    let wide_b = bar(&mut b, 120, 36);
    assert!(wide_b.contains("deepseek/deepseek-chat"));
    // a bar with no room left for it keeps the model and loses only the help
    let tight = bar(&mut a, 36, 24);
    assert!(!tight.contains("? help") && tight.contains("deepseek"), "{tight:?}");
}

#[test]
fn the_bar_keeps_the_cost_and_drops_the_billed_total_and_the_curator_figures() {
    let (mut a, _rx) = app(Detail::Quiet);
    a.cost = 0.0042;
    a.cost_shown = 0.0042;
    a.session_usage.input = 14_100;
    a.session_usage.output = 412;
    a.context = Some(nexo_tui::app::ContextInfo {
        policy: "curator".into(),
        label: "curator +2 in -1 out".into(),
        history_tokens: 14_900,
        ..Default::default()
    });
    let bar = render(&mut a, 120, 36).pop().unwrap();
    assert!(bar.contains("$0.0042"), "{bar:?}");
    assert!(!bar.contains("billed") && !bar.contains('Σ'), "{bar:?}");
    assert!(!bar.contains("curator"), "{bar:?}");
    let (mut f, _rx) = app(Detail::Full);
    f.cost = 0.0042;
    f.cost_shown = 0.0042;
    f.session_usage.input = 14_100;
    f.session_usage.output = 412;
    f.context = a.context.clone();
    let bar = render(&mut f, 120, 36).pop().unwrap();
    assert!(bar.contains("billed") && bar.contains("curator +2 in -1 out"), "{bar:?}");
}

#[test]
fn a_hand_listed_bar_still_gets_the_pieces_it_names() {
    let (mut a, _rx) = app(Detail::Quiet);
    a.opts.status_line = ["model", "context", "curator", "tokens", "cost"].map(String::from).to_vec();
    a.cost = 0.5;
    a.cost_shown = 0.5;
    a.session_usage.input = 2_000;
    a.context = Some(nexo_tui::app::ContextInfo {
        policy: "curator".into(),
        label: "curator idle".into(),
        ..Default::default()
    });
    let bar = render(&mut a, 120, 36).pop().unwrap();
    assert!(bar.contains("billed") && bar.contains("curator idle"), "{bar:?}");
}

#[test]
fn the_spinner_shows_while_the_curator_counts_a_loaded_session() {
    // opening a long session takes a moment: the default screen says so too, and stops when the figure is in
    for detail in [Detail::Quiet, Detail::Full] {
        let (mut a, _rx) = app(detail);
        assert!(!render(&mut a, 120, 36).pop().unwrap().contains("counting"), "nothing loaded, nothing to wait for");
        a.context_wait = true;
        let bar = render(&mut a, 120, 36).pop().unwrap();
        assert!(bar.contains("counting"), "{bar:?}");
    }
}

#[test]
fn a_message_and_the_queued_notice_ride_in_the_bar_and_take_no_row() {
    let (mut a, _rx) = app(Detail::Quiet);
    busy(&mut a);
    let bare = render(&mut a, 120, 36);
    a.flash("goal: make the weight tests pass");
    a.queued = 2;
    let rows = render(&mut a, 120, 36);
    assert_eq!(rows.len(), bare.len());
    let bar = rows.last().unwrap();
    assert!(bar.contains("goal: make the weight tests pass") && bar.contains("2 queued"), "{bar:?}");
    // nothing above the bar moved: the same rule and draft rows
    let n = rows.len();
    assert_eq!(rows[n - 3][..4], bare[n - 3][..4]);
    assert!(!rows[..n - 1].join("\n").contains("queued"), "no row of its own");
    // a click on the notice takes the queue back
    let hit = a.hint_hits.iter().find(|(_, h)| *h == HintHit::Queued).map(|(r, _)| *r).expect("the notice answers");
    assert_eq!(hit.y as usize, n - 1, "it sits on the status bar");
    // the session name is not drawn
    assert!(!bar.contains("20261027") && !bar.contains("demo"));
}

#[test]
fn a_narrow_bar_keeps_the_message_and_loses_pieces_of_its_own() {
    let (mut a, _rx) = app(Detail::Quiet);
    a.flash("saved");
    let bar = render(&mut a, 60, 24).pop().unwrap();
    assert!(bar.contains("saved") && bar.contains("deepseek"), "{bar:?}");
    assert!(unicode_width(&bar) <= 60);
}

fn unicode_width(s: &str) -> usize {
    s.chars().count()
}

#[test]
fn the_keys_row_returns_only_where_keys_need_saying() {
    let (mut a, _rx) = app(Detail::Quiet);
    let bare = render(&mut a, 120, 36);
    // a list that is not obvious says its keys
    key(&mut a, '/');
    let rows = render(&mut a, 120, 36);
    assert!(rows.join("\n").contains("↑↓ choose"), "the commands list says its keys");
    assert_eq!(rows.len(), bare.len());
}

#[test]
fn the_permission_dialog_opens_as_the_question_and_three_keys() {
    for (w, h) in [(120, 36), (80, 24)] {
        let (mut a, _rx) = app(Detail::Quiet);
        a.modal = Some(Modal::permission(ask(true)));
        let rows = render(&mut a, w, h);
        let text = rows.join("\n");
        assert!(dialog_height(&rows) <= 7, "{w}x{h}: {} rows\n{text}", dialog_height(&rows));
        assert!(text.contains("$ cargo test") && text.contains("[y] allow") && text.contains("[n] deny"), "{text}");
        assert!(!text.contains("Allow always with a rule"), "{text}");
        assert!(!text.contains("y/Enter allow"), "the dialog shows its keys: no line repeats them\n{text}");
    }
}

#[test]
fn the_first_a_opens_the_rules_and_a_digit_picks_one() {
    let (mut a, mut rx) = app(Detail::Quiet);
    a.modal = Some(Modal::permission(ask(true)));
    let closed = dialog_height(&render(&mut a, 120, 36));
    // a digit before the list shows answers nothing
    key(&mut a, '2');
    assert!(rx.try_recv().is_err() && a.modal.is_some());
    key(&mut a, 'a');
    assert!(rx.try_recv().is_err(), "the first a only opens the list");
    let rows = render(&mut a, 120, 36);
    let text = rows.join("\n");
    assert!(text.contains("Allow always with a rule") && text.contains("bash(cargo *)"), "{text}");
    assert!(dialog_height(&rows) > closed, "the dialog grew to hold the rules");
    key(&mut a, '2');
    match rx.try_recv() {
        Ok(AgentCommand::Permission { id, decision: PermissionDecision::AllowAlways { rule, persist } }) => {
            assert_eq!((id.as_str(), rule.as_str(), persist), ("p1", "bash(cargo *)", false));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_second_a_takes_the_first_rule_and_y_and_n_still_answer_at_once() {
    let (mut a, mut rx) = app(Detail::Quiet);
    a.modal = Some(Modal::permission(ask(true)));
    render(&mut a, 120, 36);
    key(&mut a, 'a');
    render(&mut a, 120, 36);
    key(&mut a, 'a');
    assert!(matches!(
        rx.try_recv(),
        Ok(AgentCommand::Permission { decision: PermissionDecision::AllowAlways { rule, .. }, .. }) if rule == "bash(cargo test*)"
    ));
    for (c, allow) in [('y', true), ('n', false)] {
        let (mut a, mut rx) = app(Detail::Quiet);
        a.modal = Some(Modal::permission(ask(true)));
        render(&mut a, 120, 36);
        key(&mut a, c);
        match rx.try_recv() {
            Ok(AgentCommand::Permission { decision, .. }) => {
                assert_eq!(matches!(decision, PermissionDecision::Allow), allow, "{decision:?}");
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn without_suggested_rules_a_alone_allows_always() {
    let (mut a, mut rx) = app(Detail::Quiet);
    a.modal = Some(Modal::permission(ask(false)));
    render(&mut a, 120, 36);
    key(&mut a, 'a');
    assert!(matches!(
        rx.try_recv(),
        Ok(AgentCommand::Permission { decision: PermissionDecision::AllowAlways { rule, .. }, .. }) if rule == "bash"
    ));
}

#[test]
fn full_shows_the_rules_and_the_key_line_at_once() {
    let (mut a, mut rx) = app(Detail::Full);
    a.modal = Some(Modal::permission(ask(true)));
    let rows = render(&mut a, 120, 36);
    let text = rows.join("\n");
    assert_eq!(dialog_height(&rows), 12, "{text}");
    assert!(text.contains("Allow always with a rule") && text.contains("[y] allow"), "{text}");
    // the digit and the a key answer at once
    key(&mut a, 'a');
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Permission { .. })));
}

#[test]
fn the_welcome_box_has_name_model_and_folder_only() {
    let (mut a, _rx) = app(Detail::Quiet);
    let rows = render(&mut a, 120, 36);
    let top = rows.iter().position(|r| r.starts_with('╭')).expect("the welcome box");
    let bottom = rows.iter().skip(top).position(|r| r.starts_with('╰')).unwrap();
    assert_eq!(bottom - 1, 3, "three lines inside the frame\n{}", rows[top..=top + bottom].join("\n"));
    let text = rows.join("\n");
    assert!(text.contains("nexo") && text.contains("deepseek/deepseek-chat") && text.contains("/tmp/project"));
    assert!(!text.contains("/resume picks a session"), "{text}");
    let (mut f, _rx) = app(Detail::Full);
    assert!(render(&mut f, 120, 36).join("\n").contains("/resume picks a session"));
}

#[test]
fn a_warning_on_a_bar_too_narrow_for_notes_still_reaches_a_toast_once() {
    let (mut a, _rx) = app(Detail::Quiet);
    a.flash_level(nexo_core::agent::Level::Error, "the provider refused the key: check DEEPSEEK_API_KEY");
    render(&mut a, 30, 24);
    assert_eq!(a.toasts.texts(), vec!["the provider refused the key: check DEEPSEEK_API_KEY"]);
    render(&mut a, 30, 24);
    assert_eq!(a.toasts.len(), 1, "once, not on every frame");
}

#[test]
fn a_window_with_no_room_for_a_rule_does_not_hide_a_step_behind_a() {
    let (mut a, mut rx) = app(Detail::Quiet);
    a.modal = Some(Modal::permission(ask(true)));
    // 7 rows: the dialog has no line left for a rule, so there is nothing for the first `a` to open
    let before = render(&mut a, 80, 7);
    key(&mut a, 'a');
    assert!(
        matches!(rx.try_recv(), Ok(AgentCommand::Permission { decision: PermissionDecision::AllowAlways { .. }, .. })),
        "`a` answers at once when no rule can be drawn: {before:?}"
    );
}
