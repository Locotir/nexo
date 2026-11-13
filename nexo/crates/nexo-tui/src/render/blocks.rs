//! The transcript's blocks and how each one becomes lines.

use std::time::Instant;

use nexo_core::agent::Level;
use nexo_core::pricing::Pricing;
use nexo_core::stream::Usage;
use nexo_core::tool::{TodoItem, TodoStatus, ToolDisplay, ToolOutput};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::theme::{mix, Theme};

use super::code;
use super::diff;
use super::markdown;
use super::wrap::{truncate, width, wrap_plain, wrap_plain_ex};
use crate::regions::{Kind, Meta, Region};

/// The goal loop's closing line of a reply (`GOAL DONE`, `GOAL WAIT`), read as the agent loop reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalSignal {
    Done,
    Wait,
}

/// A reply without its closing `GOAL DONE` / `GOAL WAIT` line, and which one it was. While the reply
/// streams in, a last line that is still on its way to one of them (`GOAL`, `GOAL W`) is held back too,
/// so the bare words never flash on screen.
pub fn goal_signal(text: &str, streaming: bool) -> (&str, Option<GoalSignal>) {
    let body = text.trim_end();
    let cut = body.rfind('\n').map_or(0, |i| i + 1);
    let last = body[cut..].trim().trim_end_matches(['.', '!', '*', '`', '_']).trim_start_matches(['*', '`', '_']);
    let up = last.to_ascii_uppercase();
    let sig = match up.as_str() {
        "GOAL DONE" => Some(GoalSignal::Done),
        "GOAL WAIT" => Some(GoalSignal::Wait),
        _ => None,
    };
    let partial = streaming && !up.is_empty() && ("GOAL DONE".starts_with(&up) || "GOAL WAIT".starts_with(&up));
    if sig.is_some() || partial {
        (body[..cut].trim_end(), sig)
    } else {
        (text, None)
    }
}

pub const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    Running,
    Done,
    Failed,
    Denied,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Block {
    User {
        text: String,
        /// `HH:MM` when the message was sent from this interface (None on a replay).
        at: Option<String>,
    },
    /// A message just sent, shown dim until the agent echoes it back (which turns it into a `User`).
    Pending {
        text: String,
    },
    Assistant {
        text: String,
        streaming: bool,
        /// `HH:MM` when the reply began in this interface (None on a replay).
        at: Option<String>,
        /// When it began, to time it.
        started: Option<Instant>,
        /// How long the reply took, once finished.
        took_ms: Option<u64>,
    },
    Reasoning {
        text: String,
        streaming: bool,
        started: Option<Instant>,
        elapsed_ms: Option<u64>,
        folded: bool,
    },
    Tool {
        id: String,
        name: String,
        summary: String,
        args: Value,
        status: ToolStatus,
        progress: Vec<String>,
        output: Option<ToolOutput>,
        started: Option<Instant>,
        elapsed_ms: Option<u64>,
        folded: bool,
    },
    /// The summary line of a run of tool calls: the `len` blocks after it (cards, and the thinking
    /// between them) belong to it. It is built from them when drawn, so it has no text of its own.
    Group {
        folded: bool,
        len: usize,
    },
    Notice {
        level: Level,
        text: String,
    },
    /// A failure as a card of three rows: what failed, the likely cause, what to do first. The rest
    /// (other steps, the provider's own text) opens on demand; `count` is how often it came in a turn.
    Problem {
        problem: Box<nexo_core::Problem>,
        count: u32,
        folded: bool,
    },
    /// A message between sessions, as the person sees it: `✉ -> name: text` with how far it got.
    Mail {
        id: String,
        outgoing: bool,
        peer: String,
        text: String,
        stage: nexo_core::mail::Stage,
    },
    /// What nexo added to a message of the person beyond their words: one dim line, the texts when open.
    Added {
        parts: Vec<nexo_core::agent::AddedPart>,
        folded: bool,
    },
    /// A newer nexo is waiting on `main`: a framed card that is hard to miss.
    Update {
        behind: u32,
    },
    Usage {
        usage: Usage,
        cost_usd: f64,
        elapsed_ms: u64,
        tally: super::summary::TurnTally,
        /// The provider is a subscription: the cost is API value, included in the plan.
        plan: bool,
        /// `[ui] detail = "quiet"`: only the time and the cost show; open, the whole line does.
        folded: bool,
    },
    Shell {
        command: String,
        output: String,
        exit_code: Option<i32>,
    },
    /// What is known of the model in use.
    Model {
        provider: String,
        model: String,
        pricing: Option<Pricing>,
        context_window: Option<u32>,
        max_output: Option<u32>,
        source: String,
    },
    /// The banner of an empty session; it leaves at the first message.
    Welcome {
        provider: String,
        model: String,
        cwd: String,
        branch: Option<String>,
        permission_mode: String,
    },
}

/// How far a message got, in the words of the transcript line.
pub fn mail_tail(outgoing: bool, stage: nexo_core::mail::Stage) -> String {
    use nexo_core::mail::Stage;
    match (stage, outgoing) {
        (Stage::Sent, true) => "sent".into(),
        (Stage::Sent, false) => "waiting".into(),
        (Stage::Delivered, _) => "✓ delivered".into(),
        (Stage::Read, _) => "✓✓ read".into(),
        (s, _) => s.label().into(),
    }
}

/// What the renderer needs to know besides the block.
#[derive(Debug, Clone, Copy)]
pub struct RenderOpts {
    pub show_reasoning: bool,
    pub fold_after: usize,
    pub spinner: usize,
    /// Milliseconds since the interface opened while the banner's mark is still lighting up.
    pub intro_ms: Option<u64>,
    /// Code and files shown for what they mean: Markdown rendered, JSON indented, tables.
    pub interpreted: bool,
    /// The `HH:MM` (and a reply's duration) at the right of a head line, when the block has them.
    pub timestamps: bool,
    /// Colour motion is on: the caret breathes and the thinking header is in its active colour.
    pub fx: bool,
    /// `[ui] detail`: `quiet` leaves the sentence of commands out of the welcome box.
    pub detail: nexo_core::config::Detail,
}

impl Block {
    pub fn is_foldable(&self) -> bool {
        matches!(self, Block::Tool { .. } | Block::Reasoning { .. } | Block::Group { .. } | Block::Added { .. })
            || matches!(self, Block::Problem { problem, .. } if problem.detail.is_some() || problem.steps.len() > 1)
    }

    pub fn toggle_fold(&mut self) -> bool {
        match self {
            Block::Tool { folded, .. }
            | Block::Reasoning { folded, .. }
            | Block::Group { folded, .. }
            | Block::Added { folded, .. }
            | Block::Usage { folded, .. }
            | Block::Problem { folded, .. } => {
                *folded = !*folded;
                true
            }
            _ => false,
        }
    }

    pub fn set_fold(&mut self, v: bool) {
        if let Block::Tool { folded, .. }
        | Block::Reasoning { folded, .. }
        | Block::Group { folded, .. }
        | Block::Added { folded, .. }
        | Block::Problem { folded, .. } = self
        {
            *folded = v;
        }
    }

    /// Whether this block changes between frames by itself (a spinner or a running clock).
    pub fn is_live(&self) -> bool {
        match self {
            Block::Tool { status, .. } => matches!(status, ToolStatus::Running | ToolStatus::Pending),
            Block::Reasoning { streaming, .. } | Block::Assistant { streaming, .. } => *streaming,
            _ => false,
        }
    }

    /// The plain text a person would want to copy.
    pub fn copy_text(&self) -> Option<String> {
        match self {
            Block::Assistant { text, .. } => Some(text.clone()),
            Block::User { text, .. } | Block::Pending { text } => Some(text.clone()),
            Block::Tool { output: Some(o), .. } => Some(o.content.clone()),
            Block::Shell { output, .. } => Some(output.clone()),
            Block::Problem { problem, .. } => Some(problem.plain()),
            _ => None,
        }
    }

    pub fn render(&self, w: usize, theme: &Theme, o: RenderOpts) -> Vec<Line<'static>> {
        self.render_meta(w, theme, o).0
    }

    /// The lines of the block and the boxes and wrapped rows among them.
    pub fn render_meta(&self, w: usize, theme: &Theme, o: RenderOpts) -> (Vec<Line<'static>>, Meta) {
        let w = w.max(8);
        let mut meta = Meta::default();
        let lines = match self {
            Block::User { text, .. } if is_job_note(text) => job_card(text, w, theme),
            Block::User { text, at } => {
                let bar = Span::styled("▌ ", theme.style(theme.user));
                let (mut lines, conts) = wrap_plain_ex(text, w, Style::default(), &[bar]);
                meta.conts = conts;
                if let (true, Some(at), Some(first)) = (o.timestamps, at, lines.first_mut()) {
                    stamp_right(first, w, at, theme);
                }
                // the whole message on a band of the person's colour, edge to edge
                let band = theme.user_band();
                for l in &mut lines {
                    l.style = band.patch(l.style);
                    for s in &mut l.spans {
                        s.style = band.patch(s.style);
                    }
                }
                lines
            }
            Block::Pending { text } => {
                let dim = theme.style(theme.dim2);
                let bar = Span::styled("▌ ", dim);
                let (lines, conts) = wrap_plain_ex(text, w, dim, &[bar]);
                meta.conts = conts;
                lines
            }
            Block::Assistant { text, streaming, at, took_ms, .. } => {
                // the loop's `GOAL DONE` / `GOAL WAIT` line is for the agent loop, not the person: it is
                // shown as a badge in the goal's colours instead of the bare words
                let (text, signal) = goal_signal(text, *streaming);
                let (mut lines, m) = markdown::render_as_meta(text, w.saturating_sub(2), theme, o.interpreted);
                meta = m;
                meta.add_deco(2);
                if lines.is_empty() {
                    lines.push(Line::default());
                }
                // a blinking caret where the next words will land
                if *streaming {
                    let on = (o.spinner / 4).is_multiple_of(2);
                    let caret = if o.fx {
                        crate::fx_stream::caret_style(theme, o.spinner)
                    } else if on {
                        theme.style(theme.accent)
                    } else {
                        theme.style(theme.dim2)
                    };
                    // with motion on, a caret that would not fit on the line is left out: it never wraps
                    if let Some(last) = lines.last_mut() {
                        let used: usize = last.spans.iter().map(|s| width(&s.content)).sum();
                        if !o.fx || used < w.saturating_sub(2) {
                            last.spans.push(Span::styled("▍", caret));
                        }
                    }
                }
                if let Some(sig) = signal {
                    let (icon, label, colour) = match sig {
                        GoalSignal::Done => ("✓", "goal met", theme.success),
                        GoalSignal::Wait => ("⏸", "goal paused · waiting for your answer", theme.warn),
                    };
                    if lines.last().is_some_and(|l| l.spans.iter().any(|s| !s.content.trim().is_empty())) {
                        lines.push(Line::default());
                    }
                    lines.push(Line::from(vec![
                        Span::styled(format!("{icon} "), theme.bold(colour)),
                        Span::styled(label, theme.style(colour)),
                    ]));
                }
                for (i, l) in lines.iter_mut().enumerate() {
                    let marker =
                        if i == 0 { Span::styled("◆ ", theme.style(theme.assistant)) } else { Span::raw("  ") };
                    l.spans.insert(0, marker);
                }
                if let (true, Some(at), Some(first)) = (o.timestamps, at, lines.first_mut()) {
                    let label = match took_ms {
                        Some(ms) => format!("{at} · {}", fmt_ms(*ms)),
                        None => at.clone(),
                    };
                    meta.stamped = stamp_right(first, w, &label, theme);
                }
                lines
            }
            Block::Added { parts, folded } => super::added::lines(parts, *folded, w, theme),
            Block::Reasoning { text, streaming, started, elapsed_ms, folded } => {
                if !o.show_reasoning {
                    return (Vec::new(), meta);
                }
                let n = text.lines().count();
                let elapsed = match (elapsed_ms, started) {
                    (Some(ms), _) => format!(" · {}", fmt_ms(*ms)),
                    (None, Some(s)) if *streaming => format!(" · {}", fmt_secs(s.elapsed().as_secs())),
                    _ => String::new(),
                };
                let style = theme.dim();
                let head_style =
                    if *streaming && o.fx { theme.style(crate::fx_stream::think_active(theme)) } else { style };
                // quiet: the line count is left out, the time stays
                let lines_said = match o.detail {
                    nexo_core::config::Detail::Quiet => String::new(),
                    nexo_core::config::Detail::Full => format!(" · {}", nexo_core::count(n, "line")),
                };
                let head_text = if *streaming {
                    format!("{} thinking{lines_said}{elapsed}", SPINNER[o.spinner % SPINNER.len()])
                } else if *folded {
                    format!("▸ thinking{lines_said}{elapsed}")
                } else {
                    format!("▾ thinking{lines_said}{elapsed}")
                };
                let mut lines = vec![Line::from(Span::styled(head_text, head_style))];
                if !*folded || *streaming {
                    let body = if *streaming && *folded { tail(text, 3) } else { text.clone() };
                    let guide = [Span::styled(GUIDE, theme.dim2())];
                    let (b, mut m) = markdown::md_lines_meta(&body, w, theme, style, true, &guide);
                    if !b.is_empty() {
                        m.shift(1);
                        m.regions.push(Region::new(Kind::Think, 1, b.len(), 2));
                        meta = m;
                    }
                    lines.extend(b);
                }
                close_guide(&mut lines);
                lines
            }
            // what nexo saved for later sessions: a card of its own, not a tool's output
            Block::Tool {
                status: ToolStatus::Done,
                output: Some(ToolOutput { display: ToolDisplay::Remembered { scope, items, path }, .. }),
                ..
            } => remembered_card(scope, items, path, theme, w),
            Block::Tool { name, summary, status, progress, output, started, elapsed_ms, folded, args, .. } => {
                let running = matches!(status, ToolStatus::Running | ToolStatus::Pending);
                let (icon, istyle) = match status {
                    ToolStatus::Pending => ("○", theme.dim()),
                    ToolStatus::Running => (SPINNER[o.spinner % SPINNER.len()], theme.style(theme.accent)),
                    ToolStatus::Done => ("✓", theme.style(theme.success)),
                    ToolStatus::Failed => ("✗", theme.style(theme.error)),
                    ToolStatus::Denied => ("⊘", theme.style(theme.warn)),
                };
                let elapsed = match (elapsed_ms, started, status) {
                    (Some(ms), _, _) => fmt_ms(*ms),
                    (None, Some(s), ToolStatus::Running) => fmt_secs(s.elapsed().as_secs()),
                    _ => String::new(),
                };
                let rail_color = match status {
                    ToolStatus::Pending => theme.dim2,
                    ToolStatus::Running => theme.accent,
                    ToolStatus::Done => theme.border,
                    ToolStatus::Failed => theme.error,
                    ToolStatus::Denied => theme.warn,
                };
                let body_w = w.saturating_sub(2);
                let pad: Vec<Span<'static>> = vec![Span::styled("│ ", theme.style(rail_color))];
                let mut body_meta = Meta::default();
                let body: Vec<Line<'static>> = match (status, output) {
                    (ToolStatus::Running | ToolStatus::Pending, _) => {
                        // open, a running command shows itself whole above its output
                        let mut lines = Vec::new();
                        if !*folded {
                            if let Some(c) = args.get("command").and_then(Value::as_str) {
                                lines.extend(command_lines(c, body_w, theme, &pad));
                            }
                        }
                        let keep =
                            if *folded { progress.len().min(o.fold_after.clamp(1, LIVE_TAIL)) } else { progress.len() };
                        let first = lines.len();
                        for l in &progress[progress.len() - keep..] {
                            push_plain(&mut lines, &mut body_meta, l, body_w, theme.dim(), &pad);
                        }
                        if lines.len() > first {
                            body_meta.regions.push(Region::new(Kind::Output, first, lines.len() - 1, 2));
                        }
                        lines
                    }
                    (_, Some(out)) => {
                        render_output(out, name, args, body_w, theme, &pad, o.interpreted, &mut body_meta)
                    }
                    (_, None) => Vec::new(),
                };
                // folded, a card says only what matters: failures and edits show their start,
                // a list of tasks shows itself, anything else is its head line alone
                let preview = if running || !*folded {
                    usize::MAX
                } else {
                    match (status, output.as_ref().map(|o| &o.display)) {
                        (ToolStatus::Failed | ToolStatus::Denied, _) => o.fold_after.clamp(1, FAILED_PREVIEW),
                        (_, Some(ToolDisplay::Diff { .. })) => o.fold_after.max(1),
                        // an update says what changed in its head; the list is one click away
                        (_, Some(ToolDisplay::Todo { .. })) => 0,
                        // the answer to a question is short and is the point of the card
                        _ if name == "ask_user" => usize::MAX,
                        _ => 0,
                    }
                };
                let stat = match (status, output) {
                    (ToolStatus::Denied, _) => " · denied".to_string(),
                    (_, Some(ToolOutput { display: ToolDisplay::File { text, .. }, .. })) => {
                        format!(" · {}", nexo_core::count(text.lines().count(), "line"))
                    }
                    (_, Some(ToolOutput { display: ToolDisplay::Diff { old, new, .. }, .. })) => {
                        let (a, d) = diff::stats(old, new);
                        format!(" · +{a} −{d}")
                    }
                    (_, Some(ToolOutput { display: ToolDisplay::Todo { items }, .. })) => {
                        let (done, total) = todo_counts(items);
                        format!(" · {done}/{total} done")
                    }
                    (_, Some(ToolOutput { display: ToolDisplay::Shell { exit_code: Some(c), .. }, .. })) if *c != 0 => {
                        format!(" · exit {c}")
                    }
                    (_, Some(_)) if preview == 0 && !body.is_empty() => {
                        format!(" · {}", nexo_core::count(body.len(), "line"))
                    }
                    _ => String::new(),
                };
                // a summary that already says it (`todo: 1/3 done`) is not repeated
                let stat = if !stat.is_empty() && summary.contains(stat.trim_start_matches(" · ")) {
                    String::new()
                } else {
                    stat
                };
                // the summary usually starts with the tool's name (`read a.rs`, `bash: ls`): then the
                // name is shown once, bold, and the rest follows it
                let rest = summary
                    .strip_prefix(name.as_str())
                    .map(|r| r.trim_start_matches(':').trim_start())
                    .unwrap_or(summary.as_str());
                // a chevron says there is more behind the card, and which way it folds
                let chevron = if running {
                    ""
                } else if body.len() > preview {
                    " ▸"
                } else if !*folded && body.len() > o.fold_after.min(FAILED_PREVIEW) {
                    " ▾"
                } else {
                    ""
                };
                let mut head = vec![Span::styled(format!("{icon} "), istyle)];
                // a sub-agent shows as a small robot (plain ASCII, any terminal) that blinks while it works
                // the shell tool goes by the shell that really runs it (`sh`, `bash`, `pwsh`)
                let shell;
                let label;
                let shown = if name == "task" {
                    head.push(Span::styled(format!("{} ", robot(running, o.spinner)), theme.style(theme.accent2)));
                    "agent"
                } else if name == "bash" {
                    shell = nexo_core::shell::shell().label();
                    shell.as_str()
                } else {
                    label = crate::replay::tool_label(name);
                    label.as_str()
                };
                if matches!(status, ToolStatus::Running) {
                    head.extend(shimmer(shown, o.spinner, theme));
                } else {
                    head.push(Span::styled(shown.to_string(), Style::default().add_modifier(Modifier::BOLD)));
                }
                head.push(Span::raw(" "));
                let used: usize = head.iter().map(|s| width(&s.content)).sum::<usize>()
                    + width(&elapsed)
                    + width(&stat)
                    + width(chevron)
                    + 2;
                head.push(Span::raw(truncate(rest, w.saturating_sub(used).max(4))));
                if !stat.is_empty() {
                    head.push(Span::styled(stat, theme.dim()));
                }
                if !elapsed.is_empty() {
                    head.push(Span::styled(format!("  {elapsed}"), theme.dim()));
                }
                if !chevron.is_empty() {
                    head.push(Span::styled(chevron, theme.dim()));
                }
                let mut lines = vec![Line::from(head)];
                if body.len() > preview {
                    let hidden = body.len() - preview;
                    if preview > 0 {
                        body_meta.truncate(preview);
                        meta.append(body_meta, 1);
                        lines.extend(body.into_iter().take(preview));
                        lines.push(Line::from(vec![
                            Span::styled("│ ", theme.style(rail_color)),
                            Span::styled(
                                format!("▸ {hidden} more {}", if hidden == 1 { "line" } else { "lines" }),
                                theme.dim(),
                            ),
                        ]));
                    }
                } else {
                    meta.append(body_meta, 1);
                    lines.extend(body);
                }
                close_guide(&mut lines);
                lines
            }
            // drawn by the app, which has the blocks it summarises
            Block::Group { .. } => Vec::new(),
            Block::Notice { level, text } => {
                // quiet: the curator's note on the trimmed context is left out (the gauge shows the load);
                // only this info note, never a warning or an error
                if o.detail == nexo_core::config::Detail::Quiet
                    && matches!(level, Level::Info)
                    && text.starts_with(CURATOR_PREFIX)
                {
                    return (Vec::new(), meta);
                }
                let (icon, style) = match level {
                    Level::Info => ("◇ ", theme.style(theme.info)),
                    Level::Warn => ("▲ ", theme.style(theme.warn)),
                    Level::Error => ("✗ ", theme.style(theme.error)),
                };
                // a notice of several lines (`/status`, `/tools`): the icon on the first, the
                // rest indented under it, in the text colour so the facts read as a list
                let text = user_notice(text);
                let mut parts = text.split('\n');
                // a long first line wraps under its text, not under a second icon
                let first = super::markdown::inline_runs(parts.next().unwrap_or(""), theme, style);
                let mut lines =
                    crate::render::wrap::wrap_runs(&first, w, &[Span::styled(icon, style)], &[Span::raw("  ")]);
                for rest in parts {
                    let runs = super::markdown::inline_runs(rest, theme, Style::default().fg(theme.fg));
                    lines.extend(crate::render::wrap::wrap_runs(&runs, w, &[Span::raw("  ")], &[Span::raw("  ")]));
                }
                lines
            }
            Block::Problem { problem, count, folded } => {
                let (lines, conts) = super::problem::card(problem, *count, !*folded, w, theme);
                meta.conts = conts;
                lines
            }
            Block::Mail { outgoing, peer, text, stage, .. } => {
                let first: String = text.lines().next().unwrap_or("").chars().take(200).collect();
                let arrow = if *outgoing { "->" } else { "<-" };
                let line = format!("✉ {arrow} {peer}: {first}");
                let tail = mail_tail(*outgoing, *stage);
                let mut lines = wrap_plain(&line, w.saturating_sub(tail.chars().count() + 3).max(8), theme.dim(), &[]);
                let color = if *stage == nexo_core::mail::Stage::Read { theme.success } else { theme.dim };
                if let Some(last) = lines.last_mut() {
                    last.spans.push(Span::styled(format!(" · {tail}"), theme.style(color)));
                }
                lines
            }
            Block::Update { behind } => update_card(*behind, theme, w),
            Block::Usage { usage, cost_usd, elapsed_ms, tally, plan, folded } => {
                // quiet: time and cost; the whole line opens with the block's fold
                let line = if *folded && o.detail == nexo_core::config::Detail::Quiet {
                    super::summary::short_line(theme, *cost_usd, *elapsed_ms, *plan, w)
                } else {
                    super::summary::summary_line_for(theme, usage, *cost_usd, *elapsed_ms, tally, w, *plan)
                };
                let text = super::summary::plain(&line);
                if width(&text) <= w {
                    vec![line]
                } else {
                    // narrower than the shortest form: wrap under the first word instead of cutting a figure
                    wrap_plain(text.trim(), w, theme.dim(), &[Span::raw("  ")])
                }
            }
            Block::Model { provider, model, pricing, context_window, max_output, source } => {
                let mut parts = vec![format!("{provider}/{model}")];
                if let Some(c) = context_window {
                    parts.push(format!("{} context", fmt_k(*c as u64)));
                }
                if let Some(m) = max_output {
                    parts.push(format!("{} max reply", fmt_k(*m as u64)));
                }
                match pricing {
                    Some(p) if p.is_known() => {
                        parts.push(format!("${} in", fmt_price(p.input)));
                        parts.push(format!("${} out", fmt_price(p.output)));
                        if p.cache_read > 0.0 {
                            parts.push(format!("${} cached", fmt_price(p.cache_read)));
                        }
                        if p.cache_write > 0.0 {
                            parts.push(format!("${} cache write", fmt_price(p.cache_write)));
                        }
                    }
                    _ => parts.push("no prices known (/pricing in=… out=… cache=…)".to_string()),
                }
                if !source.is_empty() {
                    parts.push(format!("from {source}"));
                }
                let text = format!("model {}", parts.join(" · "));
                crate::render::wrap::wrap_runs(
                    &[(text, theme.dim())],
                    w,
                    &[Span::styled("◈ ", theme.style(theme.accent))],
                    &[Span::raw("  ")],
                )
            }
            Block::Welcome { provider, model, cwd, branch, permission_mode } => {
                let place = match branch {
                    Some(b) => format!("{cwd} · {b}"),
                    None => cwd.clone(),
                };
                welcome(w, theme, &format!("{provider}/{model}"), &place, permission_mode, o.intro_ms, o.detail)
            }
            Block::Shell { command, output, exit_code } => {
                let mut lines = vec![Line::from(vec![
                    Span::styled("$ ", theme.style(theme.accent)),
                    Span::styled(command.clone(), Style::default().add_modifier(Modifier::BOLD)),
                    // the code is information; only a command that was not found, not runnable or killed by a
                    // signal is shown as a failure
                    Span::styled(
                        exit_code.filter(|c| *c != 0).map(|c| format!("  exit {c}")).unwrap_or_default(),
                        theme.style(if exit_code.is_some_and(nexo_core::shell::exit_is_failure) {
                            theme.error
                        } else {
                            theme.dim
                        }),
                    ),
                ])];
                let pad = [Span::styled(GUIDE, theme.style(theme.border))];
                let is_diff = command.contains("diff");
                // a command the user ran keeps its first lines and says how many it hides; a
                // block with no exit code (the system prompt) is there to be read whole
                let limit = (exit_code.is_some()).then(|| o.fold_after.max(1) * 2);
                let all: Vec<&str> = output.trim_end_matches('\n').split('\n').collect();
                let shown = limit.map_or(all.len(), |l| all.len().min(l));
                for raw in &all[..shown] {
                    let style = if is_diff { diff_line_style(raw, theme) } else { Style::default() };
                    push_plain(&mut lines, &mut meta, raw, w.saturating_sub(2), style, &pad);
                }
                meta.regions.push(Region::new(Kind::Output, 1, lines.len() - 1, 2));
                keep_source(&mut meta, 1, lines.len(), &all[..shown].join("\n"));
                if shown < all.len() {
                    lines.push(Line::from(vec![
                        Span::styled(GUIDE, theme.style(theme.border)),
                        Span::styled(format!("▸ +{} lines", all.len() - shown), theme.dim()),
                    ]));
                }
                close_guide(&mut lines);
                lines
            }
        };
        (lines, meta)
    }
}

/// The guide down the left of a body, under its head.
const GUIDE: &str = "│ ";

/// The guide of a body ends in a corner on its last line, so the eye sees where the
/// result stops and the next thing starts.
fn close_guide(lines: &mut [Line<'static>]) {
    if lines.len() < 2 {
        return;
    }
    if let Some(first) = lines.last_mut().and_then(|l| l.spans.first_mut()) {
        if first.content == GUIDE {
            first.content = "└ ".into();
        }
    }
}

/// The glyph and style of one task of the todo list: `○` pending, `◐` in
/// progress (accent), `●` done (dim, struck through where the terminal can).
pub fn todo_style(status: TodoStatus, theme: &Theme) -> (&'static str, Style) {
    match status {
        TodoStatus::Pending => ("○", theme.todo_pending()),
        TodoStatus::InProgress => ("◐", theme.todo_running()),
        TodoStatus::Done => ("●", theme.todo_done()),
    }
}

/// `(done, total)` of a todo list.
pub fn todo_counts(items: &[TodoItem]) -> (usize, usize) {
    (items.iter().filter(|i| i.status == TodoStatus::Done).count(), items.len())
}

/// A price per million tokens, as short as it reads.
pub fn fmt_price(p: f64) -> String {
    if p >= 10.0 {
        format!("{p:.0}")
    } else if p >= 1.0 {
        format!("{p:.2}")
    } else {
        let s = format!("{p:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

pub fn fmt_k(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 100_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else if n < 1_000_000 {
        format!("{}k", n / 1000)
    } else if n < 1_000_000_000 {
        format!("{:.2}M", n as f64 / 1_000_000.0)
    } else {
        format!("{:.2}B", n as f64 / 1_000_000_000.0)
    }
}

mod banner;
mod jobs;
mod live_marks;
mod notice_cards;
mod output;

pub use banner::*;
pub use jobs::*;
pub use live_marks::*;
pub use notice_cards::*;
use output::*;

#[cfg(test)]
mod tests;
