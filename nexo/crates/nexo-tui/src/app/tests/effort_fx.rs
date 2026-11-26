//! Effort motion stays on the status chip; unrelated work-line pulses are tested below.
use super::*;
use crate::fx_effort::{BREATH_MS, IGNITE_MS};
use ratatui::buffer::Buffer;

fn levels() -> Vec<String> {
    vec!["low".into(), "medium".into(), "high".into()]
}
fn effort_event(a: &mut App, current: Option<&str>, levels: Vec<String>) {
    a.handle_agent(AgentEvent::Effort { model: "m".into(), current: current.map(String::from), levels });
}
fn bar_y(b: &Buffer) -> u16 {
    (0..b.area.height)
        .rev()
        .find(|y| (0..6).map(|x| b[(x, *y)].symbol().to_string()).collect::<String>().contains("nexo"))
        .expect("status bar")
}
fn rule_y(b: &Buffer) -> u16 {
    (0..bar_y(b))
        .rev()
        .find(|y| (0..b.area.width).filter(|x| b[(*x, *y)].symbol() == "─").count() > b.area.width as usize / 4)
        .expect("rule")
}
fn row(b: &Buffer, y: u16) -> String {
    (0..b.area.width).map(|x| b[(x, y)].symbol().to_string()).collect()
}
fn chip(b: &Buffer) -> Vec<(String, ratatui::style::Color)> {
    let y = bar_y(b);
    let x = (0..b.area.width).find(|x| b[(*x, y)].symbol() == "◆").expect("chip");
    (x..x + 6).map(|x| (b[(x, y)].symbol().to_string(), b[(x, y)].fg)).collect()
}
fn fixture(t0: Instant) -> App {
    let (mut a, _rx) = app();
    a.theme = crate::theme::Theme::dark();
    a.opts.home_stars = false;
    fx_at(&mut a, t0, 0);
    effort_event(&mut a, Some("medium"), levels());
    a
}

#[test]
fn only_the_last_of_three_or_more_known_levels_is_top() {
    let (mut a, _rx) = app();
    assert!(!a.effort_top());
    for (list, current, top) in [
        (vec!["off", "on"], Some("on"), false),
        (vec!["low", "medium", "high"], None, false),
        (vec!["low", "medium", "high"], Some("medium"), false),
        (vec!["low", "medium", "high"], Some("unknown"), false),
        (vec!["low", "medium", "high"], Some("high"), true),
        (vec!["low", "medium", "high", "max"], Some("max"), true),
    ] {
        a.effort_levels = list.into_iter().map(String::from).collect();
        a.effort = current.map(String::from);
        assert_eq!(a.effort_top(), top);
    }
}

#[test]
fn breath_finishes_at_the_end_of_its_cycle() {
    use crate::fx_effort::{breath, breathing};
    use std::time::Duration;
    let t = Instant::now();
    let at = |ms| t + Duration::from_millis(ms);
    assert!(!breathing(None, None, t));
    assert_eq!(breath(Some(t), None, t), 0.0);
    assert!(breath(Some(t), None, at(BREATH_MS / 2)) > 0.99);
    let stop = Some(at(BREATH_MS / 2));
    assert!(breathing(Some(t), stop, at(BREATH_MS - 1)));
    assert!(!breathing(Some(t), stop, at(BREATH_MS)));
    assert_eq!(breath(Some(t), stop, at(BREATH_MS)), 0.0);
}

#[test]
fn default_change_ignites_the_existing_clickable_chip() {
    use ratatui::style::Modifier;
    let t0 = Instant::now();
    let mut a = fixture(t0);
    a.input.set_text("draft retained");
    effort_event(&mut a, None, levels());
    assert_eq!(a.fx.effort_at, Some(t0));
    let lit = buf_of(&mut a, 120, 20);
    let hit = a.status_hits.iter().find(|(_, h)| *h == StatusHit::Effort).unwrap().0;
    assert!(row(&lit, bar_y(&lit)).contains("◆ default"));
    a.hover = Hover::Status(StatusHit::Effort);
    let hovered = buf_of(&mut a, 120, 20);
    assert_eq!(a.status_hits.iter().find(|(_, h)| *h == StatusHit::Effort).unwrap().0, hit);
    assert!(!hovered[(hit.x, hit.y)].modifier.contains(Modifier::UNDERLINED));
    assert!(hovered[(hit.x + 1, hit.y)].modifier.contains(Modifier::UNDERLINED));
    assert!(hovered[(hit.x + hit.width - 1, hit.y)].modifier.contains(Modifier::UNDERLINED));
    a.hover = Hover::Nothing;
    a.fx.effort_at = None;
    let calm = buf_of(&mut a, 120, 20);
    assert_eq!(row(&lit, bar_y(&lit)), row(&calm, bar_y(&calm)));
    assert_eq!(a.status_hits.iter().find(|(_, h)| *h == StatusHit::Effort).unwrap().0, hit);
    assert!((hit.x..hit.x + hit.width).any(|x| lit[(x, hit.y)].fg != calm[(x, hit.y)].fg));
    let y = rule_y(&lit);
    assert_eq!(y, rule_y(&calm));
    for x in 0..lit.area.width {
        assert_eq!(lit[(x, y)], calm[(x, y)]);
    }
}

#[test]
fn effort_changes_only_the_chip_not_editor_or_rule_even_when_omitted() {
    let t0 = Instant::now();
    for omit in [false, true] {
        let mut a = fixture(t0);
        if omit {
            a.opts.status_line = vec!["model".into(), "mode".into()];
        }
        a.input.set_text("draft retained");
        let cursor = a.input.cursor_offset();
        effort_event(&mut a, Some("high"), levels());
        let lit = buf_of(&mut a, 120, 20);
        let hits = a.status_hits.clone();
        assert_eq!(a.effort_moving(t0), !omit, "only a visible chip asks for frames");
        a.fx.effort_at = None;
        let calm = buf_of(&mut a, 120, 20);
        let y = rule_y(&lit);
        assert_eq!(y, rule_y(&calm));
        assert_eq!(bar_y(&lit), bar_y(&calm));
        assert_eq!(a.status_hits, hits);
        assert_eq!(cursor, a.input.cursor_offset());
        assert!((y..bar_y(&lit)).any(|r| row(&lit, r).contains("draft retained")));
        for r in y..bar_y(&lit) {
            for x in 0..lit.area.width {
                assert_eq!(lit[(x, r)], calm[(x, r)], "editor/rule cell {x},{r}, omit={omit}");
            }
        }
        assert!(!row(&lit, y).contains('◆'));
        if omit {
            assert!(!row(&lit, bar_y(&lit)).contains('◆'));
            assert!(hits.iter().all(|(_, h)| *h != StatusHit::Effort));
        } else {
            assert_eq!(row(&lit, bar_y(&lit)).matches("◆ high").count(), 1);
            assert_ne!(chip(&lit), chip(&calm), "chip must visibly animate");
        }
    }
}

#[test]
fn change_ignites_chip_immediately_and_never_draws_on_rule() {
    let t0 = Instant::now();
    let mut a = fixture(t0);
    effort_event(&mut a, Some("high"), levels());
    assert_eq!(a.fx.effort_at, Some(t0));
    let first = buf_of(&mut a, 100, 20);
    let hits = a.status_hits.clone();
    let text = row(&first, bar_y(&first));
    assert_eq!(text.matches("◆ high").count(), 1);
    assert!(!row(&first, rule_y(&first)).contains("◆"));
    let bright = chip(&first);
    fx_at(&mut a, t0, IGNITE_MS + 1);
    let rest = buf_of(&mut a, 100, 20);
    assert_ne!(bright, chip(&rest), "ignite must visibly contrast from its resting gradient");
    assert_eq!(row(&rest, bar_y(&rest)), text);
    assert_eq!(a.status_hits, hits);
    assert_eq!(row(&first, rule_y(&first)), row(&rest, rule_y(&rest)), "no comet or tag on the rule");
    assert!(a.status.is_none(), "no second level announcement");
}

#[test]
fn ordinary_levels_also_ignite_and_repeats_do_not_restart() {
    let t0 = Instant::now();
    let mut a = fixture(t0);
    effort_event(&mut a, Some("low"), levels());
    assert_eq!(a.fx.effort_at, Some(t0));
    let lit = chip(&buf_of(&mut a, 100, 20));
    fx_at(&mut a, t0, IGNITE_MS + 10);
    let rest = chip(&buf_of(&mut a, 100, 20));
    assert_ne!(lit, rest);
    effort_event(&mut a, Some("low"), levels());
    assert_eq!(a.fx.effort_at, Some(t0));
}

#[test]
fn top_chip_flows_while_working_and_finishes_its_breath() {
    let t0 = Instant::now();
    let mut a = fixture(t0);
    effort_event(&mut a, Some("high"), levels());
    fx_at(&mut a, t0, IGNITE_MS + 100);
    a.busy = true;
    a.tick_fx();
    let start = chip(&buf_of(&mut a, 100, 20));
    fx_at(&mut a, t0, IGNITE_MS + 100 + BREATH_MS / 2);
    assert!(a.effort_moving(a.now()));
    assert_ne!(start, chip(&buf_of(&mut a, 100, 20)));
    a.busy = false;
    a.tick_fx();
    fx_at(&mut a, t0, IGNITE_MS + 101 + BREATH_MS);
    a.tick_fx();
    assert!(!a.effort_moving(a.now()));
}

#[test]
fn disabled_motion_and_monochrome_leave_the_chip_unchanged() {
    let t0 = Instant::now();
    for mono in [false, true] {
        let mut a = fixture(t0);
        if mono {
            a.theme = crate::theme::Theme::mono();
        } else {
            a.opts.animations = false;
        }
        effort_event(&mut a, Some("high"), levels());
        assert!(a.fx.effort_at.is_none());
        let first = chip(&buf_of(&mut a, 100, 20));
        a.busy = true;
        for ms in [100, 600, 1200, 2400] {
            fx_at(&mut a, t0, ms);
            a.tick_fx();
            assert!(!a.effort_moving(a.now()));
            assert_eq!(chip(&buf_of(&mut a, 100, 20)), first);
        }
    }
}

// ---- the pulses of the work line (see `fx_pulse`)

fn pulse_rule(a: &mut App) -> Vec<Option<ratatui::style::Color>> {
    let b = buf_of(a, 80, 20);
    let y = rule_y(&b);
    (0..80).filter(|x| b[(*x, y)].symbol() == "─").map(|x| b[(x, y)].style().fg).collect()
}

#[test]
fn events_pulse_the_work_line_and_the_pulse_dies_by_the_clock() {
    use crate::fx_pulse::Cue;
    let t0 = Instant::now();
    let (mut a, _rx) = app();
    a.opts.home_stars = false;
    fx_at(&mut a, t0, 0);
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    assert!(a.busy);
    fx_at(&mut a, t0, 5000);
    let calm = pulse_rule(&mut a);
    a.handle_agent(AgentEvent::ToolCall {
        id: "c".into(),
        name: "bash".into(),
        args: serde_json::json!({}),
        summary: String::new(),
    });
    assert!(a.fx.pulses.alive(a.now()));
    fx_at(&mut a, t0, 5300);
    let lit = pulse_rule(&mut a);
    assert_ne!(lit, calm, "the rule shows the tool starting");
    fx_at(&mut a, t0, 6000);
    assert!(!a.fx.pulses.alive(a.now()));
    // no frame is asked for once the pulse is over
    assert!(!a.tick_fx_with(false) || a.fx.pulses.alive(a.now()));
    // the motion off: events cost nothing
    a.opts.animations = false;
    a.pulse(Cue::UserSent);
    assert!(!a.fx.pulses.alive(a.now()));
}

#[test]
fn a_subagent_launch_and_its_end_pulse_once_each() {
    use nexo_core::subs::{SubInfo, SubState};
    let t0 = Instant::now();
    let (mut a, _rx) = app();
    fx_at(&mut a, t0, 0);
    let info = |state| SubInfo {
        id: "r1".into(),
        profile: "general".into(),
        provider: None,
        model: None,
        reasoning_effort: None,
        title: "t".into(),
        state,
        started_ms: 0,
        ended_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        cost_usd: 0.0,
    };
    a.handle_agent(AgentEvent::SubState { info: info(SubState::Background) });
    assert!(a.fx.pulses.alive(a.now()), "a launch pulses");
    fx_at(&mut a, t0, 5000);
    assert!(!a.fx.pulses.alive(a.now()));
    // the same state again (the agent repeats it after a lag) is no event
    a.handle_agent(AgentEvent::SubState { info: info(SubState::Background) });
    assert!(!a.fx.pulses.alive(a.now()));
    // its end is one
    a.handle_agent(AgentEvent::SubState { info: info(SubState::Ok) });
    assert!(a.fx.pulses.alive(a.now()), "an end pulses");
    fx_at(&mut a, t0, 9000);
    a.handle_agent(AgentEvent::SubState { info: info(SubState::Ok) });
    assert!(!a.fx.pulses.alive(a.now()), "an end already known does not pulse again");
}

fn progress(a: &mut App, id: &str, line: &str) {
    a.handle_agent(AgentEvent::ToolProgress { id: id.into(), line: line.into() });
}

fn progress_fixture(t0: Instant) -> App {
    let (mut a, _rx) = app();
    a.theme = crate::theme::Theme::dark();
    a.opts.home_stars = false;
    fx_at(&mut a, t0, 0);
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(AgentEvent::ToolCall {
        id: "progress".into(),
        name: "bash".into(),
        args: serde_json::json!({}),
        summary: String::new(),
    });
    a.handle_agent(AgentEvent::ToolStart { id: "progress".into() });
    a.input.set_text("draft retained");
    fx_at(&mut a, t0, 5000);
    a
}

#[test]
fn tool_progress_paints_only_rule_ink_then_settles() {
    let t0 = Instant::now();
    let mut a = progress_fixture(t0);
    let cursor = a.input.cursor_offset();
    progress(&mut a, "progress", "first line");
    fx_at(&mut a, t0, 5080);
    let lit = buf_of(&mut a, 100, 20);
    // Identical time, progress text and work wave; remove just the event pulse for the control.
    let pulses = std::mem::take(&mut a.fx.pulses);
    let calm = buf_of(&mut a, 100, 20);
    a.fx.pulses = pulses;
    let y = rule_y(&lit);
    assert_eq!(y, rule_y(&calm));
    assert_eq!(row(&lit, y), row(&calm, y));
    assert!((0..lit.area.width).any(|x| lit[(x, y)].symbol() == "─" && lit[(x, y)].fg != calm[(x, y)].fg));
    assert!((0..lit.area.height).any(|y| row(&lit, y).contains("draft retained")));
    assert_eq!(a.input.cursor_offset(), cursor);
    fx_at(&mut a, t0, 7000);
    assert!(!a.fx.pulses.alive(a.now()), "the pulse expires");
}

#[test]
fn tool_progress_ignores_ineligible_events_and_repeats() {
    let t0 = Instant::now();
    let mut a = progress_fixture(t0);
    for (id, line) in [("missing", "new"), ("progress", " \t")] {
        progress(&mut a, id, line);
        assert!(!a.fx.pulses.alive(a.now()), "{id:?} {line:?}");
    }
    progress(&mut a, "progress", "first");
    fx_at(&mut a, t0, 7000);
    progress(&mut a, "progress", "first");
    assert!(!a.fx.pulses.alive(a.now()), "repeated progress");
    a.handle_agent(AgentEvent::ToolDone {
        id: "progress".into(),
        output: nexo_core::ToolOutput::ok("done"),
        elapsed_ms: 1,
    });
    fx_at(&mut a, t0, 9000);
    progress(&mut a, "progress", "after done");
    assert!(!a.fx.pulses.alive(a.now()), "finished tool");
    a.handle_agent(AgentEvent::ToolCall {
        id: "pending".into(),
        name: "bash".into(),
        args: serde_json::json!({}),
        summary: String::new(),
    });
    fx_at(&mut a, t0, 11000);
    progress(&mut a, "pending", "not started");
    assert!(!a.fx.pulses.alive(a.now()), "pending tool");
    for (busy, motion, mono) in [(false, true, false), (true, false, false), (true, true, true)] {
        let mut a = progress_fixture(t0);
        a.busy = busy;
        a.opts.animations = motion;
        if mono {
            a.theme = crate::theme::Theme::mono();
        }
        progress(&mut a, "progress", "new");
        assert!(!a.fx.pulses.alive(a.now()), "busy={busy} motion={motion} mono={mono}");
    }
}
