//! The calm motions that cost almost nothing: the crest that rests between passes, the map flash that decays
//! on the beat, and the toast of a background job that ends.

use super::*;
use crate::frame::Load;
use nexo_core::jobs::{JobInfo, JobKind, JobStatus};
use ratatui::style::Modifier;

fn job(status: JobStatus) -> JobInfo {
    JobInfo { id: 3, kind: JobKind::Shell, label: "cargo test".into(), status, elapsed_ms: 0, lines: 0 }
}

#[test]
fn the_work_line_asks_for_frames_only_while_the_crest_is_on_the_rule() {
    let (mut a, _rx) = app();
    let t0 = a.opened + std::time::Duration::from_secs(10);
    a.busy = true;
    a.turn_started = Some(t0);
    fx_at(&mut a, t0, 1000);
    assert_eq!(a.load(), Load::Motion, "the pass: 2 s of every 2.6 s");
    fx_at(&mut a, t0, 2300);
    assert_eq!(a.load(), Load::Beat, "the rest: nothing moves but the spinner");
    fx_at(&mut a, t0, 2700);
    assert_eq!(a.load(), Load::Motion, "the next pass");
    a.opts.animations = false;
    fx_at(&mut a, t0, 1000);
    assert_eq!(a.load(), Load::Beat, "no crest without the motion");
}

#[test]
fn a_changed_map_cell_decays_on_the_beat_not_on_frames() {
    let (mut a, _rx) = app();
    let t0 = a.opened + std::time::Duration::from_secs(10);
    a.show_live = true;
    a.map_at = Some(t0);
    a.fx.clock = Some(t0 + std::time::Duration::from_millis(500));
    assert_ne!(a.load(), Load::Motion);
    assert_eq!(a.load(), Load::Beat, "it still decays");
    a.tick();
    assert_ne!(a.load(), Load::Motion, "and a tick does not turn it into frames");
    fx_at(&mut a, t0, MAP_FLASH_MS + 200);
    a.tick();
    assert_eq!(a.load(), Load::Idle, "then the clock rests");
}

#[test]
fn a_changed_map_cell_is_reversed_then_fades_to_its_own_colour() {
    use crate::ui::flash_style;
    let th = crate::theme::Theme::nexo();
    let own = th.dim;
    let base = th.style(own);
    let start = flash_style(&th, base, own, 100);
    assert!(start.add_modifier.contains(Modifier::REVERSED));
    let mid = flash_style(&th, base, own, 500);
    assert!(!mid.add_modifier.contains(Modifier::REVERSED));
    assert_ne!(mid.fg, Some(own));
    assert_eq!(flash_style(&th, base, own, MAP_FLASH_MS).fg, Some(own));
    let mono = crate::theme::Theme::mono();
    assert_eq!(flash_style(&mono, mono.style(mono.fg), mono.fg, 500), mono.style(mono.fg));
}

#[test]
fn a_job_that_ends_gives_one_toast_with_its_mark() {
    use crate::toast::Kind;
    let (mut a, _rx) = app();
    a.on_jobs(vec![job(JobStatus::Running)]);
    assert!(a.toasts.is_empty(), "a job that starts says nothing");
    a.on_jobs(vec![job(JobStatus::Done { ok: true, how: "exit 0".into() })]);
    assert_eq!(a.toasts.len(), 1);
    let t = a.toasts.iter().next().unwrap();
    assert_eq!((t.kind, t.text.as_str()), (Kind::Success, "job 3 done · cargo test"));
    // the same state again is not another end
    a.on_jobs(vec![job(JobStatus::Done { ok: true, how: "exit 0".into() })]);
    assert_eq!(a.toasts.len(), 1);

    let (mut b, _rx) = app();
    b.on_jobs(vec![job(JobStatus::Running)]);
    b.on_jobs(vec![job(JobStatus::Done { ok: false, how: "exit 1".into() })]);
    let t = b.toasts.iter().next().unwrap();
    assert_eq!((t.kind, t.text.as_str()), (Kind::Error, "job 3 failed · cargo test"));
}

#[test]
fn a_toast_that_only_stays_asks_for_the_beat_and_one_that_moves_for_frames() {
    let (mut a, _rx) = app();
    let t0 = a.opened + std::time::Duration::from_secs(10);
    a.fx.clock = Some(t0);
    a.toast(crate::toast::Kind::Success, "saved");
    fx_at(&mut a, t0, 100);
    a.tick();
    assert_eq!(a.load(), Load::Motion, "it slides in and draws its mark");
    fx_at(&mut a, t0, 1500);
    a.tick();
    assert_eq!(a.load(), Load::Beat, "it sits still");
    fx_at(&mut a, t0, 2800);
    a.tick();
    assert_eq!(a.load(), Load::Motion, "it fades out");
    fx_at(&mut a, t0, 3600);
    a.tick();
    a.tick();
    assert_eq!(a.load(), Load::Idle);
}

#[test]
fn the_crest_steps_through_the_cube_on_256_colours_and_blends_on_true_colour() {
    use ratatui::style::Color;
    let mut th = crate::theme::Theme::nexo();
    let secs = 1.0;
    let lit = |spans: Vec<ratatui::text::Span<'static>>| {
        let mut v: Vec<Color> = spans.iter().filter_map(|s| s.style.fg).collect();
        v.dedup();
        v
    };
    let rgb = lit(crate::ui::wave(&th, 60, secs));
    assert!(rgb.len() >= 3 && rgb.iter().all(|c| matches!(c, Color::Rgb(..))), "{rgb:?}");
    th.border = Color::Indexed(238);
    th.accent = Color::Indexed(81);
    th.degraded = true;
    let ix = lit(crate::ui::wave(&th, 60, secs));
    assert!(ix.len() >= 3 && ix.iter().all(|c| matches!(c, Color::Indexed(_))), "{ix:?}");
}

fn row_text(l: &ratatui::text::Line<'_>) -> String {
    l.spans.iter().map(|s| s.content.as_ref()).collect()
}

#[test]
fn a_taken_message_lights_with_a_mark_then_sweeps_out_without_moving() {
    use crate::ui::{taken_row, TAKEN_MS};
    let th = crate::theme::Theme::nexo();
    let first = taken_row(&th, "fix the build", 0, 40);
    let lit = taken_row(&th, "fix the build", 140, 40);
    assert_eq!(row_text(&first), " › fix the build");
    assert_eq!(first.spans[1].style.fg, Some(th.accent));
    assert_ne!(first.spans[5].style.fg, lit.spans[5].style.fg, "the text lights towards the full colour");
    // the sweep goes from the left, and the text never changes place
    let mid = taken_row(&th, "fix the build", 250, 40);
    let t = row_text(&mid);
    assert!(t.starts_with("   ") && t.ends_with("build"), "{t:?}");
    assert_eq!(t.chars().count(), row_text(&first).chars().count());
    assert!(row_text(&taken_row(&th, "fix the build", TAKEN_MS, 40)).trim().is_empty());
    // no colour at all: the mark is bold
    let mono = crate::theme::Theme::mono();
    assert!(taken_row(&mono, "x", 0, 20).spans[1].style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn a_message_the_agent_takes_keeps_its_row_for_the_sweep_and_asks_for_frames_only_then() {
    let (mut a, _rx) = app();
    let t0 = a.opened + std::time::Duration::from_secs(10);
    fx_at(&mut a, t0, 0);
    a.queued_msgs = vec!["one".into(), "two".into()];
    a.queued = 2;
    a.queued = 1;
    a.trim_queue();
    assert_eq!(a.queued_msgs, ["two"]);
    assert_eq!(crate::ui::taken_live(&a), 1);
    fx_at(&mut a, t0, 100);
    a.tick();
    assert_eq!(a.load(), Load::Motion);
    fx_at(&mut a, t0, 600);
    a.tick();
    assert_eq!(crate::ui::taken_live(&a), 0);
    // motion off: the row is gone at once
    let (mut b, _rx) = app();
    b.opts.animations = false;
    b.queued_msgs = vec!["one".into(), "two".into()];
    b.queued = 1;
    b.trim_queue();
    assert_eq!(crate::ui::taken_live(&b), 0);
}

#[test]
fn the_goal_mark_notches_on_a_new_round_without_moving_the_bar() {
    let (mut a, _rx) = app();
    let t0 = a.opened + std::time::Duration::from_secs(10);
    a.goal = Some(Goal { text: "ship it".into(), round: 1, max: 5, state: GoalState::Active });
    fx_at(&mut a, t0, 0);
    a.tick_fx();
    let bar = |a: &mut App| -> String {
        let b = buf_of(a, 120, 30);
        (0..120).map(|x| b[(x, 29)].symbol().to_string()).collect()
    };
    let rest = bar(&mut a);
    assert!(rest.contains("◎ goal"), "{rest}");
    a.goal = Some(Goal { text: "ship it".into(), round: 2, max: 5, state: GoalState::Active });
    fx_at(&mut a, t0, 100);
    a.tick_fx();
    fx_at(&mut a, t0, 160);
    let lit = bar(&mut a);
    assert!(lit.contains("◉ goal"), "{lit}");
    assert_eq!(lit.replace('◉', "◎"), rest, "only the mark changes; the width and the rest stay");
    a.tick();
    assert_eq!(a.load(), Load::Motion);
    fx_at(&mut a, t0, 900);
    a.tick_fx();
    assert_eq!(bar(&mut a), rest);
    a.opts.animations = false;
    a.goal = Some(Goal { text: "ship it".into(), round: 3, max: 5, state: GoalState::Active });
    a.tick_fx();
    assert!(!bar(&mut a).contains('◉'));
}
