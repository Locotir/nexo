//! One way to move between windows: Esc and a click outside go one level up the stack of where the person came
//! from, Enter goes deeper or confirms and never closes, Ctrl+C acts on what is on screen. One test per trap.

use super::*;
use crate::agents_view::AgentsView;
use nexo_core::jobs::{JobInfo, JobKind, JobStatus};
use nexo_core::subs::{SubInfo, SubState};
use nexo_supervise::{AgentRow, SourceKind, State};

#[test]
fn hover_multiline_message_hits_each_content_row() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(Block::User { text: "first line\nsecond line".into(), at: None });
    a.push(Block::User { text: "another message".into(), at: None });
    a.transcript_area = Rect::new(0, 1, 80, 20);
    a.layout(80);
    a.settle(20);
    let first = a.offsets[0];
    assert!(a.block_at_line(first + 1) == Some(0));
    a.pointer_moved(2, 1 + (first + 1) as u16);
    assert_eq!(a.hover, Hover::Head(0));
    a.pointer_moved(2, 1 + a.offsets[1] as u16);
    assert_ne!(a.hover, Hover::Head(0));
}

#[test]
fn hover_multiline_message_highlights_both_rows() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(Block::User { text: "first line\nsecond line".into(), at: None });
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let x = a.transcript_area.x + 2;
    let y = a.transcript_area.y + (a.offsets[0] - a.view_top()) as u16;
    let before = term.backend().buffer().clone();
    a.pointer_moved(x, y + 1);
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let after = term.backend().buffer();
    assert_ne!(before[(x, y)].style(), after[(x, y)].style(), "first row");
    assert_ne!(before[(x, y + 1)].style(), after[(x, y + 1)].style(), "second row");
}

const RUN: &str = "call_1#3";

fn with_run() -> (App, mpsc::UnboundedReceiver<AgentCommand>) {
    let (mut a, rx) = bare();
    a.handle_agent(AgentEvent::SubState { info: info(RUN, "survey the parser") });
    let job = |id: u32, kind| JobInfo {
        id,
        kind,
        label: "work".into(),
        status: JobStatus::Running,
        elapsed_ms: 0,
        lines: 0,
        held: false,
    };
    a.handle_agent(AgentEvent::Jobs { jobs: vec![job(3, JobKind::Agent), job(4, JobKind::Shell)] });
    a.session_id = Some("session".into());
    (a, rx)
}

fn info(id: &str, title: &str) -> SubInfo {
    SubInfo {
        id: id.into(),
        profile: "explore".into(),
        provider: None,
        model: None,
        reasoning_effort: None,
        title: title.into(),
        state: SubState::Background,
        started_ms: 1,
        ended_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        cost_usd: 0.0,
    }
}

fn row(key: &str) -> AgentRow {
    AgentRow {
        key: key.into(),
        name: key.into(),
        source: SourceKind::Nexo,
        program: "nexo".into(),
        state: State::Working,
        since: 0,
        last_step: None,
        last_conclusion: None,
        idle_secs: None,
        cost_usd: None,
        model: None,
        cwd: None,
        permission_mode: None,
        parent: None,
        last_action: None,
        paused: false,
        ribbon: vec![],
        spark: vec![],
        todo: None,
        tests: None,
        subs: vec![],
        unread: 0,
        held: 0,
        retry_secs: None,
        ctx_pct: None,
        goal: None,
        lane: None,
    }
}

fn panel(keys: &[&str]) -> Modal {
    Modal::Agents(Box::new(AgentsView::with_rows(keys.iter().map(|k| row(k)).collect(), 0, true)))
}

#[test]
fn panel_monitors_open_output_and_return_to_panel() {
    let (mut a, mut rx) = with_run();
    a.handle_agent(AgentEvent::Monitors {
        ids: vec![4],
        states: vec![(4, nexo_core::agent::MonitorStatus::Listening)],
    });
    a.handle_agent(AgentEvent::JobTails {
        tails: vec![nexo_core::jobs::JobTail { id: 4, text: "recent output".into(), ended_ago_ms: None }],
    });
    a.modal = Some(panel(&["session"]));
    press(&mut a, KeyCode::Char('m'));
    let Some(Modal::Jobs(v)) = &a.modal else { panic!("monitor list") };
    assert!(v.is_monitors());
    assert_eq!(v.len(), 1);
    let text = v.lines(100, &crate::theme::Theme::dark()).0.iter().map(|l| l.to_string()).collect::<String>();
    assert!(text.contains("work") && text.contains("listening") && text.contains("recent output"), "{text}");
    press(&mut a, KeyCode::Enter);
    assert!(matches!(&a.modal, Some(Modal::Jobs(v)) if v.detail));
    press(&mut a, KeyCode::Esc);
    press(&mut a, KeyCode::Esc);
    assert!(matches!(a.modal, Some(Modal::Agents(_))));
    assert!(!drain(&mut rx).iter().any(|c| matches!(c, AgentCommand::KillJob { .. })));
}

/// The run's view, entered from /panel with Enter.
fn sub_from_panel() -> (App, mpsc::UnboundedReceiver<AgentCommand>) {
    let (mut a, mut rx) = with_run();
    a.modal = Some(panel(&["session.3"]));
    press(&mut a, KeyCode::Enter);
    assert!(a.in_sub() && a.modal.is_none());
    drain(&mut rx);
    (a, rx)
}

fn ctrl_c(a: &mut App) {
    crate::event::key(a, KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
}

fn permission(a: &mut App) {
    a.handle_agent(AgentEvent::PermissionAsk {
        request: nexo_core::permission::PermissionRequest {
            id: "p1".into(),
            tool: "bash".into(),
            kind: nexo_core::tool::ToolKind::Exec,
            summary: "rm -rf build".into(),
            detail: nexo_core::tool::ToolDisplay::Plain,
            suggested_rules: vec![],
            agent: None,
        },
    });
}

fn interrupted(cmds: &[AgentCommand]) -> bool {
    cmds.iter().any(|c| matches!(c, AgentCommand::Interrupt | AgentCommand::InterruptRun))
}

fn settings_at(a: &App) -> Option<String> {
    match &a.modal {
        Some(Modal::Picker(p)) if p.kind == PickKind::Settings => p.current().map(|i| i.value.clone()),
        _ => None,
    }
}

#[test]
fn quick_escs_after_leaving_a_run_neither_interrupt_nor_rewind_and_rewind_asks_twice() {
    let (mut a, mut rx) = sub_from_panel();
    a.fx.clock = Some(Instant::now());
    a.busy = true;
    for _ in 0..4 {
        press(&mut a, KeyCode::Esc);
    }
    assert!(!a.in_sub() && a.modal.is_none(), "back in the chat, no rewind picker");
    assert!(!interrupted(&drain(&mut rx)), "the turn goes on");
    let items = vec![PickItem { label: "first".into(), value: "0".into(), hint: String::new() }];
    let mut m = Modal::Picker(Picker::new(PickKind::Rewind, "Rewind", items, None));
    assert!(matches!(m.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), ModalOut::Nothing));
    assert!(matches!(
        m.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ModalOut::Pick { kind: PickKind::Rewind, .. }
    ));
}

#[test]
fn a_permission_that_just_appeared_ignores_the_keys_that_answer_it() {
    let (mut a, mut rx) = with_run();
    let t0 = Instant::now();
    a.fx.clock = Some(t0);
    permission(&mut a);
    for code in [KeyCode::Enter, KeyCode::Esc, KeyCode::Char('y'), KeyCode::Char('n')] {
        press(&mut a, code);
    }
    assert!(matches!(a.modal, Some(Modal::Permission(_))), "the card stays");
    assert!(!drain(&mut rx).iter().any(|c| matches!(c, AgentCommand::Permission { .. })));
    a.fx.clock = Some(t0 + Duration::from_millis(500));
    press(&mut a, KeyCode::Enter);
    assert!(!matches!(a.modal, Some(Modal::Permission(_))), "read, then answered");
}

#[test]
fn this_sessions_row_says_enter_goes_back_to_the_chat() {
    let (mut a, _rx) = with_run();
    a.modal = Some(panel(&["session"]));
    let Some(Modal::Agents(v)) = &a.modal else { unreachable!() };
    assert!(v.footer_keys().contains(&"⏎ back to chat"), "{:?}", v.footer_keys());
}

#[test]
fn esc_in_a_runs_view_keeps_the_typed_text_in_history() {
    let (mut a, _rx) = sub_from_panel();
    a.input.set_text("half a thought");
    press(&mut a, KeyCode::Esc);
    assert!(a.in_sub() && a.input.is_empty());
    assert!(a.input.history().iter().any(|s| s == "half a thought"));
}

#[test]
fn ctrl_c_in_a_runs_view_stops_that_run_and_quitting_keeps_every_draft() {
    let (mut a, mut rx) = with_run();
    a.input.set_text("main draft");
    a.open_sub(RUN);
    drain(&mut rx);
    ctrl_c(&mut a);
    let sent = drain(&mut rx);
    assert!(matches!(sent.as_slice(), [AgentCommand::KillJob { id: Some(3) }]), "{sent:?}");
    assert!(a.in_sub() && !a.should_quit, "the main turn and nexo go on");
    a.input.set_text("sub draft");
    a.quit();
    let history = a.input.history();
    assert!(history.iter().any(|s| s == "main draft") && history.iter().any(|s| s == "sub draft"), "{history:?}");
}

#[test]
fn ctrl_c_in_a_panel_message_acts_as_esc_and_keeps_the_text() {
    let (mut a, _rx) = with_run();
    a.modal = Some(panel(&["nexo:peer"]));
    press(&mut a, KeyCode::Char('n'));
    assert!(matches!(&a.modal, Some(Modal::Agents(v)) if v.typing()), "a message is being typed");
    for c in "hold on".chars() {
        press(&mut a, KeyCode::Char(c));
    }
    ctrl_c(&mut a);
    let Some(Modal::Agents(v)) = &a.modal else { panic!("the panel stays") };
    assert!(!v.typing());
    assert!(v.said().is_some_and(|s| s.contains("message kept")), "{:?}", v.said());
}

#[test]
fn esc_in_a_run_opened_from_jobs_goes_back_to_jobs() {
    let (mut a, _rx) = with_run();
    a.open_jobs();
    press(&mut a, KeyCode::Down);
    press(&mut a, KeyCode::Enter);
    assert!(a.in_sub());
    press(&mut a, KeyCode::Esc);
    assert!(!a.in_sub() && matches!(a.modal, Some(Modal::Jobs(_))));
}

#[test]
fn enter_in_settings_switches_and_stays_and_its_pickers_come_back_to_it() {
    let (mut a, _rx) = with_run();
    a.modal = Some(a.settings_picker(None));
    let mouse = a.mouse_on;
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.mouse_on, !mouse);
    assert_eq!(settings_at(&a).as_deref(), Some("mouse"), "Settings stays, on the same row");
    a.modal = Some(a.settings_picker(Some("theme")));
    press(&mut a, KeyCode::Enter);
    assert!(settings_at(&a).is_none(), "the theme picker is on show");
    press(&mut a, KeyCode::Esc);
    assert_eq!(settings_at(&a).as_deref(), Some("theme"));
}

#[test]
fn enter_closes_neither_help_info_chrome_nor_the_observe_window() {
    let (mut a, _rx) = with_run();
    for cmd in ["/help", "/context", "/chrome"] {
        a.modal = None;
        a.input.set_text(cmd);
        a.submit();
        assert!(a.modal.is_some(), "{cmd} opens a window");
        press(&mut a, KeyCode::Enter);
        assert!(a.modal.is_some(), "{cmd}: Enter leaves it open");
    }
    let mut v = AgentsView::with_rows(vec![row("nexo:peer")], 0, true);
    v.observe_selected("nexo:peer");
    let key = |v: &mut AgentsView, code| v.key(KeyEvent::new(code, KeyModifiers::NONE));
    assert!(matches!(key(&mut v, KeyCode::Enter), ModalOut::Nothing));
    assert!(matches!(key(&mut v, KeyCode::Esc), ModalOut::Nothing), "Esc leaves the observe window first");
    assert!(matches!(key(&mut v, KeyCode::Esc), ModalOut::Close));
}

#[test]
fn another_sessions_view_says_where_text_goes_and_esc_leaves_while_a_message_is_on_its_way() {
    use super::super::sub::RemoteDelivery;
    let (mut a, _rx) = with_run();
    let (_snap_tx, snap_rx) = std::sync::mpsc::sync_channel(1);
    a.open_remote_receiver("peer", snap_rx);
    assert_eq!(a.sub_label().map(|l| l.0).as_deref(), Some("to peer"));
    let (_tx, rx) = std::sync::mpsc::sync_channel(1);
    a.sub_view.as_mut().unwrap().remote.as_mut().unwrap().delivery =
        RemoteDelivery::Sending { text: "message".into(), request_id: "request".into(), rx };
    press(&mut a, KeyCode::Esc);
    assert!(!a.in_sub(), "one Esc steps back at once");
}

#[test]
fn the_runs_hints_name_the_real_way_back_and_a_narrow_rule_keeps_esc() {
    let (mut a, _rx) = sub_from_panel();
    a.input.set_text("/help");
    a.submit();
    assert!(a.toasts.iter().any(|t| t.text.contains("Esc: back to the panel")), "the toast names the panel");
    let rows = frame(&mut a, 30, 8);
    let rule = rows.iter().find(|r| r.contains("sub ·")).unwrap_or_else(|| panic!("{}", rows.join("\n")));
    assert!(rule.contains(" esc "), "{rule}");
}

#[test]
fn a_click_outside_steps_back_one_level_and_the_close_box_closes_them_all() {
    let (mut a, _rx) = with_run();
    a.modal = Some(a.settings_picker(Some("theme")));
    press(&mut a, KeyCode::Enter);
    frame(&mut a, 100, 40);
    click(&mut a, 0, 0);
    assert_eq!(settings_at(&a).as_deref(), Some("theme"), "the click went one level up");
    press(&mut a, KeyCode::Enter);
    a.modal_out(ModalOut::CloseAll);
    assert!(a.modal.is_none(), "the close box leaves nothing on the way back");
}

#[test]
fn a_second_panel_opened_inside_a_run_is_the_one_esc_comes_back_to() {
    let (mut a, _rx) = sub_from_panel();
    a.handle_agent(AgentEvent::SubState { info: info("call_2#5", "second") });
    a.modal = Some(panel(&["session.5", "session.3"]));
    press(&mut a, KeyCode::Down);
    press(&mut a, KeyCode::Enter);
    assert!(a.in_sub());
    press(&mut a, KeyCode::Esc);
    let Some(Modal::Agents(v)) = &a.modal else { panic!("a panel comes back") };
    assert_eq!(v.selected_key().as_deref(), Some("session.3"), "the second panel, not the stale first one");
}

fn click(a: &mut App, x: u16, y: u16) {
    let kind = crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left);
    pointer(a, kind, x, y);
}

/// A sub-agent row of this session, running or ended.
fn sub_row(key: &str, state: State) -> AgentRow {
    AgentRow { parent: Some("session".into()), state, ..row(key) }
}

#[test]
fn stopping_a_job_asks_with_the_same_keys_as_every_question() {
    let mut v = AgentsView::with_rows(vec![row("session"), sub_row("session.3", State::Working)], 0, true);
    let mut key = |code| v.key(KeyEvent::new(code, KeyModifiers::NONE));
    key(KeyCode::Down);
    key(KeyCode::Char('k'));
    assert!(matches!(key(KeyCode::Char('x')), ModalOut::Nothing), "another key waits");
    assert!(matches!(key(KeyCode::Enter), ModalOut::Send(AgentCommand::KillJob { id: Some(3) })), "Enter confirms");
    key(KeyCode::Char('k'));
    assert!(matches!(key(KeyCode::Char('n')), ModalOut::Nothing));
    assert!(matches!(key(KeyCode::Char('y')), ModalOut::Nothing), "n cancelled: y now does nothing to the job");
}

#[test]
fn esc_with_a_kept_filter_clears_it_first_then_steps_back() {
    let (mut a, _rx) = with_run();
    a.modal = Some(panel(&["session", "nexo:peer"]));
    press(&mut a, KeyCode::Char('/'));
    press(&mut a, KeyCode::Char('p'));
    press(&mut a, KeyCode::Enter);
    press(&mut a, KeyCode::Esc);
    assert!(matches!(a.modal, Some(Modal::Agents(_))), "the first Esc clears the filter");
    press(&mut a, KeyCode::Esc);
    assert!(a.modal.is_none());
}

#[test]
fn enter_on_a_run_says_whether_it_opens_the_live_run_or_its_log() {
    let rows = vec![row("session"), sub_row("session.3", State::Working), sub_row("session.4", State::Done)];
    let mut v = AgentsView::with_rows(rows, 0, true);
    let keys_at = |v: &mut AgentsView| {
        v.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        v.footer_keys()
    };
    assert!(keys_at(&mut v).contains(&"⏎ live"));
    assert!(keys_at(&mut v).contains(&"⏎ log"));
}
