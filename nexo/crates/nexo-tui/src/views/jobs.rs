//! `/jobs`, a click on `⚙ N jobs`: the background jobs in a window. The running ones come first, then the
//! ones that ended (the newest first); each shows its command, how long it ran, how it ended and the last
//! lines of its output. Enter opens one to read more of its output; Esc goes back, then closes.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use nexo_core::agent::AgentCommand;
use nexo_core::jobs::{duration, JobInfo, JobKind, JobStatus, JobTail};
use ratatui::text::{Line, Span};

use crate::modal::ModalOut;
use crate::render::wrap::{truncate, width};
use crate::theme::Theme;

/// Lines of output each job shows in the list.
const PREVIEW: usize = 2;
/// How often the open window asks for the output again.
const REFRESH: Duration = Duration::from_millis(1000);

#[derive(Debug, Clone)]
pub struct JobsView {
    jobs: Vec<JobInfo>,
    tails: Vec<JobTail>,
    /// A read-only monitor list shares the jobs window's output navigation.
    monitors: bool,
    monitor_states: Vec<(u32, nexo_core::agent::MonitorStatus)>,
    /// The row under the cursor, in the order shown.
    pub selected: usize,
    /// Whether one job is open on its own.
    pub detail: bool,
    pub scroll: usize,
    asked: Instant,
    /// The jobs that are sub-agent runs the app can open: (job number, run id).
    runs: Vec<(u32, String)>,
}

impl JobsView {
    pub fn new(jobs: Vec<JobInfo>) -> JobsView {
        JobsView {
            jobs,
            tails: Vec::new(),
            monitors: false,
            monitor_states: Vec::new(),
            selected: 0,
            detail: false,
            scroll: 0,
            asked: Instant::now(),
            runs: Vec::new(),
        }
    }

    /// A read-only view of monitors, with the same output navigation as jobs.
    pub fn monitors(monitors: Vec<JobInfo>, tails: Vec<JobTail>) -> JobsView {
        let mut view = Self::new(monitors);
        view.monitors = true;
        view.tails = tails;
        view
    }

    pub fn set_monitor_states(&mut self, states: Vec<(u32, nexo_core::agent::MonitorStatus)>) {
        self.monitor_states = states;
    }

    fn monitor_status(&self, id: u32) -> &'static str {
        use nexo_core::agent::MonitorStatus::*;
        match self.monitor_states.iter().find(|(job, _)| *job == id).map(|(_, state)| state) {
            Some(Listening) => "listening",
            Some(Paused) => "paused",
            Some(Closing) => "closing",
            None => "unknown",
        }
    }

    pub fn is_monitors(&self) -> bool {
        self.monitors
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
        let position = id.and_then(|id| self.order().iter().position(|j| j.id == id));
        if self.detail && position.is_none() {
            self.detail = false;
            self.scroll = 0;
        }
        self.selected = position.unwrap_or(0);
        self.selected = self.selected.min(self.len().saturating_sub(1));
    }

    pub fn set_runs(&mut self, runs: Vec<(u32, String)>) {
        if !self.monitors {
            self.runs = runs;
        }
    }

    /// The run the job under the cursor is, when it is a sub-agent run that can be opened.
    fn current_run(&self) -> Option<String> {
        let id = self.current()?.id;
        self.runs.iter().find(|(job, _)| *job == id).map(|(_, run)| run.clone())
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
            // a sub-agent opens its conversation; the job's own output is behind →
            KeyCode::Enter if !self.detail && self.current_run().is_some() => {
                return ModalOut::OpenSub(self.current_run().unwrap_or_default());
            }
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
            KeyCode::Char('x') if !self.monitors => {
                if let Some(j) = self.current().filter(|j| j.status.running()) {
                    return ModalOut::Send(AgentCommand::KillJob { id: Some(j.id) });
                }
            }
            _ => {}
        }
        ModalOut::Nothing
    }

    /// A click on the list row `i`: it opens.
    pub fn click_row(&mut self, i: usize) -> ModalOut {
        if !self.detail && i < self.len() {
            self.selected = i;
            if let Some(run) = self.current_run() {
                return ModalOut::OpenSub(run);
            }
            self.detail = true;
            self.scroll = usize::MAX;
        }
        ModalOut::Nothing
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
        if self.monitors {
            return if self.detail {
                " ◉ monitor ".into()
            } else {
                format!(" ◉ monitors · {} total ", self.jobs.len())
            };
        }
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
        // with nothing listed only the key that works is offered
        let monitor_list: &[(&str, &str)] = &[("↑↓", "move"), ("Enter", "open"), ("Esc", "close")];
        let monitor_detail: &[(&str, &str)] = &[("↑↓", "scroll"), ("Esc", "back")];
        let first = if self.monitors && self.detail {
            monitor_detail
        } else if self.monitors && !self.is_empty() {
            monitor_list
        } else if self.detail {
            detail
        } else if self.is_empty() {
            &[("Esc", "close")]
        } else {
            list
        };
        let fallback: &[(&str, &str)] = if self.monitors { &[("Enter", ""), ("Esc", "")] } else { short };
        for set in [first, fallback] {
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
                Line::from(Span::styled(if self.monitors { "No monitors" } else { "No background jobs" }, theme.dim()))
                    .centered(),
                Line::from(Span::styled(
                    if self.monitors {
                        "Ask the agent to monitor a background shell job."
                    } else {
                        "Ask the agent to run a command in the background."
                    },
                    theme.dim2(),
                ))
                .centered(),
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
            if !self.monitors && !j.status.running() && !ended_header {
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
        let status =
            if self.monitors { self.monitor_status(j.id).to_string() } else { status_text(j, self.tail(j.id)) };
        let left = format!("{} {:>3}  ", if sel { "▸" } else { " " }, j.id);
        let room = w.saturating_sub(width(&left) + width(&status) + 2);
        let label = truncate(&format!("{what}{first}"), room);
        let pad = w.saturating_sub(width(&left) + width(&label) + width(&status));
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
        let lead = format!("{} {:<3} ", if self.monitors { "monitor" } else { "job" }, j.id);
        let rest = vec![Span::raw(" ".repeat(width(&lead)))];
        let wrapped = crate::render::wrap::wrap_runs(
            &[(j.label.clone(), theme.bold(theme.fg))],
            w,
            &[Span::styled(lead, theme.dim())],
            &rest,
        );
        lines.extend(wrapped);
        if self.monitors {
            lines.push(Line::from(Span::styled(format!("monitor: {}", self.monitor_status(j.id)), theme.dim())));
        }
        lines.push(Line::from(Span::styled(
            format!(
                "{}{} · {} lines of output",
                if self.monitors { "job: " } else { "" },
                status_text(j, self.tail(j.id)),
                j.lines
            ),
            theme.dim(),
        )));
        lines.push(Line::from(Span::styled("─".repeat(w), theme.dim2())));
        let tail = self.tail(j.id).map(|t| t.text.as_str()).unwrap_or("");
        if tail.is_empty() {
            lines.push(Line::from(Span::styled("no output yet", theme.dim())));
        }
        for l in tail.lines() {
            if self.monitors {
                // The list is a preview, but the detail must not hide the end of
                // a retained output line: there is no horizontal scrolling here.
                lines.extend(crate::render::wrap::wrap_runs(
                    &[(l.replace('\t', "    "), Default::default())],
                    w,
                    &[],
                    &[],
                ));
            } else {
                lines.push(Line::from(Span::raw(truncate(&l.replace('\t', "    "), w))));
            }
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

impl JobsView {
    /// What the dialog draws for this view: title, body lines, the rows pinned under it, the line
    /// each job starts at and the one to keep in sight.
    pub(crate) fn modal_body(
        &self,
        inner_w: usize,
        theme: &Theme,
    ) -> (String, Vec<Line<'static>>, Vec<Line<'static>>, Vec<usize>, usize) {
        let (lines, starts, at) = self.lines(inner_w, theme);
        let job_rows = starts;
        let job_at = at;
        let pinned = vec![Line::default(), self.keys(inner_w, theme)];
        (self.title(), lines, pinned, job_rows, job_at)
    }

    /// The scroll offset of the dialog's body of `body_len` lines shown in `body_h` rows.
    pub(crate) fn modal_scroll(&mut self, job_at: usize, body_h: usize, body_len: usize) -> usize {
        if !self.detail {
            // the selected job and its lines stay in view
            let end = job_at + 1 + 2;
            self.scroll = end.saturating_sub(body_h);
        }
        self.scroll = self.scroll.min(body_len.saturating_sub(body_h));
        self.scroll
    }

    /// What makes this dialog the same dialog from frame to frame.
    pub(crate) fn size_hash(&self, h: &mut impl std::hash::Hasher) {
        std::hash::Hash::hash(&(7u8, self.is_empty()), h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removed_monitor_closes_detail_instead_of_showing_another() {
        let job = |id| JobInfo {
            id,
            kind: JobKind::Shell,
            label: format!("job {id}"),
            status: JobStatus::Running,
            elapsed_ms: 0,
            lines: 0,
            held: false,
        };
        let mut view = JobsView::monitors(vec![job(1), job(2)], vec![]);
        view.key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(view.current().unwrap().id, 2);
        assert!(view.detail);
        view.set_jobs(vec![job(1), job(2), job(3)]);
        assert!(view.detail);
        assert_eq!(view.current().unwrap().id, 2);
        view.set_jobs(vec![job(1), job(3)]);
        assert!(!view.detail);
        assert_eq!(view.scroll, 0);
    }

    #[test]
    fn job_detail_wraps_and_preserves_the_full_command() {
        let command = "cargo test --workspace --all-features -- --nocapture";
        let job = JobInfo {
            id: 12,
            kind: JobKind::Shell,
            label: command.into(),
            status: JobStatus::Running,
            elapsed_ms: 0,
            lines: 0,
            held: false,
        };
        let view = JobsView::new(vec![job]);
        let lines = view.detail_lines(22, &Theme::dark());
        // Ratatui spans include leading/continuation padding; assert the rendered
        // command by concatenating only non-whitespace content from each row.
        let shown = lines
            .iter()
            .map(|line| line.spans.iter().map(|span| span.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>();
        let expected = command.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        let first_command_row = shown[0].split_once("  ").map(|(_, text)| text).unwrap_or(&shown[0]);
        let actual = std::iter::once(first_command_row)
            .chain(shown.iter().skip(1).take_while(|line| line.starts_with("        ")).map(String::as_str))
            .flat_map(str::chars)
            .filter(|c| !c.is_whitespace())
            .collect::<String>();
        assert_eq!(actual, expected, "{shown:?}");
        assert!(lines.len() > 2, "command should wrap across rows");
    }

    #[test]
    fn monitor_detail_preserves_long_output_and_blank_lines() {
        let job = JobInfo {
            id: 7,
            kind: JobKind::Shell,
            label: "watch.sh".into(),
            status: JobStatus::Running,
            elapsed_ms: 0,
            lines: 3,
            held: false,
        };
        let output = format!("{}END\n\n构建完成", "x".repeat(90));
        let mut view = JobsView::monitors(vec![job], vec![JobTail { id: 7, ended_ago_ms: None, text: output.clone() }]);
        view.key(KeyEvent::from(KeyCode::Enter));
        let theme = Theme::dark();
        for w in [20, 40, 80] {
            let lines = view.detail_lines(w, &theme);
            let body = &lines[lines.iter().position(|l| l.to_string() == "─".repeat(w)).unwrap() + 1..];
            assert!(body.iter().all(|line| width(&line.to_string()) <= w));
            assert_eq!(body.iter().map(|line| line.to_string()).collect::<String>(), output.replace('\n', ""));
            assert!(body.iter().any(|line| line.to_string().is_empty()), "blank line must survive");
        }
        // Reading the retained output must not expose a destructive stop key.
        assert!(matches!(view.key(KeyEvent::from(KeyCode::Char('x'))), ModalOut::Nothing));
    }

    #[test]
    fn a_wide_label_pads_by_cells_so_the_status_stays_inside_the_row() {
        let job = JobInfo {
            id: 3,
            kind: JobKind::Shell,
            label: "echo 构建完成 构建完成 🎉🎉".into(),
            status: JobStatus::Running,
            elapsed_ms: 12_000,
            lines: 0,
            held: false,
        };
        let theme = Theme::dark();
        for w in [30, 40, 60] {
            let line = JobsView::new(vec![job.clone()]).head(&job, w, &theme, false);
            let cells: usize = line.spans.iter().map(|s| width(&s.content)).sum();
            assert_eq!(cells, w, "width {w}");
        }
    }
}
