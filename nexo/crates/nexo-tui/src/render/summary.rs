//! The line that closes a finished turn: elapsed, steps, tokens in and out, cache hit, cost
//! and the tool calls by kind. Every number has a fixed width so the lines of successive turns
//! line up; narrow windows drop the least useful pieces first (calls, cache, steps).

use nexo_core::stream::Usage;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::blocks::{fmt_k, fmt_ms};
use super::wrap::width;
use crate::theme::Theme;

/// What a turn did, counted as its events arrive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnTally {
    /// Model calls made.
    pub steps: u32,
    /// Tool calls by kind word, in the order first seen.
    pub calls: Vec<(&'static str, u32)>,
}

impl TurnTally {
    pub fn count_call(&mut self, tool: &str) {
        let k = kind_word(tool);
        match self.calls.iter_mut().find(|(w, _)| *w == k) {
            Some((_, n)) => *n += 1,
            None => self.calls.push((k, 1)),
        }
    }
}

/// The short word a tool is counted under.
pub fn kind_word(tool: &str) -> &'static str {
    match tool {
        "read" | "web_fetch" => "read",
        "write" | "edit" => "edit",
        "bash" => "run",
        "grep" | "glob" | "ls" | "web_search" => "find",
        "task" => "agent",
        _ => "other",
    }
}

/// `3 read 2 edit 1 run`.
pub fn calls_text(calls: &[(&'static str, u32)]) -> String {
    calls.iter().map(|(k, n)| format!("{n} {k}")).collect::<Vec<_>>().join(" ")
}

/// Share of the input the cache served, in percent (None without input).
pub fn cache_pct(u: &Usage) -> Option<u64> {
    let t = u.total_input();
    (t > 0).then(|| ((u.cache_read as f64 / t as f64) * 100.0).round() as u64)
}

/// The pieces of the line as `(text, style)`, in display order, widest form.
fn pieces(theme: &Theme, u: &Usage, cost: f64, ms: u64, t: &TurnTally) -> Vec<(String, Style)> {
    let dim = theme.dim();
    let mut out = vec![(format!("{:>6}", fmt_ms(ms)), dim)];
    out.push((format!("{:>2} {}", t.steps.min(99), if t.steps == 1 { "step " } else { "steps" }), dim));
    out.push((format!("↑{:>5}", fmt_k(u.total_input())), theme.style(theme.info)));
    out.push((format!("↓{:>5}", fmt_k(u.output)), theme.style(theme.fg)));
    let (txt, col) = match cache_pct(u) {
        Some(p) => {
            let c = if p >= 50 {
                theme.success
            } else if p >= 20 {
                theme.warn
            } else {
                theme.dim
            };
            (format!("hit{:>3}%", p.min(100)), c)
        }
        None => ("      ".to_string(), theme.dim),
    };
    out.push((txt, theme.style(col)));
    let cost_txt = if cost > 0.0 { format!("${:<6.4}", cost.min(99.0)) } else { " ".repeat(7) };
    out.push((cost_txt, theme.style(theme.success)));
    if !t.calls.is_empty() {
        out.push((calls_text(&t.calls), theme.style(theme.accent2)));
    }
    out
}

/// Indices into `pieces` that stay at each tier, from everything to the bare minimum.
const TIERS: [&[usize]; 4] = [&[0, 1, 2, 3, 4, 5, 6], &[0, 1, 2, 3, 4, 5], &[0, 2, 3, 5], &[0, 2, 3]];

/// The summary line for a width of `w` cells.
pub fn summary_line(theme: &Theme, u: &Usage, cost: f64, ms: u64, t: &TurnTally, w: usize) -> Line<'static> {
    let all = pieces(theme, u, cost, ms, t);
    let gap = 2;
    let mut chosen: Vec<usize> = Vec::new();
    for tier in TIERS {
        let idx: Vec<usize> = tier.iter().copied().filter(|i| *i < all.len()).collect();
        let need = 2 + idx.iter().map(|i| width(&all[*i].0)).sum::<usize>() + gap * idx.len().saturating_sub(1);
        chosen = idx;
        if need <= w {
            break;
        }
    }
    let mut spans = vec![Span::raw("  ")];
    for (n, i) in chosen.iter().enumerate() {
        if n > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(all[*i].0.clone(), all[*i].1));
    }
    Line::from(spans)
}

/// The line as plain text (tests, captures).
pub fn plain(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage() -> Usage {
        Usage { input: 2000, output: 1200, cache_read: 10_000, cache_write: 0, reasoning: 0 }
    }

    fn tally() -> TurnTally {
        let mut t = TurnTally { steps: 4, ..Default::default() };
        for n in ["read", "read", "read", "edit", "edit", "bash"] {
            t.count_call(n);
        }
        t
    }

    #[test]
    fn calls_are_counted_by_kind() {
        assert_eq!(calls_text(&tally().calls), "3 read 2 edit 1 run");
    }

    #[test]
    fn cache_share_rounds() {
        assert_eq!(cache_pct(&usage()), Some(83));
        assert_eq!(cache_pct(&Usage::default()), None);
    }

    #[test]
    fn wide_line_has_everything() {
        let l = summary_line(&Theme::dark(), &usage(), 0.0123, 12_300, &tally(), 120);
        let s = plain(&l);
        assert!(s.contains("12.3s"), "{s}");
        assert!(s.contains(" 4 steps"), "{s}");
        assert!(s.contains("↑12.0k"), "{s}");
        assert!(s.contains("↓ 1.2k"), "{s}");
        assert!(s.contains("hit 83%"), "{s}");
        assert!(s.contains("$0.0123"), "{s}");
        assert!(s.ends_with("3 read 2 edit 1 run"), "{s}");
    }

    #[test]
    fn narrow_line_collapses_and_fits() {
        for w in [40usize, 50, 60, 80] {
            let s = plain(&summary_line(&Theme::dark(), &usage(), 0.0123, 12_300, &tally(), w));
            assert!(width(&s) <= w, "{w}: {s:?}");
            assert!(s.contains("↑"), "{w}: {s}");
        }
        let s = plain(&summary_line(&Theme::dark(), &usage(), 0.0123, 12_300, &tally(), 40));
        assert!(!s.contains("read"), "{s}");
    }

    #[test]
    fn width_does_not_jump_between_turns() {
        let a = plain(&summary_line(&Theme::dark(), &usage(), 0.0123, 900, &TurnTally::default(), 120));
        let u2 = Usage { input: 150_000, output: 90, cache_read: 0, ..Default::default() };
        let b = plain(&summary_line(&Theme::dark(), &u2, 1.5, 125_000, &TurnTally::default(), 120));
        assert_eq!(width(&a), width(&b), "{a:?} vs {b:?}");
    }
}
