//! Vim's normal mode and the way out of insert mode.

use super::*;

impl Input {
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
    pub(super) fn vim_normal(&mut self, key: KeyEvent) -> Edit {
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

    pub(super) fn motion(&mut self, m: CursorMove) -> Edit {
        self.editor.move_cursor(m);
        Edit::Changed
    }
}
