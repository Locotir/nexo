//! A sub-agent's conversation in the transcript area: the way in, the view, writing, the way out, the race.

use super::*;
use crate::render::blocks::TASK_HEAD;
use nexo_core::jobs::{JobInfo, JobKind, JobStatus};
use nexo_core::subs::{SubInfo, SubItem, SubState, SubTranscript};

const RUN: &str = "call_1#3";

fn info(id: &str, title: &str, state: SubState) -> SubInfo {
    SubInfo {
        id: id.into(),
        profile: "explore".into(),
        provider: None,
        model: None,
        reasoning_effort: None,
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
    JobInfo { id, kind, label: "work".into(), status: JobStatus::Running, elapsed_ms: 0, lines: 0, held: false }
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
    let request = a.sub_view.as_ref().and_then(|v| v.asking).unwrap_or(0);
    a.handle_agent(AgentEvent::SubTranscript {
        id: id.into(),
        request,
        transcript: Some(SubTranscript { items, dropped, partial: String::new() }),
    });
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
    assert!(matches!(asked.as_slice(), [AgentCommand::SubTranscript { id, .. }] if id == RUN), "{asked:?}");
    answer(&mut a, RUN, items, 0);
    (a, rx)
}

fn flush_refresh(a: &mut App) {
    if let Some(v) = a.sub_view.as_mut() {
        v.last_ask = None;
    }
    a.sub_tick();
}

fn texts(a: &App, wanted: &str) -> usize {
    a.blocks
        .iter()
        .filter(|b| match b {
            Block::User { text, .. } | Block::Assistant { text, .. } => text == wanted,
            // the main agent's task is a notice: its head, then the task as it was given
            Block::Notice { text, .. } => text.strip_prefix(&format!("{TASK_HEAD}\n")).is_some_and(|t| t == wanted),
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
    assert!(matches!(sent.as_slice(), [AgentCommand::SubTranscript { id, .. }] if id == RUN), "{sent:?}");
}

#[test]
fn the_view_draws_the_run_with_the_blocks_of_the_main_chat() {
    let (mut a, _rx) = opened(vec![
        SubItem::Task { text: "look at the parser".into() },
        SubItem::Call { id: "t1".into(), name: "read".into(), summary: "src/parser.rs".into(), args: "{}".into() },
        SubItem::Result { id: "t1".into(), ok: true, bytes: 120, output: "parser output".into() },
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
fn the_task_from_the_main_agent_is_not_drawn_as_the_persons_words() {
    let task = "look at the parser\nfix *a* and `b` and **c**";
    let (mut a, _rx) = opened(vec![SubItem::Task { text: task.into() }]);
    assert!(!a.blocks.iter().any(|b| matches!(b, Block::User { .. })), "the task is not the person's text");
    let held = a.blocks.iter().any(|b| match b {
        Block::Notice { text, .. } => text == &format!("{TASK_HEAD}\n{task}"),
        _ => false,
    });
    assert!(held, "a notice holds both lines of the task");
    let rows = frame(&mut a, 100, 30);
    for want in [TASK_HEAD, "look at the parser", "fix *a* and `b` and **c**"] {
        assert!(on_screen(&rows, want), "{want} in\n{}", rows.join("\n"));
    }
}

#[test]
fn a_streamed_reply_and_then_the_whole_text_show_once() {
    let (mut a, mut rx) = opened(vec![SubItem::Task { text: "go".into() }]);
    a.handle_agent(AgentEvent::SubDelta { id: RUN.into(), text: "Hello ".into() });
    a.handle_agent(AgentEvent::SubDelta { id: RUN.into(), text: "there".into() });
    flush_refresh(&mut a);
    assert!(matches!(drain(&mut rx).as_slice(), [AgentCommand::SubTranscript { id, .. }] if id == RUN));
    a.handle_agent(AgentEvent::SubTranscript {
        id: RUN.into(),
        request: a.sub_view.as_ref().and_then(|v| v.asking).unwrap_or(0),
        transcript: Some(SubTranscript {
            items: vec![SubItem::Task { text: "go".into() }],
            dropped: 0,
            partial: "Hello there".into(),
        }),
    });
    assert!(on_screen(&frame(&mut a, 100, 30), "Hello there"));
    item(&mut a, RUN, reply("Hello there"));
    assert_eq!(texts(&a, "Hello there"), 1);
    assert!(a.blocks.iter().all(|b| !matches!(b, Block::Assistant { streaming: true, .. })));
}

#[test]
fn concurrent_deltas_coalesce_and_refresh_after_an_older_snapshot() {
    let (mut a, mut rx) = opened(vec![]);
    for _ in 0..100 {
        a.handle_agent(AgentEvent::SubDelta { id: RUN.into(), text: "x".into() });
    }
    flush_refresh(&mut a);
    assert!(matches!(drain(&mut rx).as_slice(), [AgentCommand::SubTranscript { .. }]));
    a.handle_agent(AgentEvent::SubDelta { id: RUN.into(), text: "later".into() });
    a.handle_agent(AgentEvent::SubTranscript {
        id: RUN.into(),
        request: a.sub_view.as_ref().and_then(|v| v.asking).unwrap_or(0),
        transcript: Some(SubTranscript { items: vec![], dropped: 0, partial: "old".into() }),
    });
    flush_refresh(&mut a);
    assert!(matches!(drain(&mut rx).as_slice(), [AgentCommand::SubTranscript { .. }]));
    a.handle_agent(AgentEvent::SubTranscript {
        id: RUN.into(),
        request: a.sub_view.as_ref().and_then(|v| v.asking).unwrap_or(0),
        transcript: Some(SubTranscript { items: vec![], dropped: 0, partial: "new".into() }),
    });
    assert_eq!(texts(&a, "new"), 1);
    assert_eq!(texts(&a, "old"), 0);
    assert!(drain(&mut rx).is_empty());
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

    flush_refresh(&mut a);
    assert!(matches!(drain(&mut rx).as_slice(), [AgentCommand::SubTranscript { id, .. }] if id == RUN));
    a.input.set_text("/jobs");
    a.submit();
    assert!(drain(&mut rx).is_empty(), "no command runs from here");
    assert_eq!(a.input.text(), "/jobs", "the text stays in the box");
    assert!(a.toasts.iter().any(|t| t.text == "commands run in the main chat · Esc: back to the chat"));
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
    flush_refresh(&mut a);
    let sent = drain(&mut rx);
    assert!(matches!(sent.as_slice(), [AgentCommand::SubTranscript { id, .. }] if id == RUN), "{sent:?}");
    answer(&mut a, RUN, vec![reply("a"), reply("b")], 0);
    // from here every change is read again instead of counted
    item(&mut a, RUN, reply("c"));
    flush_refresh(&mut a);
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
    a.handle_agent(AgentEvent::SubNotDelivered {
        id: RUN.into(),
        why: "the sub-agent has ended".into(),
        text: "hello?".into(),
    });
    let toasts: Vec<_> = a.toasts.iter().map(|t| t.text.clone()).collect();
    assert_eq!(toasts, vec!["not delivered: the sub-agent has ended · restored in editor".to_string()]);
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
fn resolved_target_is_announced_once_and_updates_the_open_view() {
    let (mut a, _rx) = with_run();
    let mut resolved = info(RUN, "survey the parser", SubState::Background);
    resolved.provider = Some("provider".into());
    resolved.model = Some("model".into());
    a.handle_agent(AgentEvent::SubState { info: resolved.clone() });
    let lines = |a: &crate::app::App| {
        a.blocks
            .iter()
            .filter(|b| {
                matches!(b, Block::Agent(r) if r.profile == "explore" && r.model == "provider/model" && r.title == "survey the parser")
            })
            .count()
    };
    assert_eq!(lines(&a), 1);
    a.handle_agent(AgentEvent::SubState { info: resolved.clone() });
    assert_eq!(lines(&a), 1);
    a.open_sub(RUN);
    answer(&mut a, RUN, vec![reply("x")], 0);
    assert!(on_screen(&frame(&mut a, 100, 30), "provider/model · survey"));
    resolved.reasoning_effort = Some("high".into());
    a.handle_agent(AgentEvent::SubState { info: resolved });
    assert!(on_screen(&frame(&mut a, 100, 30), "provider/model · high"));
    let rows = frame(&mut a, 24, 8);
    assert!(rows.iter().all(|r| crate::render::wrap::width(r) <= 24));
    a.close_sub();
    assert!(a.blocks.iter().any(|b| matches!(b, Block::Agent(r) if r.effort.as_deref() == Some("high"))));
    assert_eq!(lines(&a), 1);
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
    flush_refresh(&mut a);
    assert!(matches!(drain(&mut rx).as_slice(), [AgentCommand::SubTranscript { .. }]));
    a.close_sub();
    a.handle_agent(AgentEvent::Jobs { jobs: vec![job(3, JobKind::Agent), job(4, JobKind::Shell)] });
    assert_eq!(a.sub_chip_target(), None);
}

// ------------------------------------------------------------ the fix round

fn send_text(a: &mut App, t: &str) {
    a.input.set_text(t);
    a.submit();
}

#[test]
fn a_paste_in_the_view_marks_the_screen_dirty_and_the_box_shows_it() {
    let (mut a, _rx) = opened(vec![reply("x")]);
    frame(&mut a, 100, 30);
    a.dirty = false;
    crate::event::handle(&mut a, crossterm::event::Event::Paste("also check the lexer".into()));
    assert!(a.dirty, "a paste repaints");
    assert!(on_screen(&frame(&mut a, 100, 30), "also check the lexer"));
}

#[test]
fn the_view_hands_nothing_to_the_inline_scrollback_and_the_main_flush_count_is_kept() {
    let (mut a, _rx) = bare();
    for i in 0..3 {
        a.push(Block::User { text: format!("main {i}"), at: None });
    }
    a.handle_agent(AgentEvent::SubState { info: info(RUN, "survey", SubState::Background) });
    assert!(!crate::inline::take_settled(&mut a, 100).is_empty());
    assert_eq!(a.inline_flushed, 3);
    a.open_sub(RUN);
    answer(&mut a, RUN, vec![reply("sub one"), reply("sub two")], 0);
    assert!(crate::inline::take_settled(&mut a, 100).is_empty(), "the sub transcript stays out of the scrollback");
    press(&mut a, KeyCode::Esc);
    assert!(!a.in_sub());
    assert_eq!(a.inline_flushed, 3);
    assert!(crate::inline::take_settled(&mut a, 100).is_empty());
}

#[test]
fn a_rebuilt_answer_starts_with_nothing_flushed() {
    let (mut a, _rx) = opened(vec![reply("one")]);
    a.inline_flushed = 2;
    a.handle_agent(AgentEvent::SubDelta { id: RUN.into(), text: "x".into() });
    flush_refresh(&mut a);
    answer(&mut a, RUN, vec![reply("one"), reply("two")], 0);
    assert_eq!(a.inline_flushed, 0);
}

#[test]
fn tab_right_and_up_in_the_view_leave_the_main_chats_suggestion_and_queue_alone() {
    let (mut a, mut rx) = opened(vec![reply("x")]);
    a.opts.suggest = true;
    a.suggestion = Some("run the tests".into());
    a.queued_msgs.push("queued request".into());
    a.queued = 1;
    drain(&mut rx);
    for code in [KeyCode::Tab, KeyCode::Right, KeyCode::Up] {
        press(&mut a, code);
        assert!(a.input.is_empty(), "{code:?} put {:?} in the box", a.input.text());
    }
    assert!(drain(&mut rx).iter().all(|c| !matches!(c, AgentCommand::Unqueue { .. })));
    assert_eq!(a.queued_msgs, vec!["queued request".to_string()]);
}

#[test]
fn a_suggestion_arriving_in_the_view_is_dropped_and_never_shown() {
    let (mut a, _rx) = opened(vec![reply("x")]);
    a.opts.suggest = true;
    a.handle_agent(AgentEvent::Suggestion { text: "ship it now".into() });
    assert!(!on_screen(&frame(&mut a, 100, 30), "ship it now"));
    press(&mut a, KeyCode::Tab);
    assert!(a.input.is_empty());
    press(&mut a, KeyCode::Esc);
    assert!(!a.in_sub());
    assert!(a.suggestion.is_none(), "dropped, not kept for the return");
}

#[test]
fn after_a_rewind_the_run_announced_again_still_opens_from_jobs() {
    let (mut a, mut rx) = opened(vec![reply("x")]);
    // a rewind: the main chat is cleared and the agent says where every run stands
    a.handle_agent(AgentEvent::HistoryCleared);
    assert!(!a.in_sub() && a.sub_runs.is_empty());
    a.handle_agent(AgentEvent::SubState { info: info(RUN, "survey the parser", SubState::Background) });
    a.handle_agent(AgentEvent::Jobs { jobs: vec![job(3, JobKind::Agent)] });
    drain(&mut rx);
    a.open_jobs();
    press(&mut a, KeyCode::Enter);
    assert!(a.in_sub());
    assert!(drain(&mut rx).iter().any(|c| matches!(c, AgentCommand::SubTranscript { id, .. } if id == RUN)));
}

fn pending_text(a: &mut App, w: u16) -> Vec<String> {
    frame(a, w, 30)
}

#[test]
fn a_sent_message_shows_in_the_empty_boxs_placeholder_until_it_arrives() {
    let (mut a, mut rx) = opened(vec![reply("x")]);
    send_text(&mut a, "also check the lexer");
    drain(&mut rx);
    let rows = pending_text(&mut a, 100);
    assert!(on_screen(&rows, "sent · arrives at its next step"), "{}", rows.join("\n"));
    assert!(!on_screen(&rows, "also check the lexer"), "the message is not drawn a second time");
    item(&mut a, RUN, person("also check the lexer"));
    let rows = pending_text(&mut a, 100);
    assert!(on_screen(&rows, "message this sub-agent · Esc back"), "{}", rows.join("\n"));
    assert!(!on_screen(&rows, "sent ·"));
}

#[test]
fn a_not_delivered_notice_lowers_the_count_and_two_sends_count_two() {
    let (mut a, mut rx) = opened(vec![reply("x")]);
    send_text(&mut a, "one");
    send_text(&mut a, "two");
    drain(&mut rx);
    assert!(on_screen(&pending_text(&mut a, 100), "2 sent · arrive at its next step"));
    item(&mut a, RUN, person("one"));
    assert!(on_screen(&pending_text(&mut a, 100), "sent · arrives at its next step"));
    a.handle_agent(AgentEvent::SubNotDelivered { id: RUN.into(), why: "it ended".into(), text: "two".into() });
    assert_eq!(a.input.full_text(), "two");
}

#[test]
fn leaving_the_view_forgets_the_pending_count_and_the_placeholder_is_cut_when_narrow() {
    let (mut a, mut rx) = opened(vec![reply("x")]);
    send_text(&mut a, "one");
    send_text(&mut a, "two");
    drain(&mut rx);
    let rows = pending_text(&mut a, 40);
    assert!(on_screen(&rows, "2 sent · arrive at its next step"), "{}", rows.join("\n"));
    let rows = pending_text(&mut a, 26);
    let line = rows.iter().find(|r| r.contains("sent")).unwrap_or_else(|| panic!("{:?}", rows));
    assert!(line.contains('…'), "{line}");
    press(&mut a, KeyCode::Esc);
    a.open_sub(RUN);
    answer(&mut a, RUN, vec![reply("x")], 0);
    assert!(on_screen(&pending_text(&mut a, 100), "message this sub-agent · Esc back"));
}

#[test]
fn events_of_another_run_change_nothing_in_the_open_view() {
    let (mut a, _rx) = opened(vec![reply("mine")]);
    let other = "call_2#5";
    a.handle_agent(AgentEvent::SubState { info: info(other, "other", SubState::Background) });
    frame(&mut a, 100, 30);
    a.dirty = false;
    let blocks = a.blocks.len();
    item(&mut a, other, reply("elsewhere"));
    a.handle_agent(AgentEvent::SubDelta { id: other.into(), text: "x".into() });
    assert!(!a.dirty);
    assert_eq!(a.blocks.len(), blocks);
    assert!(!on_screen(&frame(&mut a, 100, 30), "elsewhere"));
}

#[test]
fn a_resize_while_the_view_is_open_leaves_the_main_chat_where_a_plain_resize_would() {
    let build = || {
        let (mut a, rx) = bare();
        for i in 0..80 {
            a.push(Block::User { text: format!("main line {i} with some words to wrap around the screen"), at: None });
        }
        a.handle_agent(AgentEvent::SubState { info: info(RUN, "survey", SubState::Background) });
        frame(&mut a, 100, 30);
        a.scroll = 12;
        a.follow = false;
        (a, rx)
    };
    let (mut plain, _r1) = build();
    crate::event::handle(&mut plain, crossterm::event::Event::Resize(60, 20));
    let want = frame(&mut plain, 60, 20);
    let (mut a, _r2) = build();
    a.open_sub(RUN);
    answer(&mut a, RUN, vec![reply("sub text")], 0);
    frame(&mut a, 100, 30);
    crate::event::handle(&mut a, crossterm::event::Event::Resize(60, 20));
    frame(&mut a, 60, 20);
    press(&mut a, KeyCode::Esc);
    assert_eq!(frame(&mut a, 60, 20), want);
    assert_eq!(a.scroll, plain.scroll);
    assert_eq!(a.follow, plain.follow);
}

#[test]
fn right_and_l_on_a_run_row_open_the_jobs_detail_and_a_click_opens_the_conversation() {
    for code in [KeyCode::Right, KeyCode::Char('l')] {
        let (mut a, _rx) = with_run();
        a.open_jobs();
        press(&mut a, KeyCode::Down);
        press(&mut a, code);
        assert!(!a.in_sub(), "{code:?} stays in the window");
        assert!(matches!(&a.modal, Some(Modal::Jobs(v)) if v.detail), "{code:?} opens the job's own output");
    }
    let (mut a, mut rx) = with_run();
    a.open_jobs();
    press(&mut a, KeyCode::Down);
    drain(&mut rx);
    let Some(Modal::Jobs(v)) = &mut a.modal else { panic!("the jobs window") };
    let out = v.click_row(1);
    assert!(matches!(out, ModalOut::OpenSub(ref id) if id == RUN), "a click on the run row opens it");
}

fn panel_row(key: &str, source: nexo_supervise::SourceKind) -> nexo_supervise::AgentRow {
    use nexo_supervise::{AgentRow, State};
    AgentRow {
        key: key.into(),
        name: "same title".into(),
        source,
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

#[test]
fn panel_enter_opens_local_run_and_returns_to_same_panel() {
    use nexo_supervise::SourceKind;
    let (mut a, mut rx) = with_run();
    a.session_id = Some("session".into());
    a.input.set_text("main draft");
    let panel = crate::agents_view::AgentsView::with_rows(vec![panel_row("session.3", SourceKind::Nexo)], 0, true);
    a.modal = Some(Modal::Agents(Box::new(panel)));
    drain(&mut rx);
    press(&mut a, KeyCode::Enter);
    assert!(a.in_sub() && a.modal.is_none());
    flush_refresh(&mut a);
    assert!(matches!(drain(&mut rx).as_slice(), [AgentCommand::SubTranscript { id, .. }] if id == RUN));
    item(&mut a, RUN, reply("live"));
    answer(&mut a, RUN, vec![reply("live")], 0);
    item(&mut a, RUN, reply("next"));
    assert_eq!(texts(&a, "next"), 1);
    flush_refresh(&mut a);
    let snapshots = drain(&mut rx);
    assert!(matches!(snapshots.as_slice(), [AgentCommand::SubTranscript { id, .. }] if id == RUN), "{snapshots:?}");
    a.input.set_text("intervene");
    a.submit();
    assert!(
        matches!(drain(&mut rx).as_slice(), [AgentCommand::SubSubmit { id, text }] if id == RUN && text == "intervene")
    );
    press(&mut a, KeyCode::Esc);
    assert!(!a.in_sub());
    assert_eq!(a.input.text(), "main draft");
    assert!(matches!(&a.modal, Some(Modal::Agents(v)) if v.selected_key().as_deref() == Some("session.3")));
}

#[test]
fn panel_does_not_confuse_foreign_or_ambiguous_jobs_with_local_runs() {
    use nexo_supervise::SourceKind;
    for (key, source, duplicate) in [
        ("other.3", SourceKind::Nexo, false),
        ("session.3", SourceKind::External, false),
        ("session.3", SourceKind::Nexo, true),
    ] {
        let (mut a, mut rx) = with_run();
        a.session_id = Some("session".into());
        if duplicate {
            a.handle_agent(AgentEvent::SubState { info: info("other#3", "same title", SubState::Background) });
        }
        a.modal = Some(Modal::Agents(Box::new(crate::agents_view::AgentsView::with_rows(
            vec![panel_row(key, source)],
            0,
            true,
        ))));
        drain(&mut rx);
        press(&mut a, KeyCode::Enter);
        assert!(!a.in_sub(), "{key:?} {source:?}");
        assert!(drain(&mut rx).is_empty());
    }
}

#[test]
fn tool_payloads_in_snapshot_and_live_are_shown_safely() {
    let (mut a, _rx) = opened(vec![SubItem::Call {
        id: "safe".into(),
        name: "read".into(),
        summary: "file".into(),
        args: "{\"path\":\"hello\\u001bworld\"}".into(),
    }]);
    a.handle_sub(AgentEvent::Sub {
        id: RUN.into(),
        item: SubItem::Result { id: "safe".into(), ok: true, bytes: 9, output: "out\u{1b}put".into() },
    });
    let tool = a
        .blocks
        .iter()
        .find_map(|b| match b {
            Block::Tool { args, output, .. } => Some((args, output)),
            _ => None,
        })
        .unwrap();
    assert_eq!(tool.0["path"], "helloworld");
    assert_eq!(tool.1.as_ref().unwrap().content, "output");
}

#[test]
fn rejected_messages_preserve_existing_draft_and_each_other() {
    let (mut a, _) = opened(vec![reply("x")]);
    a.input.set_text("draft");
    for text in ["first", "second"] {
        a.handle_agent(AgentEvent::SubNotDelivered { id: RUN.into(), why: "full".into(), text: text.into() });
    }
    assert_eq!(a.input.full_text(), "draft\n\nfirst\n\nsecond");
}

#[test]
fn stale_reply_after_reopen_cannot_replace_current_snapshot() {
    let (mut a, mut rx) = with_run();
    a.open_sub(RUN);
    let old = a.sub_view.as_ref().unwrap().asking.unwrap();
    a.close_sub();
    a.open_sub(RUN);
    let new = a.sub_view.as_ref().unwrap().asking.unwrap();
    assert_ne!(old, new);
    drain(&mut rx);
    a.handle_agent(AgentEvent::SubTranscript {
        id: RUN.into(),
        request: old,
        transcript: Some(SubTranscript { items: vec![reply("obsolete")], dropped: 0, partial: String::new() }),
    });
    assert_eq!(texts(&a, "obsolete"), 0);
    assert_eq!(a.sub_view.as_ref().unwrap().asking, Some(new));
    answer(&mut a, RUN, vec![reply("current")], 0);
    assert_eq!(texts(&a, "current"), 1);
}

#[test]
fn refresh_keeps_search_and_selection_on_unchanged_prefix() {
    let (mut a, mut rx) = opened(vec![reply("find me")]);
    frame(&mut a, 100, 30);
    a.open_search();
    a.search.as_mut().unwrap().query = "find".into();
    a.rematch();
    a.selected = Some(0);
    a.selection = Some(crate::select::Selection::new(crate::select::Pos { line: 0, col: 0 }));
    a.handle_agent(AgentEvent::SubDelta { id: RUN.into(), text: "x".into() });
    flush_refresh(&mut a);
    drain(&mut rx);
    answer(&mut a, RUN, vec![reply("find me"), reply("new")], 0);
    assert_eq!(a.search.as_ref().unwrap().query, "find");
    assert!(!a.search.as_ref().unwrap().matches.is_empty());
    assert_eq!(a.selected, Some(0));
    assert!(a.selection.is_some());
}

#[test]
fn fast_responses_do_not_trigger_a_snapshot_per_delta() {
    let (mut a, mut rx) = opened(vec![]);
    for _ in 0..100 {
        a.handle_agent(AgentEvent::SubDelta { id: RUN.into(), text: "x".into() });
        a.sub_tick();
    }
    assert!(drain(&mut rx).is_empty(), "refresh is rate limited after the opening answer");
    flush_refresh(&mut a);
    let sent = drain(&mut rx);
    assert_eq!(sent.len(), 1);
    answer(&mut a, RUN, vec![], 0);
    for _ in 0..100 {
        a.handle_agent(AgentEvent::SubDelta { id: RUN.into(), text: "x".into() });
        a.sub_tick();
    }
    assert!(drain(&mut rx).is_empty());
    flush_refresh(&mut a);
    assert_eq!(drain(&mut rx).len(), 1);
}

#[test]
fn remote_snapshots_refresh_isolate_local_events_and_restore_panel() {
    use super::super::sub::RemoteUpdate;
    use nexo_core::agent::channels;
    use nexo_supervise::SourceKind;
    let (mut a, mut commands) = with_run();
    a.input.set_text("main draft");
    let panel =
        Box::new(crate::agents_view::AgentsView::with_rows(vec![panel_row("remote", SourceKind::Nexo)], 0, true));
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    a.open_remote_receiver(RUN, rx); // deliberately collide with a local run identity
    *a.back.last_mut().unwrap() = crate::app::types::Back::Sub(Some(Modal::Agents(panel)));
    let (observer, ends) = channels();
    ends.events.send(AgentEvent::SessionChanged { session_id: RUN.into(), title: String::new() }).unwrap();
    ends.events.send(AgentEvent::TextDelta { text: "first".into() }).unwrap();
    let first = observer.observation.snapshot();
    tx.send(Ok(RemoteUpdate::Initial(first.clone()))).unwrap();
    a.sub_tick();
    assert_eq!(texts(&a, "first"), 1);
    item(&mut a, RUN, reply("local event"));
    assert_eq!(texts(&a, "local event"), 0);
    assert!(drain(&mut commands).is_empty());
    ends.events.send(AgentEvent::TextDelta { text: " second".into() }).unwrap();
    let second = observer.observation.snapshot_since(&first).unwrap();
    tx.send(Ok(RemoteUpdate::Delta(second.clone()))).unwrap();
    a.sub_tick();
    assert_eq!(texts(&a, "first second"), 1);
    tx.send(Err("offline".into())).unwrap();
    a.sub_tick();
    assert!(format!("{:?}", a.blocks).contains("offline"));
    let empty = observer.observation.snapshot_since(&second).unwrap();
    tx.send(Ok(RemoteUpdate::Delta(empty))).unwrap();
    a.sub_tick();
    assert!(!format!("{:?}", a.blocks).contains("offline"));
    assert!(drain(&mut commands).is_empty());
    a.input.clear();
    a.esc_sub();
    assert_eq!(a.input.full_text(), "main draft");
    assert!(matches!(a.modal, Some(Modal::Agents(_))));
}

#[test]
fn remote_delivery_receipts_guard_uncertainty_and_preserve_edits() {
    use super::super::sub::{RemoteDelivery, RemoteReceipt};
    let (mut a, _) = with_run();
    let (_snap_tx, snap_rx) = std::sync::mpsc::sync_channel(1);
    a.open_remote_receiver("session", snap_rx);
    for receipt in [RemoteReceipt::Rejected("stale".into()), RemoteReceipt::BeforeSend("offline".into())] {
        a.input.clear();
        a.input.set_text("independent edit");
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        a.sub_view.as_mut().unwrap().remote.as_mut().unwrap().delivery =
            RemoteDelivery::Sending { text: "message".into(), request_id: "request".into(), rx };
        tx.send(receipt).unwrap();
        a.sub_tick();
        assert_eq!(a.input.full_text(), "independent edit\n\nmessage");
    }
    a.input.clear();
    a.input.set_text("independent edit");
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    a.sub_view.as_mut().unwrap().remote.as_mut().unwrap().delivery =
        RemoteDelivery::Sending { text: "message".into(), request_id: "request".into(), rx };
    tx.send(RemoteReceipt::Unknown).unwrap();
    a.sub_tick();
    a.submit_sub();
    assert!(matches!(a.sub_view.as_ref().unwrap().remote.as_ref().unwrap().delivery, RemoteDelivery::Unknown { .. }));
    a.esc_sub();
    assert_eq!(a.input.full_text(), "");
    a.esc_sub();
    assert!(!a.in_sub());
    assert!(a.input.history().iter().any(|s| s == "message"));
    assert!(a.input.history().iter().any(|s| s == "independent edit"));
}

#[test]
fn remote_identity_change_stays_invalid_after_disconnect() {
    use super::super::sub::RemoteUpdate;
    let (mut a, _) = with_run();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    a.open_remote_receiver("session", rx);
    let (observer, ends) = nexo_core::agent::channels();
    ends.events.send(AgentEvent::SessionChanged { session_id: "session".into(), title: String::new() }).unwrap();
    let original = observer.observation.snapshot();
    tx.send(Ok(RemoteUpdate::Initial(original.clone()))).unwrap();
    a.sub_tick();
    tx.send(Err("offline".into())).unwrap();
    a.sub_tick();
    let mut changed = original.clone();
    changed.generation += 1;
    tx.send(Ok(RemoteUpdate::Resync(changed))).unwrap();
    a.sub_tick();
    assert!(format!("{:?}", a.blocks).contains("identity changed"));
    tx.send(Ok(RemoteUpdate::Resync(original))).unwrap();
    a.sub_tick();
    a.input.set_text("draft");
    a.submit_sub();
    assert_eq!(a.input.full_text(), "draft");
}

#[test]
fn a_message_on_its_way_finishes_after_its_view_closes() {
    use super::super::sub::{RemoteDelivery, RemoteReceipt};
    let (mut a, _) = with_run();
    let (_tx, rx) = std::sync::mpsc::sync_channel(1);
    a.open_remote_receiver("session", rx);
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    a.sub_view.as_mut().unwrap().remote.as_mut().unwrap().delivery =
        RemoteDelivery::Sending { text: "message".into(), request_id: "request".into(), rx };
    a.handle_agent(AgentEvent::HistoryCleared);
    assert!(!a.in_sub());
    tx.send(RemoteReceipt::Accepted).unwrap();
    a.sub_tick();
    assert!(a.input.history().iter().any(|s| s == "message"));
}

#[test]
fn this_sessions_panel_row_goes_back_to_its_chat_from_a_run() {
    use nexo_supervise::SourceKind;
    let (mut a, mut rx) = with_run();
    a.session_id = Some("session".into());
    a.input.set_text("main draft");
    let rows = vec![panel_row("session.3", SourceKind::Nexo)];
    a.modal = Some(Modal::Agents(Box::new(crate::agents_view::AgentsView::with_rows(rows, 0, true))));
    drain(&mut rx);
    press(&mut a, KeyCode::Enter);
    assert!(a.in_sub());
    // the panel again, from inside the run: Enter on this session is the way home, not a read-only window
    let this = vec![panel_row("session", SourceKind::Nexo)];
    a.modal = Some(Modal::Agents(Box::new(crate::agents_view::AgentsView::with_rows(this, 0, true))));
    press(&mut a, KeyCode::Enter);
    assert!(!a.in_sub() && a.sub_view.is_none());
    assert!(a.modal.is_none(), "the chat is on show, no window over it");
    assert_eq!(a.input.text(), "main draft");
    // and from the chat itself it just closes the panel
    let this = vec![panel_row("session", SourceKind::Nexo)];
    a.modal = Some(Modal::Agents(Box::new(crate::agents_view::AgentsView::with_rows(this, 0, true))));
    press(&mut a, KeyCode::Enter);
    assert!(a.modal.is_none() && !a.in_sub());
}

#[test]
fn the_runs_rule_says_how_to_go_back_when_it_has_room() {
    let (mut a, _rx) = with_run();
    a.handle_agent(AgentEvent::SubState { info: info(RUN, "survey the parser", SubState::Background) });
    a.open_sub(RUN);
    answer(&mut a, RUN, vec![reply("x")], 0);
    let rule = |a: &mut App, w: u16| {
        let rows = frame(a, w, 8);
        rows.iter().find(|r| r.contains("sub ·")).cloned().unwrap_or_else(|| panic!("{}", rows.join("\n")))
    };
    let wide = rule(&mut a, 80);
    assert!(wide.trim_end().ends_with("esc back ─"), "{wide}");
    assert_eq!(wide.trim_end().chars().count(), 80, "{wide}");
    let narrow = rule(&mut a, 30);
    assert!(!narrow.contains("esc back") && narrow.starts_with("─ sub · "), "{narrow}");
}

#[test]
fn a_task_launch_has_one_face_and_badge_and_keeps_its_report() {
    let (mut a, _rx) = bare();
    a.push(Block::Tool {
        id: "launch".into(),
        name: "task".into(),
        summary: "task [explore]: survey the parser".into(),
        args: serde_json::Value::Null,
        status: crate::render::blocks::ToolStatus::Running,
        progress: vec![],
        output: None,
        started: None,
        elapsed_ms: None,
        folded: true,
    });
    let mut run = info("launch#7", "survey the parser", SubState::Attached);
    run.provider = Some("provider".into());
    run.model = Some("model".into());
    run.reasoning_effort = Some("high".into());
    a.handle_agent(AgentEvent::SubState { info: run.clone() });
    assert!(!a.hidden(0));
    assert!(a.hidden(1));
    let lines = a.lines(0, 120).to_vec();
    let text = lines.iter().map(crate::render::markdown::text_of).collect::<Vec<_>>().join("\n");
    assert!(
        text.contains("[o_o]") && text.contains("explore") && text.contains("provider/model") && text.contains("high"),
        "{text}"
    );
    assert!(!text.contains("agent [explore]"), "{text}");
    assert!(lines[0].spans.iter().any(|s| s.content == " explore " && s.style.bg == Some(a.theme.accent)));
    run.reasoning_effort = Some("max".into());
    a.handle_agent(AgentEvent::SubState { info: run.clone() });
    assert!(a.lines(0, 120).iter().map(crate::render::markdown::text_of).collect::<String>().contains("max"));
    run.state = SubState::Ok;
    a.handle_agent(AgentEvent::SubState { info: run });
    if let Block::Tool { status, output, folded, .. } = &mut a.blocks[0] {
        *status = crate::render::blocks::ToolStatus::Done;
        *output = Some(nexo_core::tool::ToolOutput::ok("unique report"));
        *folded = false;
    }
    a.invalidate(0);
    assert!(a.hidden(1));
    assert!(a.lines(0, 120).iter().map(crate::render::markdown::text_of).collect::<String>().contains("unique report"));
    // Same title is not a relationship; workflow and unrelated task runs keep their own card.
    let mut other = info("unrelated#8", "survey the parser", SubState::Background);
    other.provider = Some("provider".into());
    other.model = Some("model".into());
    a.handle_agent(AgentEvent::SubState { info: other });
    assert!(!a.hidden(2));
}

#[test]
fn indexed_task_rows_follow_group_insertion_view_switch_and_clear() {
    let (mut a, _rx) = bare();
    a.opts.group_tools = true;
    for id in ["launch", "sibling"] {
        a.push(Block::Tool {
            id: id.into(),
            name: "task".into(),
            summary: String::new(),
            args: serde_json::Value::Null,
            status: crate::render::blocks::ToolStatus::Done,
            progress: vec![],
            output: None,
            started: None,
            elapsed_ms: None,
            folded: true,
        });
    }
    let id = "launch#7";
    let mut run = info(id, "worker", SubState::Background);
    run.provider = Some("mock".into());
    run.model = Some("model".into());
    a.handle_agent(AgentEvent::SubState { info: run.clone() });
    assert_eq!(a.task_card_for_run(id), Some(1));
    assert!(!a.hidden(1) && a.hidden(2) && a.hidden(3));
    a.open_sub(id);
    answer(&mut a, id, vec![person("hello")], 0);
    assert_eq!(a.task_card_for_run(id), None);
    a.close_sub();
    assert_eq!(a.task_card_for_run(id), Some(1));
    run.state = SubState::Ok;
    a.handle_agent(AgentEvent::SubState { info: run });
    assert!(a.hidden(1) && !a.hidden(3));
    a.clear_transcript();
    assert_eq!(a.task_card_for_run(id), None);
}
