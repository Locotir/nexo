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
    /// Pasted attachments (`@path` of a pasted image) that the draft lights once, as it does a folded paste.
    lit_words: Vec<String>,
    /// The last change to the draft was a paste: no ghost completion until the next key.
    after_paste: bool,
    /// The text width of the box at the last draw (0: not drawn yet); ↑ ↓ Home End follow its rows.
    pub wrap_width: usize,
    /// Where the last press in the box was, and whether the pointer then reached another cell (or the click was a double / triple one): only then is there a selection to copy.
    press: Option<(usize, usize)>,
    dragged: bool,
}

/// Whether `c` adds nothing to the cell before it: a combining mark, a joiner, a selector, a skin
/// tone (the box draws none of them, see `wrap::clean`).
fn no_cell(c: char) -> bool {
    !c.is_control() && (unicode_width::UnicodeWidthChar::width(c) == Some(0) || crate::render::wrap::unsafe_char(c))
}

fn regional(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

/// The end of the visible character that starts at `i`: the character, its marks, and for a flag both its letters.
fn cluster_end(chars: &[char], i: usize) -> usize {
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
fn cluster_of(chars: &[char], at: usize) -> (usize, usize) {
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
fn cluster_at(chars: &[char], col: usize) -> Option<(usize, usize)> {
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
    /// A history entry was recalled: the draft changed, but it is not typing.
    Recalled,
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
            lit_words: Vec::new(),
            after_paste: false,
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

    /// Nothing typed at all: one empty line. A draft of only blanks or new lines is "empty" for
    /// sending, but it is on screen (the caret has moved), so the box must not show its placeholder.
    pub fn is_untouched(&self) -> bool {
        self.editor.lines().len() == 1 && self.editor.lines()[0].is_empty()
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
        self.lit_words.clear();
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
        self.after_paste = true;
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        // escapes and other control characters out, tabs as spaces: the box draws the text as is
        let text = crate::render::wrap::clean(&text).into_owned();
        let lines = text.trim_end_matches('\n').lines().count();
        if lines <= PASTE_LINES && text.chars().count() <= PASTE_CHARS {
            // a pasted attachment is a chip too
            let word = text.trim();
            if word.starts_with('@') && !word.contains(char::is_whitespace) {
                // words deleted since are forgotten, so a path pasted again is a new chip
                let text = self.editor.lines().join("\n");
                self.lit_words.retain(|w| text.contains(w.as_str()));
                if !self.lit_words.iter().any(|w| w == word) {
                    self.lit_words.push(word.to_string());
                }
            }
            self.editor.insert_str(text);
            return;
        }
        let n = self
            .pastes
            .iter()
            .filter_map(|(l, _)| l.strip_prefix("[Pasted text #")?.split(' ').next()?.parse::<usize>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let label = if lines > 1 {
            format!("[Pasted text #{n} +{lines} lines]")
        } else {
            format!("[Pasted text #{n} {} chars]", text.chars().count())
        };
        self.editor.insert_str(&label);
        self.pastes.push((label, text));
    }

    /// The texts of the draft that are chips: folded pastes and pasted attachments.
    pub fn chip_labels(&self) -> Vec<String> {
        let text = self.editor.lines().join("\n");
        let mut out: Vec<String> = self.pastes.iter().map(|(l, _)| l.clone()).collect();
        for w in &self.lit_words {
            if text.contains(w.as_str()) && !out.contains(w) {
                out.push(w.clone());
            }
        }
        out
    }

    /// The last change to the draft was a paste (the next key ends it).
    pub fn just_pasted(&self) -> bool {
        self.after_paste
    }

    /// Where each folded paste sits in the draft: `(line, first character, end)`.
    fn chips(&self) -> Vec<(usize, usize, usize)> {
        let mut out = Vec::new();
        for (row, line) in self.editor.lines().iter().enumerate() {
            for (label, _) in &self.pastes {
                for (b, _) in line.match_indices(label.as_str()) {
                    let start = line[..b].chars().count();
                    out.push((row, start, start + label.chars().count()));
                }
            }
        }
        out
    }

    /// A position inside a chip moved to its nearer edge, or to the edge `toward` says (true: the end).
    fn snap(&self, (row, col): (usize, usize), toward: Option<bool>) -> (usize, usize) {
        for (r, s, e) in self.chips() {
            if r == row && col > s && col < e {
                let end = toward.unwrap_or(col - s >= e - col);
                return (row, if end { e } else { s });
            }
        }
        // never inside a visible character: a base with its marks, a flag
        if let Some(line) = self.editor.lines().get(row) {
            let chars: Vec<char> = line.chars().collect();
            if let Some((s, e)) = cluster_at(&chars, col) {
                let end = toward.unwrap_or(col - s >= e - col);
                return (row, if end { e } else { s });
            }
        }
        (row, col)
    }

    /// Backspace or Delete on a visible character of several code points (a letter with its
    /// marks, a flag): it goes whole.
    fn delete_cluster(&mut self, key: KeyEvent) -> bool {
        if !key.modifiers.is_empty() || self.editor.selection_range().is_some() {
            return false;
        }
        let backward = match key.code {
            KeyCode::Backspace => true,
            KeyCode::Delete => false,
            _ => return false,
        };
        let (row, col) = self.editor.cursor();
        let Some(line) = self.editor.lines().get(row) else { return false };
        let chars: Vec<char> = line.chars().collect();
        let Some(at) = if backward { col.checked_sub(1) } else { Some(col) }.filter(|a| *a < chars.len()) else {
            return false;
        };
        let (s, e) = cluster_of(&chars, at);
        if e - s < 2 {
            return false;
        }
        self.editor.move_cursor(CursorMove::Jump(row as u16, s as u16));
        self.editor.delete_str(e - s);
        true
    }

    /// Backspace, Ctrl+W or Alt+Backspace at the end of a chip, or Delete at its start: the whole chip goes, with its paste.
    fn delete_chip(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let backward = match key.code {
            KeyCode::Backspace if !ctrl => true,
            KeyCode::Char('w') if ctrl => true,
            KeyCode::Delete if !ctrl && !alt => false,
            _ => return false,
        };
        if self.editor.selection_range().is_some() {
            return false;
        }
        let (row, col) = self.editor.cursor();
        let Some((_, s, e)) =
            self.chips().into_iter().find(|&(r, s, e)| r == row && if backward { e == col } else { s == col })
        else {
            return false;
        };
        let label: String = self.editor.lines()[row].chars().skip(s).take(e - s).collect();
        self.editor.move_cursor(CursorMove::Jump(row as u16, s as u16));
        self.editor.delete_str(e - s);
        if !self.text().contains(&label) {
            self.pastes.retain(|(l, _)| *l != label);
        }
        true
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
        let last_of_line = origins.get(crow + 1).is_none_or(|o| o.0 != line);
        // at a wrapped row's end the cursor stays before the break, not on the next row
        let at = if !end {
            start
        } else if last_of_line {
            start + n
        } else {
            start + n.saturating_sub(1)
        };
        let (line, at) = self.snap((line, at), Some(end));
        self.editor.move_cursor(CursorMove::Jump(line as u16, at as u16));
    }

    /// Handles a key the editor owns.
    pub fn key(&mut self, key: KeyEvent) -> Edit {
        self.after_paste = false;
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
            // Ctrl+U cuts back to the start of the cursor line (readline); Esc Esc and Ctrl+C clear the draft
            KeyCode::Char('u') if ctrl => {
                self.editor.cancel_selection();
                self.editor.delete_line_by_head();
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
        if self.delete_chip(key) {
            return Edit::Changed;
        }
        if self.delete_cluster(key) {
            return Edit::Changed;
        }
        let before = self.editor.cursor();
        let len = self.editor.lines().get(before.0).map_or(0, |l| l.chars().count());
        self.editor.input(key);
        let mut now = self.editor.cursor();
        // a word taken out up to a mark: the mark goes with it
        if now.0 == before.0 && self.editor.lines().get(now.0).is_some_and(|l| l.chars().count() < len) {
            let chars: Vec<char> = self.editor.lines()[now.0].chars().collect();
            if let Some((_, e)) = cluster_at(&chars, now.1) {
                self.editor.delete_str(e - now.1);
                now = self.editor.cursor();
            }
        }
        let to = self.snap(now, Some(now > before));
        if to != now {
            self.editor.move_cursor(CursorMove::Jump(to.0 as u16, to.1 as u16));
        }
        Edit::Changed
    }

    /// The word being typed: the unit (letters, digits, and the joiners of a path, a flag or an
    /// identifier) that ends at the cursor, when the cursor is at its end. None in vim's normal
    /// mode, with a selection, inside the first word of a command, or inside a paste chip.
    pub fn word_before_cursor(&self) -> Option<String> {
        if self.vim == Some(VimMode::Normal)
            || self.editor.selection_range().is_some()
            || self.after_paste
            || self.editor.lines().len() > 1
        {
            return None;
        }
        let (row, col) = self.editor.cursor();
        let chars: Vec<char> = self.editor.lines().get(row)?.chars().collect();
        let col = col.min(chars.len());
        // the ghost is only for the end of the draft
        if col < chars.len() {
            return None;
        }
        let unit = |c: char| c.is_alphanumeric() || "_-./\\:@~+%#=?&".contains(c);
        if chars.get(col).is_some_and(|c| unit(*c)) {
            return None;
        }
        let mut start = col;
        while start > 0 && unit(chars[start - 1]) {
            start -= 1;
        }
        if start == col {
            return None;
        }
        let word: String = chars[start..col].iter().collect();
        // /command: the palette owns it (a path such as /etc/ho is a word)
        if row == 0 && start == 0 && word.starts_with('/') && !word[1..].contains('/') {
            return None;
        }
        // inside a [Pasted text #n ...] chip
        let before: String = chars[..col].iter().collect();
        if let Some(open) = before.rfind('[') {
            if before[open..].starts_with("[Pasted") && !before[open..].contains(']') {
                return None;
            }
        }
        Some(word)
    }

    /// Takes the ghost the draft showed: its text goes in at the cursor.
    pub fn accept_complete(&mut self, suffix: &str) {
        self.editor.insert_str(suffix);
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
        Edit::Recalled
    }

    fn history_down(&mut self) -> Edit {
        match self.hist {
            None => Edit::Ignored,
            Some(i) if i + 1 < self.history.len() => {
                let text = self.history[i + 1].clone();
                self.set_text(&text);
                self.hist = Some(i + 1);
                Edit::Recalled
            }
            Some(_) => {
                let s = self.stash.clone();
                self.set_text(&s);
                self.hist = None;
                Edit::Recalled
            }
        }
    }
}

#[cfg(test)]
mod tests;
