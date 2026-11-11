//! The top reasoning level in motion (see `fx_effort`): when it counts, when the crest and the pop
//! start, the breath while a turn runs, and the stillness at rest, with the motion off and on a mono theme.

use super::*;
use crate::fx_effort::{BREATH_MS, IGNITE_MS};
use ratatui::buffer::Buffer;
use ratatui::style::Color;

fn levels() -> Vec<String> {
    vec!["low".into(), "medium".into(), "high".into()]
}

fn effort_event(a: &mut App, current: Option<&str>, levels: Vec<String>) {
    a.handle_agent(AgentEvent::Effort { model: "m".into(), current: current.map(String::from), levels });
}

/// An app on a model with three levels, set to `medium` first, then to the top one `high` at `t0`.
fn ignited(t0: Instant) -> App {
    let (mut a, _rx) = app();
    fx_at(&mut a, t0, 0);
    effort_event(&mut a, Some("medium"), levels());
    effort_event(&mut a, Some("medium"), levels());
    effort_event(&mut a, Some("high"), levels());
    a
}

/// The same app long after the crest and the pop: the resting look.
fn resting(t0: Instant) -> App {
    let mut a = ignited(t0);
    fx_at(&mut a, t0, IGNITE_MS + 3000);
    a.tick_fx();
    a
}

fn bar_y(b: &Buffer) -> u16 {
    (0..b.area.height)
        .find(|y| (0..6).map(|x| b[(x, *y)].symbol().to_string()).collect::<String>().contains("nexo"))
        .expect("the status bar")
}

/// The rule over the draft: the last row above the bar with a quarter of it `─`.
fn rule_y(b: &Buffer) -> u16 {
    (0..bar_y(b))
        .rev()
        .find(|y| (0..b.area.width).filter(|x| b[(*x, *y)].symbol() == "─").count() > b.area.width as usize / 4)
        .expect("the rule over the draft")
}

fn rule_fgs(b: &Buffer) -> Vec<Color> {
    let y = rule_y(b);
    (0..b.area.width).map(|x| b[(x, y)].fg).collect()
}

fn row_text(b: &Buffer, y: u16) -> String {
    (0..b.area.width).map(|x| b[(x, y)].symbol().to_string()).collect()
}

/// The `◆` of the chip: its column and its colour, and the colour of the cell after it.
fn mark(b: &Buffer) -> (u16, Color, Color) {
    let y = bar_y(b);
    let x = (0..b.area.width).find(|x| b[(*x, y)].symbol() == "◆").expect("the effort mark");
    (x, b[(x, y)].fg, b[(x + 1, y)].fg)
}

fn draw(a: &mut App) -> Buffer {
    buf_of(a, 100, 20)
}

/// The app is working a turn (the activity line is up).
fn work(a: &mut App) {
    a.busy = true;
}

#[test]
fn only_the_last_of_three_or_more_levels_is_the_top_one() {
    let (mut a, _rx) = app();
    assert!(!a.effort_top(), "no level set");
    a.effort_levels = levels();
    a.effort = Some("high".into());
    assert!(a.effort_top());
    a.effort = Some("medium".into());
    assert!(!a.effort_top());
    a.effort = None;
    assert!(!a.effort_top(), "no level set on a model that has levels");
    a.effort_levels = vec!["off".into(), "on".into()];
    a.effort = Some("on".into());
    assert!(!a.effort_top(), "a switch has no top level");
    a.effort_levels = vec!["low".into(), "high".into()];
    a.effort = Some("high".into());
    assert!(!a.effort_top(), "two levels are not enough");
    a.effort_levels = vec!["low".into(), "medium".into(), "high".into(), "max".into()];
    a.effort = Some("max".into());
    assert!(a.effort_top());
}

#[test]
fn the_crest_starts_only_when_the_level_changes_to_the_top_one() {
    let (mut a, _rx) = app();
    let t0 = Instant::now();
    fx_at(&mut a, t0, 0);
    // the first event of the session carries the level the session starts with
    effort_event(&mut a, Some("high"), levels());
    assert!(a.fx.effort_at.is_none(), "the start-up is not a change");
    // a model with other levels loading is not a change either
    effort_event(&mut a, Some("max"), vec!["low".into(), "medium".into(), "max".into()]);
    assert!(a.fx.effort_at.is_none(), "a new list of levels is not a change");
    effort_event(&mut a, Some("medium"), vec!["low".into(), "medium".into(), "max".into()]);
    assert!(a.fx.effort_at.is_none(), "leaving the top level is no effect");
    effort_event(&mut a, Some("max"), vec!["low".into(), "medium".into(), "max".into()]);
    assert_eq!(a.fx.effort_at, Some(t0), "medium to max starts it");
    // the same level again does not start it again
    fx_at(&mut a, t0, 5000);
    effort_event(&mut a, Some("max"), vec!["low".into(), "medium".into(), "max".into()]);
    assert_eq!(a.fx.effort_at, Some(t0));
    // a switch that goes `on` is not a top level
    let (mut b, _rx) = app();
    effort_event(&mut b, Some("off"), vec!["off".into(), "on".into()]);
    effort_event(&mut b, Some("on"), vec!["off".into(), "on".into()]);
    assert!(b.fx.effort_at.is_none());
    // the default (none) to the top level counts
    let (mut c, _rx) = app();
    effort_event(&mut c, None, levels());
    effort_event(&mut c, Some("high"), levels());
    assert!(c.fx.effort_at.is_some());
}

#[test]
fn mid_ignite_the_rule_has_a_lit_cell_and_after_it_the_rule_is_as_it_was() {
    let t0 = Instant::now();
    let mut a = ignited(t0);
    let rest = {
        let mut r = resting(t0);
        rule_fgs(&draw(&mut r))
    };
    let mut seen_lit = false;
    for ms in (0..IGNITE_MS).step_by(50) {
        fx_at(&mut a, t0, ms);
        let fgs = rule_fgs(&draw(&mut a));
        seen_lit |= fgs.iter().zip(&rest).any(|(now, was)| now != was);
    }
    assert!(seen_lit, "a cell of the rule is lit while the crest runs");
    fx_at(&mut a, t0, IGNITE_MS);
    assert_eq!(rule_fgs(&draw(&mut a)), rest, "after 900 ms the rule is the resting one");
    // the crest runs left to right: early it is near the start, late near the end
    fx_at(&mut a, t0, 150);
    let early = rule_fgs(&draw(&mut a));
    let first_lit = |fgs: &[Color]| fgs.iter().zip(&rest).position(|(n, w)| n != w);
    let last_lit = |fgs: &[Color]| fgs.iter().zip(&rest).rposition(|(n, w)| n != w);
    fx_at(&mut a, t0, 750);
    let late = rule_fgs(&draw(&mut a));
    assert!(last_lit(&early) < last_lit(&late), "the head moves right");
    assert!(first_lit(&early).is_some() && first_lit(&late).is_some());
}

#[test]
fn a_mode_wipe_running_at_the_same_time_wins_the_rule() {
    use nexo_core::permission::PermissionMode;
    let t0 = Instant::now();
    let wipe_of = |crest: bool| {
        let mut a = ignited(t0);
        if !crest {
            a.fx.effort_at = None;
        }
        a.permission_mode = PermissionMode::Plan;
        a.fx.mode_at = Some(t0);
        a.fx.mode_from = Some(PermissionMode::Ask);
        fx_at(&mut a, t0, 300);
        rule_fgs(&draw(&mut a))
    };
    assert_eq!(wipe_of(true), wipe_of(false), "the crest adds nothing to a mode wipe");
}

#[test]
fn the_mark_pops_at_the_start_and_settles_and_the_word_after_it_never_changes() {
    let t0 = Instant::now();
    let mut a = resting(t0);
    let (x, rest_mark, rest_word) = mark(&draw(&mut a));
    fx_at(&mut a, t0, 0);
    let (x0, pop0, word0) = mark(&draw(&mut a));
    fx_at(&mut a, t0, 450);
    let (_, pop_mid, _) = mark(&draw(&mut a));
    assert_eq!(x0, x, "the mark does not move");
    assert_ne!(pop0, rest_mark, "bright at the start");
    assert_ne!(pop_mid, pop0, "and easing down");
    assert_ne!(pop_mid, rest_mark);
    assert_eq!(word0, rest_word, "the word after it does not change");
    fx_at(&mut a, t0, IGNITE_MS);
    assert_eq!(mark(&draw(&mut a)).1, rest_mark, "settled");
}

#[test]
fn the_mark_breathes_while_a_turn_runs_and_comes_to_rest_within_a_cycle_after_it() {
    let t0 = Instant::now();
    let mut a = resting(t0);
    let start = IGNITE_MS + 3000;
    let (_, rest_mark, rest_word) = mark(&draw(&mut a));
    work(&mut a);
    fx_at(&mut a, t0, start);
    a.tick_fx();
    let begin = mark(&draw(&mut a));
    assert_eq!(begin.1, rest_mark, "a breath starts from the resting colour");
    fx_at(&mut a, t0, start + BREATH_MS / 2);
    assert!(a.tick_fx(), "a running turn at the top level asks for frames");
    let top = mark(&draw(&mut a));
    assert_ne!(top.1, rest_mark, "half a cycle later the mark is brighter");
    assert_eq!(top.2, rest_word, "the word after it never changes");
    // a smooth curve: a quarter cycle sits between the two
    fx_at(&mut a, t0, start + BREATH_MS / 4);
    let quarter = mark(&draw(&mut a)).1;
    assert!(quarter != rest_mark && quarter != top.1, "never a blink");
    // the turn ends mid-breath: it finishes its cycle and stops at rest
    fx_at(&mut a, t0, start + BREATH_MS / 2);
    a.busy = false;
    a.tick_fx();
    fx_at(&mut a, t0, start + BREATH_MS / 2 + 100);
    assert_ne!(mark(&draw(&mut a)).1, rest_mark, "it does not snap");
    assert!(a.tick_fx());
    fx_at(&mut a, t0, start + BREATH_MS + 1);
    assert!(!a.tick_fx(), "the frames stop within a cycle of the end of the turn");
    assert_eq!(mark(&draw(&mut a)).1, rest_mark);
}

#[test]
fn at_rest_at_the_top_level_the_chip_is_still_and_asks_for_no_frame() {
    let t0 = Instant::now();
    let mut a = resting(t0);
    assert!(!a.tick_fx(), "an idle app at the top level asks for no frame");
    let (_, rest_mark, _) = mark(&draw(&mut a));
    for ms in (0..10).map(|i| IGNITE_MS + 3000 + i * 700) {
        fx_at(&mut a, t0, ms);
        assert!(!a.tick_fx());
        assert_eq!(mark(&draw(&mut a)).1, rest_mark);
    }
    // below the top level a running turn does not breathe
    effort_event(&mut a, Some("medium"), levels());
    work(&mut a);
    fx_at(&mut a, t0, IGNITE_MS + 20000);
    a.tick_fx();
    // (the bar lights a changed level for a moment, which is another effect: let it settle)
    fx_at(&mut a, t0, IGNITE_MS + 24000);
    a.tick_fx();
    let (_, medium_mark, _) = mark(&draw(&mut a));
    fx_at(&mut a, t0, IGNITE_MS + 24000 + BREATH_MS / 2);
    assert!(!a.tick_fx());
    assert_eq!(mark(&draw(&mut a)).1, medium_mark);
}

#[test]
fn the_ignite_asks_for_frames_while_it_runs_and_stops_after_its_time() {
    let t0 = Instant::now();
    let mut a = ignited(t0);
    fx_at(&mut a, t0, 100);
    assert!(a.tick_fx());
    fx_at(&mut a, t0, IGNITE_MS - 1);
    assert!(a.tick_fx());
    // the green lit-for-a-moment fade of a new level is its own, longer, effect
    fx_at(&mut a, t0, crate::motion::CUT_MS + 400);
    assert!(!a.tick_fx(), "nothing moves after the ignite");
}

#[test]
fn the_bar_keeps_its_text_and_its_hit_areas_in_every_state() {
    let t0 = Instant::now();
    let mut a = resting(t0);
    let rest_buf = draw(&mut a);
    let (rest_text, rest_hits) = (row_text(&rest_buf, bar_y(&rest_buf)), a.status_hits.clone());
    assert!(rest_hits.iter().any(|(_, h)| *h == StatusHit::Effort));
    work(&mut a);
    let mut states = vec![];
    for ms in [0u64, 300, 700] {
        states.push(t0 + std::time::Duration::from_millis(ms));
    }
    for ms in [IGNITE_MS + 3000, IGNITE_MS + 3000 + BREATH_MS / 2, IGNITE_MS + 3000 + BREATH_MS * 3 / 4] {
        states.push(t0 + std::time::Duration::from_millis(ms));
    }
    for at in states {
        a.fx.clock = Some(at);
        a.tick_fx();
        let b = draw(&mut a);
        assert_eq!(row_text(&b, bar_y(&b)), rest_text, "the same cells");
        assert_eq!(a.status_hits, rest_hits, "the same hit areas");
    }
    // a narrow bar that drops the chip still has its rule effect
    let mut n = ignited(t0);
    fx_at(&mut n, t0, 400);
    let w = (26..60u16)
        .find(|w| {
            let b = buf_of(&mut n, *w, 20);
            !row_text(&b, bar_y(&b)).contains('◆')
        })
        .expect("a bar narrow enough to drop the chip");
    let b = buf_of(&mut n, w, 20);
    assert!(!row_text(&b, bar_y(&b)).contains('◆'), "the chip is dropped on a bar {w} wide");
    let quiet = rule_fgs(&b);
    fx_at(&mut n, t0, IGNITE_MS + 3000);
    assert_ne!(quiet, rule_fgs(&buf_of(&mut n, w, 20)), "the crest plays alone");
}

#[test]
fn with_the_motion_off_or_on_a_mono_theme_every_frame_is_the_resting_frame() {
    let t0 = Instant::now();
    // animations off
    let (mut off, _rx) = app();
    off.opts.animations = false;
    fx_at(&mut off, t0, 0);
    effort_event(&mut off, Some("medium"), levels());
    effort_event(&mut off, Some("high"), levels());
    assert!(off.fx.effort_at.is_none(), "nothing starts");
    fx_at(&mut off, t0, 5000);
    let bar_cells = |b: &Buffer| {
        let y = bar_y(b);
        (0..b.area.width).map(|x| (b[(x, y)].symbol().to_string(), b[(x, y)].fg)).collect::<Vec<_>>()
    };
    let rest = draw(&mut off);
    let rest_bar = bar_cells(&rest);
    work(&mut off);
    for ms in [5000, 5000 + BREATH_MS / 2, 5000 + BREATH_MS, 6000] {
        fx_at(&mut off, t0, ms);
        off.tick_fx();
        assert!(!off.effort_moving(off.now()));
        let b = draw(&mut off);
        assert_eq!(mark(&b), mark(&rest));
        assert_eq!(bar_cells(&b), rest_bar, "the bar is the resting bar");
    }
    // a mono theme
    let (mut mono, _rx) = app();
    mono.theme = crate::theme::Theme::mono();
    fx_at(&mut mono, t0, 0);
    effort_event(&mut mono, Some("medium"), levels());
    effort_event(&mut mono, Some("high"), levels());
    assert!(mono.fx.effort_at.is_none());
    let rest = draw(&mut mono);
    let rest_rule = rule_fgs(&rest);
    for ms in [0, 300, 900, 1500] {
        fx_at(&mut mono, t0, ms);
        mono.tick_fx();
        let b = draw(&mut mono);
        assert_eq!(rule_fgs(&b), rest_rule);
        assert_eq!(mark(&b), mark(&rest));
        assert!(!mono.effort_moving(mono.now()));
    }
}
