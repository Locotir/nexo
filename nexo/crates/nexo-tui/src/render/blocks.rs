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
}

impl Block {
    pub fn is_foldable(&self) -> bool {
        matches!(self, Block::Tool { .. } | Block::Reasoning { .. } | Block::Group { .. } | Block::Added { .. })
    }

    pub fn toggle_fold(&mut self) -> bool {
        match self {
            Block::Tool { folded, .. }
            | Block::Reasoning { folded, .. }
            | Block::Group { folded, .. }
            | Block::Added { folded, .. } => {
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
        | Block::Added { folded, .. } = self
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
            Block::User { text, .. } => Some(text.clone()),
            Block::Tool { output: Some(o), .. } => Some(o.content.clone()),
            Block::Shell { output, .. } => Some(output.clone()),
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
                    let on = (o.spinner / 6).is_multiple_of(2);
                    let caret = if on { theme.style(theme.accent) } else { theme.style(theme.dim2) };
                    if let Some(last) = lines.last_mut() {
                        last.spans.push(Span::styled("▍", caret));
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
                    stamp_right(first, w, &label, theme);
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
                    (None, Some(s)) if *streaming => format!(" · {}", fmt_ms(s.elapsed().as_millis() as u64)),
                    _ => String::new(),
                };
                let style = theme.dim();
                let head_text = if *streaming {
                    format!(
                        "{} thinking · {}{elapsed}",
                        SPINNER[o.spinner % SPINNER.len()],
                        nexo_core::count(n, "line")
                    )
                } else if *folded {
                    format!("▸ thinking · {}{elapsed}", nexo_core::count(n, "line"))
                } else {
                    format!("▾ thinking · {}{elapsed}", nexo_core::count(n, "line"))
                };
                let mut lines = vec![Line::from(Span::styled(head_text, style))];
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
                    (None, Some(s), ToolStatus::Running) => fmt_ms(s.elapsed().as_millis() as u64),
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
                        (_, Some(ToolDisplay::Todo { .. })) => usize::MAX,
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
                            Span::styled(format!("▸ {hidden} more lines"), theme.dim()),
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
            Block::Usage { usage, cost_usd, elapsed_ms, tally, plan } => {
                let line = super::summary::summary_line_for(theme, usage, *cost_usd, *elapsed_ms, tally, w, *plan);
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
                welcome(w, theme, &format!("{provider}/{model}"), &place, permission_mode, o.intro_ms)
            }
            Block::Shell { command, output, exit_code } => {
                let mut lines = vec![Line::from(vec![
                    Span::styled("$ ", theme.style(theme.accent)),
                    Span::styled(command.clone(), Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(
                        exit_code.filter(|c| *c != 0).map(|c| format!("  exit {c}")).unwrap_or_default(),
                        theme.style(theme.error),
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

/// The mark of nexo: four nodes tied to one centre, the link the name means.
const LOGO: [&str; 3] = ["●─╮ ╭─●", "  ╰●╯  ", "●─╯ ╰─●"];

/// How long the banner takes to come in when nexo opens (every part has settled by then).
pub const INTRO_MS: u64 = 1900;

/// The four outer nodes of the mark and when each is born, clockwise from the top left.
const NODES: [(usize, usize, u64); 4] = [(0, 0, 0), (0, 6, 110), (2, 6, 220), (2, 0, 330)];
/// A pulse leaves a node this long after it is born ...
const PULSE_AFTER: u64 = 180;
/// ... and crosses one cell of its tie per step, towards the centre.
const PULSE_STEP: u64 = 75;
/// The wave that leaves the centre once it lights moves one cell out per step.
const WAVE_STEP: u64 = 70;

/// How many cells the mark's cell at row `r`, column `c` is from the centre.
fn from_centre(r: usize, c: usize) -> u64 {
    (c as i64 - 3).unsigned_abs() + (r as i64 - 1).unsigned_abs()
}

/// When the last pulse reaches the centre and it lights.
fn centre_lit() -> u64 {
    NODES.iter().map(|n| n.2).max().unwrap_or(0) + PULSE_AFTER + 4 * PULSE_STEP
}

/// When the mark's cell at row `r`, column `c` first shows: a node when it is born, a tie
/// when the first pulse of a node on its side reaches it, the centre when the last one does.
fn cell_born(r: usize, c: usize) -> u64 {
    if (r, c) == (1, 3) {
        return centre_lit();
    }
    if let Some(n) = NODES.iter().find(|n| (n.0, n.1) == (r, c)) {
        return n.2;
    }
    NODES
        .iter()
        .filter(|n| (n.1 < 3) == (c < 3) && (r == 1 || r == n.0))
        .map(|n| n.2 + PULSE_AFTER + 4u64.saturating_sub(from_centre(r, c)) * PULSE_STEP)
        .min()
        .unwrap_or(0)
}

/// The mark's cell at row `r`, column `c`, `ms` into the intro: its glyph and colour.
/// The nodes are born one after another and grow from a dot; a pulse runs down each tie,
/// drawing it with a bright head; the centre, a faint seed until then, lights when the last
/// pulse arrives and sends a wave back out over the whole mark.
fn intro_cell(theme: &Theme, glyph: char, colour: Color, r: usize, c: usize, ms: u64) -> (char, Color) {
    let born = cell_born(r, c);
    let centre = (r, c) == (1, 3);
    if ms < born {
        return if centre { ('·', theme.dim2) } else { (' ', colour) };
    }
    let age = ms - born;
    let glyph = match (glyph, age) {
        ('●', 0..=59) => '·',
        ('●', 60..=129) => '•',
        _ => glyph,
    };
    let mut lit = if age < 90 { theme.fg } else { mix(theme.fg, colour, (age - 90) as f32 / 260.0) };
    let wave = centre_lit() + WAVE_STEP * from_centre(r, c);
    if !centre && (wave..wave + 240).contains(&ms) {
        lit = mix(theme.fg, lit, (ms - wave) as f32 / 240.0);
    }
    (glyph, lit)
}

/// `spans` as they are being typed, `ms` into the intro: the first character shows at
/// `start`, each next one `per` ms later, bright for a moment, then in its own style.
/// What is not typed yet takes no room (the row pads to its width anyway).
fn typed(spans: Vec<Span<'static>>, theme: &Theme, ms: u64, start: u64, per: u64) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut k = 0;
    for s in spans {
        let own = s.style.fg.unwrap_or(theme.fg);
        for ch in s.content.chars() {
            let at = start + k * per;
            k += 1;
            if ms < at {
                return out;
            }
            let t = (ms - at) as f32 / 200.0;
            let style = if t >= 1.0 { s.style } else { s.style.fg(mix(theme.accent, own, t)) };
            out.push(Span::styled(ch.to_string(), style));
        }
    }
    out
}

/// When each line of the banner's text starts to be typed, and the ms between characters.
const TYPE_TITLE: (u64, u64) = (560, 45);
const TYPE_MODEL: (u64, u64) = (900, 7);
const TYPE_PLACE: (u64, u64) = (1000, 7);
const TYPE_HINT: (u64, u64) = (1250, 4);
/// How long the frame of the banner takes to fade in.
const BORDER_MS: u64 = 350;

/// The banner of an empty session: the mark beside the version, the model and the place, then one line of hints.
fn welcome(
    w: usize,
    theme: &Theme,
    model: &str,
    place: &str,
    permissions: &str,
    intro_ms: Option<u64>,
) -> Vec<Line<'static>> {
    let inner = w.saturating_sub(4).clamp(20, 72);
    let border = match intro_ms {
        Some(ms) if ms < BORDER_MS => theme.style(mix(theme.bg, theme.border, ms as f32 / BORDER_MS as f32)),
        _ => theme.style(theme.border),
    };
    let type_in = |spans: Vec<Span<'static>>, (start, per): (u64, u64)| match intro_ms {
        Some(ms) => typed(spans, theme, ms, start, per),
        None => spans,
    };
    let row = |spans: Vec<Span<'static>>| -> Line<'static> {
        let used: usize = spans.iter().map(|s| width(&s.content)).sum();
        let mut v = vec![Span::styled("│ ", border)];
        v.extend(spans);
        v.push(Span::raw(" ".repeat(inner.saturating_sub(used))));
        v.push(Span::styled(" │", border));
        Line::from(v)
    };
    // Narrow boxes drop the mark and keep the text.
    let gutter = if inner >= 40 { width(LOGO[0]) + 3 } else { 0 };
    let text = inner - gutter;
    // the mode that never asks shows as YOLO, in red
    let (permissions, mode_style) = match permissions {
        "auto" => ("YOLO", theme.bold(theme.error)),
        p => (p, theme.dim()),
    };
    let info: [Vec<Span<'static>>; 3] = [
        vec![
            Span::styled("nexo", theme.bold(theme.accent)),
            Span::styled(format!(" {}", nexo_core::version::get()), theme.dim()),
        ],
        vec![
            Span::raw(truncate(model, text.saturating_sub(width(permissions) + 3))),
            Span::styled(" · ", theme.dim()),
            Span::styled(permissions.to_string(), mode_style),
        ],
        vec![Span::styled(truncate(place, text), theme.dim())],
    ];
    let mut lines = vec![Line::from(Span::styled(format!("╭{}╮", "─".repeat(inner + 2)), border))];
    for (i, spans) in info.into_iter().enumerate() {
        let mut v = Vec::new();
        if gutter > 0 {
            // The outer nodes in the second accent, the ties and the centre in the first.
            for (j, c) in LOGO[i].chars().enumerate() {
                let outer = c == '●' && i != 1;
                let colour = if outer { theme.accent2 } else { theme.accent };
                let (c, colour) = match intro_ms {
                    Some(ms) if c != ' ' => intro_cell(theme, c, colour, i, j, ms),
                    _ => (c, colour),
                };
                let style = if outer { theme.style(colour) } else { theme.bold(colour) };
                v.push(Span::styled(c.to_string(), style));
            }
            v.push(Span::raw("   "));
        }
        let pace = [TYPE_TITLE, TYPE_MODEL, TYPE_PLACE][i];
        let typing = intro_ms.is_some_and(|ms| i == 0 && ms >= pace.0 && ms < pace.0 + pace.1 * 12 + 150);
        v.extend(type_in(spans, pace));
        if typing {
            // the title is typed with a caret
            v.push(Span::styled("▍", theme.style(theme.accent)));
        }
        lines.push(row(v));
    }
    lines.push(row(vec![]));
    let hint = "/model sets up a model · /resume picks a session · ? help";
    lines.push(row(type_in(vec![Span::styled(truncate(hint, inner), theme.dim())], TYPE_HINT)));
    lines.push(Line::from(Span::styled(format!("╰{}╯", "─".repeat(inner + 2)), border)));
    lines
}

/// The colour of one line of a diff shown as shell output.
fn diff_line_style(line: &str, theme: &Theme) -> Style {
    if line.starts_with("+++")
        || line.starts_with("---")
        || line.starts_with("diff --git")
        || line.starts_with("index ")
    {
        theme.dim().add_modifier(Modifier::BOLD)
    } else if line.starts_with('+') {
        theme.diff_add_line()
    } else if line.starts_with('-') {
        theme.diff_del_line()
    } else if line.starts_with("@@") {
        theme.style(theme.hunk)
    } else {
        Style::default()
    }
}

#[allow(clippy::too_many_arguments)]
fn render_output(
    out: &ToolOutput,
    name: &str,
    args: &Value,
    w: usize,
    theme: &Theme,
    pad: &[Span<'static>],
    interpreted: bool,
    meta: &mut Meta,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    // a plan reads as the document it is, then what became of it
    if let Some(plan) = (name == "exit_plan").then(|| args.get("plan").and_then(Value::as_str)).flatten() {
        let (plan_lines, mut m) = markdown::render_as_meta(plan, w.saturating_sub(2), theme, true);
        m.regions.clear();
        m.add_deco(2);
        meta.append(m, 0);
        for l in plan_lines {
            let mut spans = pad.to_vec();
            spans.extend(l.spans);
            lines.push(Line::from(spans).style(l.style));
        }
        lines.push(Line::from(pad.to_vec()));
        push_plain(&mut lines, meta, &out.content, w.saturating_sub(2), theme.dim(), pad);
        meta.regions.push(Region::new(Kind::Plan, 0, lines.len() - 1, 2));
        return lines;
    }
    match &out.display {
        ToolDisplay::Todo { items } => {
            for it in items {
                let (icon, style) = todo_style(it.status, theme);
                let mut prefix = pad.to_vec();
                prefix.push(Span::styled(format!("{icon} "), style));
                push_plain(&mut lines, meta, &it.text, w.saturating_sub(2), style, &prefix);
            }
            boxed(meta, Kind::Output, 0, lines.len(), 2);
        }
        ToolDisplay::Diff { old, new, start_line, .. } => {
            let mut region = Region::new(Kind::Diff, 0, 0, 2);
            let (drows, dsrc) = diff::unified_rows(old, new, *start_line, theme);
            let nrows = drows.len();
            for (k, l) in drows.into_iter().enumerate() {
                // a row with a gutter and a sign is content; a hunk header or a rule is not
                if l.spans.len() == 2 {
                    if region.deco == 2 {
                        region.deco += width(&l.spans[0].content) as u16;
                    }
                } else {
                    region.skip.push(k as u32);
                }
                let mut spans = pad.to_vec();
                if let [gutter, text] = &l.spans[..] {
                    // a long row is shortened with an ellipsis, as a file's are
                    spans.push(gutter.clone());
                    spans.extend(super::wrap::clip_spans(vec![text.clone()], w.saturating_sub(width(&gutter.content))));
                } else {
                    spans.extend(l.spans);
                }
                lines.push(Line::from(spans));
            }
            region.end = lines.len().saturating_sub(1) as u32;
            if dsrc.len() == nrows
                && dsrc.iter().all(|s| super::wrap::clean(&s.replace('\t', "    ")) == s.replace('\t', "    "))
            {
                region.src = Some(std::sync::Arc::new(dsrc));
            }
            meta.regions.push(region);
        }
        ToolDisplay::File { path, text, start_line } => {
            let lang = code::lang_of_path(path);
            if let Some(view) =
                interpreted.then(|| code::interpret(lang, text.trim_end_matches('\n'), w, theme)).flatten()
            {
                // shown for what it means, the file loses its line numbers
                for l in view {
                    let mut spans = pad.to_vec();
                    spans.extend(l.spans);
                    lines.push(Line::from(spans).style(theme.code_block()));
                }
                boxed(meta, Kind::File, 0, lines.len(), 2);
                return lines;
            }
            let digits = (start_line + text.lines().count()).max(1).to_string().len();
            for (i, l) in code::highlight(text.trim_end_matches('\n'), lang, theme).into_iter().enumerate() {
                let mut spans = pad.to_vec();
                spans.push(Span::styled(format!("{:>digits$}│ ", start_line + i), theme.dim()));
                spans.extend(super::wrap::clip_spans(l.spans, w.saturating_sub(digits + 2)));
                // the file sits on the code background, like a fenced block
                lines.push(Line::from(spans).style(theme.code_block()));
            }
            if !lines.is_empty() {
                let mut r = Region::new(Kind::File, 0, lines.len() - 1, 2 + digits + 2);
                let src: Vec<String> = text.trim_end_matches('\n').split('\n').map(str::to_string).collect();
                if src.len() == lines.len() {
                    r.src = Some(std::sync::Arc::new(src));
                }
                meta.regions.push(r);
            }
        }
        ToolDisplay::Shell { command, stdout, stderr, .. } => {
            lines.extend(command_lines(command, w, theme, pad));
            let first = lines.len();
            let text = if stderr.trim().is_empty() {
                stdout.clone()
            } else if stdout.trim().is_empty() {
                stderr.clone()
            } else {
                format!("{stdout}\n{stderr}")
            };
            push_plain(&mut lines, meta, text.trim_end_matches('\n'), w, Style::default(), pad);
            boxed(meta, Kind::Output, first, lines.len(), 2);
            keep_source(meta, first, lines.len(), text.trim_end_matches('\n'));
        }
        ToolDisplay::List { items } | ToolDisplay::Remembered { items, .. } => {
            for it in items {
                push_plain(&mut lines, meta, it, w, Style::default(), pad);
            }
            boxed(meta, Kind::Output, 0, lines.len(), 2);
        }
        ToolDisplay::Plain => {
            let style = if out.is_error { theme.style(theme.error) } else { Style::default() };
            let text = if out.content.trim().is_empty() {
                if out.is_error {
                    "(error)".to_string()
                } else {
                    "(no output)".to_string()
                }
            } else {
                out.content.trim_end_matches('\n').to_string()
            };
            let _ = (name, args);
            if !out.is_error && super::markdown::looks_markdown(&text) {
                let (b, mut m) = markdown::md_lines_meta(&text, w, theme, style, false, pad);
                m.shift(lines.len());
                meta.append(m, 0);
                lines.extend(b);
            } else {
                push_plain(&mut lines, meta, &text, w, style, pad);
            }
            boxed(meta, Kind::Output, 0, lines.len(), 2);
        }
    }
    lines
}

/// The box that starts at row `from` keeps `raw` as its source when it maps one to one onto its rows
/// (nothing wrapped), so a copy has the text as it was written, tabs included.
fn keep_source(meta: &mut Meta, from: usize, to: usize, raw: &str) {
    if let Some(r) = meta.regions.iter_mut().rev().find(|r| r.start as usize == from) {
        let src: Vec<String> = raw.split('\n').map(str::to_string).collect();
        let plain = super::wrap::clean(&raw.replace('\t', "    ")) == raw.replace('\t', "    ");
        if plain && r.src.is_none() && src.len() == to - from {
            r.src = Some(std::sync::Arc::new(src));
        }
    }
}

/// Rows `from..to` of a body as one box of `kind`, `deco` columns of decoration deep.
fn boxed(meta: &mut Meta, kind: Kind, from: usize, to: usize, deco: usize) {
    if to > from {
        meta.regions.push(Region::new(kind, from, to - 1, deco));
    }
}

/// `text` wrapped under `prefix` and appended to `lines`, its wrapped rows noted in `meta`.
fn push_plain(
    lines: &mut Vec<Line<'static>>,
    meta: &mut Meta,
    text: &str,
    w: usize,
    style: Style,
    prefix: &[Span<'static>],
) {
    let (l, conts) = wrap_plain_ex(text, w, style, prefix);
    let base = lines.len() as u32;
    meta.conts.extend(conts.into_iter().map(|c| crate::regions::Cont { line: c.line + base, ..c }));
    lines.extend(l);
}

/// Lines of a folded running card's output that stay in sight.
const LIVE_TAIL: usize = 4;
/// Lines of a folded failure that stay in sight.
const FAILED_PREVIEW: usize = 4;

/// `$ command`, every line of it, wrapped to the width.
fn command_lines(command: &str, w: usize, theme: &Theme, pad: &[Span<'static>]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (i, l) in code::highlight(command.trim_end(), "bash", theme).into_iter().enumerate() {
        let mut first = pad.to_vec();
        first.push(Span::styled(if i == 0 { "$ " } else { "  " }, theme.style(theme.accent)));
        let mut cont = pad.to_vec();
        cont.push(Span::raw("  "));
        let runs: Vec<(String, Style)> = l.spans.into_iter().map(|s| (s.content.into_owned(), s.style)).collect();
        lines.extend(super::wrap::wrap_runs(&runs, w, &first, &cont));
    }
    lines
}

/// The sub-agent's face: eyes open, shut for a moment every two seconds while it works.
fn robot(running: bool, beat: usize) -> &'static str {
    if running && beat % 40 >= 37 {
        "[-_-]"
    } else {
        "[o_o]"
    }
}

/// `word` with a light running across it, frame by frame.
fn shimmer(word: &str, frame: usize, theme: &Theme) -> Vec<Span<'static>> {
    let n = word.chars().count() as f32;
    let pos = (frame as f32 * 0.6) % (n + 8.0) - 3.0;
    word.chars()
        .enumerate()
        .map(|(i, c)| {
            let near = (1.0 - ((i as f32 - pos).abs() / 2.5)).max(0.0);
            Span::styled(
                c.to_string(),
                Style::default().fg(mix(theme.fg, theme.accent, near)).add_modifier(Modifier::BOLD),
            )
        })
        .collect()
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Right-aligns a dim `label` on the head `line` of a block `w` wide; dropped when it would touch the text.
fn stamp_right(line: &mut Line<'static>, w: usize, label: &str, theme: &Theme) {
    let used: usize = line.spans.iter().map(|s| width(&s.content)).sum();
    let need = width(label) + 2;
    if used + need <= w {
        line.spans.push(Span::raw(" ".repeat(w - used - width(label))));
        line.spans.push(Span::styled(label.to_string(), theme.dim2()));
    }
}

pub fn fmt_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        fmt_secs(ms / 1000)
    }
}

/// A running clock in whole units: `42s`, `3m05s`, `1h02m`; it grows into minutes and hours by
/// itself, so a long turn never reads as thousands of seconds.
pub fn fmt_secs(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

const JOB_NOTE: &str = "[background jobs ended]\n";

/// Whether a user-role message is the note nexo-core sends when background jobs end.
pub fn is_job_note(text: &str) -> bool {
    text.starts_with(JOB_NOTE)
}

/// One ended job from the note: its id, what ran, whether it went well, and the facts.
struct JobRow<'a> {
    id: &'a str,
    what: &'a str,
    ok: bool,
    facts: String,
}

fn parse_job_row(line: &str) -> Option<JobRow<'_>> {
    let rest = line.strip_prefix("- job ")?;
    let (id, rest) = rest.split_once(' ')?;
    let (rest, lines) = rest.rsplit_once(", ")?;
    let n = lines.strip_suffix(" lines of output").or_else(|| lines.strip_suffix(" line of output"))?;
    let (rest, dur) = rest.rsplit_once(" after ")?;
    let (what, how) = if let Some(w) = rest.strip_suffix(" killed") {
        (w, "killed")
    } else {
        let i = rest.rfind(" done (")?;
        (&rest[..i], rest[i + 7..].strip_suffix(')')?)
    };
    let what = what.strip_prefix('`').and_then(|w| w.strip_suffix('`')).unwrap_or(what);
    let unit = if n == "1" { "line" } else { "lines" };
    Some(JobRow { id, what, ok: how == "exit 0", facts: format!("{how} · {dur} · {n} {unit}") })
}

/// The note as a quiet system card: a dim rule, then one row per job. Only the screen changes; the
/// model keeps the exact text.
fn job_card(text: &str, w: usize, theme: &Theme) -> Vec<Line<'static>> {
    let dim = theme.dim();
    let title = " jobs ended ";
    let rule = format!("──{title}{}", "─".repeat(w.saturating_sub(2 + width(title))));
    let mut lines = vec![Line::from(Span::styled(truncate(&rule, w), theme.style(theme.dim2)))];
    for raw in text.lines().skip(1) {
        // the note ends at its last line; a message glued after it is not a job row
        if raw.starts_with("Read the output with") {
            break;
        }
        if raw.trim().is_empty() {
            continue;
        }
        let Some(r) = parse_job_row(raw) else {
            lines.push(Line::from(Span::styled(truncate(raw, w), dim)));
            continue;
        };
        let mark = if r.ok { "✓" } else { "✗" };
        let mstyle = if r.ok { theme.style(theme.success) } else { theme.style(theme.error) };
        let head = format!("⚙ job {}  ", r.id);
        let tail = format!("{mark} {}", r.facts);
        let room = w.saturating_sub(width(&head) + width(&tail) + 2);
        let what = truncate(r.what, room);
        let gap = w.saturating_sub(width(&head) + width(&what) + width(&tail)).max(2);
        lines.push(Line::from(vec![
            Span::styled(head, dim),
            Span::raw(what),
            Span::raw(" ".repeat(gap)),
            Span::styled(mark.to_string(), mstyle),
            Span::styled(format!(" {}", r.facts), if r.ok { dim } else { mstyle }),
        ]));
    }
    lines
}

/// A notice as the user reads it: the curator's name stays inside, the screen says what happened.
/// The card for a newer nexo: a framed box in the accent colour, so it is not read past like a
/// grey notice. Too narrow for a frame, it falls back to two bold lines.
/// A `file://` address for a path, the form a Ctrl+click opens: forward slashes, spaces and other
/// characters a link cannot hold written as `%XX`.
pub fn file_url(path: &str) -> String {
    let path = path.replace('\\', "/");
    let mut url = String::from("file://");
    if !path.starts_with('/') {
        url.push('/');
    }
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' | b'@' | b'+' | b',' => {
                url.push(b as char)
            }
            _ => url.push_str(&format!("%{b:02X}")),
        }
    }
    url
}

/// What nexo saved for later sessions: `✦ remembered · this project`, the lines, and the file as a link.
fn remembered_card(scope: &str, items: &[String], path: &str, theme: &Theme, w: usize) -> Vec<Line<'static>> {
    let edge = theme.style(theme.accent2);
    let rail = || Span::styled("│ ", edge);
    let place = if scope == "project" { "this project" } else { "global" };
    let mut lines = vec![Line::from(vec![
        Span::styled("✦ ", edge),
        Span::styled("remembered", edge.add_modifier(Modifier::BOLD)),
        Span::styled(format!(" · {place}"), theme.dim()),
    ])];
    for it in items {
        lines.extend(crate::render::wrap::wrap_runs(
            &[(it.clone(), Style::default().fg(theme.fg))],
            w,
            &[rail(), Span::styled("• ", edge)],
            &[rail(), Span::raw("  ")],
        ));
    }
    // the address on one row, whole, so a Ctrl+click finds all of it
    lines.push(Line::from(vec![
        Span::styled("└ ", edge),
        Span::styled(file_url(path), Style::default().fg(theme.accent).add_modifier(Modifier::UNDERLINED)),
    ]));
    lines
}

fn update_card(behind: u32, theme: &Theme, w: usize) -> Vec<Line<'static>> {
    let edge = Style::default().fg(theme.accent2).add_modifier(Modifier::BOLD);
    let text = Style::default().fg(theme.fg);
    let cmd = Style::default().fg(theme.accent).add_modifier(Modifier::BOLD);
    let s = if behind == 1 { "" } else { "s" };
    let what = format!("{behind} new commit{s}, ready-built.");
    let run = [("Run ", text), ("nexo update", cmd), (", then restart nexo.", text)];
    let run_w: usize = run.iter().map(|(t, _)| width(t)).sum();
    let title = " ⬆ Update ready ";
    let inner = width(&what).max(run_w).max(width(title) + 1);
    if w < inner + 4 {
        let mut lines = wrap_plain(&format!("⬆ Update ready · {what}"), w, edge, &[Span::raw("  ")]);
        lines.extend(wrap_plain("Run `nexo update`, then restart nexo.", w, text, &[Span::raw("  ")]));
        return lines;
    }
    let row = |spans: Vec<Span<'static>>, used: usize| {
        let mut v = vec![Span::styled("│ ", edge)];
        v.extend(spans);
        v.push(Span::raw(" ".repeat(inner - used)));
        v.push(Span::styled(" │", edge));
        Line::from(v)
    };
    vec![
        Line::from(vec![
            Span::styled("╭─", edge),
            Span::styled(title, edge),
            Span::styled(format!("{}╮", "─".repeat(inner + 1 - width(title))), edge),
        ]),
        row(vec![Span::styled(what.clone(), text)], width(&what)),
        row(run.iter().map(|(t, st)| Span::styled(t.to_string(), *st)).collect(), run_w),
        Line::from(Span::styled(format!("╰{}╯", "─".repeat(inner + 2)), edge)),
    ]
}

fn user_notice(text: &str) -> std::borrow::Cow<'_, str> {
    match text.strip_prefix("the curator kept the view at ") {
        Some(rest) => format!("context compacted to {rest}").into(),
        None => text.into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_goal_lines_become_a_badge_and_never_show_bare() {
        assert_eq!(goal_signal("All set.\n\nGOAL DONE\n", false), ("All set.", Some(GoalSignal::Done)));
        assert_eq!(goal_signal("Waiting on you.\n**GOAL WAIT**", false), ("Waiting on you.", Some(GoalSignal::Wait)));
        assert_eq!(goal_signal("GOAL WAIT", false), ("", Some(GoalSignal::Wait)));
        assert_eq!(goal_signal("text\nGOAL W", true), ("text", None), "held back while it streams");
        assert_eq!(goal_signal("text\nGOAL W", false), ("text\nGOAL W", None), "a finished reply keeps it");
        assert_eq!(goal_signal("the GOAL DONE marker", false), ("the GOAL DONE marker", None));
        let b = Block::Assistant {
            text: "Ok.\n\nGOAL WAIT".into(),
            streaming: false,
            at: None,
            started: None,
            took_ms: None,
        };
        let shown = texts(&b.render(80, &Theme::nexo(), opts())).join("\n");
        assert!(!shown.contains("GOAL WAIT") && shown.contains("goal paused"), "{shown}");
    }

    fn texts(lines: &[Line]) -> Vec<String> {
        lines.iter().map(markdown::text_of).collect()
    }

    #[test]
    fn clocks_grow_into_minutes_and_hours() {
        let cases = [
            (0, "0s"),
            (59, "59s"),
            (60, "1m00s"),
            (185, "3m05s"),
            (3599, "59m59s"),
            (3600, "1h00m"),
            (3720, "1h02m"),
            (90_061, "25h01m"),
        ];
        for (s, want) in cases {
            assert_eq!(fmt_secs(s), want, "{s} s");
        }
        assert_eq!(fmt_ms(450), "450ms");
        assert_eq!(fmt_ms(12_340), "12.3s");
        assert_eq!(fmt_ms(185_000), "3m05s");
        assert_eq!(fmt_ms(5_400_000), "1h30m");
    }

    const CURATOR_NOTICE: &str = "the curator kept the view at 7.4k tokens (history 14.9k)";

    /// The notice as the transcript paints it at `cols` columns, one string per row.
    fn notice_rows(text: &str, cols: usize) -> Vec<String> {
        let theme = Theme::nexo();
        texts(&Block::Notice { level: Level::Info, text: text.into() }.render(cols, &theme, opts()))
    }

    #[test]
    fn the_compaction_notice_does_not_name_the_curator_at_80x24() {
        let rows = notice_rows(CURATOR_NOTICE, 78);
        assert_eq!(rows, ["◇ context compacted to 7.4k tokens (history 14.9k)"]);
    }

    #[test]
    fn the_compaction_notice_does_not_name_the_curator_at_40x12() {
        let rows = notice_rows(CURATOR_NOTICE, 38);
        assert!(rows.len() > 1 && rows.iter().all(|r| !r.contains("curator") && width(r) <= 38), "{rows:?}");
        assert_eq!(rows[0], "◇ context compacted to 7.4k tokens");
        assert_eq!(rows.join(" ").replace("   ", " "), "◇ context compacted to 7.4k tokens (history 14.9k)");
        // other notices pass through untouched
        assert_eq!(notice_rows("hello", 38), ["◇ hello"]);
    }

    #[test]
    fn the_usage_line_never_cuts_a_figure_in_a_narrow_pane() {
        let usage = nexo_core::stream::Usage { input: 1800, output: 412, cache_read: 12300, ..Default::default() };
        let b = Block::Usage { usage, cost_usd: 0.0042, elapsed_ms: 14300, tally: Default::default(), plan: false };
        // the line drops its least useful pieces first and stays on one row
        let rows = texts(&b.render(28, &Theme::dark(), opts()));
        assert!(rows.len() == 1 && width(&rows[0]) <= 28, "{rows:?}");
        assert!(rows[0].contains("14.3s") && rows[0].contains('↑') && rows[0].contains('↓'), "{rows:?}");
        // under its shortest form it wraps instead of cutting the last figure
        let rows = texts(&b.render(16, &Theme::dark(), opts()));
        assert!(rows.len() > 1 && rows.iter().all(|r| width(r) <= 16), "{rows:?}");
        let all = rows.join(" ");
        assert!(all.contains("14.3s") && all.contains("412"), "{rows:?}");
    }

    fn opts() -> RenderOpts {
        RenderOpts {
            show_reasoning: true,
            fold_after: 3,
            spinner: 0,
            intro_ms: None,
            interpreted: false,
            timestamps: true,
        }
    }

    #[test]
    fn a_tool_card_folds_after_the_limit_and_expands_on_toggle() {
        let out = ToolOutput::ok((1..=10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"));
        let mut b = Block::Tool {
            id: "c1".into(),
            name: "bash".into(),
            summary: "ls".into(),
            args: Value::Null,
            status: ToolStatus::Done,
            progress: vec![],
            output: Some(out),
            started: None,
            elapsed_ms: Some(1500),
            folded: true,
        };
        let t = texts(&b.render(60, &Theme::dark(), opts()));
        let sh = nexo_core::shell::shell().label();
        assert!(t[0].starts_with(&format!("✓ {sh} ls")), "{}", t[0]);
        assert!(t[0].contains("1.5s"));
        assert_eq!(t.len(), 1, "a finished card folds to its head: {t:?}");
        assert!(t[0].contains("· 10 lines") && t[0].ends_with('▸'), "{}", t[0]);
        assert!(b.toggle_fold());
        let t = texts(&b.render(60, &Theme::dark(), opts()));
        assert_eq!(t.len(), 11);
        assert!(t[0].ends_with('▾'), "{}", t[0]);
    }

    #[test]
    fn a_sub_agent_card_shows_a_robot_that_blinks_while_it_works() {
        let card = |status, beat| {
            let b = Block::Tool {
                id: "c9".into(),
                name: "task".into(),
                summary: "task [explore]: map the parser".into(),
                args: Value::Null,
                status,
                progress: vec![],
                output: Some(ToolOutput::ok("report")),
                started: None,
                elapsed_ms: Some(900),
                folded: true,
            };
            texts(&b.render(60, &Theme::dark(), RenderOpts { spinner: beat, ..opts() }))[0].clone()
        };
        let done = card(ToolStatus::Done, 0);
        assert!(done.starts_with("✓ [o_o] agent [explore]: map the parser"), "{done}");
        assert!(card(ToolStatus::Running, 38).contains("[-_-] agent"), "it blinks while it works");
        assert!(card(ToolStatus::Done, 38).contains("[o_o] agent"), "and rests with open eyes");
    }

    #[test]
    fn an_open_running_shell_card_shows_the_whole_command_wrapped() {
        let cmd = format!("echo {}\nls -la", "x".repeat(70));
        let b = Block::Tool {
            id: "c2".into(),
            name: "bash".into(),
            summary: cmd.clone(),
            args: serde_json::json!({ "command": cmd }),
            status: ToolStatus::Running,
            progress: vec![],
            output: None,
            started: None,
            elapsed_ms: None,
            folded: false,
        };
        let t = texts(&b.render(40, &Theme::dark(), opts()));
        let body = t[1..].join("\n");
        assert!(body.contains("$ echo"), "{t:?}");
        assert!(body.contains("ls -la"), "{t:?}");
        assert!(t.iter().all(|l| l.chars().count() <= 40), "{t:?}");
    }

    #[test]
    fn a_failed_card_keeps_a_short_preview_when_folded() {
        let out = ToolOutput::error((1..=10).map(|i| format!("e{i}")).collect::<Vec<_>>().join("\n"));
        let b = Block::Tool {
            id: "c3".into(),
            name: "bash".into(),
            summary: "false".into(),
            args: Value::Null,
            status: ToolStatus::Failed,
            progress: vec![],
            output: Some(out),
            started: None,
            elapsed_ms: Some(10),
            folded: true,
        };
        let t = texts(&b.render(60, &Theme::dark(), opts()));
        assert!(t.len() > 1 && t.len() <= 1 + 3 + 1, "{t:?}");
        assert!(t.last().unwrap().contains("more lines"), "{t:?}");
    }

    #[test]
    fn a_long_shell_output_is_cut_with_a_count_but_the_system_prompt_is_whole() {
        let out = (1..=30).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n");
        let sh = |code| Block::Shell { command: "ls".into(), output: out.clone(), exit_code: code };
        let t = texts(&sh(Some(0)).render(60, &Theme::dark(), opts()));
        assert_eq!(t.len(), 1 + 6 + 1, "{t:?}");
        assert!(t.last().unwrap().contains("+24 lines"), "{t:?}");
        let t = texts(&sh(None).render(60, &Theme::dark(), opts()));
        assert_eq!(t.len(), 31, "{t:?}");
    }

    #[test]
    fn a_long_notice_has_one_icon_and_wraps_under_its_text() {
        let b = Block::Notice { level: Level::Info, text: "word ".repeat(30) };
        let t = texts(&b.render(40, &Theme::dark(), opts()));
        assert!(t.len() > 1);
        assert!(t[0].starts_with("◇ "));
        assert!(t[1..].iter().all(|l| l.starts_with("  ") && !l.contains('◇')), "{t:?}");
    }

    #[test]
    fn model_card_and_welcome_box() {
        let b = Block::Model {
            provider: "deepseek".into(),
            model: "deepseek-chat".into(),
            pricing: Some(Pricing { input: 0.28, output: 0.42, cache_read: 0.028, cache_write: 0.0 }),
            context_window: Some(128_000),
            max_output: Some(8192),
            source: "catalogue".into(),
        };
        let t = texts(&b.render(120, &Theme::nexo(), opts()));
        assert_eq!(
            t[0],
            "◈ model deepseek/deepseek-chat · 128k context · 8.2k max reply · $0.28 in · $0.42 out · $0.028 cached · from catalogue"
        );
        let b = Block::Model {
            provider: "p".into(),
            model: "m".into(),
            pricing: None,
            context_window: None,
            max_output: None,
            source: String::new(),
        };
        assert!(texts(&b.render(120, &Theme::nexo(), opts()))[0].contains("no prices known"));
        let w = Block::Welcome {
            provider: "p".into(),
            model: "m".into(),
            cwd: "~/x".into(),
            branch: Some("main".into()),
            permission_mode: "ask".into(),
        };
        let t = texts(&w.render(60, &Theme::nexo(), opts()));
        assert!(t[0].starts_with("╭──"));
        assert!(t[1].contains("●─╮") && t[1].contains("nexo"));
        assert!(t.iter().any(|l| l.contains("p/m · ask")));
        assert!(t.iter().any(|l| l.contains("~/x · main")));
        assert!(t.iter().any(|l| l.contains("/model sets up a model")));
        let narrow = texts(&w.render(30, &Theme::nexo(), opts()));
        assert!(narrow[1].contains("nexo") && !narrow[1].contains('●'), "a narrow box drops the mark");
        assert!(t.last().unwrap().starts_with("╰──"));
        assert_eq!(fmt_price(0.28), "0.28");
        assert_eq!(fmt_price(15.0), "15");
        assert_eq!(fmt_price(3.0), "3.00");
    }

    #[test]
    fn the_caret_blinks_and_the_mark_lights_up() {
        let theme = Theme::nexo();
        let live = Block::Assistant { text: "Hello".into(), streaming: true, at: None, started: None, took_ms: None };
        let on = live.render(40, &theme, RenderOpts { spinner: 0, ..opts() });
        let last = on.last().unwrap().spans.last().unwrap().clone();
        assert_eq!(last.content, "▍");
        assert_eq!(last.style.fg, Some(theme.accent), "lit");
        let off = live.render(40, &theme, RenderOpts { spinner: 6, ..opts() });
        assert_eq!(off.last().unwrap().spans.last().unwrap().style.fg, Some(theme.dim2), "and dark half the time");
        let done = Block::Assistant { text: "Hello".into(), streaming: false, at: None, started: None, took_ms: None };
        assert!(!texts(&done.render(40, &theme, opts()))[0].contains('▍'), "no caret once the reply is done");
        // the first node is born as a dot, the last one is not there yet, the centre waits as a seed
        assert_eq!(intro_cell(&theme, '●', theme.accent2, 0, 0, 0), ('·', theme.fg));
        assert_eq!(intro_cell(&theme, '●', theme.accent2, 2, 0, 100).0, ' ', "the last node waits its turn");
        assert_eq!(intro_cell(&theme, '●', theme.accent, 1, 3, 100), ('·', theme.dim2));
        assert_eq!(intro_cell(&theme, '●', theme.accent2, 2, 0, INTRO_MS), ('●', theme.accent2));
        assert_eq!(intro_cell(&theme, '●', theme.accent, 1, 3, INTRO_MS), ('●', theme.accent), "the centre settles");
    }

    /// Every cell of `lines` with its style, so frames split into spans differently compare.
    fn cells(lines: &[Line]) -> Vec<Vec<(String, Style)>> {
        lines
            .iter()
            .map(|l| l.spans.iter().flat_map(|s| s.content.chars().map(move |c| (c.to_string(), s.style))).collect())
            .collect()
    }

    #[test]
    fn the_banner_intro_never_jumps_and_ends_on_the_still_banner() {
        for theme in [Theme::nexo(), Theme::mono()] {
            for w in [30, 44, 80, 120] {
                let at = |ms| welcome(w, &theme, "deepseek/deepseek-flash", "~/work · main", "auto", ms);
                let still = at(None);
                let widths: Vec<usize> = still.iter().map(Line::width).collect();
                let mut ms = 0;
                while ms <= INTRO_MS {
                    let frame = at(Some(ms));
                    assert_eq!(frame.iter().map(Line::width).collect::<Vec<_>>(), widths, "w {w} at {ms} ms");
                    ms += 16;
                }
                assert_eq!(cells(&at(Some(INTRO_MS))), cells(&still), "w {w}: the last frame is the still banner");
            }
        }
        // halfway the title is being typed with its caret, and the place is not there yet
        let theme = Theme::nexo();
        let mid = texts(&welcome(80, &theme, "m/m", "~/work", "auto", Some(TYPE_TITLE.0 + 2 * TYPE_TITLE.1)));
        assert!(mid[1].contains("nex▍") && !mid[3].contains("~/work"), "{mid:?}");
    }

    #[test]
    fn a_todo_card_lists_the_tasks_with_their_glyphs_and_counts() {
        let items = vec![
            TodoItem { text: "read the test".into(), status: TodoStatus::Done },
            TodoItem { text: "fix the conversion".into(), status: TodoStatus::InProgress },
            TodoItem { text: "run the suite".into(), status: TodoStatus::Pending },
        ];
        assert_eq!(todo_counts(&items), (1, 3));
        let b = Block::Tool {
            id: "t".into(),
            name: "todo".into(),
            summary: "3 tasks".into(),
            args: Value::Null,
            status: ToolStatus::Done,
            progress: vec![],
            output: Some(ToolOutput::ok("ok").with_display(ToolDisplay::Todo { items })),
            started: None,
            elapsed_ms: None,
            folded: false,
        };
        let t = texts(&b.render(60, &Theme::nexo(), opts()));
        assert!(t[0].contains("todo 3 tasks · 1/3 done"), "{}", t[0]);
        let mut said = b.clone();
        if let Block::Tool { summary, .. } = &mut said {
            *summary = "todo: 1/3 done".into();
        }
        let t = texts(&said.render(60, &Theme::nexo(), opts()));
        assert_eq!(t[0].matches("1/3 done").count(), 1, "said once: {}", t[0]);
        assert_eq!(t[1], "│ ● read the test");
        assert_eq!(t[2], "│ ◐ fix the conversion");
        assert_eq!(t[3], "└ ○ run the suite", "the guide ends in a corner on the last line");
        let done = b.render(60, &Theme::nexo(), opts())[1].spans[1].style;
        assert!(done.add_modifier.contains(Modifier::CROSSED_OUT));
    }

    #[test]
    fn reasoning_notices_and_plain_results_read_as_markdown() {
        let th = Theme::dark();
        let r = Block::Reasoning {
            text: "**plan** the `fix`\n- one".into(),
            streaming: false,
            started: None,
            elapsed_ms: Some(10),
            folded: false,
        };
        let all = texts(&r.render(40, &th, opts())).join("\n");
        assert!(all.contains("plan the fix") && !all.contains("**") && !all.contains('`'), "{all}");
        let n = notice_rows("run `nexo login` first\n  **ok** done", 60).join("\n");
        assert!(n.contains("run nexo login first") && n.contains("ok done") && !n.contains("**"), "{n}");
        let plain = |content: &str| {
            let out = ToolOutput::ok(content).with_display(ToolDisplay::Plain);
            let b = Block::Tool {
                id: "w".into(),
                name: "web".into(),
                summary: "s".into(),
                args: Value::Null,
                status: ToolStatus::Done,
                progress: vec![],
                output: Some(out),
                started: None,
                elapsed_ms: None,
                folded: false,
            };
            texts(&b.render(60, &th, opts())).join("\n")
        };
        let md = plain("## Findings\nit is **fast**");
        assert!(md.contains("Findings") && !md.contains("##") && !md.contains("**"), "{md}");
    }

    #[test]
    fn reasoning_hides_when_asked_and_folds_to_one_line() {
        let b = Block::Reasoning {
            text: "a\nb\nc".into(),
            streaming: false,
            started: None,
            elapsed_ms: Some(2100),
            folded: true,
        };
        let t = texts(&b.render(40, &Theme::dark(), opts()));
        assert_eq!(t, vec!["▸ thinking · 3 lines · 2.1s"]);
        let hidden = b.render(40, &Theme::dark(), RenderOpts { show_reasoning: false, ..opts() });
        assert!(hidden.is_empty());
    }

    fn remembered(scope: &str, items: &[&str], path: &str) -> Block {
        let display = ToolDisplay::Remembered {
            scope: scope.into(),
            items: items.iter().map(|s| s.to_string()).collect(),
            path: path.into(),
        };
        Block::Tool {
            id: "n".into(),
            name: "note".into(),
            summary: "note: Rule: x".into(),
            args: Value::Null,
            status: ToolStatus::Done,
            progress: vec![],
            output: Some(ToolOutput::ok("noted 1 line; 1 new rule saved").with_display(display)),
            started: None,
            elapsed_ms: Some(1),
            folded: true,
        }
    }

    #[test]
    fn what_nexo_remembers_is_a_card_with_its_scope_the_line_and_a_link_to_the_file() {
        let b = remembered("project", &["the API runs on port 4100"], "/home/me/my app/AGENTS.md");
        for theme in [Theme::dark(), Theme::light()] {
            let t = texts(&b.render(60, &theme, opts()));
            assert_eq!(
                t,
                vec![
                    "✦ remembered · this project",
                    "│ • the API runs on port 4100",
                    "└ file:///home/me/my%20app/AGENTS.md"
                ]
            );
            // the glyphs and the address are in colours of the theme, not the default
            let lines = b.render(60, &theme, opts());
            assert_eq!(lines[0].spans[0].style.fg, Some(theme.accent2));
            assert_eq!(lines[2].spans[1].style.fg, Some(theme.accent));
        }
        // folded or not, a finished save is not a tool card with a chevron
        let g = texts(&remembered("global", &["a", "b"], "C:\\Users\\me\\.nexo\\prompt.md").render(
            40,
            &Theme::dark(),
            opts(),
        ));
        assert_eq!(g[0], "✦ remembered · global");
        assert_eq!(&g[1..3], ["│ • a", "│ • b"]);
        assert_eq!(g[3], "└ file:///C:/Users/me/.nexo/prompt.md");
        // a long line wraps under its text; the address stays whole on its own row
        let long =
            texts(&remembered("global", &["one two three four five six seven eight nine ten"], "/h/prompt.md").render(
                20,
                &Theme::dark(),
                opts(),
            ));
        assert!(long.len() > 3 && long[1..long.len() - 1].iter().all(|l| width(l) <= 20), "{long:?}");
        assert!(long[2].starts_with("│   "), "{long:?}");
        assert_eq!(long.last().unwrap(), "└ file:///h/prompt.md");
    }

    #[test]
    fn a_newer_nexo_shows_as_a_framed_card() {
        let t = texts(&Block::Update { behind: 4 }.render(70, &Theme::dark(), opts()));
        let all = t.join("\n");
        assert_eq!(t.len(), 4, "{all}");
        assert!(t[0].starts_with("╭─ ⬆ Update ready ") && t[0].ends_with('╮'), "{all}");
        assert_eq!(t[1].trim_end(), "│ 4 new commits, ready-built.         │", "{all}");
        assert!(t[2].contains("Run nexo update, then restart nexo.") && t[3].starts_with('╰'), "{all}");
        assert!(t.iter().all(|l| width(l) == width(&t[0])), "{all}");
        // too narrow for the frame: two plain lines that still say what to do
        let n = texts(&Block::Update { behind: 1 }.render(30, &Theme::dark(), opts()));
        assert!(n[0].trim_start().starts_with("⬆ Update ready · 1 new"), "{n:?}");
        assert!(n.iter().all(|l| width(l) <= 30), "{n:?}");
    }

    #[test]
    fn ended_jobs_note_shows_as_a_compact_card() {
        let note = "[background jobs ended]\n- job 1 `cd /home/user/project && for t in a b c; do echo $t; done` done (exit 0) after 2 min 43 s, 40 lines of output\n- job 2 `false` done (exit 1) after 3 s, 1 lines of output\nRead the output with the `jobs` tool (action \"output\", the job's id).";
        let b = Block::User { text: note.into(), at: None };
        let t = texts(&b.render(70, &Theme::dark(), opts()));
        let all = t.join("\n");
        assert!(!all.contains("[background jobs ended]") && !all.contains("Read the output"), "{all}");
        assert!(!all.contains('▌'), "{all}");
        assert_eq!(t.len(), 3, "{all}");
        assert!(t[1].starts_with("⚙ job 1  cd /home"), "{all}");
        assert!(t[1].contains("…") && t[1].ends_with("✓ exit 0 · 2 min 43 s · 40 lines"), "{all}");
        assert!(t[2].starts_with("⚙ job 2  false") && t[2].ends_with("✗ exit 1 · 3 s · 1 line"), "{all}");
        assert!(t.iter().all(|l| width(l) <= 70), "{all}");
        assert_eq!(b.copy_text().as_deref(), Some(note));
    }

    #[test]
    fn a_long_diff_row_is_shortened_with_an_ellipsis_not_cut_by_the_edge() {
        let out = ToolOutput::ok("ok").with_display(ToolDisplay::Diff {
            path: "a.rs".into(),
            old: "a\n".into(),
            new: format!("a\n{}\n", "x".repeat(150)),
            start_line: 1,
        });
        let b = Block::Tool {
            id: "e".into(),
            name: "edit".into(),
            summary: "a.rs".into(),
            args: Value::Null,
            status: ToolStatus::Done,
            progress: vec![],
            output: Some(out),
            started: None,
            elapsed_ms: None,
            folded: false,
        };
        let t = texts(&b.render(60, &Theme::dark(), opts()));
        let all = t.join("\n");
        assert!(t.iter().all(|l| width(l) <= 60), "{all}");
        assert!(t.iter().any(|l| l.contains("+ xxx") && l.ends_with('…')), "{all}");
    }

    #[test]
    fn diff_cards_show_counts_and_user_lines_carry_the_bar() {
        let out = ToolOutput::ok("ok").with_display(ToolDisplay::Diff {
            path: "a.rs".into(),
            old: "x\ny\n".into(),
            new: "x\nz\nw\n".into(),
            start_line: 1,
        });
        let b = Block::Tool {
            id: "e".into(),
            name: "edit".into(),
            summary: "a.rs".into(),
            args: Value::Null,
            status: ToolStatus::Done,
            progress: vec![],
            output: Some(out),
            started: None,
            elapsed_ms: None,
            folded: false,
        };
        let t = texts(&b.render(60, &Theme::dark(), opts()));
        assert!(t[0].contains("+2 −1"), "{}", t[0]);
        assert!(t.iter().any(|l| l.contains("- y")));
        let u = Block::User { text: "hello\nworld".into(), at: None };
        assert_eq!(texts(&u.render(20, &Theme::dark(), opts())), vec!["▌ hello", "▌ world"]);
        let u = Block::User { text: "hello".into(), at: Some("12:34".into()) };
        let t = texts(&u.render(20, &Theme::dark(), opts()));
        assert_eq!(t, vec!["▌ hello        12:34"]);
        let u = Block::User { text: "a line that fills everything".into(), at: Some("12:34".into()) };
        assert!(!texts(&u.render(30, &Theme::dark(), opts()))[0].contains("12:34"), "no room, no stamp");
        let off = RenderOpts { timestamps: false, ..opts() };
        let u = Block::User { text: "hello".into(), at: Some("12:34".into()) };
        assert_eq!(texts(&u.render(20, &Theme::dark(), off)), vec!["▌ hello"], "off: no stamp");
        let r = Block::Assistant {
            text: "hi".into(),
            streaming: false,
            at: Some("14:32".into()),
            started: None,
            took_ms: Some(64_000),
        };
        assert_eq!(texts(&r.render(30, &Theme::dark(), opts())), vec!["◆ hi             14:32 · 1m04s"]);
        assert_eq!(texts(&r.render(30, &Theme::dark(), off)), vec!["◆ hi"]);
        assert_eq!(fmt_k(1234), "1.2k");
        assert_eq!(fmt_k(2_500_000), "2.50M");
        assert_eq!(fmt_k(3_100_000_000), "3.10B");
        assert_eq!(fmt_ms(61_000), "1m01s");
    }
}
