//! The editor under the transcript: a draft, its history, and the keys it owns.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use tui_textarea::{CursorMove, TextArea};

mod chips;
mod history;
mod layout;
mod select;
mod vim;

pub use layout::{cell_at, counter, row_origins, wrapped_rows};
use layout::{cluster_at, cluster_of, layout};

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
    /// The draft is untouched, but the key was a miss the user should be told about (shown as a flash).
    Notice(String),
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
            return self.vim_key(key);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // a joiner, a presentation selector or a skin tone typed on its own is left out, as in a paste:
        // the box draws the emoji without it, so keeping it would leave a mark the caret cannot reach
        if let KeyCode::Char(c) = key.code {
            if !ctrl && !alt && crate::render::wrap::unsafe_char(c) {
                return Edit::Ignored;
            }
        }
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
            // Option+Z on a Mac types Ω (U+03A9) where Option is not Meta: it redoes when there is something to
            // redo, and stays a typed letter otherwise
            KeyCode::Char('Ω') if redo_char_is_redo(ctrl, alt) && self.editor.redo() => {
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
        // Option+Left and Option+Right move by word in macOS terminals (iTerm2 sends them as Alt+arrows); the editor
        // knows the word jump as Ctrl+arrows only, which macOS keeps for Mission Control
        let key = if cfg!(target_os = "macos")
            && key.modifiers.contains(KeyModifiers::ALT)
            && !key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Left | KeyCode::Right)
        {
            KeyEvent::new(key.code, (key.modifiers - KeyModifiers::ALT) | KeyModifiers::CONTROL)
        } else {
            key
        };
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
}

/// Whether Ω typed bare is the Option+Z of a Mac (redo) and not a letter the person means.
fn redo_char_is_redo(ctrl: bool, alt: bool) -> bool {
    cfg!(target_os = "macos") && !ctrl && !alt
}

#[cfg(test)]
mod tests;
