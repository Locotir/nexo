//! The command palette (`/`), the argument list after a command (`/model …`)
//! and the file picker (`@`): a query, a ranked list, a selection.

use nexo_core::jobs::{JobInfo, JobKind, JobStatus};
use std::path::Path;

use crate::commands::{self, COMMANDS};
use crate::fuzzy;

mod filter;
mod preview;
mod values;
mod view;

pub use values::{
    arg_values, dir_values, live_values, project_files, recent_commands, session_hint, session_label, wanted,
};
pub use view::TITLES_ABOVE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteKind {
    Commands,
    Files,
    /// The values the named command takes, after `/name `.
    Args(String),
}

/// One row to show: the name, the alias the query matched (dim, in
/// brackets), the argument hint, what it does, and the letters the query
/// matched (character positions in the alias when there is one, else in the label).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    pub alias: Option<String>,
    pub args: String,
    pub hint: String,
    pub hits: Vec<usize>,
}

/// A line of the list: a section title or a row with its index in the ranking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Header(String),
    Item(Row, usize),
}

/// What fits on screen: the lines, and how many rows are hidden above and below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub entries: Vec<Entry>,
    pub above: usize,
    pub below: usize,
}

/// The head of a file, for the `@` picker to show beside the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub lines: Vec<String>,
    /// Size and lines, or why there is nothing to show.
    pub note: String,
}

#[derive(Debug, Clone)]
pub struct Palette {
    pub kind: PaletteKind,
    pub query: String,
    /// Byte offset in the draft where the query starts (after `/`, `@` or `/name `).
    pub start: usize,
    pub items: Vec<String>,
    /// What each item does (argument values); empty for commands and files.
    descs: Vec<String>,
    pub ranked: Vec<usize>,
    /// For commands: the alias each ranked row was found through.
    aliases: Vec<Option<&'static str>>,
    /// The letters each ranked row matched.
    hits: Vec<Vec<usize>>,
    /// The section title drawn above a ranked row, when one starts there.
    groups: Vec<Option<String>>,
    /// Commands of one's own `(name, description)`, listed after the built-in ones.
    custom: Vec<(String, String)>,
    /// Commands run lately, most recent first: on top while nothing is typed.
    recent: Vec<String>,
    pub selected: usize,
    /// The first line shown: the list scrolls only when the selection leaves it.
    top: usize,
    preview: Option<(String, Preview)>,
}

/// Rows the palette shows at most, section titles included.
pub const MAX_ROWS: usize = 14;
/// Recent commands shown on top.
pub const MAX_RECENT: usize = 3;

impl Palette {
    fn new(kind: PaletteKind, query: &str, start: usize, items: Vec<String>) -> Palette {
        Palette {
            kind,
            query: query.to_string(),
            start,
            items,
            descs: Vec::new(),
            ranked: Vec::new(),
            aliases: Vec::new(),
            hits: Vec::new(),
            groups: Vec::new(),
            custom: Vec::new(),
            recent: Vec::new(),
            selected: 0,
            top: 0,
            preview: None,
        }
    }

    pub fn commands(query: &str, start: usize) -> Palette {
        Palette::commands_with(query, start, Vec::new())
    }

    /// The command palette with commands of one's own after the built-in ones.
    pub fn commands_with(query: &str, start: usize, custom: Vec<(String, String)>) -> Palette {
        Palette::commands_full(query, start, custom, Vec::new())
    }

    /// The command palette with one's own commands and the recent ones on top.
    pub fn commands_full(query: &str, start: usize, custom: Vec<(String, String)>, recent: Vec<String>) -> Palette {
        let mut items = commands::names();
        items.extend(custom.iter().map(|(n, _)| n.clone()));
        let mut p = Palette::new(PaletteKind::Commands, query, start, items);
        p.custom = custom;
        p.recent = recent;
        p.refilter();
        p
    }

    pub fn files(query: &str, start: usize, files: Vec<String>) -> Palette {
        let mut p = Palette::new(PaletteKind::Files, query, start, files);
        p.refilter();
        p
    }

    /// The values `command` takes, `(value, what it does)`.
    pub fn args(command: &str, query: &str, start: usize, values: Vec<(String, String)>) -> Palette {
        let (items, descs) = values.into_iter().unzip();
        let mut p = Palette::new(PaletteKind::Args(command.to_string()), query, start, items);
        p.descs = descs;
        p.refilter();
        p
    }

    pub fn set_query(&mut self, q: &str) {
        if q != self.query {
            self.query = q.to_string();
            self.refilter();
        }
    }
    pub fn is_empty(&self) -> bool {
        self.ranked.is_empty()
    }

    pub fn len(&self) -> usize {
        self.ranked.len()
    }

    pub fn up(&mut self) {
        if !self.ranked.is_empty() {
            self.selected = (self.selected + self.ranked.len() - 1) % self.ranked.len();
        }
    }

    pub fn down(&mut self) {
        if !self.ranked.is_empty() {
            self.selected = (self.selected + 1) % self.ranked.len();
        }
    }

    /// A page up or down, stopping at the ends.
    pub fn page(&mut self, down: bool) {
        if self.ranked.is_empty() {
            return;
        }
        let step = MAX_ROWS - 1;
        self.selected =
            if down { (self.selected + step).min(self.ranked.len() - 1) } else { self.selected.saturating_sub(step) };
    }

    /// The selected item: for commands, always the primary name (what Tab completes to).
    pub fn current(&self) -> Option<&str> {
        self.ranked.get(self.selected).map(|&i| self.items[i].as_str())
    }

    /// With nothing matching: the command the query most likely meant.
    pub fn suggestion(&self) -> Option<&'static str> {
        match self.kind {
            PaletteKind::Commands if self.ranked.is_empty() => commands::closest(&self.query),
            _ => None,
        }
    }

    /// For the argument list: whether Enter runs the selected value (a known
    /// set) rather than the line as typed (the list only suggests).
    pub fn runs_selection(&self) -> bool {
        match &self.kind {
            PaletteKind::Args(c) => commands::closed_args(c),
            PaletteKind::Commands => true,
            PaletteKind::Files => false,
        }
    }

    /// For the model list, which is only a suggestion: whether Enter takes the listed value, because
    /// it was moved to or because what was typed is its model name, so the provider comes with it.
    pub fn picks_model(&self) -> bool {
        let PaletteKind::Args(c) = &self.kind else { return false };
        if c != "model" {
            return false;
        }
        let q = self.query.trim();
        self.current().is_some_and(|v| {
            self.selected > 0 || (!q.is_empty() && v.rsplit_once('/').is_some_and(|(_, m)| m.eq_ignore_ascii_case(q)))
        })
    }

    /// The command whose values are listed, for the argument list.
    pub fn command(&self) -> Option<&str> {
        match &self.kind {
            PaletteKind::Args(c) => Some(c),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
