//! The rows of the palette and the window of them that fits the screen.

use super::*;

/// A screen this tall or less has no row to spare for the section titles of a list that does not fit.
pub const TITLES_ABOVE: u16 = 30;

impl Palette {
    pub(super) fn row(&self, k: usize) -> Row {
        let i = self.ranked[k];
        let name = &self.items[i];
        let hits = self.hits.get(k).cloned().unwrap_or_default();
        match self.kind {
            PaletteKind::Commands => {
                let (args, hint) = match COMMANDS.get(i) {
                    Some(s) => (s.hint.to_string(), s.description.to_string()),
                    None => {
                        (String::new(), self.custom.get(i - COMMANDS.len()).map(|(_, d)| d.clone()).unwrap_or_default())
                    }
                };
                Row {
                    label: name.clone(),
                    alias: self.aliases.get(k).copied().flatten().map(String::from),
                    args,
                    hint,
                    hits,
                }
            }
            PaletteKind::Files => {
                Row { label: name.clone(), alias: None, args: String::new(), hint: String::new(), hits }
            }
            PaletteKind::Args(_) => Row {
                label: name.clone(),
                alias: None,
                args: String::new(),
                hint: self.descs.get(i).cloned().unwrap_or_default(),
                hits,
            },
        }
    }

    /// The rows to show, the window around the selection, with the selected index within it.
    pub fn rows(&self, max: usize) -> (Vec<Row>, usize) {
        let max = max.max(1);
        let start = self.selected.saturating_sub(max - 1).min(self.ranked.len().saturating_sub(max));
        let rows = (start..(start + max).min(self.ranked.len())).map(|k| self.row(k)).collect();
        (rows, self.selected - start)
    }

    /// Widths of the name column (name and matched alias, without the slash)
    /// and of the argument column, over every row, so they do not shift while scrolling.
    pub fn columns(&self) -> (usize, usize) {
        let mut name = 0;
        let mut args = 0;
        for k in 0..self.ranked.len() {
            let r = self.row(k);
            let alias = r.alias.as_ref().map(|a| crate::render::wrap::width(a) + 4).unwrap_or(0);
            name = name.max(crate::render::wrap::width(&r.label) + alias);
            args = args.max(crate::render::wrap::width(&r.args));
        }
        (name, args)
    }

    /// The lines that fit in `max`, section titles included, scrolled only as
    /// far as needed to keep the selection (and the title right above it) in sight.
    pub fn view(&mut self, max: usize) -> View {
        self.view_titled(max, true)
    }

    /// `view`, with the section titles left out when `titles` is false and the list does not fit.
    pub fn view_titled(&mut self, max: usize, titles: bool) -> View {
        let max = max.max(1);
        let mut lines: Vec<(bool, usize)> = Vec::new(); // (is a title, ranked index)
        // short of rows, a title would push a command off the list: the list goes flat unless it all fits
        let n_titles = (0..self.ranked.len()).filter(|&k| self.groups.get(k).is_some_and(Option::is_some)).count();
        let flat = !titles && self.ranked.len() + n_titles > max;
        for k in 0..self.ranked.len() {
            if !flat && self.groups.get(k).is_some_and(Option::is_some) {
                lines.push((true, k));
            }
            lines.push((false, k));
        }
        let Some(sel) = lines.iter().position(|&(t, k)| !t && k == self.selected) else {
            return View { entries: Vec::new(), above: 0, below: 0 };
        };
        if sel < self.top {
            self.top = sel;
        }
        if sel >= self.top + max {
            self.top = sel + 1 - max;
        }
        // the section title belongs with its first row
        if self.top == sel && sel > 0 && lines[sel - 1].0 {
            self.top = sel - 1;
        }
        self.top = self.top.min(lines.len().saturating_sub(max));
        let shown = &lines[self.top..(self.top + max).min(lines.len())];
        let first = shown.iter().find(|l| !l.0).map(|l| l.1).unwrap_or(0);
        let last = shown.iter().rev().find(|l| !l.0).map(|l| l.1).unwrap_or(0);
        let entries = shown
            .iter()
            .map(|&(t, k)| match t {
                true => Entry::Header(self.groups[k].clone().unwrap_or_default()),
                false => Entry::Item(self.row(k), k),
            })
            .collect();
        View { entries, above: first, below: self.ranked.len().saturating_sub(last + 1) }
    }
}
