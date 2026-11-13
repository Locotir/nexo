//! Turns a recording into an animated GIF: the frames laid on a timeline, the action log drawn over them.
//!
//! The browser only sends a frame when the page changes, so a click that changes nothing leaves no frame to hang its
//! circle on. The timeline is therefore cut wherever an overlay starts or ends, and a frame is repeated across the
//! pieces: the picture shown is always the one that was on screen, with whatever overlays were active at that time.

use crate::overlay::Canvas;
use crate::record::{ActionEvent, ActionKind, Frame, RecordSummary};
use crate::{Error, Result};
use std::io::Cursor;
use std::path::Path;

/// Shortest frame delay browsers honour, in 1/100 s (anything lower is shown as 10).
const MIN_DELAY_CS: i64 = 2;
/// How long the last frame is shown at least, in seconds.
const LAST_FRAME_S: f64 = 0.1;
/// Pieces shorter than this (seconds) are merged into their neighbour.
const MIN_PIECE_S: f64 = 0.03;
/// GIF frames written at most.
const MAX_OUT_FRAMES: usize = 500;
/// How long each overlay shows, in seconds: the press of a click, the whole click mark, a drag arrow, a label.
const PRESS_S: f64 = 0.4;
const CLICK_S: f64 = 0.8;
const DRAG_S: f64 = 1.0;
const LABEL_S: f64 = 1.5;
/// Labels shown together at most.
const MAX_LABELS: usize = 3;
/// How long the last action stays visible after the recording ends, at most (seconds).
const TAIL_S: f64 = 0.8;

/// What to draw on the exported GIF, and how hard to squeeze it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportOptions {
    /// Orange circles where the page was clicked.
    pub clicks: bool,
    /// Red arrows along drags.
    pub drags: bool,
    /// Black boxes naming each action.
    pub labels: bool,
    /// An orange bar along the bottom showing how far the recording has played.
    pub progress: bool,
    /// A small `nexo` mark in a corner.
    pub watermark: bool,
    /// Palette search effort, 1 (best, slowest) to 30 (fastest, roughest).
    pub quality: u8,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self { clicks: true, drags: true, labels: true, progress: true, watermark: true, quality: 10 }
    }
}

impl ExportOptions {
    /// The frames as they were captured, nothing drawn on them.
    pub fn plain() -> Self {
        Self { clicks: false, drags: false, labels: false, progress: false, watermark: false, quality: 10 }
    }
}

/// One thing drawn over a piece of the timeline: an action of the log, in one of its looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Overlay {
    /// Click circle: 0 the press, 1 the spreading ring.
    Click(usize, u8),
    Drag(usize),
    Label(usize),
}

/// A stretch of the GIF: one captured frame with the same overlays over it all along.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Piece {
    pub frame: usize,
    pub start: f64,
    pub end: f64,
    /// How far into the recording this piece ends, 0 to 1.
    pub done: f64,
    pub overlays: Vec<Overlay>,
}

/// The overlays visible at `t` (seconds), `events` having their times in `begin..`.
fn active_at(t: f64, begin: f64, events: &[ActionEvent], opts: &ExportOptions) -> Vec<Overlay> {
    let mut out = Vec::new();
    let mut labels = Vec::new();
    for (i, e) in events.iter().enumerate() {
        let age = t - e.at.max(begin);
        if age < 0.0 {
            continue;
        }
        match e.kind {
            ActionKind::Click if opts.clicks && age < CLICK_S && e.x.is_some() && e.y.is_some() => {
                out.push(Overlay::Click(i, u8::from(age >= PRESS_S)));
            }
            ActionKind::Drag if opts.drags && age < DRAG_S && e.x2.is_some() && e.y2.is_some() => {
                out.push(Overlay::Drag(i));
            }
            _ => {}
        }
        if opts.labels && age < LABEL_S && !e.label.is_empty() {
            labels.push(Overlay::Label(i));
        }
    }
    out.extend(labels.split_off(labels.len().saturating_sub(MAX_LABELS)));
    out
}

/// Lays frames (starting at `times`, ascending, in seconds) and the log on one timeline ending at `end`.
pub(crate) fn plan(times: &[f64], events: &[ActionEvent], end: f64, opts: &ExportOptions) -> Vec<Piece> {
    let Some(&begin) = times.first() else { return Vec::new() };
    let end = end.max(begin + LAST_FRAME_S);
    let mut cuts: Vec<f64> = vec![begin, end];
    cuts.extend(times.iter().copied());
    for e in events {
        let t = e.at.max(begin);
        cuts.extend([t, t + PRESS_S, t + CLICK_S, t + DRAG_S, t + LABEL_S]);
    }
    if opts.progress {
        // A steady grid so the bar moves through a long still picture.
        let step = ((end - begin) / 150.0).max(0.2);
        let mut t = begin + step;
        while t < end {
            cuts.push(t);
            t += step;
        }
    }
    cuts.retain(|t| *t >= begin && *t <= end);
    cuts.sort_by(f64::total_cmp);
    let mut kept: Vec<f64> = Vec::with_capacity(cuts.len());
    for t in cuts {
        match kept.last() {
            Some(&last) if t - last < MIN_PIECE_S => {
                // The end is never dropped; the cut before it gives way instead.
                if t >= end && kept.len() > 1 {
                    kept.pop();
                    kept.push(t);
                }
            }
            _ => kept.push(t),
        }
    }
    while kept.len() > MAX_OUT_FRAMES + 1 {
        let last = kept.len() - 1;
        let mut i = 0;
        kept.retain(|_| {
            i += 1;
            (i - 1) % 2 == 0 || i - 1 == last
        });
    }
    let span = (end - begin).max(f64::EPSILON);
    let mut pieces: Vec<Piece> = Vec::new();
    for w in kept.windows(2) {
        let (a, b) = (w[0], w[1]);
        let mid = (a + b) / 2.0;
        let frame = times.partition_point(|t| *t <= mid).saturating_sub(1);
        let overlays = active_at(mid, begin, events, opts);
        let done = (b - begin) / span;
        match pieces.last_mut() {
            Some(p) if p.frame == frame && p.overlays == overlays && !opts.progress => {
                p.end = b;
                p.done = done;
            }
            _ => pieces.push(Piece { frame, start: a, end: b, done, overlays }),
        }
    }
    pieces
}

fn encode_err(e: impl std::fmt::Display) -> Error {
    Error::Encode(e.to_string())
}

/// Decodes a PNG into RGBA8 pixels and its size.
fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(encode_err)?;
    let size = reader.output_buffer_size().ok_or_else(|| encode_err("frame too large"))?;
    let mut buf = vec![0; size];
    let info = reader.next_frame(&mut buf).map_err(encode_err)?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => {
            buf.as_chunks::<2>().0.iter().flat_map(|p| [p[0], p[0], p[0], p[1]]).collect()
        }
        png::ColorType::Indexed => return Err(encode_err("unexpanded palette frame")),
    };
    Ok((info.width, info.height, rgba))
}

/// Width and height of a PNG without decoding its pixels.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let reader = png::Decoder::new(Cursor::new(bytes)).read_info().ok()?;
    let info = reader.info();
    Some((info.width, info.height))
}

/// Draws the overlays of one piece over a frame of `w` x `h` whose page is `page_width` CSS pixels wide.
fn draw(canvas: &mut Canvas, piece: &Piece, events: &[ActionEvent], scale: f32, opts: &ExportOptions) {
    let pt = |x: Option<f64>, y: Option<f64>| Some((x? as f32 * scale, y? as f32 * scale));
    for o in &piece.overlays {
        match *o {
            Overlay::Click(i, phase) => {
                let e = &events[i];
                if let Some((x, y)) = pt(e.x, e.y) {
                    canvas.click_mark(x, y, phase);
                }
            }
            Overlay::Drag(i) => {
                let e = &events[i];
                if let (Some(a), Some(b)) = (pt(e.x, e.y), pt(e.x2, e.y2)) {
                    canvas.drag_mark(a, b);
                }
            }
            Overlay::Label(_) => {}
        }
    }
    let labels: Vec<&str> = piece
        .overlays
        .iter()
        .filter_map(|o| if let Overlay::Label(i) = o { Some(events[*i].label.as_str()) } else { None })
        .collect();
    if !labels.is_empty() {
        canvas.labels(&labels);
    }
    if opts.progress {
        canvas.progress(piece.done);
    }
    if opts.watermark {
        canvas.watermark();
    }
}

/// Encodes a recording to a looping GIF at `path`. The canvas is the size of the first frame; frames of another
/// size (the viewport was resized mid-recording) are left out. `stopped_at` is when capturing ended (epoch seconds).
pub(crate) fn render_gif(
    frames: &[Frame],
    events: &[ActionEvent],
    stopped_at: Option<f64>,
    opts: &ExportOptions,
    path: &Path,
) -> Result<RecordSummary> {
    let first = frames.first().ok_or_else(|| Error::Unusable {
        target: "recording".to_string(),
        reason: "no frames were captured (the page did not change)".to_string(),
    })?;
    let size = png_size(&first.png).ok_or_else(|| encode_err("unreadable first frame"))?;
    let usable: Vec<&Frame> = frames.iter().filter(|f| png_size(&f.png) == Some(size)).collect();
    let mut times: Vec<f64> = Vec::with_capacity(usable.len());
    for f in &usable {
        times.push(f.at.max(times.last().copied().unwrap_or(f.at)));
    }
    let last_at = times.last().copied().unwrap_or_default();
    // The last action stays visible for a moment even when the recording was stopped right after it.
    let tail = events.last().map_or(0.0, |e| e.at + TAIL_S);
    let end = stopped_at.unwrap_or(last_at).max(last_at + LAST_FRAME_S).max(tail);
    let pieces = plan(&times, events, end, opts);

    let (w, h) = size;
    let (gw, gh) = (u16::try_from(w).map_err(encode_err)?, u16::try_from(h).map_err(encode_err)?);
    let quality = i32::from(opts.quality.clamp(1, 30));
    let begin = times.first().copied().unwrap_or_default();
    let mut out = Vec::new();
    let (mut count, mut cs_total) = (0, 0_i64);
    {
        let mut enc = gif::Encoder::new(&mut out, gw, gh, &[]).map_err(encode_err)?;
        enc.set_repeat(gif::Repeat::Infinite).map_err(encode_err)?;
        let mut decoded: Option<(usize, Vec<u8>)> = None;
        for piece in &pieces {
            if decoded.as_ref().map(|d| d.0) != Some(piece.frame) {
                decoded = Some((piece.frame, decode_png(&usable[piece.frame].png)?.2));
            }
            let mut rgba = decoded.as_ref().map(|d| d.1.clone()).unwrap_or_default();
            let scale = match usable[piece.frame].page_width {
                pw if pw > 0.0 => (f64::from(w) / pw) as f32,
                _ => 1.0,
            };
            if let Some(mut canvas) = Canvas::new(w, h, &mut rgba) {
                draw(&mut canvas, piece, events, scale, opts);
            }
            let mut g = gif::Frame::from_rgba_speed(gw, gh, &mut rgba, quality);
            // Delays come from absolute times so rounding does not pile up.
            let cs = (((piece.end - begin) * 100.0).round() as i64 - cs_total).max(MIN_DELAY_CS);
            g.delay = u16::try_from(cs).unwrap_or(u16::MAX);
            enc.write_frame(&g).map_err(encode_err)?;
            count += 1;
            cs_total += cs;
        }
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, &out)?;
    Ok(RecordSummary { frames: count, millis: cs_total.max(0) as u64 * 10, bytes: out.len() as u64 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(at: f64) -> ActionEvent {
        let mut e = ActionEvent::click("click", 10.0, 10.0);
        e.at = at;
        e
    }

    fn off() -> ExportOptions {
        ExportOptions::plain()
    }

    #[test]
    fn without_overlays_each_frame_is_one_piece() {
        let p = plan(&[0.0, 1.0, 1.5], &[], 2.0, &off());
        assert_eq!(
            p.iter().map(|p| (p.frame, p.start, p.end)).collect::<Vec<_>>(),
            [(0, 0.0, 1.0), (1, 1.0, 1.5), (2, 1.5, 2.0)]
        );
        assert!(p.iter().all(|p| p.overlays.is_empty()));
    }

    #[test]
    fn a_click_between_frames_cuts_the_picture_so_its_circle_shows() {
        let opts = ExportOptions { clicks: true, ..off() };
        let p = plan(&[0.0, 3.0], &[click(1.0)], 4.0, &opts);
        // Frame 0 is cut at the click, at the end of the press, and at the end of the mark.
        let seen: Vec<(usize, f64, Vec<Overlay>)> = p.iter().map(|p| (p.frame, p.start, p.overlays.clone())).collect();
        assert_eq!(
            seen,
            [
                (0, 0.0, vec![]),
                (0, 1.0, vec![Overlay::Click(0, 0)]),
                (0, 1.4, vec![Overlay::Click(0, 1)]),
                (0, 1.8, vec![]),
                (1, 3.0, vec![]),
            ]
        );
        assert!((p.last().unwrap().end - 4.0).abs() < 1e-9);
    }

    #[test]
    fn labels_stack_the_latest_three_and_follow_the_option() {
        let events: Vec<ActionEvent> = (0..5)
            .map(|i| {
                let mut e = ActionEvent::other(format!("step {i}"));
                e.at = 1.0 + f64::from(i) * 0.1;
                e
            })
            .collect();
        let on = ExportOptions { labels: true, ..off() };
        let p = plan(&[0.0], &events, 3.0, &on);
        let busiest = p.iter().find(|p| p.start >= 1.4 && p.start < 1.5).unwrap();
        assert_eq!(busiest.overlays, [Overlay::Label(2), Overlay::Label(3), Overlay::Label(4)]);
        let p = plan(&[0.0], &events, 3.0, &off());
        assert!(p.iter().all(|p| p.overlays.is_empty()));
    }

    #[test]
    fn a_drag_shows_its_arrow_and_a_click_without_a_point_shows_nothing() {
        let mut d = ActionEvent::drag("drag", 1.0, 1.0, 9.0, 9.0);
        d.at = 0.5;
        let mut c = ActionEvent::click("click", 1.0, 1.0);
        c.at = 0.5;
        c.x = None;
        let opts = ExportOptions { clicks: true, drags: true, ..off() };
        let p = plan(&[0.0], &[d, c], 2.0, &opts);
        assert!(p.iter().any(|p| p.overlays == [Overlay::Drag(0)]));
        assert!(p.iter().all(|p| !p.overlays.iter().any(|o| matches!(o, Overlay::Click(..)))));
        let none = plan(&[0.0], &[ActionEvent::drag("drag", 1.0, 1.0, 9.0, 9.0)], 2.0, &off());
        assert!(none.iter().all(|p| p.overlays.is_empty()));
    }

    #[test]
    fn the_progress_bar_runs_to_the_end_and_events_before_the_first_frame_are_clamped() {
        let opts = ExportOptions { progress: true, clicks: true, ..off() };
        let p = plan(&[10.0, 12.0], &[click(5.0)], 13.0, &opts);
        assert!((p[0].start - 10.0).abs() < 1e-9);
        assert!(p[0].overlays == [Overlay::Click(0, 0)], "{:?}", p[0]);
        assert!((p.last().unwrap().done - 1.0).abs() < 1e-9);
        assert!(p.windows(2).all(|w| w[0].done < w[1].done && (w[0].end - w[1].start).abs() < 1e-9));
    }

    #[test]
    fn the_piece_count_is_capped_and_covers_the_whole_time() {
        let events: Vec<ActionEvent> = (0..400).map(|i| click(f64::from(i) * 0.1)).collect();
        let opts = ExportOptions { clicks: true, progress: true, labels: true, ..off() };
        let p = plan(&[0.0], &events, 40.0, &opts);
        assert!(p.len() <= MAX_OUT_FRAMES, "{}", p.len());
        assert!((p[0].start).abs() < 1e-9 && (p.last().unwrap().end - 40.0).abs() < 1e-9);
    }

    #[test]
    fn no_frames_no_plan() {
        assert!(plan(&[], &[click(1.0)], 5.0, &ExportOptions::default()).is_empty());
    }
}
