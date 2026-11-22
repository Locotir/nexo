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
