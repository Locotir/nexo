//! Pastes folded into chips in the draft, and the keys that delete them whole.

use super::*;

impl Input {
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
    pub(super) fn chips(&self) -> Vec<(usize, usize, usize)> {
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
    pub(super) fn snap(&self, (row, col): (usize, usize), toward: Option<bool>) -> (usize, usize) {
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
    pub(super) fn delete_cluster(&mut self, key: KeyEvent) -> bool {
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
    pub(super) fn delete_chip(&mut self, key: KeyEvent) -> bool {
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
        let text = self.text();
        // One pass over the original draft: each label's first occurrence is found there, so a paste that
        // happens to contain a later label is never rescanned and cannot swallow the real chip.
        let mut hits: Vec<(usize, usize, &str)> = Vec::new();
        for (label, pasted) in &self.pastes {
            if let Some(at) = text.find(label.as_str()) {
                hits.push((at, at + label.len(), pasted.as_str()));
            }
        }
        hits.sort_by_key(|h| h.0);
        let mut out = String::with_capacity(text.len());
        let mut done = 0;
        for (s, e, pasted) in hits {
            if s < done {
                continue; // overlaps an earlier chip
            }
            out.push_str(&text[done..s]);
            out.push_str(pasted);
            done = e;
        }
        out.push_str(&text[done..]);
        out
    }
}
