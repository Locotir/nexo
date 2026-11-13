//! The moving parts of a running card (sub-agent face, shimmer, tail) and its clocks.

use super::*;

/// The sub-agent's face: eyes open, shut for a moment every two seconds while it works.
pub(super) fn robot(running: bool, beat: usize) -> &'static str {
    if running && beat % 24 >= 22 {
        "[-_-]"
    } else {
        "[o_o]"
    }
}

/// `word` with a light running across it, frame by frame.
pub(super) fn shimmer(word: &str, frame: usize, theme: &Theme) -> Vec<Span<'static>> {
    let n = word.chars().count() as f32;
    let pos = (frame as f32 * 0.36) % (n + 8.0) - 3.0;
    word.chars()
        .enumerate()
        .map(|(i, c)| {
            let near = (1.0 - ((i as f32 - pos).abs() / 2.5)).max(0.0);
            Span::styled(
                c.to_string(),
                Style::default().fg(mix(theme.fg, theme.accent, near)).add_modifier(Modifier::BOLD),
            )
        })
        .collect()
}

pub(super) fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Right-aligns a dim `label` on the head `line` of a block `w` wide; dropped when it would touch the text.
/// True when the stamp was added (two spans at the end: padding, then the label).
pub(super) fn stamp_right(line: &mut Line<'static>, w: usize, label: &str, theme: &Theme) -> bool {
    let used: usize = line.spans.iter().map(|s| width(&s.content)).sum();
    let need = width(label) + 2;
    if used + need <= w {
        line.spans.push(Span::raw(" ".repeat(w - used - width(label))));
        line.spans.push(Span::styled(label.to_string(), theme.dim2()));
        return true;
    }
    false
}

pub fn fmt_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        fmt_secs(ms / 1000)
    }
}

/// A running clock in whole units: `42s`, `3m05s`, `1h02m`; it grows into minutes and hours by
/// itself, so a long turn never reads as thousands of seconds.
pub fn fmt_secs(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}
