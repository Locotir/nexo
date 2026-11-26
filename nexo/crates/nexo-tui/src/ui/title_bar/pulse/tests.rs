use super::*;
use crate::{app::App, frame::Load, ui::title_bar::draw_title, TuiOptions};
use nexo_core::agent::{AgentEvent, TurnEnd};
use ratatui::{backend::TestBackend, Terminal};
use std::time::Instant;

fn app(theme: Theme) -> (App, Instant) {
    let opts = TuiOptions {
        provider: "mock".into(),
        model: "model".into(),
        cwd: "/tmp".into(),
        status_line: vec!["curator".into(), "mode".into()],
        detail: nexo_core::config::Detail::Full,
        figure: nexo_core::config::Figure::None,
        home_stars: false,
        bursts: false,
        suggest: false,
        ..Default::default()
    };
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();
    let mut a = App::new(opts, tx);
    a.root = false;
    a.theme = theme;
    let t0 = a.opened + Duration::from_secs(10);
    a.fx.clock = Some(t0);
    (a, t0)
}

fn bar(a: &mut App, w: u16, t: Instant, ms: u64) -> Buffer {
    a.fx.clock = Some(t + Duration::from_millis(ms));
    let mut term = Terminal::new(TestBackend::new(w, 1)).unwrap();
    term.draw(|f| draw_title(f, a, f.area())).unwrap();
    term.backend().buffer().clone()
}

fn text(b: &Buffer) -> String {
    b.content.iter().map(|c| c.symbol()).collect()
}

// Locate ASCII labels in buffer columns, not UTF-8 byte offsets in the rendered row.
fn label_col(b: &Buffer, label: &str) -> usize {
    let chars: Vec<String> = label.chars().map(|c| c.to_string()).collect();
    b.content
        .windows(chars.len())
        .position(|w| w.iter().zip(&chars).all(|(cell, c)| cell.symbol() == c))
        .expect("label in buffer")
}

fn cut(a: &mut App) {
    for (view, label) in [(14_000, "curator"), (4_000, "curator -10k tok")] {
        a.handle_agent(AgentEvent::Context {
            view_tokens: view,
            history_tokens: 15_000,
            history_bytes: 60_000,
            view_bytes: 4 * view,
            messages_in_view: 2,
            messages_total: 5,
            policy: "curator".into(),
            note: None,
            label: label.into(),
            detail: None,
        });
    }
    a.cut_on_screen();
}

#[test]
fn status_pulse_cut_ripples_out_of_the_figure_without_moving_the_mode_or_its_hit() {
    for theme in [Theme::dark(), Theme::light()] {
        let (mut a, t) = app(theme);
        cut(&mut a);
        let rest = bar(&mut a, 48, t, motion::CUT_MS + 100);
        assert!(text(&rest).contains("curator -10k tok │ ask"));
        let hits = a.status_hits.clone();
        let mode = text(&rest).find("ask").unwrap();
        for (after, near, far) in [(50, 1, 5), (250, 5, 0)] {
            let b = bar(&mut a, 48, t, fx_condense::LIGHT_TO_MS + after);
            let sign = a.fx.cut_col.get().unwrap();
            assert_eq!(b[(sign, 0)].symbol(), "-");
            let base = super::super::title_segments(&a, 48)
                .into_iter()
                .flatten()
                .find(|s| s.text.contains("curator"))
                .unwrap()
                .style
                .fg
                .unwrap();
            assert_ne!(b[(sign - near, 0)].fg, base, "crest at {after} ms");
            assert_eq!(b[(sign - far, 0)].fg, base, "outside crest at {after} ms");
            assert_eq!(a.status_hits, hits);
            assert_eq!(text(&b).find("ask"), Some(mode));
        }
        // Turn motion off while the cut is hot: the whole bar is immediately the settled one.
        a.opts.animations = false;
        for ms in [0, 1050, 1250, 1500] {
            assert_eq!(bar(&mut a, 48, t, ms), rest);
        }
        a.opts.animations = true;
        let tight = bar(&mut a, 19, t, 1200);
        assert!(!text(&tight).contains("curator"));
        assert!(a.fx.cut_col.get().is_none());
        assert_eq!(bar(&mut a, 19, t, motion::CUT_MS + 200), tight);
    }
}

#[test]
fn status_pulse_signature_crosses_the_wordmark_only_on_success_and_leaves_the_clock_idle() {
    for theme in [Theme::dark(), Theme::light()] {
        let (mut a, t) = app(theme);
        a.handle_agent(AgentEvent::TurnStart { turn: 1 });
        a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
        let rest = bar(&mut a, 24, t, motion::GLOW_MS);
        let hits = a.status_hits.clone();
        for (ms, tip, unlit) in [(100, 1, 4), (380, 4, 1)] {
            let b = bar(&mut a, 24, t, ms);
            assert_eq!(text(&b), text(&rest));
            assert_eq!(a.status_hits, hits);
            assert_ne!(b[(tip, 0)].fg, rest[(tip, 0)].fg);
            assert_eq!(b[(unlit, 0)], rest[(unlit, 0)]);
            assert_eq!(b.content[5..], rest.content[5..]);
        }
        a.opts.animations = false;
        assert_eq!(bar(&mut a, 24, t, 380), rest);
        a.opts.animations = true;
        bar(&mut a, 24, t, motion::GLOW_MS + 100);
        a.tick();
        assert_eq!(a.load(), Load::Idle);
        assert_eq!(bar(&mut a, 24, t, 30_000), rest);
        for reason in [TurnEnd::Interrupted, TurnEnd::Error, TurnEnd::MaxTurns] {
            let (mut next, t) = app(a.theme.clone());
            // A quick follow-up that fails must not inherit the previous turn's still-warm stamp.
            next.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
            next.fx.clock = Some(t + Duration::from_millis(30));
            next.handle_agent(AgentEvent::TurnStart { turn: 2 });
            next.handle_agent(AgentEvent::TurnEnd { reason });
            assert_eq!(bar(&mut next, 24, t, 100).content[..5], rest.content[..5]);
        }
    }
}

fn context_event(label: &str, history: u64, policy: &str) -> AgentEvent {
    AgentEvent::Context {
        view_tokens: 4000,
        history_tokens: history,
        history_bytes: history * 4,
        view_bytes: 16000,
        messages_in_view: 2,
        messages_total: 5,
        policy: policy.into(),
        note: None,
        label: label.into(),
        detail: None,
    }
}

#[test]
fn signed_curator_wave_is_bounded_retargeted_and_visible_in_default_quiet() {
    for positive in [false, true] {
        let (mut a, t) = app(Theme::dark());
        a.opts.detail = nexo_core::config::Detail::Quiet;
        a.opts.status_line.clear();
        a.handle_agent(context_event("curator", 5000, "curator"));
        a.busy = true;
        let label = if positive { "curator +3.0k tok" } else { "curator -3.0k tok" };
        a.handle_agent(context_event(label, 5000, "curator"));
        let rest = bar(&mut a, 200, t, 1300);
        assert!(text(&rest).contains(label));
        let hits = a.status_hits.clone();
        // At 300 ms the eased 3.0k count already rounds to 3.0k; sample while it is still changing.
        let live = bar(&mut a, 200, t, 150);
        let live_text = text(&live);
        assert!(live_text.contains(if positive { "curator +" } else { "curator -" }));
        assert!(!live_text.contains(label), "the figure counts while moving");
        assert_eq!(a.status_hits, hits);
        assert_ne!(live, rest);
        let first = label_col(&rest, label);
        for x in 0..200usize {
            if !(first..first + label.chars().count()).contains(&x) {
                assert_eq!(live.content[x], rest.content[x], "outside label: {x}");
            }
        }
        a.fx.clock = Some(t + Duration::from_millis(400));
        a.handle_agent(context_event("curator idle +2k tok", 5000, "curator"));
        assert_eq!(a.fx.curator_delta.elapsed(t + Duration::from_millis(400)), Some(0));
        assert!(a.fx.curator_delta.positive);
        a.opts.animations = false;
        assert!(text(&bar(&mut a, 200, t, 500)).contains("curator idle +2k tok"));
        a.opts.animations = true;
        assert!(!text(&bar(&mut a, 19, t, 600)).contains("curator"));
        assert!(!a.fx.curator_delta.alive(t + Duration::from_millis(1600)));
    }
}

#[test]
fn unsigned_replay_rewind_and_policy_switch_do_not_start_a_wave() {
    let (mut a, _) = app(Theme::dark());
    a.handle_agent(context_event("curator -2k tok", 5000, "curator"));
    assert!(a.fx.curator_delta.at.is_none());
    a.busy = true;
    for (label, history, policy) in
        [("curator", 5000, "curator"), ("plain +2k tok", 5000, "plain"), ("curator", 4000, "curator")]
    {
        a.handle_agent(context_event(label, history, policy));
        assert!(a.fx.curator_delta.at.is_none());
    }
    a.clear_transcript();
    assert!(a.fx.curator_delta.at.is_none());
}

#[test]
fn signed_wave_uses_opposite_inks() {
    let theme = Theme::dark();
    let mut a = Buffer::empty(Rect::new(0, 0, 20, 1));
    for c in &mut a.content {
        c.fg = theme.accent2;
    }
    let mut b = a.clone();
    curator(&mut a, &theme, 0, 2..18, 300, true);
    curator(&mut b, &theme, 0, 2..18, 300, false);
    assert_ne!(a, b);
    assert_eq!(a.content[..2], b.content[..2]);
    assert_eq!(a.content[18..], b.content[18..]);
}

#[test]
fn signed_bar_direction_count_and_geometry_survive_quiet_and_opt_out() {
    for quiet in [false, true] {
        let mut bars = Vec::new();
        for positive in [true, false] {
            let (mut a, t) = app(Theme::dark());
            if quiet {
                a.opts.detail = nexo_core::config::Detail::Quiet;
            }
            a.handle_agent(context_event("curator", 5000, "curator"));
            a.busy = true;
            let label = if positive { "curator +987 tok" } else { "curator -987 tok" };
            a.handle_agent(context_event(label, 5000, "curator"));
            let final_bar = bar(&mut a, 120, t, 1300);
            let start = label_col(&final_bar, label);
            let hits = a.status_hits.clone();
            let early = bar(&mut a, 120, t, 200);
            let early_text = text(&early);
            assert_eq!(a.status_hits, hits);
            let len = label.chars().count();
            assert_eq!(&early.content[..start], &final_bar.content[..start]);
            assert_eq!(&early.content[start + len..], &final_bar.content[start + len..]);
            assert!(early_text.contains(if positive { "curator +" } else { "curator -" }));
            assert!(!early_text.contains(label));
            bars.push((early, start, len));
            a.opts.animations = false;
            assert!(text(&bar(&mut a, 120, t, 200)).contains(label));
            a.opts.animations = true;
            a.theme = Theme::mono();
            assert!(text(&bar(&mut a, 120, t, 200)).contains(label));
        }
        let (plus, start, len) = &bars[0];
        let (minus, _, _) = &bars[1];
        let bold = |b: &Buffer| {
            (*start..*start + *len)
                .filter(|&x| b.content[x].modifier.contains(ratatui::style::Modifier::BOLD))
                .collect::<Vec<_>>()
        };
        assert_eq!(bold(plus).len(), 1, "positive crest has a bold head");
        assert_eq!(bold(minus).len(), 1, "negative crest has a bold head");
        assert_ne!(bold(plus), bold(minus), "direction is visible without colour");
    }
}

#[test]
fn older_cut_does_not_land_on_a_new_positive_delta() {
    let (mut a, t) = app(Theme::dark());
    cut(&mut a);
    a.busy = true;
    a.fx.clock = Some(t + Duration::from_millis(1000));
    a.handle_agent(context_event("curator +987 tok", 15_000, "curator"));
    let with_cut = bar(&mut a, 120, t, 1100);
    assert!(a.fx.cut_col.get().is_none());
    let old_cut = a.fx.cut.take();
    let without_cut = bar(&mut a, 120, t, 1100);
    a.fx.cut = old_cut;
    assert_eq!(with_cut, without_cut, "old landing must not tint the positive label");
    assert!(a.fx.curator_delta.visible);
}

#[test]
fn every_live_event_restarts_even_identical_and_non_curator_cannot_spoof() {
    let (mut a, t) = app(Theme::dark());
    a.handle_agent(context_event("curator", 5000, "curator"));
    a.busy = true;
    a.handle_agent(context_event("curator +987 tok", 5000, "curator"));
    a.fx.clock = Some(t + Duration::from_millis(1199));
    a.handle_agent(context_event("curator +987 tok", 5000, "curator"));
    assert_eq!(a.fx.curator_delta.elapsed(t + Duration::from_millis(1199)), Some(0));
    a.handle_agent(context_event("curator +987 tok", 5000, "plain"));
    a.handle_agent(context_event("curator +987 tok", 5000, "plain"));
    assert!(a.fx.curator_delta.at.is_none());
}

#[test]
fn billed_output_does_not_override_signed_context_and_unsigned_rewind_stays_quiet() {
    for positive in [false, true] {
        let (mut a, t) = app(Theme::dark());
        a.busy = true;
        a.handle_agent(context_event("curator", 5000, "curator"));
        let call = nexo_core::Usage { input: 100, output: 20_000, ..Default::default() };
        a.handle_agent(AgentEvent::Usage { turn: call, call, session: call, cost_usd: 0.0 });
        let label = if positive { "curator +987 tok" } else { "curator -987 tok" };
        a.handle_agent(context_event(label, 5100, "curator"));
        assert_eq!(a.fx.curator_delta.elapsed(t), Some(0));
        assert_eq!(a.fx.curator_delta.positive, positive);
        bar(&mut a, 120, t, 200);
        assert!(a.fx.curator_delta.visible);
        a.handle_agent(context_event("curator", 1000, "curator"));
        assert!(a.fx.curator_delta.at.is_none());
        bar(&mut a, 120, t, 200);
        assert!(!a.fx.curator_delta.visible);
    }
}

#[test]
fn signed_count_never_restarts_when_the_map_clock_outlives_it() {
    use nexo_core::{CellKind, CellState, ContextCell};
    for theme in [Theme::dark(), Theme::light(), Theme::mono()] {
        for positive in [false, true] {
            let (mut a, t) = app(theme.clone());
            a.busy = true;
            a.handle_agent(AgentEvent::Context {
                view_tokens: 14_000,
                history_tokens: 15_000,
                history_bytes: 60_000,
                view_bytes: 56_000,
                messages_in_view: 2,
                messages_total: 5,
                policy: "curator".into(),
                note: None,
                label: "curator".into(),
                detail: None,
            });
            let map = |state| AgentEvent::ContextMap {
                cells: vec![ContextCell { kind: CellKind::Request, tokens: 50, state, zone: 0 }],
                zones: vec![],
                calls: vec![],
                call_tokens: vec![],
            };
            a.handle_agent(map(CellState::Whole));
            let label = if positive { "curator +987 tok" } else { "curator -987 tok" };
            a.handle_agent(context_event(label, 15_000, "curator"));
            a.handle_agent(map(CellState::Dropped));
            assert!(a.fx.cut.is_some());
            a.cut_on_screen();
            for ms in [1199, 1200, 1250] {
                let b = bar(&mut a, 120, t, ms);
                assert!(text(&b).contains(label), "{ms}: {}", text(&b));
                assert_eq!(a.fx.curator_delta.visible, !a.theme.mono && ms < 1200);
            }
        }
    }
}
