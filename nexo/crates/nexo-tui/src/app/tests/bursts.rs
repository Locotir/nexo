//! The bursts at the end of a piece of work and the stars of an empty session (see `fx_burst`, `fx_home`):
//! what starts them, what does not, that they keep off the text and leave the screen as it was.

use super::*;
use crate::fx_burst::{Kind, FAIL_MS, MET_MS};
use crate::fx_home::{IN_MS, OUT_MS};
use crate::fx_stars::{REST_AFTER_MS, STOP_MS};
use nexo_core::agent::{GoalState, TurnEnd};
use ratatui::buffer::Buffer;

fn goal(state: GoalState) -> AgentEvent {
    AgentEvent::Goal { text: Some("pass the tests".into()), round: 1, max: 3, state }
}

/// An app with an active goal and a conversation (the home is not empty), its clock at `t0`.
fn working(t0: Instant) -> App {
    let (mut a, _rx) = app();
    fx_at(&mut a, t0, 0);
    a.push(Block::User { text: "make the tests pass".into(), at: None });
    a.handle_agent(goal(GoalState::Active));
    a
}

fn kind(a: &App) -> Option<Kind> {
    a.fx.burst.map(|b| b.kind)
}

fn braille(b: &Buffer) -> usize {
    b.content.iter().filter(|c| c.symbol().chars().any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))).count()
}

/// Every cell that holds text in `plain` is the same in `fx`.
fn text_kept(plain: &Buffer, fx: &Buffer) {
    for (i, (p, f)) in plain.content.iter().zip(fx.content.iter()).enumerate() {
        if p.symbol() != " " {
            assert_eq!(p, f, "cell {i} holds text and was drawn over");
        }
    }
}

#[test]
fn a_goal_met_bursts_green_and_a_pause_by_esc_does_not() {
    let t0 = Instant::now();
    let mut a = working(t0);
    assert_eq!(kind(&a), None);
    a.handle_agent(goal(GoalState::Done));
    assert_eq!(kind(&a), Some(Kind::Met));
    assert!(a.tick_fx(), "the burst wants frames");
    // the person's own stops: Esc, /goal pause and an interrupted turn
    for why in ["interrupted", "paused by you"] {
        let mut a = working(t0);
        a.handle_agent(goal(GoalState::Paused(why.into())));
        assert_eq!(kind(&a), None, "{why}");
    }
    let mut a = working(t0);
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Interrupted });
    assert_eq!(kind(&a), None);
}

#[test]
fn a_failure_bursts_red_once() {
    let t0 = Instant::now();
    let mut a = working(t0);
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Error });
    assert_eq!(kind(&a), Some(Kind::Failed));
    let first = a.fx.burst.unwrap().at;
    // the goal pauses for the same failure a moment later: the burst under way goes on
    fx_at(&mut a, t0, 40);
    a.handle_agent(goal(GoalState::Paused("the turn failed".into())));
    assert_eq!(a.fx.burst.unwrap().at, first);
    let mut a = working(t0);
    a.handle_agent(goal(GoalState::Paused("the last round did nothing new".into())));
    assert_eq!(kind(&a), Some(Kind::Failed));
}

#[test]
fn no_burst_with_the_motion_or_the_switch_off() {
    let t0 = Instant::now();
    let mut a = working(t0);
    a.opts.animations = false;
    a.handle_agent(goal(GoalState::Done));
    assert_eq!(kind(&a), None);
    let mut a = working(t0);
    a.opts.bursts = false;
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Error });
    assert_eq!(kind(&a), None);
}

#[test]
fn a_burst_keeps_off_the_text_and_leaves_the_screen_as_it_was() {
    let t0 = Instant::now();
    // the same app twice, the goal met in both; one has the bursts off
    let both = || {
        let mut a = working(t0);
        a.show_files = true;
        a
    };
    let (mut a, mut plain) = (both(), both());
    plain.opts.bursts = false;
    for x in [&mut a, &mut plain] {
        x.handle_agent(goal(GoalState::Done));
    }
    for ms in [40, 300, 700, 1200] {
        fx_at(&mut a, t0, ms);
        fx_at(&mut plain, t0, ms);
        let (lit, quiet) = (buf_of(&mut a, 120, 32), buf_of(&mut plain, 120, 32));
        text_kept(&quiet, &lit);
        assert!(braille(&lit) > braille(&quiet), "{ms} ms: the burst shows");
        if (300..=700).contains(&ms) {
            assert!(lit.content.iter().any(|c| braille_cell(c) && greenish(c.fg)), "{ms} ms: green");
        }
    }
    // it starts at the goal's state in the side panel, right of the transcript
    let (sx, sy) = a.fx.goal_spot.get().expect("the panel drew the goal's state");
    assert!(sx > 60 && sy < 32, "({sx}, {sy})");
    // over: the screen is the one without it, cell for cell, and nothing asks for frames
    fx_at(&mut a, t0, MET_MS + 1);
    fx_at(&mut plain, t0, MET_MS + 1);
    assert!(!a.burst_moving(a.now()));
    assert_eq!(buf_of(&mut a, 120, 32), buf_of(&mut plain, 120, 32));
}

#[test]
fn the_red_burst_is_over_sooner() {
    let t0 = Instant::now();
    let mut a = working(t0);
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Error });
    fx_at(&mut a, t0, FAIL_MS + 1);
    assert!(!a.burst_moving(a.now()));
    let mut b = working(t0);
    b.handle_agent(goal(GoalState::Done));
    fx_at(&mut b, t0, FAIL_MS + 1);
    assert!(b.burst_moving(b.now()));
}

/// Green leads the colour (the dark theme's success is a6e22e).
fn greenish(c: ratatui::style::Color) -> bool {
    matches!(c, ratatui::style::Color::Rgb(r, g, b) if g > r && g > b)
}

fn braille_cell(c: &ratatui::buffer::Cell) -> bool {
    c.symbol().chars().any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
}

// ------------------------------------------------------------ the stars of an empty session

/// A fresh home (a model works, nothing said yet), its clock at `t0`, ticked once so the stars start.
fn home(t0: Instant) -> App {
    let (mut a, _rx) = app();
    fx_at(&mut a, t0, 0);
    a.tick_fx();
    a
}

#[test]
fn an_empty_session_has_drifting_stars_that_keep_off_the_text() {
    let t0 = Instant::now();
    let (mut a, mut plain) = (home(t0), home(t0));
    plain.opts.home_stars = false;
    fx_at(&mut a, t0, IN_MS + 2000);
    fx_at(&mut plain, t0, IN_MS + 2000);
    let (lit, quiet) = (buf_of(&mut a, 110, 36), buf_of(&mut plain, 110, 36));
    assert!(braille(&lit) > 20, "the stars show: {}", braille(&lit));
    assert_eq!(braille(&quiet), 0);
    text_kept(&quiet, &lit);
    assert!(a.home_alive(a.now()), "they drift on the beat");
    assert_eq!(a.load(), crate::frame::Load::Beat);
    // a few seconds later they have moved
    fx_at(&mut a, t0, IN_MS + 6000);
    assert_ne!(buf_of(&mut a, 110, 36), lit);
}

#[test]
fn the_stars_rest_after_a_while_and_a_key_wakes_them() {
    let t0 = Instant::now();
    let mut a = home(t0);
    buf_of(&mut a, 110, 36);
    let rest = REST_AFTER_MS + STOP_MS + 50;
    fx_at(&mut a, t0, rest);
    a.tick_fx();
    assert!(!a.home_alive(a.now()) && !a.home_moving(a.now()));
    assert_eq!(a.load(), crate::frame::Load::Idle, "no frames at rest");
    let still = buf_of(&mut a, 110, 36);
    fx_at(&mut a, t0, rest + 5000);
    assert_eq!(buf_of(&mut a, 110, 36), still, "the still frame stays");
    a.stars_input();
    assert!(a.home_alive(a.now()));
    fx_at(&mut a, t0, rest + 9000);
    assert_ne!(buf_of(&mut a, 110, 36), still);
}

#[test]
fn the_stars_go_out_once_a_message_is_sent() {
    let t0 = Instant::now();
    let mut a = home(t0);
    fx_at(&mut a, t0, IN_MS + 1000);
    assert!(braille(&buf_of(&mut a, 110, 36)) > 0);
    a.push(Block::User { text: "hello".into(), at: None });
    a.tick_fx();
    assert!(a.home_moving(a.now()), "the fade out wants frames");
    fx_at(&mut a, t0, IN_MS + 1000 + OUT_MS / 3);
    assert!(braille(&buf_of(&mut a, 110, 36)) > 0, "still going out");
    fx_at(&mut a, t0, IN_MS + 1000 + OUT_MS);
    assert_eq!(braille(&buf_of(&mut a, 110, 36)), 0);
    a.tick_fx();
    assert!(!a.home_alive(a.now()) && !a.home_moving(a.now()));
}

#[test]
fn no_stars_with_the_motion_off_on_mono_or_in_a_small_room() {
    let t0 = Instant::now();
    let mut a = home(t0);
    a.opts.animations = false;
    fx_at(&mut a, t0, IN_MS + 1000);
    a.tick_fx();
    assert_eq!(braille(&buf_of(&mut a, 110, 36)), 0);
    assert!(!a.home_alive(a.now()));
    let mut a = home(t0);
    a.theme = crate::theme::Theme::mono();
    fx_at(&mut a, t0, IN_MS + 1000);
    a.tick_fx();
    assert_eq!(braille(&buf_of(&mut a, 110, 36)), 0);
    let mut a = home(t0);
    fx_at(&mut a, t0, IN_MS + 1000);
    assert_eq!(braille(&buf_of(&mut a, 22, 12)), 0);
    assert!(!a.home_alive(a.now()), "no room, no beat");
}

#[test]
fn a_burst_starts_at_the_goal_but_a_later_failure_does_not() {
    let t0 = Instant::now();
    let mut a = working(t0);
    a.show_files = true;
    a.handle_agent(goal(GoalState::Done));
    buf_of(&mut a, 120, 32);
    let room = ratatui::layout::Rect::new(0, 0, 120, 29);
    let (gx, gy) = a.fx.goal_spot.get().expect("the panel drew the goal's state");
    assert_eq!(crate::fx_burst::spot(&a, room, Kind::Met), (gx, gy, false));
    // the goal is met: a turn that fails afterwards rises from the foot of the room, not from the ✓
    assert_eq!(crate::fx_burst::spot(&a, room, Kind::Failed), (60, 29, true));
    // while the goal runs, its failure starts at it
    a.handle_agent(goal(GoalState::Active));
    buf_of(&mut a, 120, 32);
    let (gx, gy) = a.fx.goal_spot.get().expect("the panel drew the goal's state");
    assert_eq!(crate::fx_burst::spot(&a, room, Kind::Failed), (gx, gy, false));
}
