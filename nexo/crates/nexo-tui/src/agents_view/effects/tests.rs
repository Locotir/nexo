use super::*;
use crate::fx::panel as px;
use nexo_supervise::{EvDetail, LaneInfo};
use std::collections::HashSet;

fn ev(at: Secs, kind: EvKind, n: u32) -> Ev {
    Ev { at, kind, n, detail: EvDetail::None }
}

#[test]
fn fresh_events_are_those_after_the_newest_one_seen() {
    let r = vec![ev(10, EvKind::Read, 1), ev(11, EvKind::Edit, 1), ev(12, EvKind::TestsPass, 1)];
    assert_eq!(fresh(&r, Some(Some((11, EvKind::Edit, 1))), 100).len(), 1);
    assert_eq!(fresh(&r, Some(Some((12, EvKind::TestsPass, 1))), 100).len(), 0);
    assert_eq!(fresh(&r, Some(None), 100).len(), 3);
    // the newest one folded another of its kind
    let r = vec![ev(10, EvKind::Edit, 3)];
    assert_eq!(fresh(&r, Some(Some((10, EvKind::Edit, 2))), 100).len(), 1);
    // a session that just appeared brings only the last seconds
    let r = vec![ev(10, EvKind::Read, 1), ev(98, EvKind::Edit, 1)];
    assert_eq!(fresh(&r, None, 100).len(), 1);
    // the one it knew has scrolled out: what is later counts
    let r = vec![ev(20, EvKind::Read, 1), ev(21, EvKind::Edit, 1)];
    assert_eq!(fresh(&r, Some(Some((10, EvKind::Run, 1))), 100).len(), 2);
}

#[test]
fn only_the_kinds_with_an_effect_have_a_lifetime() {
    assert_eq!(burst_ms(EvKind::TestsPass), Some(400));
    assert_eq!(burst_ms(EvKind::Compact), Some(360));
    assert_eq!(burst_ms(EvKind::Read), None);
}

#[test]
fn failing_stages_are_bad_and_the_others_are_not() {
    for s in [Stage::Refused, Stage::Expired, Stage::Failed] {
        assert!(is_bad(s));
    }
    for s in [Stage::Sent, Stage::Held, Stage::Delivered, Stage::Read] {
        assert!(!is_bad(s));
    }
}

// ------------------------------------------------------------------ motion audit

use nexo_core::mail::Stage as MailStage;
use nexo_supervise::SourceKind;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;
use std::time::Duration;

const NOW: i64 = 1_000_000;

fn agent(key: &str, state: State) -> AgentRow {
    AgentRow {
        key: key.into(),
        name: key.into(),
        source: SourceKind::Nexo,
        program: "nexo".into(),
        state,
        since: NOW - 60,
        last_step: Some("Edit  src/lib.rs".into()),
        last_conclusion: None,
        idle_secs: Some(2),
        cost_usd: None,
        model: None,
        cwd: Some(format!("/home/dev/{key}")),
        permission_mode: None,
        parent: None,
        last_action: None,
        paused: false,
        ribbon: vec![Ev { at: NOW - 3, kind: EvKind::Edit, n: 1, detail: nexo_supervise::EvDetail::None }],
        spark: vec![1, 2, 3],
        todo: Some((1, 4)),
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

fn mail(id: &str, from: &str, to: &str) -> MailRow {
    MailRow {
        id: id.into(),
        from: from.into(),
        to: to.into(),
        summary: "ship it".into(),
        stage: MailStage::Sent,
        at: NOW,
        bytes: 7,
        n: 1,
    }
}

fn at(t0: Instant, ms: u64) -> Instant {
    t0 + Duration::from_millis(ms)
}

fn paint(v: &mut AgentsView, w: u16, h: u16, now: Instant, fx: bool) -> Buffer {
    let theme = Theme::by_name("dark").unwrap();
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| v.draw(f, f.area(), &theme, now, fx)).unwrap();
    t.backend().buffer().clone()
}

fn row_text(b: &Buffer, y: u16, x0: u16, x1: u16) -> String {
    (x0..x1).map(|x| b[(x, y)].symbol().to_string()).collect()
}

fn screen(b: &Buffer) -> String {
    (0..b.area.height).map(|y| row_text(b, y, 0, b.area.width)).collect::<Vec<_>>().join("\n")
}

fn pane_text(v: &AgentsView, key: &str, w: usize) -> String {
    let row = v.rows().iter().find(|r| r.key == key).unwrap().clone();
    let kind = crate::agents_view::Kind::of(&row);
    v.pane_lines(&row, kind, w, &Theme::dark(), Instant::now(), false)
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_pane_puts_the_last_result_under_the_state_of_a_finished_session() {
    let mut row = agent("builder", State::Done);
    row.last_conclusion = Some("tests passed".into());
    let mut v = AgentsView::with_rows(vec![row], NOW, false);
    let b = paint(&mut v, 150, 30, Instant::now(), false);
    let text = screen(&b);
    assert!(text.contains("last") && text.contains("tests passed"), "{text}");
    assert!(text.contains("done"), "{text}");
}

#[test]
fn a_sub_agent_sits_under_its_session_and_its_pane_names_its_model() {
    let mut parent = agent("parent", State::Working);
    parent.last_step = Some("workflow  review".into());
    let mut child = agent("parent.3", State::Working);
    child.name = "reviewer".into();
    child.parent = Some("parent".into());
    child.model = Some("xai/grok-test".into());
    let mut v = AgentsView::with_rows(vec![parent, child], NOW, false);
    let b = paint(&mut v, 150, 30, Instant::now(), false);
    let text = screen(&b);
    let at = |s: &str| text.lines().position(|l| l.contains(s));
    assert!(at("parent") < at("reviewer"), "{text}");
    assert!(text.lines().any(|l| l.contains("reviewer") && (l.contains('╰') || l.contains('├'))), "{text}");
    let pane = pane_text(&v, "parent.3", 80);
    assert!(pane.contains("sub-agent") && pane.contains("grok-test"), "{pane}");
}

#[test]
fn a_paused_watch_is_said_in_the_pane_at_every_width() {
    let mut row = agent("builder", State::Working);
    row.paused = true;
    let v = AgentsView::with_rows(vec![row], NOW, false);
    for w in [26, 50, 80] {
        let text = pane_text(&v, "builder", w);
        assert!(text.contains("supervision paused"), "{w}: {text}");
    }
}

#[test]
fn a_draw_with_the_motion_off_asks_for_no_frame_even_when_the_fleet_is_busy() {
    let t0 = Instant::now();
    let mut v =
        AgentsView::with_rows(vec![agent("a", State::Working), agent("b", State::WaitingPermission)], NOW, false);
    paint(&mut v, 100, 30, t0, true);
    assert!(v.animating(at(t0, 500)), "a waiting card breathes");
    // the motion went off (theme switched to mono, animations disabled): the loop must rest
    paint(&mut v, 100, 30, at(t0, 600), false);
    assert!(!v.animating(at(t0, 700)), "a still draw must not keep the loop at full rate");
    paint(&mut v, 100, 30, at(t0, 800), true);
    assert!(v.animating(at(t0, 900)), "and it moves again when the motion is back");
}

#[test]
fn a_static_page_asks_for_no_frame() {
    let t0 = Instant::now();
    let mut v = AgentsView::with_rows(vec![agent("a", State::WaitingPermission)], NOW, false);
    paint(&mut v, 100, 30, t0, true);
    assert!(v.animating(t0));
    v.policy_page = true;
    assert!(!v.animating(at(t0, 100)), "the policy page has nothing that moves");
}

#[test]
fn a_burst_of_mail_flies_a_few_letters_and_the_panel_is_calm_again_within_three_seconds() {
    let t0 = Instant::now();
    let rows: Vec<AgentRow> = (0..60).map(|i| agent(&format!("s{i}"), State::Done)).collect();
    let mut v = AgentsView::with_rows(rows.clone(), NOW, false);
    v.set_rows(
        rows.iter()
            .cloned()
            .map(|mut r| {
                r.ribbon.clear();
                r.todo = None;
                r
            })
            .collect(),
    );
    let rows = v.rows().to_vec();
    for ms in [0, 3000] {
        paint(&mut v, 140, 40, at(t0, ms), true);
    }
    v.fly_new_mail(vec![mail("seed", "s0", "s1")], &rows);
    paint(&mut v, 140, 40, at(t0, 3100), true);
    // thirty senders at once
    let feed: Vec<MailRow> = std::iter::once(mail("seed", "s0", "s1"))
        .chain((1..31).map(|i| mail(&format!("m{i}"), &format!("s{i}"), &format!("s{}", i + 1))))
        .collect();
    v.fly_new_mail(feed, &rows);
    let sent = at(t0, 4000);
    paint(&mut v, 140, 40, sent, true);
    assert!(v.live.letters.len() <= px::MAX_LETTERS, "{} letters in the air", v.live.letters.len());
    assert!(v.animating(at(t0, 4500)));
    // the loop draws while the panel asks for frames: when does it rest?
    let mut ms = 0;
    while v.animating(at(sent, ms)) && ms < 10_000 {
        ms += 33;
        paint(&mut v, 140, 40, at(sent, ms), true);
    }
    assert!(ms <= 3000, "still moving {ms} ms after the burst");
}

#[test]
fn nothing_lands_on_the_right_half_of_a_wide_glyph_and_the_left_half_takes_both() {
    let mut t = Terminal::new(TestBackend::new(12, 2)).unwrap();
    t.draw(|f| {
        f.render_widget(ratatui::widgets::Paragraph::new("世界"), Rect::new(0, 0, 12, 1));
        put(f, (1, 0), "✉", Color::Red, true);
        assert_eq!(f.buffer_mut()[(0, 0)].symbol(), "世", "the glyph is whole");
        assert_ne!(f.buffer_mut()[(1, 0)].symbol(), "✉", "nothing sits on its right half");
        put(f, (2, 0), "✉", Color::Red, true);
        assert_eq!(f.buffer_mut()[(2, 0)].symbol(), "✉");
        assert_eq!(f.buffer_mut()[(3, 0)].symbol(), " ", "the right half of the glyph it replaced is cleared");
    })
    .unwrap();
}

#[test]
fn the_pulse_row_flashes_once_then_rests_even_when_a_figure_rises_every_poll() {
    let t0 = Instant::now();
    let mut v = AgentsView::with_rows(vec![agent("a", State::Done)], NOW, false);
    for ms in [0, 100] {
        paint(&mut v, 120, 40, at(t0, ms), true);
    }
    let mut flashes = Vec::new();
    for i in 1..=10u64 {
        let mut r = agent("a", State::Done);
        r.tests = Some((i as u32, 0));
        v.set_rows(vec![r]);
        let now = at(t0, 200 + i * 300);
        paint(&mut v, 120, 40, now, true);
        if v.live.pulse.flashing(now) {
            flashes.push(i);
        }
    }
    // a poll every 300 ms raises the figure: without a pause the row would be lit at nearly every one
    assert!(flashes.len() <= 3, "the row flickers: lit at polls {flashes:?}");
    assert!(!flashes.is_empty(), "and it still flashes for a new run");
}

/// Every frame of a busy moment, drawn at sizes that change under the effects: nothing is written outside
/// the window, and the footer is the one of the settled panel.
#[test]
fn no_effect_leaves_the_window_or_covers_the_footer_through_resizes() {
    let t0 = Instant::now();
    let mut base: Vec<AgentRow> = (0..8).map(|i| agent(&format!("s{i}"), State::Working)).collect();
    for r in &mut base {
        r.ribbon.clear();
    }
    let mut v = AgentsView::with_rows(base.clone(), NOW, false);
    for ms in [0, 3000] {
        paint(&mut v, 150, 44, at(t0, ms), true);
    }
    let mut busy = base.clone();
    for (i, r) in busy.iter_mut().enumerate() {
        let kinds =
            [EvKind::TestsPass, EvKind::TestsFail, EvKind::GitCommit, EvKind::GitPush, EvKind::Compact, EvKind::Error];
        r.ribbon = vec![Ev { at: NOW, kind: kinds[i % kinds.len()], n: 1, detail: nexo_supervise::EvDetail::None }];
        r.subs = vec![(format!("s{i}.1"), State::Working), (format!("s{i}.2"), State::Done)];
    }
    v.set_rows(busy.clone());
    v.fly_new_mail(vec![mail("seed", "s0", "s1")], &busy);
    paint(&mut v, 150, 44, at(t0, 3100), true);
    let feed: Vec<MailRow> = std::iter::once(mail("seed", "s0", "s1"))
        .chain((1..7).map(|i| mail(&format!("m{i}"), "s7", &format!("s{i}"))))
        .collect();
    v.fly_new_mail(feed, &busy);
    let sizes = [(150u16, 44u16), (100, 30), (60, 20), (40, 12), (150, 44), (30, 6)];
    for (k, ms) in (0..160u64).map(|k| (k, 4000 + k * 16)).collect::<Vec<_>>() {
        let (w, h) = sizes[(k as usize / 4) % sizes.len()];
        let b = paint(&mut v, w, h, at(t0, ms), true);
        let win = v.window;
        if win.width == 0 {
            continue;
        }
        for y in 0..h {
            for x in 0..w {
                let inside = x >= win.x && x < win.x + win.width && y >= win.y && y < win.y + win.height;
                if !inside {
                    assert_eq!(b[(x, y)].symbol(), " ", "{w}x{h} at {ms} ms: a cell outside the window at {x},{y}");
                }
            }
        }
        let mut rest = v.clone();
        let settled = paint(&mut rest, w, h, at(t0, 60_000), true);
        let fy = win.y + win.height.saturating_sub(2);
        assert_eq!(
            row_text(&b, fy, win.x + 2, win.x + win.width.saturating_sub(2)),
            row_text(&settled, fy, win.x + 2, win.x + win.width.saturating_sub(2)),
            "{w}x{h} at {ms} ms: the footer was covered"
        );
    }
}

#[test]
fn effects_on_two_hundred_cards_stay_cheap() {
    let t0 = Instant::now();
    let rows: Vec<AgentRow> = (0..200)
        .map(|i| {
            let mut r = agent(&format!("s{i}"), if i % 3 == 0 { State::Working } else { State::WaitingPermission });
            r.subs = vec![(format!("s{i}.1"), State::Working), (format!("s{i}.2"), State::Done)];
            r
        })
        .collect();
    let mut v = AgentsView::with_rows(rows.clone(), NOW, false);
    for ms in [0, 3000] {
        paint(&mut v, 240, 70, at(t0, ms), true);
    }
    v.fly_new_mail(vec![mail("seed", "s0", "s1")], &rows);
    let feed: Vec<MailRow> = std::iter::once(mail("seed", "s0", "s1"))
        .chain((1..150).map(|i| mail(&format!("m{i}"), &format!("s{i}"), &format!("s{}", i + 1))))
        .collect();
    v.fly_new_mail(feed, &rows);
    let start = std::time::Instant::now();
    let frames = 30u64;
    for k in 0..frames {
        paint(&mut v, 240, 70, at(t0, 3100 + k * 33), true);
        let _ = v.animating(at(t0, 3100 + k * 33));
    }
    let per = start.elapsed() / frames as u32;
    // about 25 ms a frame in a debug build here; the msrv runner took 277 ms when each letter rebuilt the tree (110 ms
    // here), 2.5x slower than this machine. The bound is well above a slow runner and still catches that rebuild.
    assert!(per < Duration::from_millis(600), "{per:?} a frame with 200 cards in motion");
}

#[test]
fn the_tree_uses_real_parents_and_keeps_the_selection_through_a_fold_and_a_refresh() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
    let root = agent("root", State::Working);
    let mut child = agent("root.1", State::Working);
    child.parent = Some("root".into());
    let mut v = AgentsView::with_rows(vec![child.clone(), root.clone()], NOW, true);
    let shown = v.shown();
    assert!(shown.iter().position(|r| r.key == "root") < shown.iter().position(|r| r.key == "root.1"));
    while v.selected_key().as_deref() != Some("root.1") {
        v.key(key(KeyCode::Down));
    }
    v.key(key(KeyCode::Up));
    assert_eq!(v.selected_key().as_deref(), Some("root"));
    v.key(key(KeyCode::Left));
    assert!(!v.shown().iter().any(|r| r.key == "root.1"), "← folds the parent");
    assert_eq!(v.selected_key().as_deref(), Some("root"));
    v.key(key(KeyCode::Right));
    assert!(v.shown().iter().any(|r| r.key == "root.1"), "→ unfolds it");
    v.key(key(KeyCode::Down));
    v.set_rows(vec![root, child]);
    assert_eq!(v.selected_key().as_deref(), Some("root.1"), "the selection stays on its row after a poll");
}

#[test]
fn the_tree_keeps_cycles_without_looping_or_dropping_rows() {
    let mut a = agent("a", State::Working);
    let mut b = agent("b", State::Working);
    a.parent = Some("b".into());
    b.parent = Some("a".into());
    let rows = super::super::tree::ordered(vec![a, b]);
    assert_eq!(rows.len(), 2);
}

#[test]
fn the_tree_draws_connectors_and_marks_orphans_and_cycles() {
    use super::super::tree::{self, Issue};
    let root = agent("root", State::Working);
    let mut a = agent("a", State::Working);
    a.parent = Some("root".into());
    let mut grand = agent("grand", State::Done);
    grand.parent = Some("a".into());
    let mut b = agent("b", State::Working);
    b.parent = Some("root".into());
    let mut orphan = agent("orphan", State::Done);
    orphan.parent = Some("missing".into());
    let mut cycle = agent("cycle", State::Done);
    cycle.parent = Some("cycle".into());
    let rows = tree::ordered(vec![root, a, grand, b, orphan, cycle]);
    let p = tree::prefixes(&rows, false);
    assert_eq!(p[0], "");
    assert!(p[1].starts_with('├') && p[3].starts_with('╰') || p[3].starts_with('└'), "{p:?}");
    let (_, issues) = tree::relations(&rows);
    assert_eq!(issues[4], Issue::Orphan);
    assert_eq!(issues[5], Issue::Cycle);
}

// ------------------------------------------------------------------ the swarm

/// A session with `kids` sub-agents in `state`, the session itself in `own`.
fn family(own: State, kids: &[State]) -> Vec<AgentRow> {
    let mut parent = agent("main", own);
    parent.last_step = Some("workflow  review".into());
    parent.model = Some("xai/grok-4.7".into());
    let mut rows = vec![parent];
    for (i, s) in kids.iter().enumerate() {
        let mut c = agent(&format!("main.{i}"), *s);
        c.name = ["explore", "tests", "reviewer", "docs"][i % 4].into();
        c.parent = Some("main".into());
        rows.push(c);
    }
    rows
}

fn lane(name: &str, state: &str) -> AgentRow {
    let mut r = agent(&format!("lane:{name}"), State::Idle { min: 1 });
    r.name = name.into();
    r.lane = Some(LaneInfo { name: name.into(), state: state.into(), runner: true, ..Default::default() });
    r
}

fn is_sat(s: &str) -> bool {
    ["●", "•", "·", "∙", "✗", ".", "o", "O", "x"].contains(&s)
}

/// The first line of the screen that holds `word`.
fn line_of(b: &Buffer, word: &str) -> u16 {
    (0..b.area.height).find(|&y| row_text(b, y, 0, b.area.width).contains(word)).unwrap()
}

/// The satellites drawn on line `y`: the satellite glyphs on cells that are blank in `plain`.
fn sats_on(b: &Buffer, plain: &Buffer, y: u16) -> Vec<String> {
    (0..b.area.width)
        .filter(|&x| plain[(x, y)].symbol() == " " && is_sat(b[(x, y)].symbol()))
        .map(|x| b[(x, y)].symbol().to_string())
        .collect()
}

/// The same rows with no parent, no lane running and the motion off: the screen with no satellite at all.
fn bare(rows: &[AgentRow], w: u16, h: u16) -> Buffer {
    let rows = rows
        .iter()
        .cloned()
        .map(|mut r| {
            if r.parent.is_some() {
                r.state = State::Done;
            }
            if let Some(l) = r.lane.as_mut() {
                l.runner = false;
                l.state = "stopped".into();
            }
            r
        })
        .collect();
    paint(&mut AgentsView::with_rows(rows, NOW, true), w, h, Instant::now(), false)
}

#[test]
fn the_swarm_flies_only_on_blank_cells_and_the_text_never_moves() {
    let t0 = Instant::now();
    let mut rows = family(State::Working, &[State::Working, State::Working, State::WaitingPermission]);
    rows[1].ribbon.push(ev(NOW - 1, EvKind::Run, 1));
    let mut v = AgentsView::with_rows(rows.clone(), NOW, false);
    let still = paint(&mut AgentsView::with_rows(rows.clone(), NOW, true), 80, 24, t0, false);
    paint(&mut v, 80, 24, t0, true);
    let y = line_of(&still, " main ");
    let mut seen = HashSet::new();
    for k in 0..90u64 {
        let b = paint(&mut v, 80, 24, at(t0, 2000 + k * 97), true);
        for (x, yy) in (0..80).flat_map(|x| (0..24).map(move |y| (x, y))) {
            let (on, off) = (b[(x, yy)].symbol(), still[(x, yy)].symbol());
            let blank_or_sat = |s: &str| s == " " || is_sat(s);
            assert!(on == off || (yy == y && blank_or_sat(on) && blank_or_sat(off)), "({x},{yy}) {on:?} over {off:?}");
        }
        seen.insert(row_text(&b, y, 0, 80));
    }
    assert!(seen.len() > 20, "the satellites go round: {} different looks of the line", seen.len());
    let line = row_text(&still, y, 0, 80);
    assert_eq!(line.matches(['●', '•', '·']).count(), 1 + 3, "the row glyph and one still satellite a sub-agent");
}

#[test]
fn a_long_step_gives_way_so_the_sub_agents_still_have_room_to_circle() {
    let mut rows = family(State::Working, &[State::Working]);
    rows[0].last_step = Some(format!("bash  cargo test -p nexo-tui -- --nocapture {}", "the_whole_suite ".repeat(8)));
    let b = paint(&mut AgentsView::with_rows(rows.clone(), NOW, true), 200, 60, Instant::now(), false);
    let plain = bare(&rows, 200, 60);
    let y = line_of(&plain, " main  ");
    assert!(row_text(&plain, y, 0, 200).contains('…'), "the step is cut short");
    assert_eq!(sats_on(&b, &plain, y).len(), 1, "and its sub-agent goes round after it");
}

#[test]
fn a_sub_agent_that_finishes_pulses_and_sinks_and_then_the_panel_rests() {
    for (ok, total) in [(true, px::SAT_DONE_MS), (false, px::SAT_FAIL_MS)] {
        let t0 = Instant::now();
        let rows = family(State::Idle { min: 1 }, &[State::Working]);
        let mut v = AgentsView::with_rows(rows.clone(), NOW, false);
        paint(&mut v, 80, 24, t0, true);
        let plain = bare(&rows, 80, 24);
        let y = line_of(&plain, " main ");
        // the row slides in from the right at first and its orbit follows it home
        paint(&mut v, 80, 24, at(t0, 1000), true);
        let b = paint(&mut v, 80, 24, at(t0, 2000), true);
        assert_eq!(sats_on(&b, &plain, y).len(), 1, "one satellite while it works");
        assert!(v.animating(at(t0, 2000)));
        let mut done = rows.clone();
        done[1].state = State::Done;
        if !ok {
            done[1].ribbon.push(ev(NOW, EvKind::Error, 1));
        }
        v.set_rows(done);
        let t1 = at(t0, 3000);
        paint(&mut v, 80, 24, t1, true);
        let b = paint(&mut v, 80, 24, at(t1, 60), true);
        let pulse = sats_on(&b, &plain, y);
        assert!(
            pulse.contains(&(if ok { "●" } else { "✗" }).to_string()) && pulse.contains(&"·".to_string()),
            "{pulse:?}"
        );
        assert!(v.animating(at(t1, total - 20)), "its end still plays");
        let b = paint(&mut v, 80, 24, at(t1, total + 20), true);
        assert!(sats_on(&b, &plain, y).is_empty(), "gone after its end");
        assert!(!v.animating(at(t1, total + 40)), "nothing moves once the last satellite has sunk");
    }
}

#[test]
fn the_lanes_that_are_on_circle_their_header_and_a_new_one_grows_from_a_point() {
    let t0 = Instant::now();
    let rows = vec![lane("WINDOWS", "idle")];
    let mut v = AgentsView::with_rows(rows.clone(), NOW, false);
    let plain = bare(&[lane("WINDOWS", "idle"), lane("IOS", "running")], 80, 24);
    let y = line_of(&plain, " LANES ");
    let b = paint(&mut v, 80, 24, t0, true);
    assert_eq!(sats_on(&b, &plain, y), ["•"], "a lane between rounds rests, dim and still");
    assert!(!v.animating(at(t0, 1000)), "a resting lane asks for no frame");
    v.set_rows(vec![lane("WINDOWS", "idle"), lane("IOS", "running")]);
    let t1 = at(t0, 2000);
    paint(&mut v, 80, 24, t1, true);
    let born = sats_on(&paint(&mut v, 80, 24, at(t1, 10), true), &plain, y);
    assert!(born.contains(&"·".to_string()), "it is a point when born: {born:?}");
    let mut seen = HashSet::new();
    for k in 0..100u64 {
        let grown = sats_on(&paint(&mut v, 80, 24, at(t1, 1000 + k * 100), true), &plain, y);
        // two, or one where the moving one passes over the one at rest
        assert!(matches!(grown.len(), 1 | 2), "{grown:?}");
        seen.extend(grown);
    }
    assert!(seen.contains("●") && seen.contains("·"), "grown, it comes round to the front and goes behind: {seen:?}");
}

#[test]
fn a_lane_whose_runner_gives_up_ends_with_a_cross_and_one_that_is_stopped_with_a_pulse() {
    for (word, mark) in [("failed", "✗"), ("stopped", "●")] {
        let t0 = Instant::now();
        let mut v = AgentsView::with_rows(vec![lane("IOS", "running")], NOW, false);
        paint(&mut v, 80, 24, t0, true);
        // a lane with a session of its own is told no error on its row: only its lane word says it failed
        let mut ended = lane("IOS", word);
        ended.lane.as_mut().unwrap().runner = false;
        assert!(ended.ribbon.iter().all(|e| e.kind != EvKind::Error));
        // a failed lane also shows under NEEDS YOU, which moves the header down: read the screen it ends on
        let plain = paint(&mut AgentsView::with_rows(vec![ended.clone()], NOW, true), 80, 24, t0, false);
        let y = line_of(&plain, " LANES ");
        v.set_rows(vec![ended]);
        let t1 = at(t0, 2000);
        paint(&mut v, 80, 24, t1, true);
        let pulse = sats_on(&paint(&mut v, 80, 24, at(t1, 60), true), &plain, y);
        assert!(pulse.contains(&mark.to_string()), "{word}: {pulse:?}");
    }
}

#[test]
fn with_the_motion_off_the_swarm_stands_still_and_asks_for_no_frame() {
    let t0 = Instant::now();
    let rows = family(State::Working, &[State::Working, State::Idle { min: 2 }]);
    let mut v = AgentsView::with_rows(rows.clone(), NOW, true);
    let a = paint(&mut v, 80, 24, t0, false);
    let b = paint(&mut v, 80, 24, at(t0, 1234), false);
    assert_eq!(a, b);
    let plain = bare(&rows, 80, 24);
    assert_eq!(sats_on(&a, &plain, line_of(&plain, " main ")).len(), 2);
    assert!(!v.animating(at(t0, 1300)));
    // the ASCII fallback draws them in plain letters
    v.set_ascii(true);
    let c = paint(&mut v, 80, 24, t0, false);
    let line = row_text(&c, line_of(&c, " main "), 1, 79);
    assert!(line.is_ascii() && line.contains(" o "), "{line}");
}
