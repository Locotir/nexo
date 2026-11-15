//! The mouse selection inside the box.

use super::*;

impl Input {
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
}
