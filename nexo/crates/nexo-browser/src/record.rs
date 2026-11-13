//! Screen recording: the page is streamed over `Page.startScreencast` and kept in memory, together with a log of
//! the actions taken meanwhile, until it is exported as an animated GIF (see `export.rs`).

use crate::{lock, Error, Result};
use base64::Engine;
use chromiumoxide::cdp::browser_protocol::page::{
    EventScreencastFrame, ScreencastFrameAckParams, StartScreencastFormat, StartScreencastParams, StopScreencastParams,
};
use chromiumoxide::page::Page;
use futures::StreamExt;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::task::JoinHandle;

/// Actions kept in the log at most; later ones are dropped.
const MAX_EVENTS: usize = 500;

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

#[derive(Default)]
pub(crate) struct Shared {
    pub frames: Vec<Frame>,
    pub events: Vec<ActionEvent>,
    bytes: usize,
    pub truncated: bool,
}

impl Frame {
    /// A frame from its PNG bytes and capture time in seconds (the relay backend builds them from raw protocol events).
    pub(crate) fn new(png: Vec<u8>, at: f64) -> Frame {
        Frame { png, at, page_width: 0.0 }
    }
}

/// A recording on one page: capturing until [`Recorder::stop`], then holding its frames until the browser drops it.
pub(crate) struct Recorder {
    page: Page,
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
    pub(crate) async fn start(page: Page, opts: &RecordOptions) -> Result<Self> {
        let shared = Arc::new(Mutex::new(Shared::default()));
        // The listener goes up before the screencast starts so the first frame is not lost.
        let mut stream = page.event_listener::<EventScreencastFrame>().await?;
        let sink = Arc::clone(&shared);
        let acker = page.clone();
        let (cap, byte_cap, span) = (opts.max_frames.max(1), opts.max_bytes, opts.max_seconds.max(1) as f64);
        let task = tokio::spawn(async move {
            while let Some(event) = stream.next().await {
                // Chrome sends no further frame until this one is acknowledged.
                let _ = acker.execute(ScreencastFrameAckParams::new(event.session_id)).await;
                let Ok(png) = base64::engine::general_purpose::STANDARD.decode(AsRef::<str>::as_ref(&event.data))
                else {
                    continue;
                };
                let at = event.metadata.timestamp.as_ref().map_or_else(now, |t| *t.inner());
                let page_width = event.metadata.device_width;
                let mut s = lock(&sink);
                let first = s.frames.first().map_or(at, |f| f.at);
                if s.frames.len() >= cap || s.bytes + png.len() > byte_cap || at - first > span {
                    s.truncated = true;
                    continue;
                }
                s.bytes += png.len();
                s.frames.push(Frame { png, at, page_width });
            }
        });
        let recorder = Recorder { page, shared, task: Some(task), stopped_at: None };
        let params = StartScreencastParams {
            format: Some(StartScreencastFormat::Png),
            quality: None,
            max_width: Some(i64::from(opts.max_width.max(1))),
            max_height: Some(i64::from(opts.max_height.max(1))),
            every_nth_frame: Some(i64::from(opts.every_nth_frame.max(1))),
        };
        // On failure the recorder is dropped here, which stops the listener.
        recorder.page.execute(params).await?;
        Ok(recorder)
    }

    pub(crate) fn running(&self) -> bool {
        self.task.is_some()
    }

    /// Stops capturing; the frames and the log stay. Stopping a stopped recording does nothing.
    pub(crate) async fn stop(&mut self) -> Result<()> {
        let Some(task) = self.task.take() else { return Ok(()) };
        let stopped = self.page.execute(StopScreencastParams::default()).await;
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
