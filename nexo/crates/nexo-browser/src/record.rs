//! Screen recording: the page is streamed over `Page.startScreencast` and kept in memory, together with a log of
//! the actions taken meanwhile, until it is exported as an animated GIF (see `export.rs`).
//!
//! Some systems' Chrome sends no screencast frames at all (the headless Chrome of a macOS runner sent none while the
//! page changed). A recording therefore does not rest on the screencast alone: while it has sent nothing, the page is
//! captured with `Page.captureScreenshot` at the start, after each action and at the stop, scaled the way screencast
//! frames are, so the recording still holds every state the actions left. Once the screencast has sent a frame it is
//! trusted as before: it sends one whenever the page changes, and no screenshot is taken.

use crate::export::{decode_png, png_size};
use crate::{lock, Error, Result};
use base64::Engine;
use chromiumoxide::cdp::browser_protocol::page::{
    CaptureScreenshotFormat, CaptureScreenshotParams, EventScreencastFrame, ScreencastFrameAckParams,
    StartScreencastFormat, StartScreencastParams, StopScreencastParams,
};
use chromiumoxide::page::Page;
use futures::StreamExt;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Actions kept in the log at most; later ones are dropped.
const MAX_EVENTS: usize = 500;
/// How long after the start the screencast has to send its first frame before screenshots stand in for it. A
/// working screencast sends one at once, whether or not the page moves; the margin is for a loaded machine, where a
/// screenshot taken too early would only add a second copy of that first frame.
const FIRST_FRAME_WAIT: Duration = Duration::from_secs(1);
/// Longest one screenshot is waited for, so a page that cannot paint does not hold up the action that follows.
const SHOT_TIMEOUT: Duration = Duration::from_secs(10);

/// How a recording is captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordOptions {
    /// Frames are scaled down to fit this width in pixels.
    pub max_width: u32,
    /// Frames are scaled down to fit this height in pixels.
    pub max_height: u32,
    /// Keep every n-th frame the browser produces (1 keeps all).
    pub every_nth_frame: u32,
    /// Frames kept at most; later ones are dropped to bound memory.
    pub max_frames: usize,
    /// Bytes of PNG kept at most; frames that would go over are dropped.
    pub max_bytes: usize,
    /// Seconds of page time kept at most, counted from the first frame; later frames are dropped.
    pub max_seconds: u64,
    /// Tests only: `false` never starts the screencast, as on a system whose browser sends no screencast frames.
    #[cfg(test)]
    pub(crate) screencast: bool,
    /// Tests only: `false` makes every screenshot taken in place of the screencast fail.
    #[cfg(test)]
    pub(crate) screenshots: bool,
}

impl Default for RecordOptions {
    fn default() -> Self {
        Self {
            max_width: 800,
            max_height: 600,
            every_nth_frame: 1,
            max_frames: 300,
            max_bytes: 48 * 1024 * 1024,
            max_seconds: 180,
            #[cfg(test)]
            screencast: true,
            #[cfg(test)]
            screenshots: true,
        }
    }
}

/// What a finished export came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RecordSummary {
    pub frames: usize,
    /// Playing time of the GIF in milliseconds.
    pub millis: u64,
    /// Size of the written file.
    pub bytes: u64,
}

/// What a recording holds right now.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct RecordInfo {
    /// Still capturing (`false` once stopped).
    pub running: bool,
    /// Frames captured.
    pub frames: usize,
    /// Actions logged.
    pub events: usize,
    /// Page time covered, in milliseconds.
    pub millis: u64,
    /// A cap (frames, bytes or time) was hit and later frames were dropped.
    pub truncated: bool,
}

/// What kind of action an [`ActionEvent`] is, which decides how it is drawn on export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    /// A click at a point: an orange circle.
    Click,
    /// A drag from one point to another: a red arrow.
    Drag,
    /// Anything else: only its label.
    Other,
}

/// One browser action taken while recording. Points are in CSS pixels of the viewport.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionEvent {
    /// Seconds since the Unix epoch; set when the event is logged.
    pub at: f64,
    pub kind: ActionKind,
    pub label: String,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub x2: Option<f64>,
    pub y2: Option<f64>,
}

impl ActionEvent {
    pub fn click(label: impl Into<String>, x: f64, y: f64) -> Self {
        Self { at: 0.0, kind: ActionKind::Click, label: label.into(), x: Some(x), y: Some(y), x2: None, y2: None }
    }

    pub fn drag(label: impl Into<String>, x: f64, y: f64, x2: f64, y2: f64) -> Self {
        Self {
            at: 0.0,
            kind: ActionKind::Drag,
            label: label.into(),
            x: Some(x),
            y: Some(y),
            x2: Some(x2),
            y2: Some(y2),
        }
    }

    pub fn other(label: impl Into<String>) -> Self {
        Self { at: 0.0, kind: ActionKind::Other, label: label.into(), x: None, y: None, x2: None, y2: None }
    }
}

/// Seconds since the Unix epoch.
pub(crate) fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// One captured frame.
pub(crate) struct Frame {
    pub png: Vec<u8>,
    /// Time the browser swapped it in, in seconds since the epoch.
    pub at: f64,
    /// Width of the page the frame shows, in CSS pixels (frame pixels per CSS pixel is its width over this).
    pub page_width: f64,
}

pub(crate) struct Shared {
    pub frames: Vec<Frame>,
    pub events: Vec<ActionEvent>,
    bytes: usize,
    pub truncated: bool,
    /// Why the last screenshot taken in place of the screencast failed, for the export to say when no frame came.
    pub shot_error: Option<String>,
    /// Screenshots taken in place of the screencast, for the tests.
    #[cfg(test)]
    pub shots: usize,
    max_frames: usize,
    max_bytes: usize,
    max_seconds: f64,
}

impl Shared {
    fn new(opts: &RecordOptions) -> Self {
        Shared {
            frames: Vec::new(),
            events: Vec::new(),
            bytes: 0,
            truncated: false,
            shot_error: None,
            #[cfg(test)]
            shots: 0,
            max_frames: opts.max_frames.max(1),
            max_bytes: opts.max_bytes,
            max_seconds: opts.max_seconds.max(1) as f64,
        }
    }

    /// Keeps a frame, unless a cap (frames, bytes or time) is hit; then the recording is marked cut short.
    fn push(&mut self, frame: Frame) {
        let first = self.frames.first().map_or(frame.at, |f| f.at);
        if self.frames.len() >= self.max_frames
            || self.bytes + frame.png.len() > self.max_bytes
            || frame.at - first > self.max_seconds
        {
            self.truncated = true;
            return;
        }
        self.bytes += frame.png.len();
        self.frames.push(frame);
    }
}

impl Frame {
    /// A frame from its PNG bytes and capture time in seconds (the relay backend builds them from raw protocol events).
    pub(crate) fn new(png: Vec<u8>, at: f64) -> Frame {
        Frame { png, at, page_width: 0.0 }
    }
}

/// Takes the frame a silent screencast did not send. Cheap to clone, so the browser can take one without holding
/// its lock on the recorder across the screenshot.
#[derive(Clone)]
pub(crate) struct Fallback {
    page: Page,
    shared: Arc<Mutex<Shared>>,
    /// Turns true with the first screencast frame.
    cast: watch::Receiver<bool>,
    /// Until then the screencast may still send its first frame.
    grace_until: Instant,
    /// Frames are scaled down to fit this box, as the screencast scales its own.
    max: (u32, u32),
    #[cfg(test)]
    screenshots: bool,
}

impl Fallback {
    /// Makes sure the recording shows the page as it is now. While the screencast has sent nothing, this waits out
    /// what is left of its first-frame time and then, if still nothing came, adds a screenshot in its place.
    pub(crate) async fn fill_in(&self) {
        let mut cast = self.cast.clone();
        let left = self.grace_until.saturating_duration_since(Instant::now());
        let _ = tokio::time::timeout(left, cast.wait_for(|seen| *seen)).await;
        if *cast.borrow() {
            return;
        }
        #[cfg(test)]
        {
            lock(&self.shared).shots += 1;
        }
        let at = now();
        let shot = match tokio::time::timeout(SHOT_TIMEOUT, self.shoot()).await {
            Ok(shot) => shot,
            Err(_) => Err(format!("the browser did not answer within {} s", SHOT_TIMEOUT.as_secs())),
        };
        let mut s = lock(&self.shared);
        match shot {
            Ok((png, page_width)) => s.push(Frame { png, at, page_width }),
            Err(e) => s.shot_error = Some(e),
        }
    }

    /// The page as it is now, as a PNG of the size the screencast would give it, and the page's width in CSS pixels.
    /// The error is the text the export gives when no frame was captured at all.
    async fn shoot(&self) -> std::result::Result<(Vec<u8>, f64), String> {
        #[cfg(test)]
        if !self.screenshots {
            return Err("screenshots are switched off by the test".to_string());
        }
        let params = CaptureScreenshotParams::builder().format(CaptureScreenshotFormat::Png).build();
        let shot = self.page.execute(params).await.map_err(|e| Error::from(e).to_string())?;
        let png = base64::engine::general_purpose::STANDARD
            .decode(AsRef::<str>::as_ref(&shot.result.data))
            .map_err(|e| format!("unreadable screenshot: {e}"))?;
        let png = fit_png(png, self.max).map_err(|e| e.to_string())?;
        // The page's width places the action marks on the frame; without it they are drawn unscaled.
        let page_width = match self.page.evaluate("innerWidth").await {
            Ok(r) => r.into_value::<f64>().unwrap_or(0.0),
            Err(_) => 0.0,
        };
        Ok((png, page_width))
    }
}

/// The size the screencast gives a frame of `w` x `h` pixels: scaled down to fit the box (never up), rounded, the
/// way Chrome sizes its screencast frames.
pub(crate) fn fitted((w, h): (u32, u32), (max_w, max_h): (u32, u32)) -> (u32, u32) {
    let (w, h) = (w.max(1), h.max(1));
    let scale = 1f64.min(f64::from(max_w.max(1)) / f64::from(w)).min(f64::from(max_h.max(1)) / f64::from(h)) as f32;
    let side = |n: u32| ((n as f32 * scale).round() as u32).max(1);
    (side(w), side(h))
}

/// A screenshot as the screencast would have sent it: the same PNG when it already fits the box, else shrunk to
/// the screencast's size.
fn fit_png(png: Vec<u8>, max: (u32, u32)) -> Result<Vec<u8>> {
    let size = png_size(&png).ok_or_else(|| Error::Encode("unreadable screenshot".to_string()))?;
    let want = fitted(size, max);
    if want == size {
        return Ok(png);
    }
    let (w, h, rgba) = decode_png(&png)?;
    let rgb = shrink(&rgba, (w, h), want);
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, want.0, want.1);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut writer = enc.write_header().map_err(|e| Error::Encode(e.to_string()))?;
    writer.write_image_data(&rgb).map_err(|e| Error::Encode(e.to_string()))?;
    writer.finish().map_err(|e| Error::Encode(e.to_string()))?;
    Ok(out)
}

/// Scales RGBA pixels of `w` x `h` down to RGB of `dw` x `dh`, each pixel the mean of the source pixels it covers.
pub(crate) fn shrink(rgba: &[u8], (w, h): (u32, u32), (dw, dh): (u32, u32)) -> Vec<u8> {
    let cover = |o: u32, d: u32, s: u32| {
        let a = (u64::from(o) * u64::from(s) / u64::from(d)) as usize;
        let b = (u64::from(o + 1) * u64::from(s)).div_ceil(u64::from(d)) as usize;
        a..b.clamp(a + 1, s as usize)
    };
    let mut out = Vec::with_capacity(dw as usize * dh as usize * 3);
    for oy in 0..dh {
        let rows = cover(oy, dh, h);
        for ox in 0..dw {
            let cols = cover(ox, dw, w);
            let mut sum = [0u32; 3];
            for y in rows.clone() {
                let row = &rgba[(y * w as usize + cols.start) * 4..(y * w as usize + cols.end) * 4];
                for p in row.as_chunks::<4>().0 {
                    sum = [sum[0] + u32::from(p[0]), sum[1] + u32::from(p[1]), sum[2] + u32::from(p[2])];
                }
            }
            let n = (rows.len() * cols.len()) as u32;
            out.extend(sum.map(|c| ((c + n / 2) / n) as u8));
        }
    }
    out
}

/// A recording on one page: capturing until [`Recorder::stop`], then holding its frames until the browser drops it.
pub(crate) struct Recorder {
    fallback: Fallback,
    pub shared: Arc<Mutex<Shared>>,
    task: Option<JoinHandle<()>>,
    /// Epoch seconds at which capturing stopped.
    pub stopped_at: Option<f64>,
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl Recorder {
    /// Starts the screencast and returns once the recording holds the page as it is: the screencast's first frame,
    /// or a screenshot in its place when none came in time.
    pub(crate) async fn start(page: Page, opts: &RecordOptions) -> Result<Self> {
        let shared = Arc::new(Mutex::new(Shared::new(opts)));
        let (seen, cast) = watch::channel(false);
        // The listener goes up before the screencast starts so the first frame is not lost.
        let mut stream = page.event_listener::<EventScreencastFrame>().await?;
        let sink = Arc::clone(&shared);
        let acker = page.clone();
        let task = tokio::spawn(async move {
            while let Some(event) = stream.next().await {
                // Chrome sends no further frame until this one is acknowledged.
                let _ = acker.execute(ScreencastFrameAckParams::new(event.session_id)).await;
                let Ok(png) = base64::engine::general_purpose::STANDARD.decode(AsRef::<str>::as_ref(&event.data))
                else {
                    continue;
                };
                seen.send_replace(true);
                let at = event.metadata.timestamp.as_ref().map_or_else(now, |t| *t.inner());
                let page_width = event.metadata.device_width;
                lock(&sink).push(Frame { png, at, page_width });
            }
        });
        let (max_width, max_height) = (opts.max_width.max(1), opts.max_height.max(1));
        let fallback = Fallback {
            page,
            shared: Arc::clone(&shared),
            cast,
            grace_until: Instant::now(),
            max: (max_width, max_height),
            #[cfg(test)]
            screenshots: opts.screenshots,
        };
        let mut recorder = Recorder { fallback, shared, task: Some(task), stopped_at: None };
        let params = StartScreencastParams {
            format: Some(StartScreencastFormat::Png),
            quality: None,
            max_width: Some(i64::from(max_width)),
            max_height: Some(i64::from(max_height)),
            every_nth_frame: Some(i64::from(opts.every_nth_frame.max(1))),
        };
        #[cfg(test)]
        let cast = opts.screencast;
        #[cfg(not(test))]
        let cast = true;
        if cast {
            // On failure the recorder is dropped here, which stops the listener.
            recorder.fallback.page.execute(params).await?;
        }
        recorder.fallback.grace_until = Instant::now() + FIRST_FRAME_WAIT;
        recorder.fallback.fill_in().await;
        Ok(recorder)
    }

    pub(crate) fn running(&self) -> bool {
        self.task.is_some()
    }

    /// What takes the frame after an action while capturing; `None` once stopped.
    pub(crate) fn fallback(&self) -> Option<Fallback> {
        self.running().then(|| self.fallback.clone())
    }

    /// Stops capturing; the frames and the log stay. With `last_frame` the page as it is now is made sure of first
    /// (see [`Fallback::fill_in`]). Stopping a stopped recording does nothing.
    pub(crate) async fn stop(&mut self, last_frame: bool) -> Result<()> {
        let Some(task) = self.task.take() else { return Ok(()) };
        if last_frame {
            self.fallback.fill_in().await;
        }
        let stopped = self.fallback.page.execute(StopScreencastParams::default()).await;
        // Frames the browser already sent are given a moment to reach the sink before the listener goes.
        tokio::task::yield_now().await;
        task.abort();
        self.stopped_at = Some(now());
        stopped.map(drop).map_err(Error::from)
    }

    /// Logs an action at the current time; ignored once stopped or when the log is full.
    pub(crate) fn note(&self, mut event: ActionEvent) {
        if self.task.is_none() {
            return;
        }
        let mut s = lock(&self.shared);
        if s.events.len() < MAX_EVENTS {
            event.at = now();
            s.events.push(event);
        }
    }

    pub(crate) fn info(&self) -> RecordInfo {
        let s = lock(&self.shared);
        let millis = match (s.frames.first(), s.frames.last()) {
            (Some(a), Some(b)) => {
                let end = self.stopped_at.map_or(b.at, |t| t.max(b.at));
                ((end - a.at).max(0.0) * 1000.0).round() as u64
            }
            _ => 0,
        };
        RecordInfo {
            running: self.running(),
            frames: s.frames.len(),
            events: s.events.len(),
            millis,
            truncated: s.truncated,
        }
    }
}

/// Why a recording holds no frame, saying only what is known: the screencast sent none, and, when screenshots
/// were tried in its place, how they failed.
pub(crate) fn why_empty(shot_error: Option<&str>) -> String {
    match shot_error {
        Some(e) => format!(
            "no frames were captured: the browser sent no screencast frames, and the screenshots taken in their place \
             failed too ({e})"
        ),
        None => "no frames were captured: the browser sent no screencast frames".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_sized_like_the_screencast_scales_them() {
        assert_eq!(fitted((1280, 800), (800, 600)), (800, 500));
        assert_eq!(fitted((800, 600), (800, 600)), (800, 600));
        // Never scaled up.
        assert_eq!(fitted((400, 300), (800, 600)), (400, 300));
        assert_eq!(fitted((1000, 1000), (800, 600)), (600, 600));
        assert_eq!(fitted((2560, 1600), (800, 600)), (800, 500));
    }

    #[test]
    fn shrinking_averages_the_pixels_each_one_covers() {
        // 4x2 RGBA: a black and a white column pair per half, shrunk to 2x1.
        let px = |v: u8| [v, v, v, 255];
        let rgba: Vec<u8> = [px(0), px(0), px(255), px(255), px(0), px(0), px(255), px(255)].concat();
        assert_eq!(shrink(&rgba, (4, 2), (2, 1)), [0, 0, 0, 255, 255, 255]);
        // An odd ratio still covers every source pixel and stays in bounds.
        let wide: Vec<u8> = (0..5 * 3).flat_map(|i| px((i * 17) as u8)).collect();
        assert_eq!(shrink(&wide, (5, 3), (3, 2)).len(), 3 * 2 * 3);
    }

    #[test]
    fn the_empty_reason_says_only_what_is_known() {
        let plain = why_empty(None);
        assert!(plain.contains("sent no screencast frames") && !plain.contains("did not change"), "{plain}");
        let failed = why_empty(Some("Target closed"));
        assert!(failed.contains("screenshots taken in their place failed too (Target closed)"), "{failed}");
    }
}
