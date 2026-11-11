//! The top reasoning level in motion (see `fx_effort`): when it counts, when the crest and the pop
//! start, the breath while a turn runs, and the stillness at rest, with the motion off and on a mono theme.

use super::*;
use crate::fx_effort::{BAND_MS, BREATH_MS, IGNITE_MS};
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
fn the_mark_pops_when_the_tag_lights_and_settles_and_the_word_after_it_never_changes() {
    let t0 = Instant::now();
    let mut a = resting(t0);
    let (x, rest_mark, rest_word) = mark(&draw(&mut a));
    // the comet runs first: the mark waits for the landing
    for ms in [0, 300, 599] {
        fx_at(&mut a, t0, ms);
        assert_eq!(mark(&draw(&mut a)).1, rest_mark, "the mark waits for the landing at {ms} ms");
    }
    fx_at(&mut a, t0, 600);
    let (x0, pop0, word0) = mark(&draw(&mut a));
    fx_at(&mut a, t0, 850);
    let (_, pop_mid, _) = mark(&draw(&mut a));
    assert_eq!(x0, x, "the mark does not move");
    assert_ne!(pop0, rest_mark, "bright when the tag lights up");
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
    assert_eq!(mark(&draw(&mut a)).1, rest_mark, "the mark is at rest after its cycle");
    // the band along the rule takes a longer pass: the frames stop when that is done
    fx_at(&mut a, t0, start + BAND_MS + 1);
    assert!(!a.tick_fx(), "the frames stop within a pass of the end of the turn");
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
    // a turn runs here too: the breath and the band stay out
    work(&mut mono);
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

// ---- round 2: the comet into the tag, the landing, the gradient, the band ----

/// The rule row's cells from `x` for `n` cells: symbol, colour, bold.
fn cells(b: &Buffer, y: u16, x: u16, n: u16) -> Vec<(String, Color, bool)> {
    (x..x + n)
        .map(|x| {
            (b[(x, y)].symbol().to_string(), b[(x, y)].fg, b[(x, y)].modifier.contains(ratatui::style::Modifier::BOLD))
        })
        .collect()
}

/// Where the tag `◆ high` starts on the rule row, and its six cells.
fn tag(b: &Buffer) -> (u16, Vec<(String, Color, bool)>) {
    let y = rule_y(b);
    let x = (0..b.area.width).find(|x| b[(*x, y)].symbol() == "◆").expect("the tag on the rule");
    (x, cells(b, y, x, 6))
}

fn tag_text(t: &[(String, Color, bool)]) -> String {
    t.iter().map(|c| c.0.as_str()).collect()
}

/// The columns of the rule row that hold a heavy `━`.
fn heavy(b: &Buffer) -> Vec<u16> {
    let y = rule_y(b);
    (0..b.area.width).filter(|x| b[(*x, y)].symbol() == "━").collect()
}

/// The screen from the rule over the draft down as text, with the heavy head read as the plain rule (the
/// welcome card above it has motion of its own).
fn plain_text(b: &Buffer) -> Vec<String> {
    (rule_y(b)..b.area.height).map(|y| row_text(b, y).replace('━', "─")).collect()
}

fn brightness(c: Color) -> u32 {
    match c {
        Color::Rgb(r, g, b) => r as u32 + g as u32 + b as u32,
        other => panic!("a truecolor theme was expected, got {other:?}"),
    }
}

#[test]
fn the_comet_is_one_heavy_cell_left_of_the_tag_and_leaves_the_rule_as_it_was() {
    let t0 = Instant::now();
    let rest = {
        let mut r = resting(t0);
        let b = draw(&mut r);
        (rule_fgs(&b), plain_text(&b))
    };
    let mut a = ignited(t0);
    fx_at(&mut a, t0, 350);
    let b = draw(&mut a);
    assert_eq!(heavy(&b).len(), 1, "one heavy head at 350 ms");
    assert!(heavy(&b)[0] < tag(&b).0, "and it lies left of the tag");
    let mut last = None;
    for ms in (0..IGNITE_MS + 100).step_by(25) {
        fx_at(&mut a, t0, ms);
        let b = draw(&mut a);
        let (x, t) = tag(&b);
        assert_eq!(tag_text(&t), "◆ high", "the comet never reaches into the tag ({ms} ms)");
        let h = heavy(&b);
        assert!(h.len() <= 1 && h.iter().all(|c| *c < x), "at {ms} ms");
        assert_eq!(h.is_empty(), ms >= 700, "the head is drawn while the comet runs, at {ms} ms");
        if let Some(c) = h.first() {
            assert!(last.is_none_or(|l| *c >= l), "the head only moves right");
            last = Some(*c);
        }
        assert_eq!(plain_text(&b), rest.1, "the same text at {ms} ms");
    }
    fx_at(&mut a, t0, 1101);
    let b = draw(&mut a);
    assert!(heavy(&b).is_empty(), "no heavy cell after the comet");
    assert_eq!(rule_fgs(&b), rest.0, "the rule is the resting one");
    assert_eq!(tag(&b).0, tag(&draw(&mut resting(t0))).0, "the tag does not move");
}

#[test]
fn the_comet_is_plainly_seen_and_runs_with_a_slow_start_and_a_soft_arrival() {
    let t0 = Instant::now();
    let rest = rule_fgs(&draw(&mut resting(t0)));
    let mut a = ignited(t0);
    let mut at = |ms: u64| {
        fx_at(&mut a, t0, ms);
        let b = draw(&mut a);
        (heavy(&b)[0], rule_fgs(&b).iter().zip(&rest).filter(|(n, w)| n != w).count())
    };
    let (h0, _) = at(0);
    let (h100, _) = at(100);
    let (h300, _) = at(300);
    let (h350, lit) = at(350);
    let (h400, _) = at(400);
    let (h600, _) = at(600);
    let (h690, _) = at(690);
    assert!(h100 < 10, "a slow start, at column {h100}");
    assert!(h350 > 30 && h350 < 60, "about halfway at the middle, at column {h350}");
    assert!(h400 - h300 > 2 * (h100 - h0), "faster in the middle than at the start");
    assert!(h400 - h300 > 2 * (h690 - h600), "and than at the arrival");
    assert!(h690 >= h600, "a soft arrival, still moving");
    assert!(lit >= 14, "the tail lights {lit} cells");
}

#[test]
fn the_tag_lights_bold_when_the_comet_lands_and_settles_to_its_gradient() {
    let t0 = Instant::now();
    let rest = tag(&draw(&mut resting(t0))).1;
    let mut a = ignited(t0);
    for ms in [0, 300, 599] {
        fx_at(&mut a, t0, ms);
        assert_eq!(tag(&draw(&mut a)).1, rest, "the tag waits for the comet ({ms} ms)");
    }
    fx_at(&mut a, t0, 700);
    let lit = tag(&draw(&mut a)).1;
    assert!(brightness(lit[0].1) > brightness(rest[0].1), "the ◆ is brighter than at rest");
    assert!(lit[0].2 && !rest[0].2, "and bold");
    assert!(lit.iter().filter(|c| c.0 != " ").all(|c| c.2), "every letter is bold, not the blank");
    assert!(!lit[1].2, "the blank is not");
    fx_at(&mut a, t0, 900);
    let mid = tag(&draw(&mut a)).1;
    assert!(brightness(mid[0].1) < brightness(lit[0].1) && brightness(mid[0].1) > brightness(rest[0].1), "easing down");
    fx_at(&mut a, t0, 1101);
    assert_eq!(tag(&draw(&mut a)).1, rest, "the resting gradient, not bold");
}

#[test]
fn at_rest_at_the_top_level_the_tag_is_a_still_gradient_and_below_it_the_tag_is_dim() {
    let t0 = Instant::now();
    let mut a = resting(t0);
    let (_, t) = tag(&draw(&mut a));
    assert_ne!(t[0].1, t[5].1, "the first and the last letter differ");
    assert!(t.iter().all(|c| !c.2), "not bold");
    assert!(!a.tick_fx(), "a still gradient asks for no frame");
    for ms in [IGNITE_MS + 5000, IGNITE_MS + 6234, IGNITE_MS + 9000] {
        fx_at(&mut a, t0, ms);
        assert!(!a.tick_fx());
        assert_eq!(tag(&draw(&mut a)).1, t, "the same at every time");
    }
    effort_event(&mut a, Some("medium"), levels());
    fx_at(&mut a, t0, IGNITE_MS + 30000);
    a.tick_fx();
    let b = draw(&mut a);
    let (_, low) = tag(&b);
    let dim = a.theme.dim2;
    assert!(low.iter().filter(|c| c.0 != " ").all(|c| c.1 == dim), "a lower level keeps the dim tag");
    assert!(low.iter().all(|c| !c.2));
}

/// Working at the top level since `start`; the app has been drawn once at that moment.
fn working(t0: Instant, start: u64) -> App {
    let mut a = resting(t0);
    work(&mut a);
    fx_at(&mut a, t0, start);
    a.tick_fx();
    draw(&mut a);
    a
}

#[test]
fn while_the_model_works_a_band_drifts_over_the_rule_cells_and_the_tag_flows() {
    let t0 = Instant::now();
    let start = IGNITE_MS + 3000;
    let mut a = working(t0, start);
    fx_at(&mut a, t0, start);
    let t_start = tag(&draw(&mut a)).1;
    // a whole pass later the band is at its start again (the line has come up out of the rule by then)
    fx_at(&mut a, t0, start + BAND_MS);
    let base = draw(&mut a);
    let (tx, _) = tag(&base);
    let y = rule_y(&base);
    fx_at(&mut a, t0, start + BAND_MS + BAND_MS / 2);
    assert!(a.tick_fx());
    let mid = draw(&mut a);
    let mut changed = 0;
    for x in 0..base.area.width {
        if x >= tx && x < tx + 6 {
            continue;
        }
        let (was, now) = (&base[(x, y)], &mid[(x, y)]);
        assert_eq!(was.symbol(), now.symbol(), "no symbol changes at column {x}");
        if now.fg != was.fg {
            assert_eq!(now.symbol(), "─", "only a rule cell changes colour, column {x}");
            changed += 1;
        }
    }
    assert!(changed >= 10, "the band is {changed} cells wide");
    assert!(changed <= base.area.width as usize / 3, "and calm: not more than a third of the rule");
    // the tag: its letters move through the gradient, half a turn later they are not the same
    fx_at(&mut a, t0, start + BREATH_MS / 2);
    let t_half = tag(&draw(&mut a)).1;
    assert_ne!(t_half.iter().map(|c| c.1).collect::<Vec<_>>(), t_start.iter().map(|c| c.1).collect::<Vec<_>>());
    assert_eq!(tag_text(&t_half), "◆ high");
    // never bold, never moved
    assert!(t_half.iter().all(|c| !c.2));
    fx_at(&mut a, t0, start + BREATH_MS);
    assert_eq!(tag(&draw(&mut a)).1, t_start, "a whole turn is back at the start");
}

#[test]
fn when_the_turn_ends_the_band_and_the_flow_finish_their_pass_and_stop_at_rest() {
    let t0 = Instant::now();
    let start = IGNITE_MS + 3000;
    let rest = {
        let mut r = resting(t0);
        let b = buf_of(&mut r, 100, 20);
        (rule_fgs(&b), plain_text(&b), tag(&b).1)
    };
    let mut a = working(t0, start);
    fx_at(&mut a, t0, start + 1000);
    a.busy = false;
    a.tick_fx();
    fx_at(&mut a, t0, start + 1100);
    assert!(a.tick_fx(), "the pass it is on goes on");
    fx_at(&mut a, t0, start + BAND_MS - 1);
    assert!(a.tick_fx());
    fx_at(&mut a, t0, start + BAND_MS + 1);
    assert!(!a.tick_fx(), "then no frame is asked for");
    let b = draw(&mut a);
    assert_eq!(rule_fgs(&b), rest.0, "the rule is the resting rule");
    assert_eq!(plain_text(&b), rest.1);
    assert_eq!(tag(&b).1, rest.2, "the tag stops on the still gradient");
    assert!(a.fx.band_from.is_none() && a.fx.breath_from.is_none(), "both clocks are done");
}

#[test]
fn the_text_the_widths_and_the_hit_areas_are_the_same_in_every_state() {
    let t0 = Instant::now();
    let mut rest = resting(t0);
    let rest_buf = draw(&mut rest);
    let (rest_text, rest_hits) = (plain_text(&rest_buf), rest.status_hits.clone());
    let mut a = ignited(t0);
    for ms in [0, 200, 350, 650, 750, 900, 1050, 1101] {
        fx_at(&mut a, t0, ms);
        let b = draw(&mut a);
        assert_eq!(plain_text(&b), rest_text, "the same cells at {ms} ms");
        assert_eq!(a.status_hits, rest_hits, "the same hit areas at {ms} ms");
    }
    let start = IGNITE_MS + 3000;
    let mut w = working(t0, start);
    fx_at(&mut w, t0, start + 700);
    let base = plain_text(&draw(&mut w));
    let base_hits = w.status_hits.clone();
    for ms in [800, 1600, 2400, 3000] {
        fx_at(&mut w, t0, start + ms);
        let b = draw(&mut w);
        assert_eq!(plain_text(&b), base, "the same cells mid-work at {ms} ms");
        assert_eq!(w.status_hits, base_hits);
    }
}

#[test]
fn on_a_row_where_the_tag_does_not_fit_the_comet_runs_to_the_end_and_nothing_panics() {
    let t0 = Instant::now();
    let (mut a, _rx) = app();
    fx_at(&mut a, t0, 0);
    // a level with a name too long for the 24 cells of the narrowest window: the tag is dropped
    let long = vec!["low".to_string(), "medium".to_string(), "a_level_with_a_long_name".to_string()];
    effort_event(&mut a, Some("medium"), long.clone());
    effort_event(&mut a, Some("a_level_with_a_long_name"), long);
    fx_at(&mut a, t0, 690);
    let b = buf_of(&mut a, 24, 20);
    let y = rule_y(&b);
    assert!(!row_text(&b, y).contains('◆'), "no tag on the rule");
    let h: Vec<u16> = (0..b.area.width).filter(|x| b[(*x, y)].symbol() == "━").collect();
    assert_eq!(h, vec![23], "the head is at the end of the row");
    // sizes of nothing and of one cell
    let cx = ratatui::style::Color::Reset;
    for w in 0..3u16 {
        for edge in [None, Some(0), Some(w), Some(9)] {
            for ms in [0, 350, 700, 1000] {
                fx_at(&mut a, t0, ms);
                let area = ratatui::layout::Rect::new(0, 0, w, 1);
                let mut buf = Buffer::empty(area);
                for x in 0..w {
                    buf[(x, 0)].set_char('─').set_fg(cx);
                }
                let before = buf.clone();
                a.paint_crest(&mut buf, area, edge);
                if w < 2 {
                    assert_eq!(buf, before, "width {w} draws nothing");
                }
            }
        }
    }
}

#[test]
fn leaving_the_top_level_clears_every_clock_and_asks_for_no_frame() {
    let t0 = Instant::now();
    let start = IGNITE_MS + 3000;
    // in the middle of an ignite
    let mut a = ignited(t0);
    fx_at(&mut a, t0, 300);
    assert!(a.effort_moving(a.now()));
    effort_event(&mut a, Some("medium"), levels());
    assert!(!a.effort_moving(a.now()), "off the top level nothing of it is drawn");
    a.tick_fx();
    assert!(a.fx.effort_at.is_none(), "the clock is cleared");
    // at work
    let mut w = working(t0, start);
    fx_at(&mut w, t0, start + 1000);
    assert!(w.effort_moving(w.now()));
    effort_event(&mut w, Some("medium"), levels());
    assert!(!w.effort_moving(w.now()));
    let b = draw(&mut w);
    assert!(heavy(&b).is_empty());
    let low = tag(&b).1;
    let dim = w.theme.dim2;
    assert!(low.iter().filter(|c| c.0 != " ").all(|c| c.1 == dim && !c.2), "the very next frame is the resting one");
    assert!(w.fx.breath_from.is_none() && w.fx.band_from.is_none() && w.fx.effort_at.is_none());
    // the bar lights a changed level on its own and a turn has motion of its own: with this module done,
    // the app asks for a frame exactly as a turn at a lower level does
    let mut c = resting(t0);
    effort_event(&mut c, Some("medium"), levels());
    work(&mut c);
    for app in [&mut w, &mut c] {
        fx_at(app, t0, start + 1000 + crate::motion::CUT_MS + 400);
    }
    assert_eq!(w.tick_fx(), c.tick_fx(), "no frame is asked for because of the level it left");
    // the list of levels reloads without the level: the same
    let mut r = working(t0, start);
    r.effort_levels = vec!["low".into(), "medium".into(), "high".into(), "max".into()];
    fx_at(&mut r, t0, start + 500);
    assert!(!r.effort_moving(r.now()));
    r.tick_fx();
    assert!(r.fx.breath_from.is_none() && r.fx.band_from.is_none());
}

#[test]
fn with_the_motion_off_or_on_a_mono_theme_the_new_pieces_are_the_resting_look_too() {
    let t0 = Instant::now();
    let rest_of = |a: &mut App| {
        work(a);
        fx_at(a, t0, 5000);
        let b = draw(a);
        (rule_fgs(&b), tag(&b).1, plain_text(&b))
    };
    let mut apps = vec![];
    let (mut off, _rx) = app();
    off.opts.animations = false;
    apps.push(off);
    let (mut mono, _rx) = app();
    mono.theme = crate::theme::Theme::mono();
    apps.push(mono);
    for a in &mut apps {
        fx_at(a, t0, 0);
        effort_event(a, Some("medium"), levels());
        effort_event(a, Some("high"), levels());
        let rest = rest_of(a);
        let dim = a.theme.dim2;
        assert!(rest.1.iter().filter(|c| c.0 != " ").all(|c| c.1 == dim && !c.2), "the tag stays dim");
        for ms in [5000, 5350, 5700, 5900, 6101, 6600, 8200, 10000, 11200, 14000] {
            fx_at(a, t0, ms);
            a.tick_fx();
            assert!(!a.effort_moving(a.now()));
            let b = draw(a);
            assert!(heavy(&b).is_empty(), "no heavy cell at {ms} ms");
            assert_eq!(rule_fgs(&b), rest.0, "the same rule at {ms} ms");
            assert_eq!(tag(&b).1, rest.1, "the same tag at {ms} ms");
        }
    }
}
