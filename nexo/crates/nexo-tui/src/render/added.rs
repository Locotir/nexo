//! The line under a message of the person that says what nexo added to it: `+ nexo added · 1.2k tokens
//! (start note, 2 files, hook)`. Open, it shows each added text as it went to the model, long ones cut.

use nexo_core::agent::{AddedKind, AddedPart};
use ratatui::text::{Line, Span};

use super::blocks::fmt_k;
use super::wrap::{truncate, width, wrap_plain};
use crate::theme::Theme;

/// Lines of one added text shown before `… N more lines`.
pub const SHOWN_LINES: usize = 12;

/// The name of a kind, in the plural when `n` is more than one.
fn word(kind: AddedKind, n: usize) -> String {
    let (one, many) = match kind {
        AddedKind::Mention => ("mention", "mentions"),
        AddedKind::File => ("file", "files"),
        AddedKind::Jobs => ("jobs note", "jobs notes"),
        AddedKind::Hook => ("hook", "hooks"),
        AddedKind::Memory => ("shared memory", "shared memories"),
        AddedKind::StartNote => ("start note", "start notes"),
        AddedKind::Supervisor => ("supervisor note", "supervisor notes"),
    };
    if n == 1 {
        one.to_string()
    } else {
        format!("{n} {many}")
    }
}

/// `start note, 2 files, hook`: each kind once, in the order it came.
pub fn what(parts: &[AddedPart]) -> String {
    let mut kinds: Vec<(AddedKind, usize)> = Vec::new();
    for p in parts {
        match kinds.iter_mut().find(|(k, _)| *k == p.kind) {
            Some((_, n)) => *n += 1,
            None => kinds.push((p.kind, 1)),
        }
    }
    kinds.into_iter().map(|(k, n)| word(k, n)).collect::<Vec<_>>().join(", ")
}

/// The head line: `+ nexo added · 340 tokens (start note, hook)`; `-` when open. Cut to `w` columns.
pub fn head(parts: &[AddedPart], folded: bool, w: usize) -> String {
    let tokens: u64 = parts.iter().map(AddedPart::tokens).sum();
    let only = if parts.iter().all(|p| p.request_only) { " · this request only" } else { "" };
    let text =
        format!("{} nexo added · {} tokens ({}){only}", if folded { "+" } else { "-" }, fmt_k(tokens), what(parts));
    truncate(&text, w)
}

/// The block at `w` columns.
pub fn lines(parts: &[AddedPart], folded: bool, w: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(Span::styled(head(parts, folded, w), theme.dim()))];
    if folded {
        return out;
    }
    let guide = [Span::styled("│ ", theme.dim2())];
    let inner = w.saturating_sub(2).max(1);
    for p in parts {
        let mut label = format!("{} · {}", p.label, word(p.kind, 1));
        label.push_str(&format!(" · {} tokens", fmt_k(p.tokens())));
        if p.request_only {
            label.push_str(" · this request only");
        }
        out.push(Line::from(vec![
            Span::styled("│ ", theme.dim2()),
            Span::styled(truncate(&label, inner), theme.style(theme.accent)),
        ]));
        let all: Vec<&str> = p.text.lines().collect();
        let shown = all[..all.len().min(SHOWN_LINES)].join("\n");
        out.extend(wrap_plain(&shown, w, theme.dim(), &guide));
        if all.len() > SHOWN_LINES {
            let more = format!("… {} more lines", all.len() - SHOWN_LINES);
            debug_assert!(width(&more) <= w || w < 16);
            out.push(Line::from(vec![Span::styled("│ ", theme.dim2()), Span::styled(more, theme.dim2())]));
        }
    }
    if let Some(first) = out.last_mut().and_then(|l| l.spans.first_mut()) {
        if first.content == "│ " {
            first.content = "└ ".into();
        }
    }
    out
}
