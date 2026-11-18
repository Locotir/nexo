//! The capture actions of `browser` (`capture_start`, `capture_stop`, `capture_save`): keep what the page sends to its
//! API and write it as a HAR file that `nexo api analyze` reads. The browser turns every secret into a marker before
//! it keeps anything, so the file holds no live token. This module only words the answers and picks the file.

use std::path::{Path, PathBuf};

use nexo_browser::CaptureStats;

use crate::util::resolve;

/// What a capture from the person's own browser lacks, said every time it matters.
const NO_DETAIL: &str =
    "This is the person's own browser: its capture holds only URLs, methods, types and statuses, no \
                         headers or bodies, so `nexo api analyze` will see endpoints and statuses but no request or \
                         response shapes. `select_browser` with id `headless` and browsing again in nexo's own browser \
                         captures them.";

/// The answer of `capture_start`.
pub(super) fn started(real: bool) -> String {
    let mut text = "capturing: every request the browser makes from now on is kept (secrets are turned into markers). \
                    Browse the flows to map, then capture_stop, then capture_save with a `path`."
        .to_string();
    if real {
        text.push('\n');
        text.push_str(NO_DETAIL);
    }
    text
}

/// The answer of `capture_stop`: what was kept and what was left out, in plain words.
pub(super) fn stopped(s: &CaptureStats, real: bool) -> String {
    let kept = s.requests.saturating_sub(s.dropped_requests);
    let mut text = format!(
        "capture stopped: {kept} of {} requests kept, {} request bodies and {} response bodies ({} bytes).",
        s.requests, s.request_bodies, s.response_bodies, s.body_bytes
    );
    if s.ws_sockets > 0 || s.sse_events > 0 {
        text.push_str(&format!(
            " Live channels: {} WebSockets with {} frames, {} event-stream messages.",
            s.ws_sockets, s.ws_frames, s.sse_events
        ));
    }
    let left_out = [
        (s.dropped_requests, "requests dropped (the capture was full)"),
        (s.bodies_cut, "bodies cut at the size cap"),
        (s.skipped_total_cap, "bodies left out (total size cap)"),
        (s.skipped_binary, "responses left out (not JSON or text)"),
        (s.skipped_too_large, "bodies left out (too large)"),
        (s.fetch_failed, "bodies the browser could not give"),
        (s.ws_sockets_dropped, "WebSockets left out (too many)"),
        (s.ws_frames_dropped, "WebSocket frames left out (frame or size cap)"),
        (s.ws_frames_cut, "WebSocket frames cut at the size cap"),
        (s.ws_frames_binary, "binary or control frames kept without payload (length and opcode only)"),
        (s.sse_events_dropped, "event-stream messages left out"),
    ];
    let notes: Vec<String> = left_out.iter().filter(|(n, _)| *n > 0).map(|(n, what)| format!("{n} {what}")).collect();
    if !notes.is_empty() {
        text.push_str(&format!(" Left out: {}.", notes.join("; ")));
    }
    if s.requests == 0 {
        text.push_str(" Nothing was captured: browse after capture_start.");
    } else {
        text.push_str(" Next: capture_save with a `path`.");
    }
    if real {
        text.push('\n');
        text.push_str(NO_DETAIL);
    }
    text
}

/// Where the HAR goes: the path under the tool's folder (a `.har` is added when it has no extension; a folder gets
/// `capture.har` inside it).
pub(super) fn target(cwd: &Path, given: &str) -> PathBuf {
    let path = resolve(cwd, given);
    if path.is_dir() || given.ends_with('/') || given.ends_with('\\') {
        return path.join("capture.har");
    }
    if path.extension().is_none() {
        return path.with_extension("har");
    }
    path
}

/// Writes the HAR text to `file` (making its folder) and says what was saved and what to do with it.
pub(super) fn write(file: &Path, har: &str, s: &CaptureStats, real: bool) -> Result<String, String> {
    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    std::fs::write(file, har).map_err(|e| format!("could not write {}: {e}", file.display()))?;
    let name = file.display();
    let kept = s.requests.saturating_sub(s.dropped_requests);
    let mut text = format!(
        "saved {name}: HAR with {kept} requests, {} bytes; secrets are markers, no live token is in it. \
         Next: `nexo api analyze {name}`.",
        har.len()
    );
    if kept == 0 {
        text.push_str(" The capture is empty: capture_start, browse, capture_stop first.");
    }
    if real {
        text.push('\n');
        text.push_str(NO_DETAIL);
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats() -> CaptureStats {
        CaptureStats {
            requests: 12,
            dropped_requests: 2,
            request_bodies: 3,
            response_bodies: 5,
            body_bytes: 900,
            ..Default::default()
        }
    }

    #[test]
    fn stopping_says_what_was_kept_and_what_was_left_out() {
        let text = stopped(&CaptureStats { skipped_binary: 4, fetch_failed: 1, ..stats() }, false);
        assert!(text.contains("10 of 12 requests kept") && text.contains("5 response bodies"), "{text}");
        assert!(text.contains("2 requests dropped") && text.contains("4 responses left out"), "{text}");
        assert!(text.contains("1 bodies the browser could not give") && text.contains("capture_save"), "{text}");
        assert!(!text.contains("own browser"), "{text}");
        assert!(stopped(&CaptureStats::default(), false).contains("Nothing was captured"));
        let live = stopped(&CaptureStats { ws_sockets: 1, ws_frames: 9, ws_frames_dropped: 2, ..stats() }, false);
        assert!(live.contains("1 WebSockets with 9 frames") && live.contains("2 WebSocket frames left out"), "{live}");
    }

    #[test]
    fn the_persons_own_browser_is_told_it_has_no_headers_or_bodies() {
        for text in [started(true), stopped(&stats(), true)] {
            assert!(text.contains("no headers or bodies") && text.contains("headless"), "{text}");
        }
        assert!(!started(false).contains("no headers or bodies"));
    }

    #[test]
    fn the_file_is_under_the_folder_and_ends_in_har() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(target(dir.path(), "site.har"), dir.path().join("site.har"));
        assert_eq!(target(dir.path(), "site"), dir.path().join("site.har"));
        assert_eq!(target(dir.path(), "out/site.json"), dir.path().join("out/site.json"));
        assert_eq!(target(dir.path(), "new/"), dir.path().join("new/capture.har"));
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        assert_eq!(target(dir.path(), "sub"), dir.path().join("sub/capture.har"));
    }

    #[test]
    fn saving_writes_the_file_and_names_the_next_step() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("out/a.har");
        let text = write(&file, "{\"log\":{}}", &stats(), false).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{\"log\":{}}");
        assert!(text.contains("10 requests") && text.contains("nexo api analyze"), "{text}");
        assert!(!text.contains("no headers or bodies"));
        let text = write(&file, "{}", &CaptureStats::default(), true).unwrap();
        assert!(text.contains("is empty") && text.contains("no headers or bodies"), "{text}");
        // a folder where the file should be is an error, not a panic
        assert!(write(dir.path(), "{}", &stats(), false).is_err());
    }
}
