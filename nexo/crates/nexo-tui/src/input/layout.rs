//! The draft laid out in the box's rows, and the cells those rows stand for.

use super::*;

/// Whether `c` adds nothing to the cell before it: a combining mark, a joiner, a selector, a skin
/// tone (the box draws none of them, see `wrap::clean`).
pub(super) fn no_cell(c: char) -> bool {
    !c.is_control() && (unicode_width::UnicodeWidthChar::width(c) == Some(0) || crate::render::wrap::unsafe_char(c))
}

pub(super) fn regional(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

/// The end of the visible character that starts at `i`: the character, its marks, and for a flag both its letters.
pub(super) fn cluster_end(chars: &[char], i: usize) -> usize {
    let mut e = i + 1;
    if regional(chars[i]) && chars.get(e).is_some_and(|c| regional(*c)) {
        e += 1;
    }
    while chars.get(e).is_some_and(|c| no_cell(*c)) {
        e += 1;
    }
    e
}

/// The visible character (`start`, `end` in code points) that holds the code point `at`.
pub(super) fn cluster_of(chars: &[char], at: usize) -> (usize, usize) {
    let mut i = 0;
    loop {
        let e = cluster_end(chars, i);
        if at < e {
            return (i, e);
        }
        i = e;
    }
}

/// The visible character the position `col` falls inside of (not at an edge of).
pub(super) fn cluster_at(chars: &[char], col: usize) -> Option<(usize, usize)> {
    if col == 0 || col >= chars.len() {
        return None;
    }
    let (s, e) = cluster_of(chars, col);
    (s < col).then_some((s, e))
}

/// The draft laid out in rows of at most `width` cells (a line is cut where the next
/// character would not fit), with the cursor's row and column in that layout.
pub fn wrapped_rows(input: &Input, width: usize) -> (Vec<String>, (usize, usize)) {
    let (rows, cursor, _) = layout(input, width);
    (rows, cursor)
}

/// Where in the draft a cell of the box is: `(line, character)`, for a click or a drag.
pub fn cell_at(input: &Input, width: usize, vrow: usize, vcol: usize) -> (usize, usize) {
    let (rows, _, origins) = layout(input, width);
    let Some(((line, start), row)) =
        origins.get(vrow.min(rows.len().saturating_sub(1))).zip(rows.get(vrow.min(rows.len().saturating_sub(1))))
    else {
        return (0, 0);
    };
    // the cursor goes before the character under the cell; past the row's end, at its end
    let (mut used, mut n) = (0, 0);
    for c in row.chars() {
        // combining marks and joiners take no cell, as ratatui draws them
        let w = crate::render::wrap::width(c.encode_utf8(&mut [0; 4]));
        if vcol < used + w {
            break;
        }
        used += w;
        n += 1;
    }
    input.snap((*line, start + n), None)
}

/// The `(line, first character)` each row of the layout starts at.
pub fn row_origins(input: &Input, width: usize) -> Vec<(usize, usize)> {
    layout(input, width).2
}

/// The rows of the box, the cursor in them, and where each row starts in the draft.
pub(super) type Layout = (Vec<String>, (usize, usize), Vec<(usize, usize)>);

pub(super) fn layout(input: &Input, width: usize) -> Layout {
    let width = width.max(1);
    let mut origins: Vec<(usize, usize)> = Vec::new();
    let (crow, ccol) = input.editor.cursor();
    let mut rows = Vec::new();
    let mut cursor = (0, 0);
    for (li, line) in input.editor.lines().iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        // a zero-width character (a combining mark) adds no column: ratatui draws it in the cell before
        let cw = |c: char| crate::render::wrap::width(c.encode_utf8(&mut [0; 4]));
        let mut start = 0;
        loop {
            // the row takes what fits; at a word it breaks after the last space
            let (mut end, mut used) = (start, 0);
            while end < chars.len() && used + cw(chars[end]) <= width {
                used += cw(chars[end]);
                end += 1;
            }
            // a character wider than the box still takes a row of its own
            if end == start && start < chars.len() {
                end = start + 1;
                used = cw(chars[start]);
            }
            if end < chars.len() {
                if let Some(sp) = chars[start..end].iter().rposition(|c| *c == ' ') {
                    end = start + sp + 1;
                    used = chars[start..end].iter().map(|c| cw(*c)).sum();
                }
            }
            if li == crow && ccol >= start && (ccol < end || (end == chars.len() && ccol == end)) {
                let col: usize = chars[start..ccol].iter().map(|c| cw(*c)).sum();
                cursor = (rows.len(), col);
                // the cursor past a full last row opens a new one
                if end == chars.len() && ccol == end && used >= width {
                    origins.push((li, start));
                    rows.push(chars[start..end].iter().collect());
                    cursor = (rows.len(), 0);
                    origins.push((li, end));
                    rows.push(String::new());
                    break;
                }
            }
            origins.push((li, start));
            rows.push(chars[start..end].iter().collect());
            if end >= chars.len() {
                break;
            }
            start = end;
        }
    }
    (rows, cursor, origins)
}

/// The size of a long or multiline draft as the box shows it (`3 lines · 120 chars`);
/// None for a short one-liner.
pub fn counter(input: &Input) -> Option<String> {
    let lines = input.line_count();
    let chars = input.text().chars().count();
    if lines < 2 && chars < 200 {
        return None;
    }
    let l = if lines == 1 { "line" } else { "lines" };
    Some(format!("{lines} {l} · {chars} chars"))
}
