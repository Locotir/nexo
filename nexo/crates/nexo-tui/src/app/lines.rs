//! Helpers that shape a block's lines: steps, indent, folds, the call mark at the head of a card.

use super::*;

/// The steps of a turn: a tool call with its result, the model's thinking, a group of calls.
pub(super) fn is_step(b: &Block) -> bool {
    matches!(b, Block::Tool { .. } | Block::Reasoning { .. } | Block::Group { .. })
}

/// Columns before a block's lines: a margin of one for every block (where the selection's
/// bar is drawn), and two more for the steps of a turn, so a call and its result sit under
/// the reply's text. Narrow terminals keep every column for the text.
/// The word at the right of a tool card's head: where its output stands for the last model call (in the
/// cached part of the request, paid in full, trimmed, set aside, recalled). A glyph and a word, coloured like the bar,
/// and only when it fits beside the head. Set aside is the glyph alone: it is where every card of an answered request
/// stands, and the same word on each of them would only be noise.
pub(super) fn mark_head(head: &mut Line<'static>, m: CallMark, cost: Option<f64>, w: usize, theme: &Theme) {
    use crate::render::wrap::width;
    let (word, style) = match m {
        CallMark::Cached => ("● cached", theme.style(theme.success)),
        CallMark::Sent => ("● full price", theme.style(theme.accent)),
        CallMark::Cut => ("◐ trimmed", theme.style(theme.warn)),
        CallMark::Out => ("○", theme.dim2()),
        CallMark::Back => ("↺ recalled", theme.style(theme.accent2)),
    };
    // the mark wins over the end of a long head: the command is cut with an ellipsis, never below this
    const HEAD_KEEP: usize = 16;
    let used: usize = head.spans.iter().map(|s| width(&s.content)).sum();
    // the price of this output in the last request goes after the word when it fits, else the word alone
    let priced = cost.map(|c| format!("{word} {c:.5} $"));
    let Some((text, limit)) = priced.into_iter().chain([word.to_string()]).find_map(|t| {
        let limit = w.saturating_sub(width(&t) + 3);
        (used <= limit || limit >= HEAD_KEEP).then_some((t, limit))
    }) else {
        return;
    };
    if used > limit {
        cut_line(head, limit);
    }
    let used: usize = head.spans.iter().map(|s| width(&s.content)).sum();
    head.spans.push(Span::raw(" ".repeat(w.saturating_sub(used + width(&text)))));
    head.spans.push(Span::styled(text, style));
}

/// Cuts `line` to at most `w` cells, ending in an ellipsis (or before a trailing fact that does not fit); the spans keep their styles.
pub(super) fn cut_line(line: &mut Line<'static>, w: usize) {
    use crate::render::wrap::{truncate, width};
    let mut acc = 0;
    for i in 0..line.spans.len() {
        let sw = width(&line.spans[i].content);
        let rem = w.saturating_sub(acc);
        if sw < rem {
            acc += sw;
            continue;
        }
        // this span reaches the edge. A fact after the text (` · 40 lines`, `  44ms`) is shown whole
        // or not at all: cut, it would end in `· …` or a figure with its tail missing
        if i > 0 && line.spans[i].content.starts_with(' ') {
            line.spans.truncate(i);
            return;
        }
        // any other span ends in the ellipsis and the rest goes
        let c = line.spans[i].content.to_string();
        line.spans[i].content = if sw > rem {
            truncate(&c, rem)
        } else if rem > 1 {
            truncate(&c, rem - 1)
        } else {
            "…".to_string()
        }
        .into();
        line.spans.truncate(i + 1);
        return;
    }
}

pub(super) fn indent_of(b: &Block, width: usize) -> usize {
    if width < 24 || matches!(b, Block::Welcome { .. }) {
        0
    } else if is_step(b) {
        3
    } else {
        1
    }
}

/// Blocks with no fold of their own that still fold to their first line.
pub(super) fn folds_alone(b: &Block) -> bool {
    matches!(
        b,
        Block::User { .. }
            | Block::Assistant { streaming: false, .. }
            | Block::Shell { .. }
            | Block::Notice { .. }
            | Block::Problem { .. }
    )
}
