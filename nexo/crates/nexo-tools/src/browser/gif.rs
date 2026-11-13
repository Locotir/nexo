//! The recording actions of `browser` (`start_recording`, `stop_recording`, `export_gif`, `clear_recording`) and the
//! action log that feeds the exported GIF: every action the tool runs while a recording is going is noted once, at
//! the dispatch, so the GIF can draw click circles, drag arrows and action labels over the captured frames.

use std::path::{Path, PathBuf};

use nexo_browser::{ActionEvent, AnyBrowser as Browser, ExportOptions, RecordInfo, RecordOptions, RecordSummary};
use serde_json::Value;

use super::{read, Action, Step};
use crate::util::{one_line, resolve, str_arg};

/// Names of the actions of this module, as the model writes them.
pub(super) const NAMES: [&str; 4] = ["start_recording", "stop_recording", "export_gif", "clear_recording"];

/// Characters of an action label.
const MAX_LABEL: usize = 60;

/// One recording call, read from the arguments.
#[derive(Debug, PartialEq)]
pub(super) enum GifCall<'a> {
    Start,
    Stop,
    Export { path: Option<&'a str>, opts: ExportOptions },
    Clear,
}

/// The call for a recording action, or `None` when `action` is not one of them.
pub(super) fn parse<'a>(action: &str, args: &'a Value) -> Option<Result<GifCall<'a>, String>> {
    Some(match action {
        "start_recording" => Ok(GifCall::Start),
        "stop_recording" => Ok(GifCall::Stop),
        "clear_recording" => Ok(GifCall::Clear),
        "export_gif" => export_args(args).map(|(path, opts)| GifCall::Export { path, opts }),
        _ => return None,
    })
}

/// `value` (or `gif.path`) is the file; `gif` holds the options.
fn export_args(args: &Value) -> Result<(Option<&str>, ExportOptions), String> {
    let mut opts = ExportOptions::default();
    let gif = args.get("gif").filter(|g| !g.is_null());
    if let Some(g) = gif {
        let g = g
            .as_object()
            .ok_or("browser export_gif: `gif` must be an object like {\"clicks\": false, \"quality\": 5}")?;
        for (key, slot) in [
            ("clicks", &mut opts.clicks),
            ("drags", &mut opts.drags),
            ("labels", &mut opts.labels),
            ("progress", &mut opts.progress),
            ("watermark", &mut opts.watermark),
        ] {
            match g.get(key) {
                None | Some(Value::Null) => {}
                Some(Value::Bool(b)) => *slot = *b,
                Some(_) => return Err(format!("browser export_gif: `gif.{key}` must be true or false")),
            }
        }
        match g.get("quality") {
            None | Some(Value::Null) => {}
            Some(q) => match q.as_u64().or_else(|| q.as_str().and_then(|s| s.trim().parse().ok())) {
                Some(n @ 1..=30) => opts.quality = n as u8,
                _ => {
                    return Err(
                        "browser export_gif: `gif.quality` must be a number from 1 (best) to 30 (smallest)".into()
                    )
                }
            },
        }
    }
    let path = str_arg(args, "value").or_else(|| gif.and_then(|g| str_arg(g, "path")));
    Ok((path, opts))
}

/// Logs an action of the tool in the recording, if one is running. The one hook at the dispatch of an action.
pub(super) async fn note(b: &Browser, action: &Action<'_>) {
    if !b.recording() {
        return;
    }
    match action {
        Action::Click(target) => b.record_note_click("click", target).await,
        Action::ClickXy(x, y) => b.record_note(ActionEvent::click(format!("click ({x:.0}, {y:.0})"), *x, *y)),
        other => {
            if let Some(label) = label_of(other) {
                b.record_note(ActionEvent::other(one_line(&label, MAX_LABEL)));
            }
        }
    }
}

/// What to say on the GIF about an action that has no mark of its own. What was typed is never shown: it may be a
/// password. Reads (`snapshot`, `eval`, ...) change nothing on screen and have no label.
fn label_of(action: &Action<'_>) -> Option<String> {
    Some(match action {
        Action::Navigate(url) => format!("open {url}"),
        Action::Fill(target, _) => format!("fill {target}"),
        Action::FillBatch(pairs) => format!("fill {} fields", pairs.len()),
        Action::Select(_, option) => format!("select \"{option}\""),
        Action::SetCheckbox(target, on) => format!("{} {target}", if *on { "check" } else { "uncheck" }),
        Action::Press(keys) => format!("press {keys}"),
        Action::Hover(target) => format!("hover {target}"),
        Action::Scroll(to) => format!("scroll {to}"),
        Action::Back => "back".to_string(),
        Action::Forward => "forward".to_string(),
        Action::Reload => "reload".to_string(),
        Action::TabNew(url) => format!("new tab {url}"),
        Action::TabSwitch(i) => format!("switch to tab {i}"),
        Action::TabClose(i) => format!("close tab {i}"),
        Action::Upload(target, _) => format!("upload to {target}"),
        Action::Resize(w, h) => format!("resize {w}x{h}"),
        Action::TypeText(t) => format!("type {} characters", t.chars().count()),
        _ => return None,
    })
}

/// Runs a recording action.
pub(super) async fn run(b: &Browser, call: GifCall<'_>, cwd: &Path) -> Result<Step, Step> {
    match call {
        GifCall::Start => read(
            "start_recording",
            b.record_start(RecordOptions::default()).await.map(|_| {
                "recording the active tab: the actions you take now are logged for the GIF. Next: stop_recording, then export_gif."
                    .to_string()
            }),
        ),
        GifCall::Stop => read("stop_recording", b.record_stop().await.map(|i| format!("recording stopped. {} Next: export_gif or clear_recording.", held(&i)))),
        GifCall::Clear => read("clear_recording", b.record_clear().await.map(|i| format!("discarded the recording ({}).", held(&i)))),
        GifCall::Export { path, opts } => {
            let info = b.record_info();
            let file = out_path(cwd, path);
            let name = file.display().to_string();
            read(
                "export_gif",
                b.record_export(&file, &opts).await.map(|s| {
                    let truncated = info.is_some_and(|i| i.truncated);
                    exported(&name, &s, info.map_or(0, |i| i.events), truncated)
                }),
            )
        }
    }
}

/// `12 frames, 3.4 s, 5 actions logged` (plus a note when a cap cut the recording short).
fn held(i: &RecordInfo) -> String {
    let cut = if i.truncated { "; a cap was hit, later frames were dropped" } else { "" };
    format!("{} frames over {:.1} s, {} actions logged{cut}.", i.frames, i.millis as f64 / 1000.0, i.events)
}

fn exported(path: &str, s: &RecordSummary, actions: usize, truncated: bool) -> String {
    let cut = if truncated { " The recording hit a cap, so its end is missing." } else { "" };
    format!(
        "saved {path}: {} frames, {:.1} s, {} bytes, {actions} actions logged.{cut}",
        s.frames,
        s.millis as f64 / 1000.0,
        s.bytes
    )
}

/// Where the GIF goes: the given file (a `.gif` is added when it has no extension; a folder gets the default name
/// inside it), or `recording-<n>.gif` in the session directory with the first `n` that is free.
fn out_path(cwd: &Path, given: Option<&str>) -> PathBuf {
    let free = |dir: &Path| {
        (1..)
            .map(|n| dir.join(format!("recording-{n}.gif")))
            .find(|p| !p.exists())
            .unwrap_or_else(|| dir.join("recording.gif"))
    };
    let Some(given) = given else { return free(cwd) };
    let path = resolve(cwd, given);
    if path.is_dir() || given.ends_with('/') || given.ends_with('\\') {
        return free(&path);
    }
    if path.extension().is_none() {
        return path.with_extension("gif");
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(args: Value) -> Result<GifCall<'static>, String> {
        let args: &'static Value = Box::leak(Box::new(args));
        parse(args["action"].as_str().unwrap(), args).unwrap()
    }

    #[test]
    fn the_four_actions_parse_and_others_are_not_ours() {
        assert_eq!(call(json!({"action": "start_recording"})), Ok(GifCall::Start));
        assert_eq!(call(json!({"action": "stop_recording"})), Ok(GifCall::Stop));
        assert_eq!(call(json!({"action": "clear_recording"})), Ok(GifCall::Clear));
        assert_eq!(
            call(json!({"action": "export_gif"})),
            Ok(GifCall::Export { path: None, opts: ExportOptions::default() })
        );
        assert!(parse("click", &json!({})).is_none());
        assert_eq!(NAMES.len(), 4);
    }

    #[test]
    fn export_takes_a_path_and_options_like_claudes_gif_creator() {
        let opts =
            ExportOptions { clicks: false, drags: true, labels: false, progress: false, watermark: false, quality: 5 };
        let got = call(json!({"action": "export_gif", "value": "demo.gif",
            "gif": {"clicks": false, "labels": false, "progress": false, "watermark": false, "quality": "5"}}));
        assert_eq!(got, Ok(GifCall::Export { path: Some("demo.gif"), opts }));
        // gif.path works too, and `value` wins when both are given
        let got = call(json!({"action": "export_gif", "gif": {"path": "a.gif", "quality": 30}}));
        assert_eq!(
            got,
            Ok(GifCall::Export {
                path: Some("a.gif"),
                opts: ExportOptions { quality: 30, ..ExportOptions::default() }
            })
        );
        let got = call(json!({"action": "export_gif", "value": "v.gif", "gif": {"path": "a.gif"}}));
        assert!(matches!(got, Ok(GifCall::Export { path: Some("v.gif"), .. })));
    }

    #[test]
    fn bad_export_options_say_what_is_wrong() {
        let err = |v: Value| call(v).unwrap_err();
        assert!(err(json!({"action": "export_gif", "gif": {"quality": 0}})).contains("1 (best) to 30"));
        assert!(err(json!({"action": "export_gif", "gif": {"quality": 31}})).contains("1 (best) to 30"));
        assert!(err(json!({"action": "export_gif", "gif": {"quality": "high"}})).contains("1 (best) to 30"));
        assert!(err(json!({"action": "export_gif", "gif": {"clicks": "yes"}})).contains("`gif.clicks`"));
        assert!(err(json!({"action": "export_gif", "gif": 5})).contains("must be an object"));
    }

    #[test]
    fn labels_never_show_what_was_typed() {
        assert_eq!(label_of(&Action::Fill("Password", "hunter2")).as_deref(), Some("fill Password"));
        assert_eq!(label_of(&Action::TypeText("hunter2")).as_deref(), Some("type 7 characters"));
        assert_eq!(label_of(&Action::FillBatch(vec![("a", "1"), ("b", "2")])).as_deref(), Some("fill 2 fields"));
        assert_eq!(label_of(&Action::Press("Enter")).as_deref(), Some("press Enter"));
        assert_eq!(label_of(&Action::Navigate("https://a.b")).as_deref(), Some("open https://a.b"));
        assert_eq!(label_of(&Action::SetCheckbox("e2", false)).as_deref(), Some("uncheck e2"));
        // reads and recording calls have none
        assert_eq!(label_of(&Action::Snapshot), None);
        assert_eq!(label_of(&Action::Eval("1")), None);
        assert_eq!(label_of(&Action::Gif(GifCall::Stop)), None);
    }

    #[test]
    fn the_default_file_is_the_first_free_recording_n_in_the_session_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(out_path(dir.path(), None), dir.path().join("recording-1.gif"));
        std::fs::write(dir.path().join("recording-1.gif"), b"x").unwrap();
        std::fs::write(dir.path().join("recording-2.gif"), b"x").unwrap();
        assert_eq!(out_path(dir.path(), None), dir.path().join("recording-3.gif"));
        assert_eq!(out_path(dir.path(), Some("demo.gif")), dir.path().join("demo.gif"));
        assert_eq!(out_path(dir.path(), Some("demo")), dir.path().join("demo.gif"));
        assert_eq!(out_path(dir.path(), Some("out/x.gif")), dir.path().join("out/x.gif"));
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        assert_eq!(out_path(dir.path(), Some("sub")), dir.path().join("sub/recording-1.gif"));
        assert_eq!(out_path(dir.path(), Some("new/")), dir.path().join("new/recording-1.gif"));
    }

    #[test]
    fn the_answers_carry_path_frames_time_and_bytes() {
        let s = RecordSummary { frames: 12, millis: 3400, bytes: 51_200 };
        let text = exported("/tmp/a.gif", &s, 4, false);
        assert_eq!(text, "saved /tmp/a.gif: 12 frames, 3.4 s, 51200 bytes, 4 actions logged.");
        assert!(exported("/tmp/a.gif", &s, 4, true).contains("hit a cap"));
        let i = RecordInfo { running: false, frames: 7, events: 2, millis: 1500, truncated: false };
        assert_eq!(held(&i), "7 frames over 1.5 s, 2 actions logged.");
    }
}
