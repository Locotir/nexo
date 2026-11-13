//! The recording verbs of [`Browser`]: start, stop (the frames stay), export to GIF, clear, and the action log that
//! the exported GIF draws over the frames.

use crate::export::{self, ExportOptions};
use crate::record::{ActionEvent, RecordInfo, RecordOptions, RecordSummary, Recorder};
use crate::{lock, Browser, Error, Result, UNMARK_JS};
use std::path::Path;
use std::sync::Arc;

/// Reads the element marked by `act.js`: scrolls it into view as a click would, then answers the centre of the part
/// of it that is on screen, and a short name for it.
const MARKED_CENTER_JS: &str = r#"(() => {
  const el = document.querySelector('[data-nexo-click]');
  if (!el) return JSON.stringify(null);
  if (el.scrollIntoViewIfNeeded) el.scrollIntoViewIfNeeded(true); else el.scrollIntoView({ block: 'center' });
  const r = el.getBoundingClientRect();
  const l = Math.max(r.left, 0), rt = Math.min(r.right, window.innerWidth);
  const t = Math.max(r.top, 0), b = Math.min(r.bottom, window.innerHeight);
  const name = (el.getAttribute('aria-label') || el.innerText || el.value || el.title || el.getAttribute('alt') || el.placeholder || '')
    .replace(/\s+/g, ' ').trim().slice(0, 40);
  return JSON.stringify({ x: (l + rt) / 2, y: (t + b) / 2, name });
})()"#;

fn none() -> Error {
    Error::Unusable { target: "recording".to_string(), reason: "no recording in progress".to_string() }
}

impl Browser {
    /// Starts recording the active tab: frames are captured as the page changes, and actions noted with
    /// [`Browser::record_note`] are logged, until [`Browser::record_stop`]. Only one recording exists at a time; a
    /// stopped one that has not been exported or cleared is still held and must go first.
    pub async fn record_start(&self, opts: RecordOptions) -> Result<()> {
        if let Some(held) = lock(&self.recorder).as_ref() {
            let reason = if held.running() {
                "already recording".to_string()
            } else {
                format!(
                    "a stopped recording of {} frames is still held; export it or clear it first",
                    held.info().frames
                )
            };
            return Err(Error::Unusable { target: "recording".to_string(), reason });
        }
        let recorder = Recorder::start(self.page(), &opts).await?;
        *lock(&self.recorder) = Some(recorder);
        Ok(())
    }

    /// Stops capturing; the frames and the action log are kept for [`Browser::record_export`].
    pub async fn record_stop(&self) -> Result<RecordInfo> {
        let mut recorder = lock(&self.recorder).take().ok_or_else(none)?;
        let stopped = recorder.stop().await;
        let info = recorder.info();
        *lock(&self.recorder) = Some(recorder);
        // Frames already captured are worth keeping even if the page went away before the stop reached it.
        match stopped {
            Err(e) if info.frames == 0 => Err(e),
            _ => Ok(info),
        }
    }

    /// Writes the recording to `path` as a looping animated GIF, the actions drawn over it as `opts` say. A recording
    /// still running is stopped first. The recording is kept, so it can be exported again with other options.
    /// The encoding runs on a blocking thread.
    pub async fn record_export(&self, path: impl AsRef<Path>, opts: &ExportOptions) -> Result<RecordSummary> {
        if lock(&self.recorder).as_ref().is_some_and(Recorder::running) {
            self.record_stop().await?;
        }
        let (shared, stopped_at) = {
            let held = lock(&self.recorder);
            let r = held.as_ref().ok_or_else(none)?;
            (Arc::clone(&r.shared), r.stopped_at)
        };
        let (path, opts) = (path.as_ref().to_path_buf(), *opts);
        tokio::task::spawn_blocking(move || {
            let s = lock(&shared);
            export::render_gif(&s.frames, &s.events, stopped_at, &opts, &path)
        })
        .await
        .map_err(|e| Error::Encode(e.to_string()))?
    }

    /// Throws the recording away (stopping it first if it runs) and says what it held.
    pub async fn record_clear(&self) -> Result<RecordInfo> {
        let mut recorder = lock(&self.recorder).take().ok_or_else(none)?;
        let _ = recorder.stop().await;
        Ok(recorder.info())
    }

    /// Stops the recording and writes it to `path` as a plain looping GIF (nothing drawn on it), then forgets it.
    pub async fn record_stop_gif(&self, path: impl AsRef<Path>) -> Result<RecordSummary> {
        self.record_stop().await?;
        let summary = self.record_export(path, &ExportOptions::plain()).await;
        lock(&self.recorder).take();
        summary
    }

    /// What the recording holds, or `None` when there is none.
    pub fn record_info(&self) -> Option<RecordInfo> {
        lock(&self.recorder).as_ref().map(Recorder::info)
    }

    /// Whether frames are being captured right now.
    pub fn recording(&self) -> bool {
        lock(&self.recorder).as_ref().is_some_and(Recorder::running)
    }

    /// Logs an action at the current time. Does nothing unless a recording is running.
    pub fn record_note(&self, event: ActionEvent) {
        if let Some(r) = lock(&self.recorder).as_ref() {
            r.note(event);
        }
    }

    /// Logs a click on `target` (ref, CSS selector or visible text) as `verb "name"`, at the centre of the element
    /// as it is now. Call it just before clicking, while the element is where the click will land. Does nothing
    /// unless a recording is running; a target that cannot be found is logged as a label only.
    pub async fn record_note_click(&self, verb: &str, target: &str) {
        if !self.recording() {
            return;
        }
        let marked =
            matches!(self.act("mark", &[(target, "")]).await.map(|o| o.first().is_some_and(|o| o.ok)), Ok(true));
        let found = if marked {
            self.eval_as::<String>(MARKED_CENTER_JS)
                .await
                .ok()
                .and_then(|j| serde_json::from_str::<Option<Located>>(&j).ok().flatten())
        } else {
            None
        };
        if marked {
            // The mark is only a handle for this look.
            let _ = self.eval_as::<bool>(UNMARK_JS).await;
        }
        let event = match found {
            Some(Located { x, y, name }) => {
                let shown = if name.is_empty() { target } else { name.as_str() };
                ActionEvent::click(format!("{verb} \"{shown}\""), x, y)
            }
            None => ActionEvent::other(format!("{verb} {target}")),
        };
        self.record_note(event);
    }
}

#[derive(serde::Deserialize)]
struct Located {
    x: f64,
    y: f64,
    name: String,
}
