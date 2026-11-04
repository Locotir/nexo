//! The editor under the transcript: a draft, its history, and the keys it owns.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use tui_textarea::{CursorMove, TextArea};

pub const MAX_HISTORY: usize = 500;

/// The two modes of the vim keys (`/vim`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimMode {
    Normal,
    Insert,
}

pub struct Input {
    /// The vim keys, when on: the mode the editor is in.
    pub vim: Option<VimMode>,
    /// A vim operator waiting for its motion (`d` of `dd`, `c` of `cc`, `g` of `gg`).
    pending: Option<char>,
    pub editor: TextArea<'static>,
    history: Vec<String>,
    /// Where in the history ↑ is (None: at the draft).
    hist: Option<usize>,
    stash: String,
    pub placeholder: String,
    /// Where the history is kept between runs (one JSON string a line); None: not kept.
    history_file: Option<std::path::PathBuf>,
    /// Long pastes, shown in the draft as `[Pasted text #n +L lines]` and sent whole.
    pastes: Vec<(String, String)>,
    /// The text width of the box at the last draw (0: not drawn yet); ↑ ↓ Home End follow its rows.
    pub wrap_width: usize,
    /// Where the last press in the box was, and whether the pointer then reached another cell (or the click was a double / triple one): only then is there a selection to copy.
    press: Option<(usize, usize)>,
    dragged: bool,
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
    (*line, start + n)
}

/// The `(line, first character)` each row of the layout starts at.
pub fn row_origins(input: &Input, width: usize) -> Vec<(usize, usize)> {
    layout(input, width).2
}

/// The rows of the box, the cursor in them, and where each row starts in the draft.
type Layout = (Vec<String>, (usize, usize), Vec<(usize, usize)>);

fn layout(input: &Input, width: usize) -> Layout {
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

/// A draft set aside while the commands window has the box.
pub struct Draft {
    editor: TextArea<'static>,
    pastes: Vec<(String, String)>,
}
/// A paste longer than this many lines (or [`PASTE_CHARS`] characters) is folded in the draft.
pub const PASTE_LINES: usize = 3;
pub const PASTE_CHARS: usize = 800;

/// What a key did to the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// The draft changed (or the cursor moved).
    Changed,
    /// Enter on the draft.
    Submit,
    /// Nothing the editor handles.
    Ignored,
}

impl Input {
    pub fn new(placeholder: &str) -> Input {
        let mut i = Input {
            vim: None,
            pending: None,
            editor: TextArea::default(),
            history: Vec::new(),
            hist: None,
            stash: String::new(),
            placeholder: placeholder.to_string(),
            history_file: None,
            pastes: Vec::new(),
            wrap_width: 0,
            press: None,
            dragged: false,
        };
        i.style();
        i
    }

    fn style(&mut self) {
        self.editor.set_cursor_line_style(Style::default());
        self.editor.set_placeholder_text(self.placeholder.clone());
    }

    pub fn set_placeholder_style(&mut self, style: Style) {
        self.editor.set_placeholder_style(style);
    }

    pub fn text(&self) -> String {
        self.editor.lines().join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.editor.lines().iter().all(|l| l.trim().is_empty())
    }

    pub fn line_count(&self) -> usize {
        self.editor.lines().len()
    }

    /// Byte offset of the cursor in `text()`.
    pub fn cursor_offset(&self) -> usize {
        let (row, col) = self.editor.cursor();
        let lines = self.editor.lines();
        let mut off = 0;
        for (i, l) in lines.iter().enumerate() {
            if i == row {
                off += l.chars().take(col).map(char::len_utf8).sum::<usize>();
                break;
            }
            off += l.len() + 1;
        }
        off
    }

    pub fn set_text(&mut self, text: &str) {
        // what the box draws goes to the terminal as is: escapes and tabs never reach it
        let lines: Vec<String> = crate::render::wrap::clean(text).split('\n').map(String::from).collect();
        self.editor = TextArea::new(lines);
        self.style();
        self.editor.move_cursor(CursorMove::Bottom);
        self.editor.move_cursor(CursorMove::End);
    }

    pub fn clear(&mut self) {
        self.set_text("");
        self.hist = None;
        self.pastes.clear();
    }

    /// Takes the draft out whole (text, cursor, undo, folded pastes) and leaves the box empty.
    pub fn take_draft(&mut self) -> Draft {
        let editor = std::mem::take(&mut self.editor);
        let pastes = std::mem::take(&mut self.pastes);
        self.hist = None;
        self.style();
        Draft { editor, pastes }
    }

    /// Puts a draft from [`Input::take_draft`] back, as it was.
    pub fn put_draft(&mut self, d: Draft) {
        self.editor = d.editor;
        self.pastes = d.pastes;
        self.hist = None;
        self.style();
    }

    /// Replaces the bytes `start..cursor` with `with`.
    pub fn replace_range(&mut self, start: usize, with: &str) {
        let text = self.text();
        let cur = self.cursor_offset().min(text.len());
        let start = start.min(cur);
        let new = format!("{}{}{}", &text[..start], with, &text[cur..]);
        let keep_tail = text.len() - cur;
        self.set_text(&new);
        // put the cursor right after the insertion
        let target = new.len() - keep_tail;
        self.editor.move_cursor(CursorMove::Top);
        self.editor.move_cursor(CursorMove::Head);
        let mut off = 0;
        for (row, l) in new.split('\n').enumerate() {
            if off + l.len() >= target {
                let col = new[off..target].chars().count();
                self.editor.move_cursor(CursorMove::Jump(row as u16, col as u16));
                break;
            }
            off += l.len() + 1;
        }
    }

    pub fn push_history(&mut self, text: &str) {
        if self.history.last().map(String::as_str) != Some(text) {
            self.history.push(text.to_string());
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
            }
            if let Some(f) = &self.history_file {
                use std::io::Write;
                let line = serde_json::to_string(text).unwrap_or_default();
                if let Ok(mut out) = std::fs::OpenOptions::new().create(true).append(true).open(f) {
                    let _ = writeln!(out, "{line}");
                }
            }
        }
        self.hist = None;
    }

    /// Keeps the history in `file` from now on, and starts from what it holds (the last
    /// `MAX_HISTORY` entries; the file is trimmed when it grows to twice that).
    pub fn persist_history(&mut self, file: std::path::PathBuf) {
        if let Some(d) = file.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let all: Vec<String> = text.lines().filter_map(|l| serde_json::from_str::<String>(l).ok()).collect();
        let from = all.len().saturating_sub(MAX_HISTORY);
        self.history = all[from..].to_vec();
        if all.len() > 2 * MAX_HISTORY {
            let kept: Vec<String> = self.history.iter().map(|h| serde_json::to_string(h).unwrap_or_default()).collect();
            let _ = std::fs::write(&file, kept.join("\n") + "\n");
        }
        self.history_file = Some(file);
    }

    /// Every request sent, oldest first.
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Pasted text goes in as typed; a long one is folded to `[Pasted text #n +L lines]`,
    /// which [`Input::full_text`] puts back.
    pub fn insert_paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        // escapes and other control characters out, tabs as spaces: the box draws the text as is
        let text = crate::render::wrap::clean(&text).into_owned();
        let lines = text.trim_end_matches('\n').lines().count();
        if lines <= PASTE_LINES && text.chars().count() <= PASTE_CHARS {
            self.editor.insert_str(text);
            return;
        }
        let n = self.pastes.len() + 1;
        let label = if lines > 1 {
            format!("[Pasted text #{n} +{lines} lines]")
        } else {
            format!("[Pasted text #{n} {} chars]", text.chars().count())
        };
        self.editor.insert_str(&label);
        self.pastes.push((label, text));
    }

    /// The draft with every folded paste put back: what is sent.
    pub fn full_text(&self) -> String {
        let mut text = self.text();
        for (label, pasted) in &self.pastes {
            text = text.replacen(label.as_str(), pasted, 1);
        }
        text
    }

    /// Esc with the vim keys in insert mode: back to normal mode (true when it did).
    pub fn vim_escape(&mut self) -> bool {
        if self.vim == Some(VimMode::Insert) {
            self.vim = Some(VimMode::Normal);
            self.editor.move_cursor(CursorMove::Back);
            return true;
        }
        self.pending = None;
        false
    }

    /// A key in vim's normal mode: motions, edits, the ways into insert mode.
    fn vim_normal(&mut self, key: KeyEvent) -> Edit {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let KeyCode::Char(c) = key.code else {
            return match key.code {
                KeyCode::Enter => Edit::Submit,
                KeyCode::Up | KeyCode::Down => self.history_step(key.code == KeyCode::Up),
                KeyCode::Left => self.motion(CursorMove::Back),
                KeyCode::Right => self.motion(CursorMove::Forward),
                _ => Edit::Ignored,
            };
        };
        if ctrl {
            return Edit::Ignored;
        }
        let e = &mut self.editor;
        let pending = self.pending.take();
        match (pending, c) {
            (Some('d'), 'd') | (Some('c'), 'c') => {
                e.move_cursor(CursorMove::Head);
                e.delete_line_by_end();
                if pending == Some('d') && !e.delete_next_char() {
                    e.delete_char();
                }
                if pending == Some('c') {
                    self.vim = Some(VimMode::Insert);
                }
            }
            (Some('g'), 'g') => e.move_cursor(CursorMove::Top),
            (Some(_), _) => {}
            (None, 'd' | 'c' | 'g') => self.pending = Some(c),
            (None, 'h') => e.move_cursor(CursorMove::Back),
            (None, 'l') => e.move_cursor(CursorMove::Forward),
            (None, 'j') => {
                let (row, _) = e.cursor();
                if row + 1 >= e.lines().len() {
                    return self.history_step(false);
                }
                e.move_cursor(CursorMove::Down);
            }
            (None, 'k') => {
                if e.cursor().0 == 0 {
                    return self.history_step(true);
                }
                e.move_cursor(CursorMove::Up);
            }
            (None, 'w') => e.move_cursor(CursorMove::WordForward),
            (None, 'b') => e.move_cursor(CursorMove::WordBack),
            (None, 'e') => e.move_cursor(CursorMove::WordEnd),
            (None, '0' | '^') => e.move_cursor(CursorMove::Head),
            (None, '$') => e.move_cursor(CursorMove::End),
            (None, 'G') => e.move_cursor(CursorMove::Bottom),
            (None, 'x') => {
                e.delete_next_char();
            }
            (None, 'X') => {
                e.delete_char();
            }
            (None, 'D') => {
                e.delete_line_by_end();
            }
            (None, 'C') => {
                e.delete_line_by_end();
                self.vim = Some(VimMode::Insert);
            }
            (None, 'u') => {
                e.undo();
            }
            (None, 'p') => {
                e.paste();
            }
            (None, 'i') => self.vim = Some(VimMode::Insert),
            (None, 'a') => {
                e.move_cursor(CursorMove::Forward);
                self.vim = Some(VimMode::Insert);
            }
            (None, 'A') => {
                e.move_cursor(CursorMove::End);
                self.vim = Some(VimMode::Insert);
            }
            (None, 'I') => {
                e.move_cursor(CursorMove::Head);
                self.vim = Some(VimMode::Insert);
            }
            (None, 'o') => {
                e.move_cursor(CursorMove::End);
                e.insert_newline();
                self.vim = Some(VimMode::Insert);
            }
            (None, 'O') => {
                e.move_cursor(CursorMove::Head);
                e.insert_newline();
                e.move_cursor(CursorMove::Up);
                self.vim = Some(VimMode::Insert);
            }
            _ => return Edit::Ignored,
        }
        Edit::Changed
    }

    fn motion(&mut self, m: CursorMove) -> Edit {
        self.editor.move_cursor(m);
        Edit::Changed
    }

    fn history_step(&mut self, up: bool) -> Edit {
        if up {
            self.history_up()
        } else {
            self.history_down()
        }
    }

    /// A click in the box: the cursor goes there and a selection starts from it.
    pub fn press_at(&mut self, pos: (usize, usize)) {
        self.editor.cancel_selection();
        self.editor.move_cursor(CursorMove::Jump(pos.0 as u16, pos.1 as u16));
        self.editor.start_selection();
        self.press = Some(pos);
        self.dragged = false;
    }

    /// A double click: the word (or the run of spaces or signs) under the cell is selected.
    pub fn select_word(&mut self, pos: (usize, usize)) {
        let Some(chars) = self.editor.lines().get(pos.0).map(|l| l.chars().collect::<Vec<_>>()) else { return };
        if chars.is_empty() {
            return;
        }
        let class = |c: char| {
            if c.is_alphanumeric() || c == '_' {
                0
            } else if c.is_whitespace() {
                1
            } else {
                2
            }
        };
        let at = pos.1.min(chars.len() - 1);
        let k = class(chars[at]);
        let mut a = at;
        while a > 0 && class(chars[a - 1]) == k {
            a -= 1;
        }
        let mut b = at;
        while b + 1 < chars.len() && class(chars[b + 1]) == k {
            b += 1;
        }
        self.editor.cancel_selection();
        self.editor.move_cursor(CursorMove::Jump(pos.0 as u16, a as u16));
        self.editor.start_selection();
        self.editor.move_cursor(CursorMove::Jump(pos.0 as u16, b as u16));
        self.dragged = true;
    }

    /// A triple click: the whole line is selected.
    pub fn select_line(&mut self, row: usize) {
        let len = self.editor.lines().get(row).map_or(0, |l| l.chars().count());
        if len == 0 {
            return;
        }
        self.editor.cancel_selection();
        self.editor.move_cursor(CursorMove::Jump(row as u16, 0));
        self.editor.start_selection();
        self.editor.move_cursor(CursorMove::Jump(row as u16, (len - 1) as u16));
        self.dragged = true;
    }

    /// A drag in the box: the selection reaches the cell.
    pub fn drag_to(&mut self, pos: (usize, usize)) {
        if self.press.is_some_and(|p| p != pos) {
            self.dragged = true;
        }
        self.editor.move_cursor(CursorMove::Jump(pos.0 as u16, pos.1 as u16));
    }

    /// The selection as the box paints and copies it: the cell under the mouse at the far end
    /// is in, as in a terminal; `(start, end)` with `end` exclusive.
    pub fn selection_cells(&self) -> Option<((usize, usize), (usize, usize))> {
        let ((r0, c0), (r1, c1)) = self.editor.selection_range()?;
        let len = self.editor.lines().get(r1).map_or(0, |l| l.chars().count());
        Some(((r0, c0), (r1, if c1 < len { c1 + 1 } else { c1 })))
    }

    /// The selected text of the draft; None when nothing is selected.
    pub fn selected_text(&self) -> Option<String> {
        let ((r0, c0), (r1, c1)) = self.selection_cells()?;
        let lines = self.editor.lines();
        let mut out = Vec::new();
        for r in r0..=r1 {
            let chars: Vec<char> = lines.get(r)?.chars().collect();
            let from = if r == r0 { c0.min(chars.len()) } else { 0 };
            let to = if r == r1 { c1.min(chars.len()) } else { chars.len() };
            out.push(chars[from..to].iter().collect::<String>());
        }
        let text = out.join("\n");
        (!text.is_empty()).then_some(text)
    }

    /// The button comes up: the selected text (to copy), and the selection is dropped.
    pub fn release_selection(&mut self) -> Option<String> {
        // a click that went nowhere selects nothing and copies nothing
        let t = if self.dragged { self.selected_text() } else { None };
        self.dragged = false;
        self.press = None;
        self.editor.cancel_selection();
        t
    }

    /// The cursor one row up (-1) or down (1) in the box's rows, in the same column; false at the first / last row.
    fn move_visual(&mut self, d: isize) -> bool {
        let (rows, (crow, ccol), _) = layout(self, self.wrap_width);
        let target = crow as isize + d;
        if target < 0 || target as usize >= rows.len() {
            return false;
        }
        let (line, ch) = cell_at(self, self.wrap_width, target as usize, ccol);
        self.editor.move_cursor(CursorMove::Jump(line as u16, ch as u16));
        true
    }

    /// The cursor to the first or the last character of its row in the box.
    fn row_edge(&mut self, end: bool) {
        let (rows, (crow, _), origins) = layout(self, self.wrap_width);
        let (line, start) = origins[crow.min(origins.len() - 1)];
        let n = rows[crow.min(rows.len() - 1)].chars().count();
        let last_of_line = origins.get(crow + 1).map_or(true, |o| o.0 != line);
        // at a wrapped row's end the cursor stays before the break, not on the next row
        let at = if !end {
            start
        } else if last_of_line {
            start + n
        } else {
            start + n.saturating_sub(1)
        };
        self.editor.move_cursor(CursorMove::Jump(line as u16, at as u16));
    }

    /// Handles a key the editor owns.
    pub fn key(&mut self, key: KeyEvent) -> Edit {
        if self.vim == Some(VimMode::Normal) {
            return self.vim_normal(key);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Enter if !ctrl && !alt && !key.modifiers.contains(KeyModifiers::SHIFT) => return Edit::Submit,
            KeyCode::Enter | KeyCode::Char('j') if ctrl || alt || key.code == KeyCode::Enter => {
                self.editor.insert_newline();
                return Edit::Changed;
            }
            // readline: Ctrl+U deletes from the cursor back to the start of the line
            KeyCode::Char('u') if ctrl => {
                self.editor.delete_line_by_head();
                self.hist = None;
                return Edit::Changed;
            }
            // undo and redo of the draft's edits
            KeyCode::Char('z') if ctrl => {
                self.editor.undo();
                return Edit::Changed;
            }
            KeyCode::Char('z') if alt => {
                self.editor.redo();
                return Edit::Changed;
            }
            KeyCode::Up if !ctrl && !alt => {
                if self.wrap_width > 0 {
                    if self.move_visual(-1) {
                        return Edit::Changed;
                    }
                    return self.history_up();
                }
                let (row, _) = self.editor.cursor();
                if row == 0 {
                    return self.history_up();
                }
            }
            KeyCode::Down if !ctrl && !alt => {
                if self.wrap_width > 0 {
                    if self.move_visual(1) {
                        return Edit::Changed;
                    }
                    return self.history_down();
                }
                let (row, _) = self.editor.cursor();
                if row + 1 >= self.line_count() {
                    return self.history_down();
                }
            }
            // Home and End go to the start and the end of the row as the box shows it
            KeyCode::Home if !ctrl && !alt && self.wrap_width > 0 => {
                self.row_edge(false);
                return Edit::Changed;
            }
            KeyCode::End if !ctrl && !alt && self.wrap_width > 0 => {
                self.row_edge(true);
                return Edit::Changed;
            }
            _ => {}
        }
        if self.editor.input(key) {
            self.hist = None;
            Edit::Changed
        } else {
            Edit::Changed
        }
    }

    fn history_up(&mut self) -> Edit {
        if self.history.is_empty() {
            return Edit::Ignored;
        }
        let next = match self.hist {
            None => {
                self.stash = self.text();
                self.history.len() - 1
            }
            Some(0) => return Edit::Ignored,
            Some(i) => i - 1,
        };
        let text = self.history[next].clone();
        self.set_text(&text);
        self.hist = Some(next);
        Edit::Changed
    }

    fn history_down(&mut self) -> Edit {
        match self.hist {
            None => Edit::Ignored,
            Some(i) if i + 1 < self.history.len() => {
                let text = self.history[i + 1].clone();
                self.set_text(&text);
                self.hist = Some(i + 1);
                Edit::Changed
            }
            Some(_) => {
                let s = self.stash.clone();
                self.set_text(&s);
                self.hist = None;
                Edit::Changed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paste_or_a_recalled_draft_never_carries_escapes_or_tabs() {
        let mut i = Input::new("");
        i.insert_paste("a\tb\x1b[2Jc\x07\nd");
        assert_eq!(i.text(), "a    bc\nd", "escapes and controls out, tabs as spaces, line breaks kept");
        let long: String = (1..=40).map(|n| format!("\x1b[31mline\t{n}\n")).collect();
        i.clear();
        i.insert_paste(&long);
        assert!(!i.full_text().contains(['\x1b', '\t']), "{:?}", i.full_text());
        i.set_text("x\x1b]0;title\x07y\tz");
        assert_eq!(i.text(), "xy    z");
        let (rows, _) = wrapped_rows(&i, 20);
        assert!(rows.iter().all(|r| !r.chars().any(char::is_control)), "{rows:?}");
    }

    #[test]
    fn a_combining_mark_takes_no_column() {
        // "e" + U+0301 is one cell on screen: the caret after it sits at column 1, not 2
        let mut i = Input::new("");
        i.set_text("e\u{301}x");
        let (rows, cursor) = wrapped_rows(&i, 20);
        assert_eq!((rows.len(), cursor), (1, (0, 2)));
        // the row of two cells is not cut early in a box two cells wide
        let (rows, _) = wrapped_rows(&i, 2);
        assert_eq!(rows[0], "e\u{301}x");
        // a click on the second cell lands before the "x", past the mark
        assert_eq!(cell_at(&i, 20, 0, 1), (0, 2));
    }

    #[test]
    fn a_wide_character_in_a_one_cell_box_still_lays_out() {
        // QA 13:2x: it looped forever (a 2-cell character never fits 1 cell), growing rows until the OOM killer
        let mut i = Input::new("");
        i.insert_paste("宽x宽");
        for width in 0..4 {
            let (rows, _) = wrapped_rows(&i, width);
            assert!(rows.len() <= 4, "width {width}: {rows:?}");
        }
    }

    #[test]
    fn a_long_paste_is_folded_in_the_draft_and_sent_whole() {
        let mut i = Input::new("");
        i.insert_paste("one line");
        assert_eq!(i.text(), "one line", "a short paste goes in as typed");
        let long: String = (1..=40).map(|n| format!("line {n}\r\n")).collect();
        i.insert_paste(" ");
        i.insert_paste(&long);
        i.insert_paste(" explain it");
        assert_eq!(i.text(), "one line [Pasted text #1 +40 lines] explain it");
        let full = i.full_text();
        assert!(full.starts_with("one line line 1\nline 2\n") && full.ends_with("line 40\n explain it"), "{full}");
        assert!(!full.contains('\r'));
        let wide = "x".repeat(2000);
        i.insert_paste(&wide);
        assert!(i.text().ends_with("[Pasted text #2 2000 chars]"), "{}", i.text());
        assert!(i.full_text().ends_with(&wide));
        i.clear();
        i.insert_paste(&long);
        assert_eq!(i.text(), "[Pasted text #1 +40 lines]", "numbering starts again with a new draft");
    }

    fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn the_counter_shows_for_multiline_or_long_drafts_only() {
        let mut i = Input::new("");
        i.set_text("short");
        assert_eq!(counter(&i), None);
        i.set_text("a\nbc");
        assert_eq!(counter(&i).as_deref(), Some("2 lines · 4 chars"));
        i.set_text(&"x".repeat(250));
        assert_eq!(counter(&i).as_deref(), Some("1 line · 250 chars"));
    }

    #[test]
    fn long_lines_wrap_to_the_width_and_the_cursor_follows() {
        let mut i = Input::new("");
        i.set_text("abcdefgh\nxy");
        let (rows, cur) = wrapped_rows(&i, 3);
        assert_eq!(rows, ["abc", "def", "gh", "xy"]);
        assert_eq!(cur, (3, 2));
        i.set_text("abc");
        let (rows, cur) = wrapped_rows(&i, 3);
        assert_eq!(rows, ["abc", ""]);
        assert_eq!(cur, (1, 0));
    }

    #[test]
    fn a_wide_character_in_a_one_cell_box_keeps_the_cursor_row() {
        let mut i = Input::new("");
        i.set_text("宽a");
        let (rows, _) = wrapped_rows(&i, 1);
        assert_eq!(&rows[..2], ["宽", "a"], "the cursor after the last cell opens a row");
    }

    #[test]
    fn readline_ctrl_u_k_w_delete_to_the_start_to_the_end_and_a_word() {
        let mut i = Input::new("");
        i.set_text("one two three");
        i.key(k(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(i.text(), "one two ");
        i.key(k(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(i.text(), "");
        i.set_text("abc def");
        i.key(k(KeyCode::Char('a'), KeyModifiers::CONTROL));
        i.key(k(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(i.text(), "");
    }

    #[test]
    fn a_click_and_a_drag_in_the_box_select_text_to_copy() {
        let mut i = Input::new("");
        i.set_text("hello world\nsecond");
        let a = cell_at(&i, 20, 0, 6);
        assert_eq!(a, (0, 6));
        i.press_at(a);
        i.drag_to(cell_at(&i, 20, 1, 3));
        assert_eq!(i.selected_text().as_deref(), Some("world\nseco"), "the cell under the mouse is in");
        assert_eq!(i.release_selection().as_deref(), Some("world\nseco"));
        assert_eq!(i.selected_text(), None);
        // past the end of a row the cell is the row's end; a wrapped row counts from its start
        assert_eq!(cell_at(&i, 20, 0, 19), (0, 11));
        assert_eq!(cell_at(&i, 4, 1, 1), (0, 5));
    }

    #[test]
    fn up_down_home_end_follow_the_rows_of_the_box() {
        let mut i = Input::new("");
        i.wrap_width = 5;
        i.set_text("aaaa bbbb cccc");
        // rows: "aaaa ", "bbbb ", "cccc"; the cursor is at the end, in the last row
        i.key(k(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(i.editor.cursor(), (0, 9), "up goes to the row above, same column");
        i.key(k(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(i.editor.cursor(), (0, 5), "home: start of the row");
        i.key(k(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(i.editor.cursor(), (0, 9), "end of a wrapped row: before its break");
        i.key(k(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(i.editor.cursor().0, 0);
        assert!(i.editor.cursor().1 >= 10);
    }

    #[test]
    fn what_a_drag_paints_is_what_it_copies() {
        let mut i = Input::new("");
        i.set_text("hello world\nsecond");
        i.press_at((0, 6));
        i.drag_to((1, 2));
        // the cells the box paints, read from the draft, are the text that is copied
        let ((r0, c0), (r1, c1)) = i.selection_cells().unwrap();
        let lines = i.editor.lines();
        let painted: Vec<String> = (r0..=r1)
            .map(|r| {
                let cs: Vec<char> = lines[r].chars().collect();
                let (a, b) = (if r == r0 { c0 } else { 0 }, if r == r1 { c1 } else { cs.len() });
                cs[a..b].iter().collect()
            })
            .collect();
        assert_eq!(painted.join("\n"), i.selected_text().unwrap());
        assert_eq!(i.selected_text().as_deref(), Some("world\nsec"));
    }

    #[test]
    fn a_double_click_selects_a_word_and_a_triple_click_the_line() {
        let mut i = Input::new("");
        i.set_text("foo bar_1, baz");
        i.select_word((0, 5));
        assert_eq!(i.selected_text().as_deref(), Some("bar_1"));
        i.select_word((0, 3));
        assert_eq!(i.selected_text().as_deref(), Some(" "));
        i.select_word((0, 99));
        assert_eq!(i.selected_text().as_deref(), Some("baz"));
        i.select_line(0);
        assert_eq!(i.selected_text().as_deref(), Some("foo bar_1, baz"));
    }

    #[test]
    fn a_single_click_copies_nothing() {
        let mut i = Input::new("");
        i.set_text("foo bar_1, baz");
        i.press_at((0, 8));
        assert_eq!(i.release_selection(), None, "a click without a drag copies nothing");
        assert_eq!(i.selected_text(), None, "and leaves no selection");
        assert_eq!(i.editor.cursor(), (0, 8), "the cursor is where the click was");
        // dragging back onto the same cell is still a click
        i.press_at((0, 8));
        i.drag_to((0, 8));
        assert_eq!(i.release_selection(), None);
        // a drag to another cell copies
        i.press_at((0, 4));
        i.drag_to((0, 6));
        assert_eq!(i.release_selection().as_deref(), Some("bar"));
    }

    #[test]
    fn ctrl_z_undoes_an_edit_and_alt_z_redoes_it() {
        let mut i = Input::new("");
        i.set_text("hello");
        i.key(k(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_eq!(i.text(), "hello!");
        i.key(k(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(i.text(), "hello");
        i.key(k(KeyCode::Char('z'), KeyModifiers::ALT));
        assert_eq!(i.text(), "hello!");
    }

    #[test]
    fn enter_submits_and_ctrl_j_adds_a_line() {
        let mut i = Input::new("");
        for c in "hi".chars() {
            i.key(k(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(i.key(k(KeyCode::Char('j'), KeyModifiers::CONTROL)), Edit::Changed);
        assert_eq!(i.line_count(), 2);
        assert_eq!(i.key(k(KeyCode::Enter, KeyModifiers::NONE)), Edit::Submit);
        assert_eq!(i.text(), "hi\n");
    }

    #[test]
    fn history_browses_up_and_down_and_keeps_the_draft() {
        let mut i = Input::new("");
        i.push_history("first");
        i.push_history("second");
        i.set_text("draft");
        i.key(k(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(i.text(), "second");
        i.key(k(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(i.text(), "first");
        i.key(k(KeyCode::Down, KeyModifiers::NONE));
        i.key(k(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(i.text(), "draft");
    }

    #[test]
    fn replaces_the_word_at_the_cursor() {
        let mut i = Input::new("");
        i.set_text("see @src/ma");
        i.replace_range(5, "src/main.rs ");
        assert_eq!(i.text(), "see @src/main.rs ");
        assert_eq!(i.cursor_offset(), i.text().len());
    }

    #[test]
    fn the_vim_keys_move_edit_and_switch_modes() {
        let mut i = Input::new("");
        i.vim = Some(VimMode::Insert);
        for c in "hello world".chars() {
            i.key(k(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert!(i.vim_escape(), "Esc leaves insert mode");
        assert_eq!(i.vim, Some(VimMode::Normal));
        let n = |i: &mut Input, c: char| i.key(k(KeyCode::Char(c), KeyModifiers::NONE));
        n(&mut i, '0');
        n(&mut i, 'x');
        assert_eq!(i.text(), "ello world");
        n(&mut i, 'w');
        n(&mut i, 'D');
        assert_eq!(i.text(), "ello ");
        n(&mut i, 'u');
        assert_eq!(i.text(), "ello world", "u undoes");
        n(&mut i, 'A');
        assert_eq!(i.vim, Some(VimMode::Insert));
        i.key(k(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_eq!(i.text(), "ello world!");
        i.vim_escape();
        n(&mut i, 'o');
        i.key(k(KeyCode::Char('z'), KeyModifiers::NONE));
        i.vim_escape();
        assert_eq!(i.text(), "ello world!\nz");
        n(&mut i, 'd');
        n(&mut i, 'd');
        assert_eq!(i.text(), "ello world!", "dd deletes the line");
        assert_eq!(i.key(k(KeyCode::Enter, KeyModifiers::NONE)), Edit::Submit, "Enter sends from normal mode");
        assert!(!i.vim_escape(), "Esc in normal mode is the app's (interrupt, clear)");
    }
}
