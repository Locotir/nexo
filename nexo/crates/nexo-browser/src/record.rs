//! Screen recording: the page is streamed over `Page.startScreencast` and encoded to an animated GIF.

use crate::{lock, Error, Result};
use base64::Engine;
use chromiumoxide::cdp::browser_protocol::page::{
    EventScreencastFrame, ScreencastFrameAckParams, StartScreencastFormat, StartScreencastParams, StopScreencastParams,
};
use chromiumoxide::page::Page;
use futures::StreamExt;
use serde::Serialize;
use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::task::JoinHandle;

/// Shortest frame delay browsers honour, in 1/100 s (anything lower is shown as 10).
const MIN_DELAY_CS: u64 = 2;
/// Delay of the last frame, which has no successor to measure against, in 1/100 s.
const LAST_DELAY_CS: u64 = 10;
/// Palette quantizer speed of the `gif` crate: 1 is best and slowest, 30 fastest.
const QUANT_SPEED: i32 = 10;

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
}

impl Default for RecordOptions {
    fn default() -> Self {
        Self { max_width: 800, max_height: 600, every_nth_frame: 1, max_frames: 300 }
    }
}

/// What a finished recording came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RecordSummary {
    pub frames: usize,
    /// Playing time of the GIF in milliseconds.
    pub millis: u64,
    /// Size of the written file.
    pub bytes: u64,
}

/// One captured frame: PNG bytes and the time the browser swapped it in, in seconds.
pub(crate) struct Frame {
    png: Vec<u8>,
    at: f64,
}

/// A screencast in progress on one page; frames collect until [`Recorder::stop`].
pub(crate) struct Recorder {
    page: Page,
    frames: Arc<Mutex<Vec<Frame>>>,
    task: JoinHandle<()>,
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Recorder {
    pub(crate) async fn start(page: Page, opts: &RecordOptions) -> Result<Self> {
        let frames = Arc::new(Mutex::new(Vec::new()));
        // The listener goes up before the screencast starts so the first frame is not lost.
        let mut stream = page.event_listener::<EventScreencastFrame>().await?;
        let sink = Arc::clone(&frames);
        let acker = page.clone();
        let cap = opts.max_frames.max(1);
        let began = Instant::now();
        let task = tokio::spawn(async move {
            while let Some(event) = stream.next().await {
                // Chrome sends no further frame until this one is acknowledged.
                let _ = acker.execute(ScreencastFrameAckParams::new(event.session_id)).await;
                let Ok(png) = base64::engine::general_purpose::STANDARD.decode(AsRef::<str>::as_ref(&event.data))
                else {
                    continue;
                };
                let at =
                    event.metadata.timestamp.as_ref().map_or_else(|| began.elapsed().as_secs_f64(), |t| *t.inner());
                let mut frames = lock(&sink);
                if frames.len() < cap {
                    frames.push(Frame { png, at });
                }
            }
        });
        let recorder = Recorder { page, frames, task };
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

    /// Stops the screencast and hands over the frames captured so far.
    pub(crate) async fn stop(self) -> Result<Vec<Frame>> {
        let stopped = self.page.execute(StopScreencastParams::default()).await;
        self.task.abort();
        stopped?;
        Ok(std::mem::take(&mut *lock(&self.frames)))
    }
}

/// Frame delays in 1/100 s from the gaps between capture times; the last frame gets a fixed one.
fn delays(times: &[f64]) -> Vec<u16> {
    let mut out: Vec<u16> = times
        .windows(2)
        .map(|w| {
            let cs = ((w[1] - w[0]).max(0.0) * 100.0).round() as u64;
            u16::try_from(cs.max(MIN_DELAY_CS)).unwrap_or(u16::MAX)
        })
        .collect();
    out.push(LAST_DELAY_CS as u16);
    out
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

/// Encodes frames to a looping GIF at `path`. The canvas is the size of the first frame; frames of another
/// size (the viewport was resized mid-recording) are skipped.
pub(crate) fn encode_gif(frames: &[Frame], path: &Path) -> Result<RecordSummary> {
    let first = frames.first().ok_or_else(|| Error::Unusable {
        target: "recording".to_string(),
        reason: "no frames were captured (the page did not change)".to_string(),
    })?;
    let (w, h, _) = decode_png(&first.png)?;
    let (gw, gh) = (u16::try_from(w).map_err(encode_err)?, u16::try_from(h).map_err(encode_err)?);
    let times: Vec<f64> = frames.iter().map(|f| f.at).collect();
    let delays = delays(&times);

    let mut out = Vec::new();
    let mut count = 0;
    let mut cs_total: u64 = 0;
    {
        let mut enc = gif::Encoder::new(&mut out, gw, gh, &[]).map_err(encode_err)?;
        enc.set_repeat(gif::Repeat::Infinite).map_err(encode_err)?;
        for (frame, delay) in frames.iter().zip(delays) {
            let (fw, fh, mut rgba) = decode_png(&frame.png)?;
            if (fw, fh) != (w, h) {
                continue;
            }
            let mut g = gif::Frame::from_rgba_speed(gw, gh, &mut rgba, QUANT_SPEED);
            g.delay = delay;
            enc.write_frame(&g).map_err(encode_err)?;
            count += 1;
            cs_total += u64::from(delay);
        }
    }
    std::fs::write(path, &out)?;
    Ok(RecordSummary { frames: count, millis: cs_total * 10, bytes: out.len() as u64 })
}
