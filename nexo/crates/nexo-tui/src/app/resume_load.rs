//! The loading cover of a resumed session.
//!
//! A long history used to be laid out inside `draw_transcript`, so every frame while the replay
//! arrived stalled for most of a second. The cover takes the whole screen instead, and the layout
//! runs a slice per tick, off the draw path. The transcript appears in the frame the layout ends.

use std::time::Duration;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as Box_, Clear, Paragraph};
use ratatui::Frame;

use super::App;
use crate::render::blocks::SPINNER;
use crate::render::wrap::width;
use crate::theme::mix;

/// Blocks wrapped in one tick: enough that a few thousand finish in well under a second, little
/// enough that one tick stays inside a frame.
const LAYOUT_BUDGET: usize = 64;
/// A tick may not spend longer than this wrapping, so a few heavy blocks cannot stall the cover.
const LAYOUT_SLICE: Duration = Duration::from_millis(6);
/// The cover's card, centred. Narrow screens get less.
const CARD_W: u16 = 42;
const CARD_H: u16 = 7;

/// Set by a cleared transcript and spent by the history that follows it. A first message of a fresh
/// session never finds it set, so it never raises the cover.
#[derive(Debug, Default)]
pub(crate) struct ResumeArm {
    on: bool,
    expected: Option<usize>,
}

/// A resume whose history is still arriving or still being laid out.
pub(crate) struct ResumeLoad {
    /// Blocks wrapped so far.
    pub done: usize,
    /// Blocks received so far.
    pub total: usize,
    /// The replay has ended (its usage summary arrived): the cover stays until `done`
    /// catches `total`, so the transcript is never shown half laid out.
    pub sealed: bool,
    /// The line count `pump` measured for each block, at the width it measured it. A later `height`
    /// of the same block at that width must agree: the cover and the transcript share one count.
    heights: Vec<(usize, usize)>,
    /// Message boundaries in block coordinates; the last message is pending until the next or seal.
    expected: Option<usize>,
    boundaries: Vec<usize>,
    pending: bool,
}

impl App {
    /// One more message of a resume. The cover opens only for history that follows a clear (a replay);
    /// the first message of a fresh session never raises it.
    pub(super) fn note_resume_load(&mut self) {
        if self.resume_load.is_none() && !self.resume_arm.on {
            return;
        }
        self.resume_arm.on = false;
        let load = self.resume_load.get_or_insert_with(|| ResumeLoad {
            done: 0,
            total: 0,
            sealed: false,
            heights: Vec::new(),
            expected: self.resume_arm.expected.take(),
            boundaries: Vec::new(),
            pending: false,
        });
        load.pending = true;
        load.total = self.blocks.len();
        self.dirty = true;
    }

    /// Record the end of the preceding message, including any replayed tool blocks.
    pub(super) fn resume_message_boundary(&mut self) {
        if let Some(load) = &mut self.resume_load {
            if load.pending {
                load.boundaries.push(self.blocks.len());
                load.pending = false;
            }
        }
    }

    /// Only a replay following a clear raises the cover, as with older agents.
    pub(super) fn announce_resume_load(&mut self, total: usize) {
        if self.resume_arm.on {
            self.resume_arm.expected = Some(total);
        }
    }

    /// The transcript was cleared: the history that follows, if any, is a replay.
    pub(super) fn arm_resume_load(&mut self) {
        self.resume_arm.on = true;
        self.resume_arm.expected = None;
    }

    /// The replay is over. What is already wrapped shows at once; the rest finishes under the cover.
    pub(super) fn finish_resume_load(&mut self) {
        self.resume_message_boundary();
        self.resume_arm.on = false;
        self.resume_arm.expected = None;
        let Some(load) = &mut self.resume_load else { return };
        load.sealed = true;
        self.dirty = true;
    }

    /// Esc while the cover is up: stop the agent and show what is laid out so far.
    pub(super) fn cancel_resume_load(&mut self) {
        self.resume_arm.on = false;
        self.resume_arm.expected = None;
        self.resume_load = None;
        self.context_wait = false;
        self.words.defer(false);
        self.send(nexo_core::agent::AgentCommand::Interrupt);
        self.esc_interrupted = true;
        self.flash("resume stopped");
        self.dirty = true;
    }

    /// Wraps a slice of the resumed history. The cover drops in the tick that finishes the layout,
    /// before the frame is drawn, so that frame shows the transcript and never a blank one.
    pub(super) fn pump_resume_load(&mut self) {
        let Some(load) = &mut self.resume_load else { return };
        load.total = self.blocks.len();
        if load.done >= load.total && load.sealed {
            self.resume_load = None;
            self.dirty = true;
            return;
        }
        let cap = load.total.div_ceil(4).clamp(1, LAYOUT_BUDGET);
        let width = self.last_width.max(1);
        let until = std::time::Instant::now() + LAYOUT_SLICE;
        let mut n = 0;
        while n < cap && std::time::Instant::now() < until {
            let done = self.resume_load.as_ref().map(|l| l.done).unwrap_or(0);
            let total = self.resume_load.as_ref().map(|l| l.total).unwrap_or(0);
            if done >= total {
                break;
            }
            // wrapping is the work the draw used to do; doing it here keeps the draw cheap
            let h = self.height(done, width);
            if let Some(load) = &mut self.resume_load {
                if load.heights.len() == done {
                    load.heights.push((width, h));
                }
                load.done += 1;
            }
            n += 1;
        }
        let finished = self.resume_load.as_ref().is_some_and(|l| l.sealed && l.done >= l.total);
        if finished {
            self.resume_load = None;
        }
        self.dirty = true;
    }
}

#[cfg(test)]
impl ResumeLoad {
    /// The line count the cover measured for block `i` at `width`, when it has measured it.
    pub(crate) fn height(&self, i: usize, width: usize) -> Option<usize> {
        self.heights.get(i).and_then(|(w, h)| (*w == width).then_some(*h))
    }
}

/// The full-screen cover: a spinner, the percent, `n / total`, and a bar. The bar shimmers between
/// the theme's accent and accent2 unless animations are off, when it stays still.
pub(crate) fn draw_resume_cover(f: &mut Frame, app: &App, area: Rect) {
    let Some(load) = &app.resume_load else { return };
    let theme = &app.theme;
    f.render_widget(Clear, area);
    f.render_widget(Box_::default().style(theme.base()), area);

    let w = CARD_W.min(area.width.saturating_sub(2).max(1));
    let h = CARD_H.min(area.height);
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let card = Rect { x, y, width: w, height: h };

    let (done, total) = if let Some(expected) = load.expected {
        let done = load.boundaries.partition_point(|&end| end <= load.done)
            + usize::from(load.pending && load.done >= load.total);
        (done.min(expected), expected.max(1))
    } else {
        (load.done, load.total.max(1))
    };
    let pct = (done as u64 * 100 / total as u64).min(100);
    let spin = if app.opts.animations { SPINNER[app.spinner % SPINNER.len()] } else { "·" };
    let title = format!("{spin}  Loading session");
    let count = format!("{pct}%    {done} / {total}");
    let bar_w = (w as usize).saturating_sub(4).clamp(8, 40);
    let filled = (bar_w * done / total).min(bar_w);
    let frame = if app.opts.animations { app.spinner } else { 0 };

    let mut lines = vec![
        Line::from(Span::styled(title, theme.bold(theme.accent))),
        Line::from(""),
        Line::from(Span::styled(count, theme.dim())),
        Line::from(""),
        bar_line(filled, bar_w, frame, app.opts.animations, theme),
        Line::from(""),
        Line::from(Span::styled("Esc cancels", theme.dim2())),
    ];
    // a card shorter than the text keeps the title and the bar
    if h < lines.len() as u16 {
        lines.truncate(h as usize);
    }
    // centre each row inside the card
    let lines: Vec<Line> = lines
        .into_iter()
        .map(|l| {
            let used: usize = l.spans.iter().map(|s| width(&s.content)).sum();
            let pad = (w as usize).saturating_sub(used) / 2;
            let mut spans = vec![Span::raw(" ".repeat(pad))];
            spans.extend(l.spans);
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), card);
}

/// The progress bar: filled cells shimmer, the rest stay on the dim ground.
fn bar_line(filled: usize, bar_w: usize, frame: usize, animate: bool, theme: &crate::theme::Theme) -> Line<'static> {
    let mut spans = Vec::with_capacity(bar_w);
    for i in 0..bar_w {
        if i < filled {
            let near = if animate {
                let pos = (frame as f32 * 0.7) % (bar_w as f32 + 6.0) - 3.0;
                (1.0 - ((i as f32 - pos).abs() / 3.0)).max(0.0)
            } else {
                0.0
            };
            let colour = mix(theme.accent, theme.accent2, near);
            spans.push(Span::styled("━", Style::default().fg(colour).add_modifier(Modifier::BOLD)));
        } else {
            spans.push(Span::styled("━", theme.dim2()));
        }
    }
    Line::from(spans)
}
