//! The command palette (`/`), the argument list after a command (`/model …`)
//! and the file picker (`@`): a query, a ranked list, a selection.

use nexo_core::jobs::{JobInfo, JobKind, JobStatus};
use std::path::Path;

use crate::commands::{self, COMMANDS};
use crate::fuzzy;

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
/// How much of a file the preview reads.
const PREVIEW_BYTES: usize = 16 * 1024;

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

    fn refilter(&mut self) {
        self.aliases.clear();
        self.hits.clear();
        self.groups.clear();
        match &self.kind {
            PaletteKind::Commands => {
                let blank = self.query.trim().is_empty();
                let mut ranked = Vec::new();
                if blank {
                    let recent: Vec<usize> = self
                        .recent
                        .iter()
                        .filter_map(|n| self.items.iter().position(|i| i == n))
                        .take(MAX_RECENT)
                        .collect();
                    for (k, i) in recent.into_iter().enumerate() {
                        ranked.push(i);
                        self.aliases.push(None);
                        self.hits.push(Vec::new());
                        self.groups.push((k == 0).then(|| "recent".to_string()));
                    }
                    let mut last = "";
                    for (g, i) in commands::grouped() {
                        // a command listed under "recent" is not listed again below
                        if ranked.iter().take(self.recent.len().min(MAX_RECENT)).any(|r| *r == i) {
                            continue;
                        }
                        ranked.push(i);
                        self.aliases.push(None);
                        self.hits.push(Vec::new());
                        self.groups.push((g != last).then(|| g.to_string()));
                        last = g;
                    }
                } else {
                    for r in commands::rank(&self.query) {
                        ranked.push(r.index);
                        self.aliases.push(r.via_alias);
                        self.hits.push(r.hits);
                        self.groups.push(None);
                    }
                }
                let names: Vec<String> = self.custom.iter().map(|(n, _)| n.clone()).collect();
                for (k, (i, h)) in fuzzy::rank_hits(&self.query, &names).into_iter().enumerate() {
                    ranked.push(COMMANDS.len() + i);
                    self.aliases.push(None);
                    self.hits.push(h);
                    self.groups.push((blank && k == 0).then(|| "yours".to_string()));
                }
                self.ranked = ranked;
            }
            PaletteKind::Files => {
                let mut r = fuzzy::rank_hits(&self.query, &self.items);
                r.truncate(200);
                (self.ranked, self.hits) = r.into_iter().unzip();
                self.groups = vec![None; self.ranked.len()];
            }
            // sessions are found by id or by title; only the id's letters light up
            PaletteKind::Args(ref c) if c == "resume" => {
                let hay: Vec<String> = self.items.iter().zip(&self.descs).map(|(i, d)| format!("{i} {d}")).collect();
                let items = &self.items;
                (self.ranked, self.hits) = rank_words(&self.query, &hay)
                    .into_iter()
                    .map(|(k, h)| {
                        let n = items[k].chars().count();
                        (k, h.into_iter().filter(|&x| x < n).collect())
                    })
                    .unzip();
                self.groups = vec![None; self.ranked.len()];
            }
            PaletteKind::Args(_) => {
                (self.ranked, self.hits) = rank_words(&self.query, &self.items).into_iter().unzip();
                self.groups = vec![None; self.ranked.len()];
            }
        }
        self.aliases.resize(self.ranked.len(), None);
        self.selected = 0;
        self.top = 0;
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

    /// The command whose values are listed, for the argument list.
    pub fn command(&self) -> Option<&str> {
        match &self.kind {
            PaletteKind::Args(c) => Some(c),
            _ => None,
        }
    }

    fn row(&self, k: usize) -> Row {
        let i = self.ranked[k];
        let name = &self.items[i];
        let hits = self.hits.get(k).cloned().unwrap_or_default();
        match self.kind {
            PaletteKind::Commands => {
                let (args, hint) = match COMMANDS.get(i) {
                    Some(s) => (s.hint.to_string(), s.description.to_string()),
                    None => (
                        String::new(),
                        self.custom.get(i - COMMANDS.len()).map(|(_, d)| format!("{d}  · yours")).unwrap_or_default(),
                    ),
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
        let max = max.max(1);
        let mut lines: Vec<(bool, usize)> = Vec::new(); // (is a title, ranked index)
        for k in 0..self.ranked.len() {
            if self.groups.get(k).is_some_and(Option::is_some) {
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

    /// The head of the selected file (read once per file), for the `@` picker.
    pub fn preview(&mut self, root: &Path, lines: usize) -> Option<&Preview> {
        if self.kind != PaletteKind::Files {
            return None;
        }
        let path = self.current()?.to_string();
        if !matches!(&self.preview, Some((p, _)) if *p == path) {
            let p = read_preview(&root.join(&path), lines.max(1));
            self.preview = Some((path, p));
        }
        self.preview.as_ref().map(|(_, p)| p)
    }
}

fn read_preview(path: &Path, max_lines: usize) -> Preview {
    use std::io::Read;
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::new();
    let read = std::fs::File::open(path).and_then(|f| f.take(PREVIEW_BYTES as u64).read_to_end(&mut buf));
    if let Err(e) = read {
        return Preview { lines: Vec::new(), note: format!("can't read: {}", e.kind()) };
    }
    let size_s = human_size(size);
    if buf.contains(&0) {
        return Preview { lines: Vec::new(), note: format!("binary · {size_s}") };
    }
    let text = String::from_utf8_lossy(&buf);
    let all = text.lines().count();
    let lines = text.lines().take(max_lines).map(|l| crate::render::wrap::clean(l).into_owned()).collect();
    let count = if (size as usize) > PREVIEW_BYTES { format!("{all}+ lines") } else { format!("{all} lines") };
    Preview { lines, note: format!("{count} · {size_s}") }
}

fn human_size(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{:.1} MB", n as f64 / (1024.0 * 1024.0)),
    }
}

/// Values best first: a prefix, then the letters in order; ties keep the given order.
fn rank_words(query: &str, items: &[String]) -> Vec<(usize, Vec<usize>)> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return (0..items.len()).map(|i| (i, Vec::new())).collect();
    }
    let mut prefix = Vec::new();
    let mut loose = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let low = it.to_ascii_lowercase();
        let h = commands::hits(&q, it);
        if low.starts_with(&q) {
            prefix.push((i, h));
        } else if h.len() == q.chars().count() {
            loose.push((i, h));
        }
    }
    prefix.extend(loose);
    prefix
}

/// The values the argument list offers for `command`: the models one has
/// set up, the effort levels the model takes, the themes, or the fixed ones.
pub fn arg_values(
    command: &str,
    models: &[(String, String)],
    current_model: &str,
    effort_levels: &[String],
) -> Vec<(String, String)> {
    match command {
        "model" => models
            .iter()
            .map(|(p, m)| {
                let hint = if m == current_model { "in use" } else { "" };
                (format!("{p}/{m}"), hint.to_string())
            })
            .collect(),
        "effort" => {
            let mut v = vec![("default".to_string(), "what the model does without being told".to_string())];
            if effort_levels.is_empty() {
                v.extend(["off", "low", "medium", "high"].iter().map(|l| (l.to_string(), String::new())));
            } else {
                v.extend(effort_levels.iter().map(|l| (l.clone(), String::new())));
            }
            v
        }
        "theme" => crate::theme::NAMES
            .iter()
            .map(|n| {
                let d = crate::theme::Theme::by_name(n).map(|t| t.description.to_string()).unwrap_or_default();
                (n.to_string(), d)
            })
            .collect(),
        other => commands::fixed_args(other).iter().map(|(v, d)| (v.to_string(), d.to_string())).collect(),
    }
}

/// Values taken from what is going on now, after the fixed ones: each running
/// background job for `/jobs kill <id>`, and the sessions deleted in this run
/// for `/undelete`, the last one first (the one a bare `/undelete` brings back),
/// and the earlier sessions here for `/resume`, with their titles.
pub fn live_values(
    command: &str,
    jobs: &[JobInfo],
    trashed: &[String],
    sessions: &[(String, String)],
    peers: &[(String, String)],
) -> Vec<(String, String)> {
    match command {
        "resume" => sessions.to_vec(),
        "msg" => peers.to_vec(),
        "jobs" => jobs
            .iter()
            .filter(|j| j.status == JobStatus::Running)
            .map(|j| {
                let what = match j.kind {
                    JobKind::Shell => "stop",
                    JobKind::Agent => "stop sub-agent",
                };
                (format!("kill {}", j.id), format!("{what}: {}", j.label.lines().next().unwrap_or("")))
            })
            .collect(),
        "undelete" => trashed
            .iter()
            .rev()
            .enumerate()
            .map(|(k, id)| {
                let hint = if k == 0 { "deleted last · the one /undelete brings back" } else { "deleted in this run" };
                (id.clone(), hint.to_string())
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The commands of `history` (lines that start with `/`), most recent first,
/// each once, by primary name.
pub fn recent_commands(history: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in history.iter().rev() {
        let Some(rest) = line.trim().strip_prefix('/') else { continue };
        let word = rest.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        if let Some(s) = commands::lookup(&word) {
            if !out.iter().any(|n| n == s.name) {
                out.push(s.name.to_string());
            }
        }
        if out.len() >= MAX_RECENT {
            break;
        }
    }
    out
}

/// The files of a project, relative paths, gitignore-aware, at most `max`.
pub fn project_files(root: &Path, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root).hidden(true).git_ignore(true).git_global(true).build();
    for entry in walker.flatten() {
        if out.len() >= max {
            break;
        }
        if entry.file_type().is_some_and(|t| t.is_file()) {
            if let Ok(rel) = entry.path().strip_prefix(root) {
                out.push(nexo_core::shell::slashed(rel));
            }
        }
    }
    out.sort_by(|a, b| a.matches('/').count().cmp(&b.matches('/').count()).then(a.cmp(b)));
    out
}

/// What the draft asks for at the cursor: a command palette when the first
/// line starts with `/` and holds no space yet, the values of the command's
/// argument while the cursor is in its first word, a file picker after the
/// last `@` of the current word.
pub fn wanted(draft: &str, cursor: usize) -> Option<(PaletteKind, usize, String)> {
    let cursor = cursor.min(draft.len());
    let head = &draft[..cursor];
    if let Some(rest) = draft.strip_prefix('/') {
        if !rest.contains(char::is_whitespace) && !head.is_empty() {
            return Some((PaletteKind::Commands, 1, rest.to_string()));
        }
        if let Some(sp) = rest.find(char::is_whitespace) {
            let after = &rest[sp..];
            let at = 1 + sp + (after.len() - after.trim_start().len());
            let arg = &draft[at..];
            if cursor >= at && !arg.contains(char::is_whitespace) && !after.contains('\n') {
                let typed = rest[..sp].to_ascii_lowercase();
                if let Some(s) = commands::lookup(&typed).filter(|s| commands::takes_listed_args(s.name)) {
                    return Some((PaletteKind::Args(s.name.to_string()), at, arg.to_string()));
                }
            }
        }
    }
    let word_start = head.rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
    let word = &head[word_start..];
    if let Some(at) = word.rfind('@') {
        let q = &word[at + 1..];
        return Some((PaletteKind::Files, word_start + at + 1, q.to_string()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_commands_and_moves_the_selection() {
        let mut p = Palette::commands("mo", 1);
        assert_eq!(p.current(), Some("model"));
        p.down();
        assert_ne!(p.current(), Some("model"));
        p.up();
        assert_eq!(p.current(), Some("model"));
        let (rows, sel) = p.rows(3);
        assert_eq!(sel, 0);
        assert!(rows[0].hint.contains("set one up"));
        assert_eq!(rows[0].args, "[name]");
        assert_eq!(rows[0].alias, None);
        assert_eq!(rows[0].hits, vec![0, 1]);
    }

    #[test]
    fn an_alias_finds_its_command_first_and_tab_completes_the_primary_name() {
        let p = Palette::commands("ses", 1);
        assert_eq!(p.current(), Some("resume"), "Tab completes to the primary name");
        let (rows, _) = p.rows(3);
        assert_eq!(rows[0].label, "resume");
        assert_eq!(rows[0].alias.as_deref(), Some("sessions"));
        let p = Palette::commands("re", 1);
        let (rows, _) = p.rows(3);
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert!(labels.starts_with(&["resume", "rewind"]), "{labels:?}");
        assert!(rows.iter().take(2).all(|r| r.alias.is_none()));
    }

    #[test]
    fn a_recent_command_is_listed_once_not_again_under_its_group() {
        let mut p = Palette::commands_full("", 1, Vec::new(), vec!["resume".into(), "rewind".into()]);
        let v = p.view(200);
        let labels: Vec<String> = v
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::Item(r, _) => Some(r.label.clone()),
                _ => None,
            })
            .collect();
        for name in ["resume", "rewind"] {
            assert_eq!(labels.iter().filter(|l| *l == name).count(), 1, "{name} once: {labels:?}");
        }
        assert_eq!(p.len(), labels.len(), "no row is dropped from the list");
    }

    #[test]
    fn with_nothing_typed_it_lists_recent_commands_then_every_group() {
        let mut p = Palette::commands_full("", 1, vec![("deploy".into(), "ship it".into())], vec!["theme".into()]);
        assert_eq!(p.current(), Some("theme"), "the most recent first");
        let v = p.view(4);
        assert_eq!(v.entries[0], Entry::Header("recent".into()));
        assert!(matches!(&v.entries[1], Entry::Item(r, 0) if r.label == "theme"));
        assert_eq!(v.entries[2], Entry::Header("common".into()));
        assert_eq!(v.above, 0);
        assert_eq!(v.below, p.len() - 2);
        // the titles scroll with their rows; the last section is one's own
        p.up();
        assert_eq!(p.current(), Some("deploy"));
        let v = p.view(4);
        assert_eq!(v.entries[v.entries.len() - 2], Entry::Header("yours".into()));
        assert_eq!(v.below, 0);
        // typing drops the sections
        p.set_query("th");
        assert!(p.view(4).entries.iter().all(|e| matches!(e, Entry::Item(..))));
    }

    #[test]
    fn the_list_scrolls_only_when_the_selection_leaves_it() {
        let mut p = Palette::files("", 1, (0..20).map(|i| format!("f{i:02}")).collect());
        assert_eq!(p.view(5).below, 15);
        for _ in 0..4 {
            p.down();
        }
        assert_eq!(p.view(5).above, 0, "still in sight");
        p.down();
        let v = p.view(5);
        assert_eq!((v.above, v.below), (1, 14));
        p.up();
        p.up();
        assert_eq!(p.view(5).above, 1, "going back up inside the window does not move it");
        p.page(true);
        assert_eq!(p.current(), Some("f16"));
        p.page(false);
        p.page(false);
        assert_eq!(p.current(), Some("f00"));
    }

    #[test]
    fn a_typo_gets_a_suggestion() {
        let p = Palette::commands("modle", 1);
        assert!(p.is_empty());
        assert_eq!(p.suggestion(), Some("model"));
        assert_eq!(Palette::commands("mo", 1).suggestion(), None);
    }

    #[test]
    fn lists_the_values_of_an_argument() {
        let vals = arg_values("permissions", &[], "", &[]);
        let p = Palette::args("permissions", "au", 13, vals);
        assert_eq!(p.current(), Some("auto"));
        assert!(p.runs_selection());
        assert_eq!(p.rows(3).0[0].hint, "never ask");
        let p = Palette::args("permissions", "ro", 13, arg_values("permissions", &[], "", &[]));
        assert_eq!(p.current(), Some("read-only"), "the letters in order");
        let models = vec![("ds".to_string(), "chat".to_string()), ("ol".to_string(), "qwen".to_string())];
        let p = Palette::args("model", "q", 7, arg_values("model", &models, "chat", &[]));
        assert_eq!(p.current(), Some("ol/qwen"));
        assert!(!p.runs_selection(), "a model can be one not listed yet");
        let e = arg_values("effort", &[], "", &["low".into(), "max".into()]);
        assert_eq!(e.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>(), ["default", "low", "max"]);
        assert!(arg_values("theme", &[], "", &[]).iter().any(|(v, _)| v == "light"));
    }

    #[test]
    fn lists_running_jobs_and_sessions_deleted_in_this_run() {
        let job = |id, kind, status| JobInfo {
            id,
            kind,
            label: format!("cargo build {id}\nmore"),
            status,
            elapsed_ms: 0,
            lines: 0,
        };
        let jobs = vec![
            job(3, JobKind::Shell, JobStatus::Running),
            job(4, JobKind::Shell, JobStatus::Killed),
            job(7, JobKind::Agent, JobStatus::Running),
        ];
        let v = live_values("jobs", &jobs, &[], &[], &[]);
        assert_eq!(v.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>(), ["kill 3", "kill 7"], "only running ones");
        assert_eq!(v[0].1, "stop: cargo build 3", "the first line of the command");
        assert_eq!(v[1].1, "stop sub-agent: cargo build 7");
        let t = vec!["s1".to_string(), "s2".to_string()];
        let u = live_values("undelete", &[], &t, &[], &[]);
        assert_eq!(u.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>(), ["s2", "s1"], "the last deleted first");
        assert!(u[0].1.contains("brings back"));
        assert!(live_values("theme", &jobs, &t, &[], &[]).is_empty());
        // /resume finds a session by a word of its title
        let s =
            vec![("20261101-1".to_string(), "parser fix".to_string()), ("20261102-1".to_string(), "docs".to_string())];
        let p = Palette::args("resume", "pars", 8, live_values("resume", &[], &[], &s, &[]));
        assert_eq!(p.current(), Some("20261101-1"));
        assert!(p.runs_selection(), "Enter opens it");
    }

    #[test]
    fn remembers_recent_commands_once_each() {
        let h: Vec<String> =
            ["/theme light", "fix the parser", "/m x", "/sessions", "/theme dark", "/nope"].map(String::from).to_vec();
        assert_eq!(recent_commands(&h), ["theme", "resume", "model"]);
    }

    #[test]
    fn knows_what_the_draft_wants() {
        assert_eq!(wanted("/mo", 3), Some((PaletteKind::Commands, 1, "mo".into())));
        assert_eq!(wanted("/model x", 8), Some((PaletteKind::Args("model".into()), 7, "x".into())));
        assert_eq!(wanted("/m  ds/c", 8), Some((PaletteKind::Args("model".into()), 4, "ds/c".into())));
        assert_eq!(wanted("/goal fix the tests", 19), None, "free text past the first word");
        assert_eq!(wanted("/compact x", 10), None, "no values to list");
        assert_eq!(wanted("/model x", 3), None, "the cursor is in the name");
        assert_eq!(wanted("look at @src/ma", 15), Some((PaletteKind::Files, 9, "src/ma".into())));
        assert_eq!(wanted("plain text", 10), None);
    }

    #[test]
    fn walks_a_project() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/main.rs"), "fn main(){}").unwrap();
        std::fs::write(d.path().join("README.md"), "x").unwrap();
        let f = project_files(d.path(), 100);
        assert_eq!(f, vec!["README.md", "src/main.rs"]);
    }

    #[test]
    fn previews_the_selected_file_safely() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "fn a() {}\n\x1b[31mred\x1b[0m\tx\nthird\n").unwrap();
        std::fs::write(d.path().join("b.bin"), [0u8, 1, 2, 3]).unwrap();
        let mut p = Palette::files("", 1, vec!["a.rs".into(), "b.bin".into()]);
        let pv = p.preview(d.path(), 2).cloned().unwrap();
        assert_eq!(pv.lines.len(), 2);
        assert_eq!(pv.lines[0], "fn a() {}");
        assert!(!pv.lines[1].contains('\x1b'), "{:?}", pv.lines[1]);
        assert!(pv.note.starts_with("3 lines"), "{}", pv.note);
        p.down();
        let pv = p.preview(d.path(), 2).cloned().unwrap();
        assert!(pv.lines.is_empty() && pv.note.starts_with("binary"), "{pv:?}");
        assert!(Palette::commands("", 1).preview(d.path(), 2).is_none());
    }
}
