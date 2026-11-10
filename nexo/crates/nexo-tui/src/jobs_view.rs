//! `/jobs`, a click on `⚙ N jobs`: the background jobs in a window. The running ones come first, then the
//! ones that ended (the newest first); each shows its command, how long it ran, how it ended and the last
//! lines of its output. Enter opens one to read more of its output; Esc goes back, then closes.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use nexo_core::agent::AgentCommand;
use nexo_core::jobs::{duration, JobInfo, JobKind, JobStatus, JobTail};
use ratatui::text::{Line, Span};

use crate::modal::ModalOut;
use crate::render::wrap::truncate;
use crate::theme::Theme;

/// Lines of output each job shows in the list.
const PREVIEW: usize = 2;
/// How often the open window asks for the output again.
const REFRESH: Duration = Duration::from_millis(1000);

#[derive(Debug, Clone)]
pub struct JobsView {
    jobs: Vec<JobInfo>,
    tails: Vec<JobTail>,
    /// The row under the cursor, in the order shown.
    pub selected: usize,
    /// Whether one job is open on its own.
    pub detail: bool,
    pub scroll: usize,
    asked: Instant,
}

impl JobsView {
    pub fn new(jobs: Vec<JobInfo>) -> JobsView {
        JobsView { jobs, tails: Vec::new(), selected: 0, detail: false, scroll: 0, asked: Instant::now() }
    }

    /// The jobs in the order shown: running first, then ended, the newest first.
    pub fn order(&self) -> Vec<&JobInfo> {
        let mut v: Vec<&JobInfo> = self.jobs.iter().collect();
        v.sort_by_key(|j| (!j.status.running(), std::cmp::Reverse(j.id)));
        v
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// The job under the cursor.
    pub fn current(&self) -> Option<&JobInfo> {
        self.order().get(self.selected).copied()
    }

    /// The agent listed the jobs again. The cursor stays on the same job when it can.
    pub fn set_jobs(&mut self, jobs: Vec<JobInfo>) {
        let id = self.current().map(|j| j.id);
        self.jobs = jobs;
        self.selected = id.and_then(|id| self.order().iter().position(|j| j.id == id)).unwrap_or(0);
        self.selected = self.selected.min(self.len().saturating_sub(1));
    }

    pub fn set_tails(&mut self, tails: Vec<JobTail>) {
        self.tails = tails;
    }

    fn tail(&self, id: u32) -> Option<&JobTail> {
        self.tails.iter().find(|t| t.id == id)
    }

    /// The request for fresh output, when the last one is old enough.
    pub fn refresh(&mut self) -> Option<AgentCommand> {
        if self.asked.elapsed() < REFRESH {
            return None;
        }
        self.asked = Instant::now();
        Some(AgentCommand::JobTails)
    }

    pub fn key(&mut self, key: KeyEvent) -> ModalOut {
        let last = self.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') if self.detail => {
                self.detail = false;
                self.scroll = 0;
            }
            KeyCode::Esc | KeyCode::Char('q') => return ModalOut::Close,
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') if !self.detail && !self.is_empty() => {
                self.detail = true;
                self.scroll = usize::MAX;
            }
            KeyCode::Up | KeyCode::Char('k') if self.detail => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') if self.detail => self.scroll = self.scroll.saturating_add(1),
            KeyCode::PageUp if self.detail => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::PageDown if self.detail => self.scroll = self.scroll.saturating_add(10),
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = last,
            KeyCode::Char('x') => {
                if let Some(j) = self.current().filter(|j| j.status.running()) {
                    return ModalOut::Send(AgentCommand::KillJob { id: Some(j.id) });
                }
            }
            _ => {}
        }
        ModalOut::Nothing
    }

    /// A click on the list row `i`: it opens.
    pub fn click_row(&mut self, i: usize) {
        if !self.detail && i < self.len() {
            self.selected = i;
            self.detail = true;
            self.scroll = usize::MAX;
        }
    }

    pub fn scroll_by(&mut self, delta: i32) {
        if self.detail {
            self.scroll =
                if self.scroll == usize::MAX { usize::MAX } else { (self.scroll as i32 + delta).max(0) as usize };
        } else if !self.is_empty() {
            self.selected = (self.selected as i32 + delta).clamp(0, self.len() as i32 - 1) as usize;
        }
    }

    pub fn title(&self) -> String {
        if self.detail {
            return " ⚙ job ".into();
        }
        if self.jobs.is_empty() {
            return " ⚙ background jobs ".into();
        }
        let running = self.jobs.iter().filter(|j| j.status.running()).count();
        format!(" ⚙ background jobs · {running} running ")
    }

    /// The keys, in the row pinned under the body; the longest set that fits `w`.
    pub fn keys(&self, w: usize, theme: &Theme) -> Line<'static> {
        let list: &[(&str, &str)] = &[("↑↓", "move"), ("Enter", "open"), ("x", "stop"), ("Esc", "close")];
        let detail: &[(&str, &str)] = &[("↑↓", "scroll"), ("x", "stop"), ("Esc", "back")];
        let short: &[(&str, &str)] = &[("Enter", ""), ("x", ""), ("Esc", "")];
        let first = if self.detail { detail } else { list };
        for set in [first, short] {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (k, what) in set {
                if !spans.is_empty() {
                    spans.push(Span::styled(" · ", theme.dim2()));
                }
                spans.push(Span::styled((*k).to_string(), theme.style(theme.accent)));
                if !what.is_empty() {
                    spans.push(Span::styled(format!(" {what}"), theme.dim()));
                }
            }
            if spans.iter().map(|s| crate::render::wrap::width(&s.content)).sum::<usize>() <= w {
                return Line::from(spans);
            }
        }
        Line::from(Span::styled("Esc close", theme.dim()))
    }

    /// The body at `w` columns: the lines, the line each job's row starts at (the list) and the line
    /// to keep in sight.
    pub fn lines(&self, w: usize, theme: &Theme) -> (Vec<Line<'static>>, Vec<usize>, usize) {
        if self.is_empty() {
            let empty = vec![
                Line::from(Span::styled("⚙", theme.dim2())).centered(),
                Line::from(Span::styled("No background jobs", theme.dim())).centered(),
            ];
            return (empty, Vec::new(), 0);
        }
        if self.detail {
            return (self.detail_lines(w, theme), Vec::new(), 0);
        }
        let mut lines = Vec::new();
        let mut starts = Vec::new();
        let mut ended_header = false;
        for (i, j) in self.order().into_iter().enumerate() {
            if !j.status.running() && !ended_header {
                if i > 0 {
                    lines.push(Line::default());
                }
                lines.push(Line::from(Span::styled("ended", theme.dim2())));
                ended_header = true;
            }
            starts.push(lines.len());
            let sel = i == self.selected;
            let row = self.head(j, w, theme, sel);
            lines.push(row);
            let tail = self.tail(j.id).map(|t| t.text.as_str()).unwrap_or("");
            let all: Vec<&str> = tail.lines().filter(|l| !l.trim().is_empty()).collect();
            for l in &all[all.len().saturating_sub(PREVIEW)..] {
                let text = truncate(&l.replace('\t', "    "), w.saturating_sub(6));
                lines.push(Line::from(Span::styled(format!("      {text}"), theme.dim())));
            }
        }
        let at = starts.get(self.selected).copied().unwrap_or(0);
        (lines, starts, at)
    }

    /// `▸ 3  cargo build           running  12 s`
    fn head(&self, j: &JobInfo, w: usize, theme: &Theme, sel: bool) -> Line<'static> {
        let what = match j.kind {
            JobKind::Shell => String::new(),
            JobKind::Agent => "sub-agent: ".into(),
        };
        let first = j.label.lines().next().unwrap_or("");
        let status = status_text(j, self.tail(j.id));
        let left = format!("{} {:>3}  ", if sel { "▸" } else { " " }, j.id);
        let room = w.saturating_sub(left.chars().count() + status.chars().count() + 2);
        let label = truncate(&format!("{what}{first}"), room);
        let pad = w.saturating_sub(left.chars().count() + label.chars().count() + status.chars().count());
        let base =
            if sel { theme.selection().add_modifier(ratatui::style::Modifier::BOLD) } else { Default::default() };
        let tone = match &j.status {
            JobStatus::Running => theme.accent,
            JobStatus::Done { ok: true, .. } => theme.success,
            JobStatus::Done { .. } => theme.error,
            JobStatus::Killed => theme.warn,
        };
        Line::from(vec![
            Span::styled(left, base),
            Span::styled(label, base),
            Span::styled(" ".repeat(pad), base),
            Span::styled(status, if sel { base } else { theme.style(tone) }),
        ])
    }

    fn detail_lines(&self, w: usize, theme: &Theme) -> Vec<Line<'static>> {
        let Some(j) = self.current() else { return Vec::new() };
        let mut lines = Vec::new();
        for (i, l) in j.label.lines().enumerate() {
            let text = truncate(l, w.saturating_sub(8));
            let lead = if i == 0 { format!("job {:<3} ", j.id) } else { "        ".into() };
            lines.push(Line::from(vec![Span::styled(lead, theme.dim()), Span::styled(text, theme.bold(theme.fg))]));
        }
        lines.push(Line::from(Span::styled(
            format!("{} · {} lines of output", status_text(j, self.tail(j.id)), j.lines),
            theme.dim(),
        )));
        lines.push(Line::from(Span::styled("─".repeat(w), theme.dim2())));
        let tail = self.tail(j.id).map(|t| t.text.as_str()).unwrap_or("");
        if tail.is_empty() {
            lines.push(Line::from(Span::styled("no output yet", theme.dim())));
        }
        for l in tail.lines() {
            lines.push(Line::from(Span::raw(truncate(&l.replace('\t', "    "), w))));
        }
        lines
    }
}

/// `running 12 s`, `done (exit 0) 3 s, 4 min ago`.
fn status_text(j: &JobInfo, tail: Option<&JobTail>) -> String {
    let ran = duration(j.elapsed_ms);
    match (&j.status, tail.and_then(|t| t.ended_ago_ms)) {
        (JobStatus::Running, _) => format!("running {ran}"),
        (s, Some(ago)) => format!("{} after {ran}, {} ago", s.word(), duration(ago / 1000 * 1000)),
        (s, None) => format!("{} after {ran}", s.word()),
    }
}
