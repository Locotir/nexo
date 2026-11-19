//! A double or held Enter while a turn runs only queues the message: the turn is stopped by an Enter that
//! comes after a pause, not by the one that follows the queueing Enter.

use super::*;
use crossterm::event::{Event, KeyEventKind};
use std::time::{Duration, Instant};

fn enter(a: &mut App, kind: KeyEventKind) {
    let mut k = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    k.kind = kind;
    crate::event::handle(a, Event::Key(k));
}

fn interrupts(rx: &mut mpsc::UnboundedReceiver<AgentCommand>) -> usize {
    drain(rx).iter().filter(|c| matches!(c, AgentCommand::InterruptRun)).count()
}

/// A busy app with "next" just queued at `t0`.
fn queued_at(t0: Instant) -> (App, mpsc::UnboundedReceiver<AgentCommand>) {
    let (mut a, mut rx) = app();
    a.busy = true;
    a.fx.clock = Some(t0);
    a.input.set_text("next");
    enter(&mut a, KeyEventKind::Press);
    assert_eq!(a.queued, 1, "the message is queued");
    assert_eq!(interrupts(&mut rx), 0, "queueing does not stop the turn");
    (a, rx)
}

#[test]
fn an_enter_right_after_queueing_does_not_stop_the_turn() {
    let t0 = Instant::now();
    let (mut a, mut rx) = queued_at(t0);
    a.fx.clock = Some(t0 + Duration::from_millis(100));
    enter(&mut a, KeyEventKind::Press);
    assert_eq!(interrupts(&mut rx), 0, "100 ms after the queueing Enter");
    // measured from the last Enter, the ignored one: 600 ms
    a.fx.clock = Some(t0 + Duration::from_millis(700));
    enter(&mut a, KeyEventKind::Press);
    assert_eq!(interrupts(&mut rx), 0, "600 ms after the last Enter");
    a.fx.clock = Some(t0 + Duration::from_millis(1500));
    enter(&mut a, KeyEventKind::Press);
    assert_eq!(interrupts(&mut rx), 1, "a deliberate Enter after a pause stops the turn");
    assert_eq!(a.queued, 1, "the queue is the agent's to run");
}

#[test]
fn a_repeat_enter_during_a_turn_neither_interrupts_nor_queues() {
    let t0 = Instant::now();
    let (mut a, mut rx) = queued_at(t0);
    for secs in [2, 5] {
        a.fx.clock = Some(t0 + Duration::from_secs(secs));
        enter(&mut a, KeyEventKind::Repeat);
        assert_eq!(interrupts(&mut rx), 0, "a held Enter at +{secs} s");
        assert_eq!(a.queued, 1, "and nothing more is queued");
    }
    // the held key counts as the last Enter: a press 100 ms after it is still too soon
    a.fx.clock = Some(t0 + Duration::from_millis(5100));
    enter(&mut a, KeyEventKind::Press);
    assert_eq!(interrupts(&mut rx), 0, "a press right after the repeats");
}
