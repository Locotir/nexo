//! A request sent from the box never vanishes: it shows at once, dim, until the agent echoes it; a
//! late agent is said so on screen; a gone agent keeps the draft. Enter as the kitty keyboard
//! protocol reports it (modifiers, keypad, release) still sends.

use super::*;
use crossterm::event::{Event, KeyEventKind, KeyEventState};

fn pending(a: &App) -> usize {
    a.blocks.iter().filter(|b| matches!(b, Block::Pending { .. })).count()
}

fn users(a: &App) -> usize {
    a.blocks.iter().filter(|b| matches!(b, Block::User { .. })).count()
}

fn enter(kind: KeyEventKind, mods: KeyModifiers, state: KeyEventState) -> Event {
    let mut k = KeyEvent::new(KeyCode::Enter, mods);
    k.kind = kind;
    k.state = state;
    Event::Key(k)
}

#[test]
fn a_sent_message_shows_at_once_and_the_echo_takes_its_place() {
    let (mut a, mut rx) = app();
    assert!(a.fresh(), "the welcome stars are up");
    a.input.set_text("hello there");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Submit { .. })));
    assert_eq!(pending(&a), 1, "the message is in the transcript before the agent says a word");
    assert!(!a.fresh(), "the welcome gives way to it");
    assert!(a.input.is_empty());
    let n = a.blocks.len();
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("hello there") });
    assert_eq!((pending(&a), users(&a), a.blocks.len()), (0, 1, n), "the dim line became the message, no copy");
    // the echo of a request sent elsewhere (no dim line) is still added
    a.handle_agent(AgentEvent::History { index: 2, message: Message::user("from a peer") });
    assert_eq!(users(&a), 2);
}

#[test]
fn the_dim_line_reads_dim_and_the_message_reads_as_the_persons() {
    let (mut a, _rx) = app();
    a.input.set_text("hello there");
    a.submit();
    let i = a.blocks.iter().position(|b| matches!(b, Block::Pending { .. })).unwrap();
    let theme = a.theme.clone();
    let dim = a.blocks[i].render(40, &theme, a.render_opts());
    assert!(dim.iter().any(|l| crate::render::markdown::text_of(l).contains("hello there")));
    assert!(dim.iter().all(|l| l.style.bg.is_none()), "no band of the person's colour until it is taken");
}

#[test]
fn an_agent_that_does_not_answer_is_said_after_three_seconds_and_the_text_is_recoverable() {
    let t0 = Instant::now();
    let (mut a, _rx) = app();
    fx_at(&mut a, t0, 0);
    a.input.set_text("hello there");
    a.submit();
    assert_ne!(a.load(), crate::frame::Load::Idle, "the clock wakes to say it");
    let notices = |a: &App| a.blocks.iter().filter(|b| matches!(b, Block::Notice { .. })).count();
    let before = notices(&a);
    fx_at(&mut a, t0, 2_000);
    a.tick();
    assert_eq!(notices(&a), before, "not yet");
    fx_at(&mut a, t0, 3_100);
    a.tick();
    assert_eq!(notices(&a), before + 1);
    let said = a.blocks.iter().rev().find_map(|b| match b {
        Block::Notice { text, .. } => Some(text.clone()),
        _ => None,
    });
    assert!(said.unwrap().contains("not taken yet"));
    fx_at(&mut a, t0, 9_000);
    a.tick();
    assert_eq!(notices(&a), before + 1, "said once");
    // the message is in the history: Up brings it back
    press(&mut a, KeyCode::Up);
    assert_eq!(a.input.full_text(), "hello there");
    // and the echo, however late, still settles the dim line
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("hello there") });
    assert_eq!((pending(&a), users(&a)), (0, 1));
}

#[test]
fn a_gone_agent_keeps_the_draft_and_says_so() {
    let (mut a, rx) = app();
    drop(rx);
    a.input.set_text("hello there");
    a.submit();
    assert_eq!(a.input.full_text(), "hello there", "nothing was sent, nothing is lost");
    assert_eq!(pending(&a), 0);
    assert!(a.blocks.iter().any(|b| matches!(b, Block::Problem { .. })), "the screen says the agent stopped");
}

#[test]
fn a_queued_message_has_its_own_line_and_no_dim_copy() {
    let (mut a, mut rx) = app();
    a.busy = true;
    a.input.set_text("while you work");
    a.submit();
    drain(&mut rx);
    assert_eq!((pending(&a), a.queued), (0, 1));
}

#[test]
fn enter_as_the_kitty_protocol_reports_it_sends() {
    let none = KeyEventState::NONE;
    for (what, ev) in [
        ("plain press", enter(KeyEventKind::Press, KeyModifiers::NONE, none)),
        ("repeat", enter(KeyEventKind::Repeat, KeyModifiers::NONE, none)),
        ("keypad", enter(KeyEventKind::Press, KeyModifiers::NONE, KeyEventState::KEYPAD)),
        (
            "locks on",
            enter(KeyEventKind::Press, KeyModifiers::NONE, KeyEventState::CAPS_LOCK | KeyEventState::NUM_LOCK),
        ),
        ("super held", enter(KeyEventKind::Press, KeyModifiers::SUPER, none)),
        ("hyper and meta", enter(KeyEventKind::Press, KeyModifiers::HYPER | KeyModifiers::META, none)),
    ] {
        let (mut a, mut rx) = app();
        a.input.set_text("hello there");
        crate::event::handle(&mut a, ev);
        assert!(
            matches!(rx.try_recv(), Ok(AgentCommand::Submit { ref text, .. }) if text == "hello there"),
            "{what}: Enter sends"
        );
        assert_eq!(pending(&a), 1, "{what}: and shows");
    }
    // the release of the key (reported with the protocol's event types) sends nothing
    let (mut a, mut rx) = app();
    a.input.set_text("hello there");
    crate::event::handle(&mut a, enter(KeyEventKind::Release, KeyModifiers::NONE, none));
    assert!(rx.try_recv().is_err());
    assert_eq!(a.input.full_text(), "hello there");
    // press then release sends once
    crate::event::handle(&mut a, enter(KeyEventKind::Press, KeyModifiers::NONE, none));
    crate::event::handle(&mut a, enter(KeyEventKind::Release, KeyModifiers::NONE, none));
    assert_eq!(drain(&mut rx).len(), 1);
}
