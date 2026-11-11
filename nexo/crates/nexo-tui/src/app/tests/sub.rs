//! A sub-agent's conversation in the transcript area: the way in, the view, writing, the way out, the race.

use super::*;
use nexo_core::jobs::{JobInfo, JobKind, JobStatus};
use nexo_core::subs::{SubInfo, SubItem, SubState, SubTranscript};

const RUN: &str = "call_1#3";

fn info(id: &str, title: &str, state: SubState) -> SubInfo {
    SubInfo {
        id: id.into(),
        profile: "explore".into(),
        title: title.into(),
        state,
        started_ms: 1,
        ended_ms: (!state.running()).then_some(2),
        tokens_in: 0,
        tokens_out: 0,
        cost_usd: 0.0,
    }
}

fn job(id: u32, kind: JobKind) -> JobInfo {
    JobInfo { id, kind, label: "work".into(), status: JobStatus::Running, elapsed_ms: 0, lines: 0 }
}

fn item(a: &mut App, id: &str, it: SubItem) {
    a.handle_agent(AgentEvent::Sub { id: id.into(), item: it });
}

fn person(text: &str) -> SubItem {
    SubItem::Person { text: text.into() }
}

fn reply(text: &str) -> SubItem {
    SubItem::Text { text: text.into() }
}

fn answer(a: &mut App, id: &str, items: Vec<SubItem>, dropped: usize) {
    a.handle_agent(AgentEvent::SubTranscript { id: id.into(), transcript: Some(SubTranscript { items, dropped }) });
}

/// An app with one running sub-agent run (job 3) and one shell job (job 4), the draft empty.
fn with_run() -> (App, mpsc::UnboundedReceiver<AgentCommand>) {
    let (mut a, rx) = bare();
    a.handle_agent(AgentEvent::SubState { info: info(RUN, "survey the parser", SubState::Background) });
    a.handle_agent(AgentEvent::Jobs { jobs: vec![job(3, JobKind::Agent), job(4, JobKind::Shell)] });
    (a, rx)
}

/// The view open and answered with `items`.
fn opened(items: Vec<SubItem>) -> (App, mpsc::UnboundedReceiver<AgentCommand>) {
    let (mut a, mut rx) = with_run();
    // the run did this while the view was shut
    for it in items.clone() {
        item(&mut a, RUN, it);
    }
    a.open_sub(RUN);
    let asked = drain(&mut rx);
    assert!(matches!(asked.as_slice(), [AgentCommand::SubTranscript { id }] if id == RUN), "{asked:?}");
    answer(&mut a, RUN, items, 0);
    (a, rx)
}

fn texts(a: &App, wanted: &str) -> usize {
    a.blocks
        .iter()
        .filter(|b| match b {
            Block::User { text, .. } | Block::Assistant { text, .. } => text == wanted,
            _ => false,
        })
        .count()
}

fn on_screen(rows: &[String], what: &str) -> bool {
    rows.iter().any(|r| r.contains(what))
}

#[test]
fn enter_on_a_sub_agent_row_opens_its_conversation_and_on_a_shell_job_opens_the_detail() {
    let (mut a, mut rx) = with_run();
    a.open_jobs();
    // the list shows running jobs newest first: the shell job (4), then the run (3)
    let Some(Modal::Jobs(v)) = &mut a.modal else { panic!("the jobs window") };
    assert_eq!(v.current().map(|j| j.id), Some(4));
    press(&mut a, KeyCode::Enter);
    match &a.modal {
        Some(Modal::Jobs(v)) => assert!(v.detail, "a shell job opens its detail as before"),
        _ => panic!("still the jobs window"),
    }
    assert!(!a.in_sub());
    press(&mut a, KeyCode::Esc);
    press(&mut a, KeyCode::Down);
    if let Some(Modal::Jobs(v)) = &a.modal {
        assert_eq!(v.current().map(|j| j.id), Some(3));
    } else {
        panic!("the jobs window");
    }
    drain(&mut rx);
    press(&mut a, KeyCode::Enter);
    assert!(a.modal.is_none(), "the window closes");
    assert!(a.in_sub());
    let sent = drain(&mut rx);
    assert!(matches!(sent.as_slice(), [AgentCommand::SubTranscript { id }] if id == RUN), "{sent:?}");
}

#[test]
fn the_view_draws_the_run_with_the_blocks_of_the_main_chat() {
    let (mut a, _rx) = opened(vec![
        SubItem::Task { text: "look at the parser".into() },
        SubItem::Call { id: "t1".into(), name: "read".into(), summary: "src/parser.rs".into() },
        SubItem::Result { id: "t1".into(), ok: true, bytes: 120 },
        person("and the lexer?"),
        reply("The lexer is small."),
    ]);
    let rows = frame(&mut a, 100, 30);
    for want in ["look at the parser", "src/parser.rs", "and the lexer?", "The lexer is small."] {
        assert!(on_screen(&rows, want), "{want} in\n{}", rows.join("\n"));
    }
    // the same person and reply in the main chat give the same rows
    let (mut m, _rx) = bare();
    m.push(Block::User { text: "and the lexer?".into(), at: None });
    m.push(Block::Assistant {
        text: "The lexer is small.".into(),
        streaming: false,
        at: None,
        started: None,
        took_ms: None,
    });
    let main = frame(&mut m, 100, 30);
    let find = |rows: &[String], what: &str| rows.iter().position(|r| r.contains(what)).unwrap();
    let (sub_user, sub_reply) = (find(&rows, "and the lexer?"), find(&rows, "The lexer is small."));
    let (main_user, main_reply) = (find(&main, "and the lexer?"), find(&main, "The lexer is small."));
    assert_eq!(sub_reply - sub_user, main_reply - main_user, "same rows between the two items");
    assert_eq!(rows[sub_user], main[main_user]);
}

#[test]
fn a_streamed_reply_and_then_the_whole_text_show_once() {
    let (mut a, _rx) = opened(vec![SubItem::Task { text: "go".into() }]);
    a.handle_agent(AgentEvent::SubDelta { id: RUN.into(), text: "Hello ".into() });
    a.handle_agent(AgentEvent::SubDelta { id: RUN.into(), text: "there".into() });
    assert!(on_screen(&frame(&mut a, 100, 30), "Hello there"));
    item(&mut a, RUN, reply("Hello there"));
    assert_eq!(texts(&a, "Hello there"), 1);
    assert!(a.blocks.iter().all(|b| !matches!(b, Block::Assistant { streaming: true, .. })));
}

#[test]
fn enter_sends_to_that_run_only_and_a_slash_text_sends_nothing() {
    let (mut a, mut rx) = opened(vec![SubItem::Task { text: "go".into() }]);
    a.input.set_text("check the tests too");
    a.submit();
    let sent = drain(&mut rx);
    assert!(
        matches!(sent.as_slice(), [AgentCommand::SubSubmit { id, text }] if id == RUN && text == "check the tests too"),
        "{sent:?}"
    );
    assert!(a.input.is_empty());
    assert_eq!(texts(&a, "check the tests too"), 0, "it appears when the registry's own item arrives");
    item(&mut a, RUN, person("check the tests too"));
    assert_eq!(texts(&a, "check the tests too"), 1);

    a.input.set_text("/jobs");
    a.submit();
    assert!(drain(&mut rx).is_empty(), "no command runs from here");
    assert_eq!(a.input.text(), "/jobs", "the text stays in the box");
    assert!(a.toasts.iter().any(|t| t.text == "commands run in the main chat · Esc back"));
    assert!(a.in_sub());
}

#[test]
fn esc_clears_the_text_then_leaves_and_never_interrupts() {
    let (mut a, mut rx) = bare();
    for i in 0..80 {
        a.push(Block::User { text: format!("main line {i}"), at: None });
    }
    a.handle_agent(AgentEvent::SubState { info: info(RUN, "survey", SubState::Background) });
    frame(&mut a, 100, 30);
    a.scroll = 12;
    a.follow = false;
    a.input.set_text("main draft");
    let (blocks, scroll) = (a.blocks.len(), a.scroll);
    a.open_sub(RUN);
    drain(&mut rx);
    answer(&mut a, RUN, vec![reply("sub text")], 0);
    assert!(a.input.is_empty(), "the main draft waits");
    assert_eq!(a.blocks.len(), 1);
    a.input.set_text("typed here");
    press(&mut a, KeyCode::Esc);
    assert!(a.in_sub() && a.input.is_empty(), "the first Esc clears the text");
    a.busy = true;
    press(&mut a, KeyCode::Esc);
    assert!(!a.in_sub(), "the second leaves");
    assert!(
        drain(&mut rx).iter().all(|c| !matches!(c, AgentCommand::Interrupt | AgentCommand::InterruptRun)),
        "nothing is interrupted"
    );
    assert_eq!(a.blocks.len(), blocks);
    assert_eq!(a.scroll, scroll, "the main chat is where it was");
    assert!(!a.follow);
    assert_eq!(a.input.text(), "main draft");
}

#[test]
fn the_main_chat_goes_on_receiving_its_events_underneath() {
    let (mut a, _rx) = opened(vec![reply("sub text")]);
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(AgentEvent::TextDelta { text: "main words".into() });
    assert_eq!(a.blocks.len(), 1, "the view is untouched");
    assert!(!on_screen(&frame(&mut a, 100, 30), "main words"));
    a.close_sub();
    assert!(on_screen(&frame(&mut a, 100, 30), "main words"));
}

#[test]
fn events_of_a_run_that_is_not_open_keep_nothing_and_draw_nothing() {
    let (mut a, _rx) = with_run();
    frame(&mut a, 100, 30);
    a.dirty = false;
    let before = a.blocks.len();
    item(&mut a, RUN, reply("quiet work"));
    a.handle_agent(AgentEvent::SubDelta { id: RUN.into(), text: "more".into() });
    assert!(!a.dirty, "no redraw");
    assert_eq!(a.blocks.len(), before);
    // another run's events while one is open draw nothing either
    let other = "call_2#5";
    a.handle_agent(AgentEvent::SubState { info: info(other, "other", SubState::Background) });
    a.open_sub(RUN);
    a.dirty = false;
    item(&mut a, other, reply("elsewhere"));
    a.handle_agent(AgentEvent::SubDelta { id: other.into(), text: "x".into() });
    assert!(!a.dirty);
}

#[test]
fn the_answer_racing_with_events_loses_none_and_doubles_none() {
    let (mut a, mut rx) = with_run();
    item(&mut a, RUN, SubItem::Task { text: "task".into() });
    item(&mut a, RUN, reply("a"));
    a.open_sub(RUN);
    drain(&mut rx);
    // handled before the answer; the answer holds it
    item(&mut a, RUN, person("m1"));
    a.handle_agent(AgentEvent::SubDelta { id: RUN.into(), text: "partial".into() });
    // the answer was made after m1 and after m2, which is still queued behind it
    answer(&mut a, RUN, vec![SubItem::Task { text: "task".into() }, reply("a"), person("m1"), person("m2")], 0);
    assert_eq!(texts(&a, "m1"), 1);
    assert_eq!(texts(&a, "m2"), 1);
    // the backlog: m2 again (in the answer already), then new work
    item(&mut a, RUN, person("m2"));
    assert_eq!(texts(&a, "m2"), 1, "not doubled");
    a.handle_agent(AgentEvent::SubDelta { id: RUN.into(), text: "fresh ".into() });
    a.handle_agent(AgentEvent::SubDelta { id: RUN.into(), text: "words".into() });
    item(&mut a, RUN, reply("fresh words"));
    item(&mut a, RUN, person("m3"));
    assert_eq!(texts(&a, "task"), 1);
    assert_eq!(texts(&a, "a"), 1);
    assert_eq!(texts(&a, "m1"), 1);
    assert_eq!(texts(&a, "m2"), 1);
    assert_eq!(texts(&a, "m3"), 1, "not lost");
    assert_eq!(texts(&a, "fresh words"), 1);
    assert_eq!(texts(&a, "partial"), 0, "a delta before the answer is not written again");
    assert_eq!(a.blocks.len(), 6, "task, a, m1, m2, fresh words, m3");
}

#[test]
fn items_dropped_by_the_registry_count_in_the_race() {
    let (mut a, mut rx) = with_run();
    for i in 0..5 {
        item(&mut a, RUN, reply(&format!("old {i}")));
    }
    a.open_sub(RUN);
    drain(&mut rx);
    // the registry kept the last two of the five and says three went
    answer(&mut a, RUN, vec![reply("old 3"), reply("old 4")], 3);
    item(&mut a, RUN, reply("new"));
    assert_eq!(texts(&a, "new"), 1);
    assert_eq!(texts(&a, "old 4"), 1);
}

#[test]
fn a_lag_notice_asks_for_the_conversation_again() {
    let (mut a, mut rx) = opened(vec![reply("a")]);
    // the agent sends the state of every run again after a lag: same state as the app knows
    a.handle_agent(AgentEvent::SubState { info: info(RUN, "survey the parser", SubState::Background) });
    let sent = drain(&mut rx);
    assert!(matches!(sent.as_slice(), [AgentCommand::SubTranscript { id }] if id == RUN), "{sent:?}");
    answer(&mut a, RUN, vec![reply("a"), reply("b")], 0);
    // from here every change is read again instead of counted
    item(&mut a, RUN, reply("c"));
    let sent = drain(&mut rx);
    assert!(matches!(sent.as_slice(), [AgentCommand::SubTranscript { .. }]), "{sent:?}");
    answer(&mut a, RUN, vec![reply("a"), reply("b"), reply("c")], 0);
    assert_eq!((texts(&a, "a"), texts(&a, "b"), texts(&a, "c")), (1, 1, 1));
}

#[test]
fn not_delivered_is_one_line_and_an_ended_run_says_so() {
    let (mut a, _rx) = opened(vec![reply("done")]);
    a.handle_agent(AgentEvent::SubState { info: info(RUN, "survey the parser", SubState::Ok) });
    let rows = frame(&mut a, 100, 30);
    assert!(on_screen(&rows, "sub · survey the parser · ended"), "{}", rows.join("\n"));
    assert!(on_screen(&rows, "ended · Esc back"));
    a.input.set_text("hello?");
    a.submit();
    a.handle_agent(AgentEvent::SubNotDelivered { id: RUN.into(), why: "the sub-agent has ended".into() });
    let toasts: Vec<_> = a.toasts.iter().map(|t| t.text.clone()).collect();
    assert_eq!(toasts, vec!["not delivered: the sub-agent has ended".to_string()]);
}

#[test]
fn the_rule_and_the_placeholder_name_the_run_in_the_accent_colour() {
    let (mut a, _rx) = opened(vec![reply("x")]);
    let rows = frame(&mut a, 100, 30);
    assert!(on_screen(&rows, "─ sub · survey the parser ─"), "{}", rows.join("\n"));
    assert!(on_screen(&rows, "message this sub-agent · Esc back"));
    let buf = buf_of(&mut a, 100, 30);
    let row = rows.iter().position(|r| r.contains("sub · survey")).unwrap() as u16;
    let col = rows[row as usize].chars().position(|c| c == 's').unwrap() as u16;
    assert_eq!(buf[(col, row)].fg, a.theme.accent);
}

#[test]
fn a_narrow_short_screen_overflows_nothing_and_cuts_the_title() {
    let (mut a, _rx) = with_run();
    a.handle_agent(AgentEvent::SubState {
        info: info(RUN, "survey the parser and every place it is used in the project", SubState::Background),
    });
    a.open_sub(RUN);
    answer(&mut a, RUN, vec![reply("x")], 0);
    let rows = frame(&mut a, 40, 8);
    assert_eq!(rows.len(), 8);
    assert!(rows.iter().all(|r| r.chars().count() <= 40));
    let rule = rows.iter().find(|r| r.contains("sub ·")).unwrap_or_else(|| panic!("{}", rows.join("\n")));
    assert!(rule.contains('…'), "{rule}");
    assert!(rule.starts_with("─ sub · "));
}

#[test]
fn the_view_adds_no_fixed_rows() {
    for detail in [nexo_core::config::Detail::Full, nexo_core::config::Detail::Quiet] {
        let (mut a, _rx) = with_run();
        a.opts.detail = detail;
        a.push(Block::User { text: "main".into(), at: None });
        frame(&mut a, 100, 30);
        let (t, e) = (a.transcript_area.height, a.editor_area.height);
        a.open_sub(RUN);
        answer(&mut a, RUN, vec![reply("x")], 0);
        frame(&mut a, 100, 30);
        assert_eq!((a.transcript_area.height, a.editor_area.height), (t, e), "{detail:?}");
    }
}

#[test]
fn a_permission_question_opens_over_the_view_and_answering_leaves_the_person_in_it() {
    let (mut a, mut rx) = opened(vec![reply("x")]);
    a.handle_agent(AgentEvent::PermissionAsk {
        request: nexo_core::permission::PermissionRequest {
            id: "p1".into(),
            tool: "bash".into(),
            kind: nexo_core::tool::ToolKind::Exec,
            summary: "rm -rf build".into(),
            detail: nexo_core::tool::ToolDisplay::Plain,
            suggested_rules: vec![],
            agent: Some("explore".into()),
        },
    });
    assert!(matches!(a.modal, Some(Modal::Permission(_))));
    a.modal_out(ModalOut::Permission { id: "p1".into(), decision: PermissionDecision::Allow });
    assert!(a.in_sub());
    assert!(matches!(drain(&mut rx).as_slice(), [AgentCommand::Permission { id, .. }] if id == "p1"));
}

#[test]
fn clear_closes_the_view() {
    let (mut a, _rx) = opened(vec![reply("x")]);
    a.handle_agent(AgentEvent::HistoryCleared);
    assert!(!a.in_sub());
    assert!(a.sub_runs.is_empty());
}

#[test]
fn the_jobs_chip_opens_the_one_run_and_the_list_when_there_are_several() {
    let (mut a, mut rx) = bare();
    a.handle_agent(AgentEvent::SubState { info: info(RUN, "survey", SubState::Background) });
    a.handle_agent(AgentEvent::Jobs { jobs: vec![job(3, JobKind::Agent)] });
    assert_eq!(a.sub_chip_target().as_deref(), Some(RUN));
    a.open_hint(HintHit::Jobs);
    assert!(a.in_sub());
    assert!(matches!(drain(&mut rx).as_slice(), [AgentCommand::SubTranscript { .. }]));
    a.close_sub();
    a.handle_agent(AgentEvent::Jobs { jobs: vec![job(3, JobKind::Agent), job(4, JobKind::Shell)] });
    assert_eq!(a.sub_chip_target(), None);
}
