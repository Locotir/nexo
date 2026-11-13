use super::*;
use nexo_agent::supervise::EvDetail;

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

use nexo_agent::supervise::SourceKind;
use nexo_core::mail::Stage as MailStage;
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
        idle_secs: Some(2),
        cost_usd: None,
        model: None,
        cwd: Some(format!("/home/dev/{key}")),
        permission_mode: None,
        parent: None,
        last_action: None,
        paused: false,
        ribbon: vec![Ev { at: NOW - 3, kind: EvKind::Edit, n: 1, detail: nexo_agent::supervise::EvDetail::None }],
        spark: vec![1, 2, 3],
        todo: Some((1, 4)),
        tests: None,
        subs: vec![],
        unread: 0,
        held: 0,
        retry_secs: None,
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
fn the_count_of_a_letter_stays_inside_the_body() {
    let mut v = AgentsView::with_rows(vec![agent("a", State::Done), agent("b", State::Done)], NOW, false);
    let t0 = Instant::now();
    v.drawn = Layout::Cards { cols: 1, card_w: 20 };
    // the receiver's card ends two columns before the body does
    let body = Rect::new(0, 2, 10, 6);
    v.cards = vec![(0, Rect::new(0, 2, 4, 3)), (1, Rect::new(5, 4, 5, 4))];
    v.live.letters.push(Letter { from: None, to: "b".into(), born: t0, delay_ms: 0, count: 5, bounce: false });
    let rule = Rect::new(0, 1, 10, 1);
    let shown = v.rows().to_vec();
    let theme = Theme::by_name("dark").unwrap();
    let mut t = Terminal::new(TestBackend::new(30, 10)).unwrap();
    // landed: the head sits on the receiver's card
    let now = at(t0, px::LIFT_MS + px::FLY_MS + 10);
    t.draw(|f| v.draw_flights(f, rule, body, &shown, &theme, now)).unwrap();
    let b = t.backend().buffer();
    for y in 0..10 {
        for x in 10..30 {
            assert_eq!(b[(x, y)].symbol(), " ", "a cell outside the body was written at {x},{y}");
        }
    }
}

#[test]
fn a_fail_mark_and_a_commit_mark_never_leave_their_card() {
    // a card two columns wide inside: the ribbon column is the card's own border
    let mut v = AgentsView::with_rows(vec![agent("a", State::Working)], NOW, false);
    let t0 = Instant::now();
    v.live.bursts.push(Burst { key: "a".into(), kind: EvKind::GitCommit, born: t0 });
    let theme = Theme::by_name("dark").unwrap();
    let row = v.rows()[0].clone();
    let card = Rect::new(2, 2, 6, 5);
    let marks = vec![(30usize, EvKind::GitCommit, NOW)];
    let mut t = Terminal::new(TestBackend::new(40, 10)).unwrap();
    t.draw(|f| v.draw_ribbon_fx(f, card, 3, 4, &marks, &row, &theme, t0)).unwrap();
    let b = t.backend().buffer();
    let touched = (0..10).flat_map(|y| (0..40).map(move |x| (x, y))).any(|(x, y)| b[(x, y)].symbol() != " ");
    assert!(!touched, "a mark far past the card's edge was drawn");
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
        r.ribbon =
            vec![Ev { at: NOW, kind: kinds[i % kinds.len()], n: 1, detail: nexo_agent::supervise::EvDetail::None }];
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
    // measured in a debug build on a shared machine: a generous bound that still catches a quadratic loop
    assert!(per < Duration::from_millis(250), "{per:?} a frame with 200 cards in motion");
}
