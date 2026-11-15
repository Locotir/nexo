//! The look and the motion of `/panel`'s events: which glyph and colour each kind of event has, the ribbon of
//! the last twelve, the bars and sparklines, and the shapes of the effects (a letter's flight, the burst of a
//! passing test run, the squash of a compaction). Everything here is a pure function of the milliseconds
//! since the effect was born, so a test can ask for any instant; the view decides when an effect starts.

use nexo_agent::supervise::{Ev, EvKind, Secs};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::anim::ease_out;
use crate::motion::{ease_in_out, fade};
use crate::render::wrap::width;
use crate::theme::Theme;

/// A cell of the screen: column, row.
pub type Cell = (u16, u16);

// ------------------------------------------------------------------------------------------ durations

/// The letter lifts off the sender, flies, lands: milliseconds of each leg.
pub const LIFT_MS: u64 = 120;
pub const FLY_MS: u64 = 900;
pub const LAND_MS: u64 = 420;
pub const LETTER_MS: u64 = LIFT_MS + FLY_MS + LAND_MS;
/// A letter that comes back (refused, failed, expired).
pub const BOUNCE_MS: u64 = 600;
/// A letter in a line view or a far-zoom view.
pub const HOP_MS: u64 = 700;
/// A letter whose receiver is out of sight stays at the edge this long.
pub const EDGE_MS: u64 = 1200;
/// The delay between letters when more than three are in the air.
pub const STAGGER_MS: u64 = 150;
/// Letters sent by one look past this many are not flown (the cards still show their mail): a burst of two
/// hundred would keep the panel moving for half a minute.
pub const MAX_LETTERS: usize = 9;
pub const SQUASH_MS: u64 = 360;
pub const BURST_MS: u64 = 400;
pub const SHAKE_STEP_MS: u64 = 60;
pub const SHAKE_MS: u64 = SHAKE_STEP_MS * 4;
pub const COMMIT_FRAME_MS: u64 = 80;
pub const COMMIT_MS: u64 = 300;
pub const PUSH_MS: u64 = 300;
pub const ERROR_MS: u64 = 450;
pub const SAT_SPAWN_MS: u64 = 300;
pub const SAT_DONE_MS: u64 = 500;
pub const SAT_FADE_MS: u64 = 300;
pub const SAT_FAIL_MS: u64 = 5000;
/// A satellite breathes once in this long.
pub const SAT_BREATH_MS: u64 = 1600;
/// The border of a card waiting for a permission breathes in this long.
pub const LOCK_BREATH_MS: u64 = 1600;
/// A mail waiting for its reader's idle point breathes slowly.
pub const WAIT_BREATH_MS: u64 = 2400;
/// The heartbeat's bright cell crosses the line in this long.
pub const SWEEP_MS: u64 = 2400;
/// The pulse row flashes this long when a figure changes.
pub const FLASH_MS: u64 = 200;
/// A new flash waits this long after the last one ended, so a busy fleet does not make the row flicker.
pub const FLASH_GAP_MS: u64 = 2000;
/// A receipt stays on the sender's card this many seconds.
pub const RECEIPT_SECS: i64 = 20;
/// An error keeps its red this many seconds in the ribbon.
pub const ERROR_RED_SECS: i64 = 30;
/// How long the longest effect of an event lives (the view forgets it after this).
pub const EFFECT_MS: u64 = 1500;

// ------------------------------------------------------------------------------------------ glyphs

/// The glyph of an event kind: the Unicode form, or the ASCII one when `ascii`.
pub fn glyph(kind: EvKind, ascii: bool) -> &'static str {
    use EvKind::*;
    let (u, a) = match kind {
        SessionStart => ("▸", ">"),
        SessionEnd => ("■", "#"),
        TurnStart => ("›", ">"),
        TurnEnd => ("▪", "."),
        Thinking => ("∴", ":"),
        Read => ("◦", "r"),
        Search => ("◎", "s"),
        Edit => ("✎", "e"),
        FileWritten => ("▪", "w"),
        Run => ("❯", "$"),
        Web => ("◍", "w"),
        Tool => ("•", "t"),
        PermissionWait => ("⚿", "!"),
        PermissionDone => ("✓", "+"),
        Question => ("◆", "?"),
        Answer => ("◇", "."),
        SubSpawn => ("●", "o"),
        SubDone => ("✓", "+"),
        MsgOut => ("✉", "M"),
        MsgIn => ("✉", "M"),
        MsgRead => ("✓", "+"),
        Compact => ("▤", "="),
        Error => ("✗", "x"),
        Retry => ("⟳", "~"),
        RateLimit => ("⧖", "z"),
        TestsPass => ("✔", "+"),
        TestsFail => ("✘", "x"),
        GitCommit => ("◉", "c"),
        GitPush => ("⇡", "^"),
        Todo => ("≡", "="),
        JobStart => ("⠋", "*"),
        JobDone => ("✓", "+"),
        Idle => ("○", "o"),
        Stall => ("◐", "!"),
        Loop => ("↻", "@"),
        ModelSwitch => ("⇄", "%"),
        Cost => ("$", "$"),
    };
    if ascii {
        a
    } else {
        u
    }
}

/// The colour of an event kind (a `Theme` field).
pub fn color(kind: EvKind, theme: &Theme) -> Color {
    use EvKind::*;
    match kind {
        SessionStart | PermissionDone | SubSpawn | SubDone | MsgRead | TestsPass => theme.success,
        SessionEnd | Read | Tool | JobDone | Idle | Cost => theme.dim,
        TurnStart | Edit | FileWritten | MsgIn | GitCommit | GitPush | Todo => theme.accent,
        TurnEnd | Run => theme.fg,
        Thinking => theme.dim2,
        Search | Web | Question | Answer | MsgOut | Compact | JobStart | ModelSwitch => theme.accent2,
        PermissionWait => theme.warn2,
        Error | TestsFail | Loop => theme.error,
        Retry | RateLimit | Stall => theme.warn,
    }
}

/// Kinds that must read in a mono theme without colour: bold carries them.
pub fn loud(kind: EvKind) -> bool {
    matches!(
        kind,
        EvKind::Error
            | EvKind::PermissionWait
            | EvKind::Question
            | EvKind::Stall
            | EvKind::Loop
            | EvKind::TestsFail
            | EvKind::RateLimit
            | EvKind::Run
    )
}

/// The frames of a live slot, per kind, and how long each lasts.
fn live_frames(kind: EvKind, ascii: bool) -> Option<(&'static [&'static str], u64)> {
    const SPIN: [&str; 4] = ["◜", "◝", "◞", "◟"];
    const WAVE: [&str; 4] = ["⠁", "⠂", "⠄", "⠂"];
    const BAR: [&str; 4] = ["|", "/", "-", "\\"];
    match kind {
        EvKind::Thinking if ascii => Some((&BAR, 120)),
        EvKind::Thinking => Some((&WAVE, 120)),
        EvKind::Run | EvKind::Web | EvKind::Tool if ascii => Some((&BAR, 100)),
        EvKind::Run | EvKind::Web | EvKind::Tool => Some((&SPIN, 100)),
        _ => None,
    }
}

/// The frame of a live slot `phase_ms` into the animation; None for a kind that has none.
pub fn live_frame(kind: EvKind, phase_ms: u64, ascii: bool) -> Option<&'static str> {
    let (frames, each) = live_frames(kind, ascii)?;
    Some(frames[(phase_ms / each) as usize % frames.len()])
}

/// The four-phase spinner of a retry (and a job), `phase_ms` into the animation.
pub fn retry_frame(phase_ms: u64, ascii: bool) -> &'static str {
    const RING: [&str; 4] = ["◐", "◓", "◑", "◒"];
    const BAR: [&str; 4] = ["|", "/", "-", "\\"];
    let i = (phase_ms / 125) as usize % 4;
    if ascii {
        BAR[i]
    } else {
        RING[i]
    }
}

/// The braille spinner of a running job.
pub fn job_frame(phase_ms: u64, ascii: bool) -> &'static str {
    const DOTS: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    const BAR: [&str; 4] = ["|", "/", "-", "\\"];
    if ascii {
        BAR[(phase_ms / 100) as usize % 4]
    } else {
        DOTS[(phase_ms / 80) as usize % 10]
    }
}

// ------------------------------------------------------------------------------------------ the ribbon

/// One cell group of the ribbon: a glyph (and a repeat count) with its look.
#[derive(Debug, Clone, PartialEq)]
pub struct RibbonCell {
    pub text: String,
    pub style: Style,
    pub kind: Option<EvKind>,
}

impl RibbonCell {
    pub fn width(&self) -> usize {
        width(&self.text)
    }
}

/// Where the live slot's animation is: `Some(ms)` since the view opened, for a session that works now.
pub type LivePhase = Option<u64>;

/// The last events as `cells` columns, newest at the right, no gaps; the left is blank when there are fewer
/// events. A run of the same kind folded by the supervisor (`n`) takes two columns (`✎3`). Older cells fade
/// towards the dim ground by position (the oldest 40%); an error keeps its red for 30 s. Returns the cells and,
/// for each event drawn, the column it starts at (for the effects that act on one glyph).
pub fn ribbon(
    events: &[Ev],
    cells: usize,
    theme: &Theme,
    ascii: bool,
    clock: Secs,
    live: LivePhase,
) -> (Vec<RibbonCell>, Vec<(usize, EvKind, Secs)>) {
    // from the newest back, while it fits
    let mut items: Vec<(&Ev, usize)> = Vec::new();
    let mut used = 0;
    for e in events.iter().rev() {
        let w = if e.n > 1 { 2 } else { 1 };
        if used + w > cells {
            break;
        }
        items.push((e, w));
        used += w;
    }
    items.reverse();
    let count = items.len().max(1);
    let mut out: Vec<RibbonCell> = Vec::new();
    let mut marks: Vec<(usize, EvKind, Secs)> = Vec::new();
    let mut col = cells - used;
    if col > 0 {
        out.push(RibbonCell { text: " ".repeat(col), style: Style::default(), kind: None });
    }
    for (i, (e, w)) in items.iter().enumerate() {
        let last = i + 1 == items.len();
        let mut g = glyph(e.kind, ascii).to_string();
        if last {
            if let Some(f) = live.and_then(|ms| live_frame(e.kind, ms, ascii)) {
                g = f.to_string();
            }
        }
        if *w == 2 {
            g.push(char::from_digit(e.n.min(9), 10).unwrap_or('+'));
        }
        // the oldest 40% fade towards the dim ground
        let pos = i as f32 / count as f32;
        let age = if pos < 0.4 { (0.4 - pos) / 0.4 * 0.6 } else { 0.0 };
        let keep_red = e.kind == EvKind::Error && clock - e.at <= ERROR_RED_SECS;
        let base = color(e.kind, theme);
        let c = if keep_red { base } else { fade(theme, base, theme.dim2, age) };
        let mut style = theme.style(c);
        if theme.mono {
            style = Style::default();
            if loud(e.kind) {
                style = style.add_modifier(Modifier::BOLD);
            }
            if age > 0.3 {
                style = style.add_modifier(Modifier::DIM);
            }
        } else if loud(e.kind) && e.kind == EvKind::Run {
            style = style.add_modifier(Modifier::BOLD);
        }
        marks.push((col, e.kind, e.at));
        col += w;
        out.push(RibbonCell { text: g, style, kind: Some(e.kind) });
    }
    (out, marks)
}

/// The ribbon as spans.
pub fn ribbon_spans(cells: &[RibbonCell]) -> Vec<Span<'static>> {
    cells.iter().map(|c| Span::styled(c.text.clone(), c.style)).collect()
}

// ------------------------------------------------------------------------------------------ bars

const EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

/// A bar of `cells` cells filled to `frac` (0..1) in eighths: the filled part (with its partial cell) and the
/// empty part, so the caller colours them apart.
pub fn eighths(frac: f64, cells: usize) -> (String, String) {
    let total = cells * 8;
    let n = (frac.clamp(0.0, 1.0) * total as f64).round() as usize;
    let (full, part) = (n / 8, n % 8);
    let mut filled = "█".repeat(full.min(cells));
    if full < cells && part > 0 {
        filled.push_str(EIGHTHS[part]);
    }
    let used = width(&filled);
    (filled, "░".repeat(cells.saturating_sub(used)))
}

/// A todo bar as spans: `▕███▌░░▏ 3/7`, the figures `done/total`. Done is green.
pub fn todo_spans(theme: &Theme, shown: f64, done: u16, total: u16, ascii: bool) -> Vec<Span<'static>> {
    let (filled, empty) = eighths(shown, 6);
    let all = total > 0 && done >= total;
    let fill = if all { theme.success } else { theme.accent };
    let mut spans = vec![
        Span::styled("▕", theme.dim2()),
        Span::styled(filled, theme.style(fill)),
        Span::styled(empty, theme.dim2()),
        Span::styled("▏", theme.dim2()),
        Span::styled(format!(" {done}/{total}"), if all { theme.style(theme.success) } else { theme.dim() }),
    ];
    if ascii {
        for s in &mut spans {
            s.content = s.content.chars().map(|c| crate::ascii::fold_char(c).unwrap_or(c)).collect::<String>().into();
        }
    }
    spans
}

/// The width of [`todo_spans`] for these figures.
pub fn todo_width(done: u16, total: u16) -> usize {
    8 + 1 + format!("{done}/{total}").len()
}

/// A one-cell-per-value sparkline scaled to its own maximum. Zero is the lowest bar; no values, nothing.
pub fn spark(values: &[u32], cells: usize) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let from = values.len().saturating_sub(cells);
    let v = &values[from..];
    let max = v.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return String::new();
    }
    v.iter()
        .map(|&x| if x == 0 { '▁' } else { BARS[((x as f64 / max as f64) * 7.0).round().clamp(0.0, 7.0) as usize] })
        .collect()
}

/// The drain bar of a rate-limit wait: `▰▰▰▱▱`, shrinking as `left` seconds of `total` pass.
pub fn drain(left: u32, total: u32, cells: usize) -> String {
    let total = total.max(left).max(1);
    let on = ((left as f64 / total as f64) * cells as f64).ceil() as usize;
    format!("{}{}", "▰".repeat(on.min(cells)), "▱".repeat(cells - on.min(cells)))
}

// ------------------------------------------------------------------------------------------ squash

/// A line of spans squeezed to its centre: of the `cw` columns the line takes, the middle `cw * (1 - s)` stay,
/// the rest is blank. The colours of what stays are kept.
pub fn squash(spans: Vec<Span<'static>>, cw: usize, s: f32) -> Vec<Span<'static>> {
    if s <= 0.0 || cw == 0 {
        return spans;
    }
    let keep = ((cw as f32) * (1.0 - s.clamp(0.0, 1.0))).round() as usize;
    let lo = (cw - keep.min(cw)) / 2;
    let hi = lo + keep;
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut col = 0usize;
    for sp in spans {
        let mut text = String::new();
        for ch in sp.content.chars() {
            let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if col >= lo && col + w <= hi {
                text.push(ch);
            } else {
                text.push_str(&" ".repeat(w));
            }
            col += w;
        }
        out.push(Span::styled(text, sp.style));
    }
    out
}

/// How squashed a card is `ms` after a compaction: 0.8 at the peak of a hump, 0 outside.
pub fn squash_level(ms: u64) -> f32 {
    if ms >= SQUASH_MS {
        return 0.0;
    }
    0.8 * (ms as f32 / SQUASH_MS as f32 * std::f32::consts::PI).sin()
}

// ------------------------------------------------------------------------------------------ the letter

/// Where a letter is in its timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Leg {
    /// Bright at the sender.
    Lift,
    /// On its way; 0..1 along the path.
    Fly(f32),
    /// On the receiver's card; 0..1 of the landing.
    Land(f32),
    Gone,
}

pub fn leg(ms: u64) -> Leg {
    if ms < LIFT_MS {
        Leg::Lift
    } else if ms < LIFT_MS + FLY_MS {
        Leg::Fly(ease_in_out((ms - LIFT_MS) as f64 / FLY_MS as f64) as f32)
    } else if ms < LETTER_MS {
        Leg::Land((ms - LIFT_MS - FLY_MS) as f32 / LAND_MS as f32)
    } else {
        Leg::Gone
    }
}

/// How far along the path a letter is `ms` after it was born (0 while lifting, 1 once landed).
pub fn path_p(ms: u64) -> f32 {
    match leg(ms) {
        Leg::Lift => 0.0,
        Leg::Fly(p) => p,
        Leg::Land(_) | Leg::Gone => 1.0,
    }
}

/// How far a bounced letter is out along the way: 0 to 0.5 and back to 0 over [`BOUNCE_MS`].
pub fn bounce_p(ms: u64) -> f32 {
    if ms >= BOUNCE_MS {
        return 0.0;
    }
    let t = ms as f32 / BOUNCE_MS as f32;
    0.5 * (t * std::f32::consts::PI).sin()
}

/// The path of a letter between two anchors: a quadratic curve whose control point is the midpoint raised
/// `clamp(dist / 4, 2, 6)` rows (a row counting as two columns); cards in the same column bow right by 3.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Path {
    pub from: (f32, f32),
    pub ctl: (f32, f32),
    pub to: (f32, f32),
}

impl Path {
    pub fn new(from: Cell, to: Cell) -> Path {
        let (x0, y0) = (from.0 as f32, from.1 as f32);
        let (x1, y1) = (to.0 as f32, to.1 as f32);
        let (dx, dy) = (x1 - x0, y1 - y0);
        let dist = (dx * dx + (2.0 * dy) * (2.0 * dy)).sqrt();
        let h = (dist / 4.0).clamp(2.0, 6.0);
        let mid = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        // the control point sits twice as far as the bow the curve reaches
        let ctl = if dx.abs() < 2.0 { (mid.0 + 6.0, mid.1) } else { (mid.0, mid.1 - 2.0 * h) };
        Path { from: (x0, y0), ctl, to: (x1, y1) }
    }

    /// The point `p` (0..1) along the curve, as a cell clamped into `body`.
    pub fn at(&self, p: f32, body: Rect) -> Cell {
        let p = p.clamp(0.0, 1.0);
        let q = 1.0 - p;
        let x = q * q * self.from.0 + 2.0 * q * p * self.ctl.0 + p * p * self.to.0;
        let y = q * q * self.from.1 + 2.0 * q * p * self.ctl.1 + p * p * self.to.1;
        clamp_in(x.round(), y.round(), body)
    }
}

fn clamp_in(x: f32, y: f32, body: Rect) -> Cell {
    if body.width == 0 || body.height == 0 {
        return (body.x, body.y);
    }
    let cx = x.clamp(body.x as f32, (body.x + body.width - 1) as f32) as u16;
    let cy = y.clamp(body.y as f32, (body.y + body.height - 1) as f32) as u16;
    (cx, cy)
}

/// The trail glyphs behind a letter, nearest first.
pub fn trail_glyph(k: usize, ascii: bool) -> &'static str {
    if ascii {
        [".", ".", ".", ".", "."][k.min(4)]
    } else {
        ["•", "∙", "·", "·", "."][k.min(4)]
    }
}

/// The path positions of the trail: five samples, 32 ms apart in time, nearest first. Empty while the letter
/// has not left.
pub fn trail(ms: u64, path: &Path, body: Rect) -> Vec<Cell> {
    if ms < LIFT_MS {
        return Vec::new();
    }
    (1..=5u64).map(|k| path.at(path_p(ms.saturating_sub(k * 32)), body)).collect()
}

/// The colour of the `k`th trail sample (1..=5): the accent fading into the ground.
pub fn trail_color(theme: &Theme, ground: Color, k: usize) -> Color {
    fade(theme, theme.accent, ground, k as f32 / 6.0)
}

/// A hop from block to block in `stops` steps: the cell after `p` (0..1) of the way.
pub fn hop(from: Cell, to: Cell, p: f32, stops: u32) -> Cell {
    let i = ((p.clamp(0.0, 1.0) * stops as f32).floor() as u32).min(stops);
    let t = i as f32 / stops.max(1) as f32;
    let lerp = |a: u16, b: u16| (a as f32 + (b as f32 - a as f32) * t).round() as u16;
    (lerp(from.0, to.0), lerp(from.1, to.1))
}

// ------------------------------------------------------------------------------------------ small effects

/// The six particles of a test run that passed: cell offsets (columns, rows) from the glyph, `ms` after it.
/// The radius grows `ease_out(t) * 3`; columns count double so the ring looks round.
pub fn particles(ms: u64) -> Vec<(i32, i32)> {
    if ms >= BURST_MS {
        return Vec::new();
    }
    let r = ease_out(ms as f64 / BURST_MS as f64) as f32 * 3.0;
    (0..6)
        .map(|k| {
            let a = k as f32 * std::f32::consts::TAU / 6.0;
            ((a.cos() * r * 2.0).round() as i32, (a.sin() * r * 0.5).round() as i32)
        })
        .collect()
}

/// How far a failed run's glyph is moved `ms` after it appeared: +1, -1, +1, -1 every 60 ms, then still.
pub fn shake(ms: u64) -> i32 {
    if ms >= SHAKE_MS {
        return 0;
    }
    if (ms / SHAKE_STEP_MS) & 1 == 0 {
        1
    } else {
        -1
    }
}

/// The commit glyph's three frames (`·` `•` `◉`), 80 ms each, then the final one.
pub fn commit_frame(ms: u64, ascii: bool) -> &'static str {
    let frames = if ascii { [".", "o", "c"] } else { ["·", "•", "◉"] };
    frames[((ms / COMMIT_FRAME_MS) as usize).min(2)]
}

/// How far a push arrow has slid into its cell (0..1).
pub fn push_p(ms: u64) -> f32 {
    ease_out(ms as f64 / PUSH_MS as f64) as f32
}

/// A hump 0..1..0 over `total` ms; 0 outside.
pub fn hump_ms(ms: u64, total: u64) -> f32 {
    if ms >= total || total == 0 {
        return 0.0;
    }
    (ms as f32 / total as f32 * std::f32::consts::PI).sin()
}

/// A slow breath 0..1..0 with period `period` (a pure function of the time).
pub fn breath(ms: u64, period: u64) -> f32 {
    let phase = (ms % period.max(1)) as f32 / period.max(1) as f32 * std::f32::consts::TAU;
    0.5 - 0.5 * phase.cos()
}

/// Where the satellites of a card sit on its bottom border: the first one cell in from the corner by two,
/// the others two cells apart going left. `slot` counts from 0.
pub fn satellite_x(card: Rect, slot: u16) -> u16 {
    (card.x + card.width).saturating_sub(3 + 2 * slot)
}

// ------------------------------------------------------------------------------------------ the pulse row

/// A figure of the pulse row: glyph, text, colour name by role.
#[derive(Debug, Clone, PartialEq)]
pub struct Figure {
    pub id: &'static str,
    pub text: String,
    pub color: Color,
    pub bold: bool,
}

/// The heartbeat as cells: `▕▁▃▅▇▅▃▏`, with the bright cell at `lit` (None when still).
pub fn heartbeat(buckets: &[u32], cells: usize, theme: &Theme, lit: Option<usize>) -> Vec<Span<'static>> {
    let mut chars: Vec<char> = spark(buckets, cells).chars().collect();
    while chars.len() < cells {
        chars.insert(0, '▁');
    }
    let mut out = vec![Span::styled("▕", theme.dim2())];
    for (i, ch) in chars.iter().enumerate() {
        let c = if lit == Some(i) { theme.accent } else { theme.accent2 };
        let mut st = theme.style(c);
        if theme.mono && lit == Some(i) {
            st = st.add_modifier(Modifier::BOLD);
        }
        out.push(Span::styled(ch.to_string(), st));
    }
    out.push(Span::styled("▏", theme.dim2()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_agent::supervise::EvDetail;

    fn theme() -> Theme {
        Theme::by_name("dark").unwrap()
    }

    fn ev(kind: EvKind, at: Secs) -> Ev {
        Ev::new(at, kind)
    }

    const ALL: [EvKind; 37] = [
        EvKind::SessionStart,
        EvKind::SessionEnd,
        EvKind::TurnStart,
        EvKind::TurnEnd,
        EvKind::Thinking,
        EvKind::Read,
        EvKind::Search,
        EvKind::Edit,
        EvKind::Run,
        EvKind::Web,
        EvKind::Tool,
        EvKind::PermissionWait,
        EvKind::PermissionDone,
        EvKind::Question,
        EvKind::Answer,
        EvKind::SubSpawn,
        EvKind::SubDone,
        EvKind::MsgOut,
        EvKind::MsgIn,
        EvKind::MsgRead,
        EvKind::Compact,
        EvKind::Error,
        EvKind::Retry,
        EvKind::RateLimit,
        EvKind::FileWritten,
        EvKind::TestsPass,
        EvKind::TestsFail,
        EvKind::GitCommit,
        EvKind::GitPush,
        EvKind::Todo,
        EvKind::JobStart,
        EvKind::JobDone,
        EvKind::Idle,
        EvKind::Stall,
        EvKind::Loop,
        EvKind::ModelSwitch,
        EvKind::Cost,
    ];

    #[test]
    fn every_glyph_is_one_cell_wide_and_the_ascii_form_is_one_plain_character() {
        for k in ALL {
            let u = glyph(k, false);
            let a = glyph(k, true);
            assert_eq!(width(u), 1, "{k:?} {u}");
            assert!(a.len() == 1 && a.is_ascii(), "{k:?} {a}");
            // no emoji presentation selector hides in any of them
            assert!(!u.contains('\u{fe0f}'), "{k:?}");
        }
        for f in ["◜", "◝", "◞", "◟", "⠁", "⠂", "⠄", "◐", "◓", "◑", "◒", "·", "•", "◉", "⠋", "⠏"]
        {
            assert_eq!(width(f), 1, "{f}");
        }
    }

    #[test]
    fn the_ribbon_is_exactly_its_width_newest_at_the_right() {
        let t = theme();
        let evs: Vec<Ev> = [EvKind::Read, EvKind::Search, EvKind::Edit, EvKind::Run, EvKind::TestsPass]
            .iter()
            .enumerate()
            .map(|(i, k)| ev(*k, 100 + i as i64))
            .collect();
        let (cells, marks) = ribbon(&evs, 12, &t, false, 110, None);
        let text: String = cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(width(&text), 12);
        assert!(text.ends_with("◦◎✎❯✔"), "{text:?}");
        assert!(text.starts_with("       "));
        assert_eq!(marks.len(), 5);
        assert_eq!(marks[4], (11, EvKind::TestsPass, 104));
    }

    #[test]
    fn a_run_of_one_kind_takes_two_columns_and_the_oldest_events_make_room() {
        let t = theme();
        let mut evs: Vec<Ev> = (0..12).map(|i| ev(EvKind::Read, i)).collect();
        evs.push(Ev { at: 20, kind: EvKind::Edit, n: 3, detail: EvDetail::None });
        let (cells, _) = ribbon(&evs, 12, &t, false, 21, None);
        let text: String = cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(width(&text), 12);
        assert!(text.ends_with("✎3"), "{text:?}");
        assert_eq!(text.matches('◦').count(), 10);
    }

    #[test]
    fn the_live_slot_animates_only_when_asked_and_the_oldest_cells_fade() {
        let t = theme();
        let evs: Vec<Ev> = (0..12).map(|i| ev(if i == 11 { EvKind::Run } else { EvKind::Edit }, i)).collect();
        let still: String = ribbon(&evs, 12, &t, false, 12, None).0.iter().map(|c| c.text.clone()).collect();
        assert!(still.ends_with('❯'));
        let frames: Vec<String> = [0u64, 100, 200, 300]
            .iter()
            .map(|ms| ribbon(&evs, 12, &t, false, 12, Some(*ms)).0.last().map(|c| c.text.clone()).unwrap_or_default())
            .collect();
        assert_eq!(frames, ["◜", "◝", "◞", "◟"]);
        let (cells, _) = ribbon(&evs, 12, &t, false, 12, None);
        assert_ne!(cells[0].style.fg, cells[5].style.fg, "the oldest cell is dimmer than the middle ones");
        assert_eq!(cells[5].style.fg, Some(t.accent));
        assert_eq!(cells[6].style.fg, Some(t.accent), "past 40% they are the same");
    }

    #[test]
    fn an_error_keeps_its_red_for_thirty_seconds_and_then_fades_like_the_rest() {
        let t = theme();
        let evs = vec![ev(EvKind::Error, 0), ev(EvKind::Read, 1), ev(EvKind::Read, 2), ev(EvKind::Read, 3)];
        let fresh = ribbon(&evs, 4, &t, false, 20, None).0;
        let old = ribbon(&evs, 4, &t, false, 100, None).0;
        assert_eq!(fresh[0].style.fg, Some(t.error));
        assert_ne!(old[0].style.fg, Some(t.error));
    }

    #[test]
    fn the_todo_bar_fills_in_eighths() {
        assert_eq!(eighths(0.0, 6), (String::new(), "░░░░░░".to_string()));
        assert_eq!(eighths(1.0, 6), ("██████".to_string(), String::new()));
        let (f, e) = eighths(3.5 / 6.0, 6);
        assert_eq!((f.as_str(), e.as_str()), ("███▌", "░░"));
        for i in 0..=48 {
            let (f, e) = eighths(i as f64 / 48.0, 6);
            assert_eq!(width(&f) + width(&e), 6, "{i}");
        }
    }

    #[test]
    fn the_todo_bar_has_a_fixed_width() {
        let t = theme();
        for (d, n) in [(0u16, 7u16), (3, 7), (7, 7), (12, 40)] {
            let spans = todo_spans(&t, d as f64 / n as f64, d, n, false);
            let w: usize = spans.iter().map(|s| width(&s.content)).sum();
            assert_eq!(w, todo_width(d, n), "{d}/{n}");
        }
    }

    #[test]
    fn the_sparkline_is_scaled_to_its_own_maximum() {
        assert_eq!(spark(&[], 12), "");
        assert_eq!(spark(&[0, 0, 0], 12), "");
        assert_eq!(spark(&[1, 2, 4, 8], 12), "▂▃▅█");
        assert_eq!(spark(&[0, 8], 12), "▁█");
        assert_eq!(width(&spark(&(0..30).collect::<Vec<u32>>(), 12)), 12);
    }

    #[test]
    fn the_drain_bar_shrinks_with_the_wait() {
        assert_eq!(drain(30, 30, 5), "▰▰▰▰▰");
        assert_eq!(drain(18, 30, 5), "▰▰▰▱▱");
        assert_eq!(drain(1, 30, 5), "▰▱▱▱▱");
        assert_eq!(drain(0, 30, 5), "▱▱▱▱▱");
    }

    #[test]
    fn squash_keeps_the_centre_and_the_colours() {
        let a = Style::default().fg(Color::Red);
        let b = Style::default().fg(Color::Blue);
        let spans = vec![Span::styled("abcde", a), Span::styled("fghij", b)];
        let out = squash(spans.clone(), 10, 0.0);
        assert_eq!(out, spans);
        let out = squash(spans, 10, 0.6);
        let text: String = out.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "   defg   ");
        assert_eq!(out[0].style, a);
        assert_eq!(out[1].style, b);
        // all squeezed away
        let gone: String = squash(vec![Span::raw("abc")], 3, 1.0).iter().map(|s| s.content.to_string()).collect();
        assert_eq!(gone, "   ");
    }

    #[test]
    fn squash_level_peaks_in_the_middle_and_is_zero_outside() {
        assert_eq!(squash_level(0), 0.0);
        assert!((squash_level(180) - 0.8).abs() < 0.001);
        assert_eq!(squash_level(360), 0.0);
        assert_eq!(squash_level(9999), 0.0);
        assert!(squash_level(90) < squash_level(180));
    }

    #[test]
    fn the_letter_legs_follow_the_timeline() {
        assert_eq!(leg(0), Leg::Lift);
        assert_eq!(leg(119), Leg::Lift);
        assert!(matches!(leg(120), Leg::Fly(p) if p == 0.0));
        assert!(matches!(leg(570), Leg::Fly(p) if (p - 0.5).abs() < 0.01));
        assert!(matches!(leg(1019), Leg::Fly(p) if p > 0.99));
        assert!(matches!(leg(1020), Leg::Land(p) if p == 0.0));
        assert!(matches!(leg(1439), Leg::Land(p) if p > 0.99));
        assert_eq!(leg(1440), Leg::Gone);
        assert_eq!(path_p(0), 0.0);
        assert_eq!(path_p(1200), 1.0);
        let mut last = 0.0;
        for ms in (0..1440).step_by(30) {
            let p = path_p(ms);
            assert!(p >= last, "never goes back");
            last = p;
        }
    }

    #[test]
    fn a_bounce_goes_out_half_way_and_comes_back() {
        assert_eq!(bounce_p(0), 0.0);
        assert!((bounce_p(300) - 0.5).abs() < 0.001);
        assert_eq!(bounce_p(600), 0.0);
        assert_eq!(bounce_p(5000), 0.0);
    }

    #[test]
    fn the_path_runs_from_anchor_to_anchor_and_never_leaves_the_body() {
        let body = Rect::new(2, 3, 60, 20);
        for (a, b) in
            [((50, 5), (5, 5)), ((5, 5), (50, 20)), ((30, 5), (30, 18)), ((10, 3), (12, 3)), ((59, 22), (2, 3))]
        {
            let path = Path::new(a, b);
            assert_eq!(path.at(0.0, body), clamp_in(a.0 as f32, a.1 as f32, body));
            assert_eq!(path.at(1.0, body), clamp_in(b.0 as f32, b.1 as f32, body));
            for i in 0..=40 {
                let (x, y) = path.at(i as f32 / 40.0, body);
                assert!(
                    x >= body.x && x < body.x + body.width && y >= body.y && y < body.y + body.height,
                    "{a:?}->{b:?} {x},{y}"
                );
            }
        }
    }

    #[test]
    fn the_curve_rises_above_a_flat_pair_and_bows_right_for_one_column() {
        let body = Rect::new(0, 0, 100, 40);
        let flat = Path::new((60, 20), (10, 20));
        let mid = flat.at(0.5, body);
        assert!(mid.1 < 20, "the letter arcs up: {mid:?}");
        let tall = Path::new((30, 5), (30, 25));
        let mid = tall.at(0.5, body);
        assert!(mid.0 > 30, "bows right: {mid:?}");
    }

    #[test]
    fn the_trail_has_five_samples_behind_the_head_and_none_before_the_lift() {
        let body = Rect::new(0, 0, 100, 40);
        let path = Path::new((70, 10), (10, 10));
        assert!(trail(50, &path, body).is_empty());
        let t = trail(570, &path, body);
        assert_eq!(t.len(), 5);
        let head = path.at(path_p(570), body);
        assert!(t[0].0 >= head.0, "behind the head, nearer the sender: {t:?} {head:?}");
        assert_eq!(trail_glyph(0, false), "•");
        assert_eq!(trail_glyph(4, false), ".");
        assert_eq!(trail_glyph(0, true), ".");
    }

    #[test]
    fn a_hop_steps_between_blocks() {
        let (a, b) = ((2, 2), (50, 8));
        assert_eq!(hop(a, b, 0.0, 5), a);
        assert_eq!(hop(a, b, 1.0, 5), b);
        let mid = hop(a, b, 0.5, 5);
        assert!(mid.0 > 2 && mid.0 < 50);
        assert_eq!(hop(a, b, 0.19, 5), a, "holds a stop until the next");
    }

    #[test]
    fn the_burst_radiates_six_particles_and_dies_at_400_ms() {
        assert_eq!(particles(0).len(), 6);
        assert!(particles(0).iter().all(|p| *p == (0, 0)));
        let wide = particles(399).iter().map(|p| p.0.abs()).max().unwrap();
        assert!(wide >= 5, "{wide}");
        assert!(particles(400).is_empty());
        assert!(particles(100).iter().map(|p| p.0.abs()).max() < particles(300).iter().map(|p| p.0.abs()).max());
    }

    #[test]
    fn a_failed_run_shakes_four_times_then_stops() {
        let seq: Vec<i32> = [0u64, 60, 120, 180, 240, 500].iter().map(|m| shake(*m)).collect();
        assert_eq!(seq, [1, -1, 1, -1, 0, 0]);
    }

    #[test]
    fn a_commit_goes_through_three_frames() {
        assert_eq!(commit_frame(0, false), "·");
        assert_eq!(commit_frame(80, false), "•");
        assert_eq!(commit_frame(160, false), "◉");
        assert_eq!(commit_frame(900, false), "◉");
        assert_eq!(commit_frame(0, true), ".");
    }

    #[test]
    fn satellites_sit_two_cells_apart_from_the_right_corner() {
        let card = Rect::new(10, 5, 36, 6);
        assert_eq!(satellite_x(card, 0), 43);
        assert_eq!(satellite_x(card, 1), 41);
        assert_eq!(satellite_x(card, 3), 37);
        assert!(satellite_x(card, 3) > card.x);
    }

    #[test]
    fn breath_and_hump_are_bounded_and_periodic() {
        assert!(breath(0, 1600) < 0.001);
        assert!(breath(800, 1600) > 0.999);
        assert!((breath(1600, 1600) - breath(0, 1600)).abs() < 0.001);
        assert_eq!(hump_ms(450, 450), 0.0);
        assert!(hump_ms(225, 450) > 0.99);
    }

    #[test]
    fn the_heartbeat_has_a_bright_cell_only_when_asked() {
        let t = theme();
        let beat = heartbeat(&[1, 3, 5, 7, 5, 3, 1, 2], 8, &t, Some(2));
        let text: String = beat.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(width(&text), 10);
        assert_eq!(beat[3].style.fg, Some(t.accent));
        assert_eq!(beat[1].style.fg, Some(t.accent2));
        let calm = heartbeat(&[1, 3, 5], 8, &t, None);
        assert!(calm.iter().all(|s| s.style.fg != Some(t.accent)));
    }
}
