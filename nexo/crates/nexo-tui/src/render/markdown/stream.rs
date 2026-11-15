//! A reply that keeps growing is drawn on every tick, and parsing and highlighting all of it each time
//! costs more the longer it gets. The lines of its closed part are kept here and only the open tail is
//! rendered again.
//!
//! The closed part ends at a cut: the start of a line that follows a blank line outside any code fence and
//! that begins like a new block at column 0 (a letter, `#`, a fence, `**`, `|`). Nothing after such a line can
//! change how the text before it reads: a list or a quote has ended, a paragraph cannot take a setext underline
//! past a blank line, a table is over. Whatever depends on the whole text (link reference definitions) or
//! leaks across blocks (HTML, tabs and escapes the cleaning rewrites, fences the scan cannot follow) makes the
//! text render from its first byte instead, or stops the cuts, so the output is the one a full render gives.

use std::cell::RefCell;

use ratatui::text::Line;

use crate::regions::Meta;
use crate::theme::Theme;

use super::render_whole;

/// Texts shorter than this are not worth keeping lines for.
pub(super) const MIN_LEN: usize = 4096;

/// Entries kept: a reply being streamed and a few older ones that a resize or a scroll draws again.
const KEEP: usize = 4;

/// The rendering of the first `text.len()` bytes of a reply.
struct Entry {
    text: String,
    width: usize,
    interpreted: bool,
    theme: Theme,
    lines: Vec<Line<'static>>,
    meta: Meta,
}

thread_local! {
    static KEPT: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
}

/// `md` rendered as [`render_whole`] renders it, from the kept lines of its closed part when it has one.
pub(super) fn render(
    md: &str,
    width: usize,
    theme: &Theme,
    interpreted: bool,
    min_len: usize,
) -> (Vec<Line<'static>>, Meta) {
    if md.len() < min_len {
        return render_whole(md, width, theme, interpreted);
    }
    let width = width.max(8);
    // taken out while it is used: drawing a code block that is itself Markdown comes back through here
    let mut base = KEPT.with(|k| {
        let mut k = k.borrow_mut();
        let at = k
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.width == width
                    && e.interpreted == interpreted
                    && md.as_bytes().starts_with(e.text.as_bytes())
                    && e.theme == *theme
            })
            .max_by_key(|(_, e)| e.text.len())
            .map(|(i, _)| i)?;
        Some(k.remove(at))
    });
    let start = base.as_ref().map_or(0, |e| e.text.len());
    let Some(cut) = closed_part(&md[start..]) else {
        keep(base);
        return render_whole(md, width, theme, interpreted);
    };
    if cut > 0 {
        let (lines, meta) = render_whole(&md[start..start + cut], width, theme, interpreted);
        let text = md[..start + cut].to_string();
        base = Some(match base {
            Some(mut e) => {
                join(&mut e.lines, &mut e.meta, lines, meta);
                Entry { text, ..e }
            }
            None => Entry { text, width, interpreted, theme: theme.clone(), lines, meta },
        });
    }
    let Some(e) = base else {
        return render_whole(md, width, theme, interpreted);
    };
    let from = e.text.len();
    let tail = &md[from..];
    let (mut lines, mut meta) = (e.lines.clone(), e.meta.clone());
    if !tail.is_empty() {
        let (tail_lines, tail_meta) = render_whole(tail, width, theme, interpreted);
        // a tail that says nothing although it has words in it is a case this does not know
        if tail_lines.is_empty() && !tail.trim().is_empty() {
            keep(Some(e));
            return render_whole(md, width, theme, interpreted);
        }
        join(&mut lines, &mut meta, tail_lines, tail_meta);
    }
    keep(Some(e));
    (lines, meta)
}

fn keep(e: Option<Entry>) {
    if let Some(e) = e {
        KEPT.with(|k| {
            let mut k = k.borrow_mut();
            k.insert(0, e);
            k.truncate(KEEP);
        });
    }
}

/// `next` after `lines`, with the blank row a block start puts between two blocks.
fn join(lines: &mut Vec<Line<'static>>, meta: &mut Meta, next: Vec<Line<'static>>, next_meta: Meta) {
    if next.is_empty() {
        return;
    }
    if lines.last().is_some_and(|l| !l.spans.is_empty()) {
        lines.push(Line::default());
    }
    let at = lines.len();
    lines.extend(next);
    meta.append(next_meta, at);
}

/// How many bytes of `text` form its closed part (0: none, it is all open), or None when the text must be
/// rendered whole. `text` starts at a cut, or at the start of the reply, so it begins outside any block.
fn closed_part(text: &str) -> Option<usize> {
    if !matches!(crate::render::wrap::clean(text), std::borrow::Cow::Borrowed(_)) {
        return None;
    }
    let mut fence: Option<(char, usize)> = None;
    let mut cut = 0;
    let mut stopped = false;
    let mut blank_before = false;
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let body = line.trim_end_matches('\n');
        let trimmed = body.trim_start_matches(' ');
        let indent = body.len() - trimmed.len();
        if let Some((ch, n)) = fence {
            if indent <= 3 && closes(trimmed, ch, n) {
                fence = None;
            }
            continue;
        }
        if body.trim().is_empty() {
            blank_before = true;
            continue;
        }
        // a reference definition reaches back to every link before it
        if body.contains("]:") {
            return None;
        }
        // HTML text is held back by the renderer until the next block closes, so it can reach into it
        if trimmed.starts_with('<') {
            stopped = true;
        }
        if blank_before && !stopped && indent == 0 && starts_block(body) {
            cut = start;
        }
        blank_before = false;
        let opens = if indent <= 3 { opens(trimmed) } else { None };
        match opens {
            Some(f) => fence = Some(f),
            // a fence inside a list or a quote, an indented one, or three marks inside a line: not followed
            None if body.contains("```") || body.contains("~~~") => stopped = true,
            None => {}
        }
    }
    // an unclosed fence is not yet a block of its own, but it keeps the cut before it, which is already outside
    Some(cut)
}

/// A line that can only begin a block of its own: it is no list marker, quote, indented code or
/// continuation of what came before.
fn starts_block(line: &str) -> bool {
    let mut it = line.chars();
    match it.next() {
        Some(c) if c.is_alphabetic() => true,
        Some('#' | '`' | '~' | '|') => true,
        Some('*') => it.next() == Some('*'),
        _ => false,
    }
}

/// The mark and length of a code fence this line opens.
fn opens(trimmed: &str) -> Option<(char, usize)> {
    let ch = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let n = trimmed.chars().take_while(|c| *c == ch).count();
    if n < 3 {
        return None;
    }
    // an info string of a backtick fence holds no backtick: that is inline code
    if ch == '`' && trimmed[n..].contains('`') {
        return None;
    }
    Some((ch, n))
}

/// Whether this line closes a fence of `n` marks `ch`.
fn closes(trimmed: &str, ch: char, n: usize) -> bool {
    let marks = trimmed.chars().take_while(|c| *c == ch).count();
    marks >= n && trimmed[marks..].trim().is_empty()
}

#[cfg(test)]
mod tests;
