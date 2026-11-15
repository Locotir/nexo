//! The colours and text of a card: contrast-checked hues, the tone of a chip and a step cut to fit.

use nexo_agent::supervise::State;
use ratatui::style::Color;

use super::attention;
use crate::motion::fade;
use crate::render::wrap::{truncate, width};
use crate::theme::Theme;

/// The contrast ratio (WCAG) of two RGB colours; 21 for none that is not RGB (a mapped theme keeps its own colours).
fn contrast(a: Color, b: Color) -> f32 {
    let lum = |c: Color| match c {
        Color::Rgb(r, g, b) => {
            let f = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            Some(0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b))
        }
        _ => None,
    };
    match (lum(a), lum(b)) {
        (Some(x), Some(y)) => (x.max(y) + 0.05) / (x.min(y) + 0.05),
        _ => 21.0,
    }
}

/// `c` moved towards the text colour until it reads at 4.5:1 (AA) on a card, resting and at its most lit. A hue
/// that already reads is returned as it is; a mono theme has no hues.
pub(super) fn legible(theme: &Theme, c: Color) -> Color {
    if theme.mono {
        return c;
    }
    let sel = theme.selection().bg.unwrap_or(theme.border);
    let grounds = [theme.modal_bg, fade(theme, theme.modal_bg, sel, CARD_LIT_MAX)];
    (0..=10)
        .map(|n| crate::theme::mix(c, theme.fg, n as f32 / 10.0))
        .find(|x| grounds.iter().all(|g| contrast(*x, *g) >= 4.5))
        .unwrap_or(theme.fg)
}

/// How far a card's ground may go towards the selection colour: past this the dim text on it stops reading (4.5:1);
/// the accent border says "selected" on its own.
pub(super) const CARD_LIT_MAX: f32 = 0.15;

/// The colour a chip is drawn in: a finished session is softened, an attention chip breathes towards the dim text
/// colour by `wave` (0..1) of its depth.
pub(super) fn tone(theme: &Theme, state: State, base: Color, wave: f32) -> Color {
    if attention(state) {
        legible(theme, fade(theme, base, theme.dim, 0.45 * wave))
    } else if state == State::Done {
        fade(theme, base, theme.dim, 0.4)
    } else {
        base
    }
}

/// A step cut to `room` columns. A path at its end keeps its tail (the file is what tells steps apart), the verb in
/// front stays whole; anything else keeps its start.
pub(super) fn fit_step(step: &str, room: usize) -> String {
    if width(step) <= room {
        return step.to_string();
    }
    if let Some((head, path)) = step.rsplit_once(char::is_whitespace) {
        let left = room.saturating_sub(width(head) + 1);
        if path.contains('/') && left >= 6 {
            let mut tail = String::new();
            for g in unicode_segmentation::UnicodeSegmentation::graphemes(path, true).rev() {
                if width(&tail) + width(g) + 1 > left {
                    break;
                }
                tail.insert_str(0, g);
            }
            return format!("{head} …{tail}");
        }
    }
    truncate(step, room)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents_view::layout::{balanced, split_evenly};
    use crate::agents_view::{chip, chip_color, AgentsView, Layout, CARD_MAX_W, GAP};

    fn lum(c: Color) -> f64 {
        let Color::Rgb(r, g, b) = c else { return 0.0 };
        let f = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)
    }

    fn ratio(a: Color, b: Color) -> f64 {
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    const STATES: [State; 7] = [
        State::WaitingPermission,
        State::WaitingAnswer,
        State::Stalled,
        State::Looping,
        State::Working,
        State::Idle { min: 0 },
        State::Done,
    ];

    /// The coloured text of a card (chips, breathing and softened, tags, the supervisor's action, waits) reads at
    /// AA on the card's ground, resting and at its most lit, in every theme that has colours; the plain, dim and
    /// dimmer text keeps the contrast its theme promises (dark, nexo and light).
    #[test]
    fn card_text_reads_on_its_ground_in_every_theme() {
        let mut weak: Vec<String> = Vec::new();
        for name in crate::theme::NAMES {
            let t = Theme::by_name(name).unwrap();
            if t.mono {
                continue;
            }
            let selected = t.selection().bg.unwrap_or(t.border);
            let lit = crate::motion::fade(&t, t.modal_bg, selected, CARD_LIT_MAX);
            for ground in [t.modal_bg, lit] {
                let mut hues = vec![
                    ("tag nexo", legible(&t, t.accent)),
                    ("tag other", legible(&t, t.accent2)),
                    ("action", legible(&t, t.accent2)),
                    ("retry", legible(&t, t.warn)),
                ];
                for s in STATES {
                    let base = chip_color(&t, s);
                    for wave in [0.0, 1.0] {
                        hues.push((chip(s).1, tone(&t, s, base, wave)));
                    }
                }
                for (what, c) in hues {
                    // a finished session is softened on purpose, down to 3:1
                    let need = if what == "done" { 3.0 } else { 4.5 };
                    let r = ratio(c, ground);
                    if r < need {
                        weak.push(format!("{name}: {what} is {r:.2}:1 on {ground:?}, wants {need}"));
                    }
                }
            }
            if matches!(*name, "dark" | "nexo" | "light") {
                for (what, c, floor) in [("name", t.fg, 4.5), ("dim", t.dim, 4.0), ("dim2", t.dim2, 3.0)] {
                    for ground in [t.modal_bg, lit] {
                        if ratio(c, ground) < floor {
                            weak.push(format!("{name}: {what} is {:.2}:1 on {ground:?}", ratio(c, ground)));
                        }
                    }
                }
            }
        }
        assert!(weak.is_empty(), "{}", weak.join("\n"));
    }

    #[test]
    fn a_hue_that_reads_is_left_alone_and_one_that_does_not_moves_to_the_text_colour() {
        let t = Theme::dark();
        assert_eq!(legible(&t, t.success), t.success);
        let faint = Color::Rgb(0x30, 0x30, 0x30);
        let fixed = legible(&t, faint);
        assert!(ratio(fixed, t.modal_bg) >= 4.5, "{fixed:?}");
        assert_eq!(legible(&Theme::mono(), faint), faint);
    }

    #[test]
    fn workflow_label_uses_only_the_observed_tool_step() {
        assert_eq!(AgentsView::workflow_step(Some("workflow  review")), Some("workflow  review"));
        assert_eq!(AgentsView::workflow_step(Some("Workflow  build")), Some("Workflow  build"));
        assert_eq!(AgentsView::workflow_step(Some("read  workflow.toml")), None);
        assert_eq!(AgentsView::workflow_step(Some("workflowing")), None);
        assert_eq!(AgentsView::workflow_step(None), None);
    }

    #[test]
    fn quiet_times_are_one_short_unit() {
        let got: Vec<String> =
            [0, 2, 59, 60, 95, 3599, 3600, 7300, 86399, 86400, 400_000].map(crate::agents_view::age).into();
        assert_eq!(got, ["0s", "2s", "59s", "1m", "1m", "59m", "1h", "2h", "23h", "1d", "4d"]);
    }

    #[test]
    fn every_state_has_its_own_glyph_so_colour_is_never_the_only_cue() {
        let glyphs: std::collections::HashSet<_> = STATES.iter().map(|s| chip(*s).0).collect();
        assert_eq!(glyphs.len(), STATES.len());
        let labels: std::collections::HashSet<_> = STATES.iter().map(|s| chip(*s).1).collect();
        assert_eq!(labels.len(), STATES.len());
    }

    #[test]
    fn a_step_keeps_the_end_of_its_path_and_cuts_other_text_at_the_end() {
        assert_eq!(fit_step("Edit  src/parser.rs", 40), "Edit  src/parser.rs");
        let cut = fit_step("Edit  crates/nexo-tui/src/agents_view/draw.rs", 24);
        assert!(cut.starts_with("Edit  …") && cut.ends_with("view/draw.rs") || cut.ends_with("draw.rs"), "{cut}");
        assert!(width(&cut) <= 24, "{cut}");
        let cut = fit_step("Bash  cargo test -p parser --lib", 16);
        assert!(cut.starts_with("Bash  cargo") && cut.ends_with('…') && width(&cut) <= 16, "{cut}");
        // CJK path: never wider than the room
        let cut = fit_step("Read  /项目/源代码/解析器/语法分析.rs", 20);
        assert!(width(&cut) <= 20, "{cut}");
    }

    #[test]
    fn rows_balance_and_cards_split_a_strip_to_the_cell() {
        assert_eq!(balanced(4, 3), 2);
        assert_eq!(balanced(5, 3), 3);
        assert_eq!(balanced(7, 4), 4);
        assert_eq!(balanced(5, 4), 3);
        assert_eq!(balanced(1, 4), 1);
        assert_eq!(balanced(0, 4), 1);
        assert_eq!(split_evenly(113, 3), vec![38, 38, 37]);
        let l = Layout::Cards { cols: 3, card_w: 37 };
        for n in 1..=4 {
            let w = AgentsView::row_widths(l, 116, 37, n);
            let gaps = GAP * (n as u16 - 1);
            assert_eq!(w.iter().sum::<u16>() + gaps, 116.min(n as u16 * CARD_MAX_W + gaps), "{n}: {w:?}");
        }
    }
}
