//! `browser`: drives a real Chrome, Chromium or Edge. One tool with an `action` parameter; the browser is
//! launched headless with a throwaway profile on the first action that needs a page, stays up for the
//! session and is closed by the `close` action (or killed when the session ends).
//!
//! Every action that changes the page answers with a fresh snapshot of it (one line per actionable element,
//! `e3 button "Submit"`), so the model sees the new state in the same round-trip and never has to ask for it.

use async_trait::async_trait;
use base64::Engine;
use nexo_browser::{Browser, FillReport, LaunchOptions, Snapshot, Wait, WAIT_CAP_MS};
use nexo_core::message::ImageSource;
use nexo_core::{Compact, Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::util::{bool_arg, cap_text, int_arg, one_line, str_arg};

/// Elements of a snapshot shown at most.
const MAX_ELEMENTS: usize = 400;
/// Characters of `read_text` shown at most.
const MAX_TEXT: usize = 30_000;
/// Bytes of a screenshot shown at most.
const MAX_SHOT: usize = 5_000_000;

/// The `browser` tool. The browser is started on first use and kept in the tool, like `todo` keeps its list.
#[derive(Default)]
pub struct BrowserTool {
    browser: Mutex<Option<Browser>>,
}

impl BrowserTool {
    pub fn new() -> BrowserTool {
        BrowserTool::default()
    }
}

/// One call, read from the arguments. Borrows from them.
#[derive(Debug, PartialEq)]
enum Action<'a> {
    Navigate(&'a str),
    Snapshot,
    ReadText(Option<&'a str>),
    Click(&'a str),
    Fill(&'a str, &'a str),
    FillBatch(Vec<(&'a str, &'a str)>),
    Select(&'a str, &'a str),
    SetCheckbox(&'a str, bool),
    Press(&'a str),
    Hover(&'a str),
    Scroll(&'a str),
    Wait(Option<&'a str>, Option<u64>),
    Screenshot(bool),
    Close,
}

const ACTIONS: &str =
    "navigate, snapshot, read_text, click, fill, fill_batch, select, set_checkbox, press, hover, scroll, wait, screenshot, close";

/// A required text argument; the error says which action needs it.
fn need<'a>(args: &'a Value, key: &str, action: &str) -> Result<&'a str, String> {
    str_arg(args, key).ok_or_else(|| format!("browser {action}: `{key}` is required"))
}

/// A `value` may be empty (clearing a field), so it is not trimmed or refused when empty.
fn value_arg<'a>(v: &'a Value, action: &str) -> Result<&'a str, String> {
    match v.get("value") {
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(Value::Number(n)) => Err(format!("browser {action}: `value` must be a string, not {n}")),
        _ => Err(format!("browser {action}: `value` is required (use \"\" to clear a field)")),
    }
}

fn parse(args: &Value) -> Result<Action<'_>, String> {
    let action = str_arg(args, "action").ok_or_else(|| format!("browser: `action` is required (one of: {ACTIONS})"))?;
    Ok(match action {
        "navigate" => Action::Navigate(need(args, "url", action)?),
        "snapshot" => Action::Snapshot,
        "read_text" => Action::ReadText(str_arg(args, "selector")),
        "click" => Action::Click(need(args, "target", action)?),
        "fill" => Action::Fill(need(args, "target", action)?, value_arg(args, action)?),
        "fill_batch" => {
            let list = args
                .get("fields")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
                .ok_or("browser fill_batch: `fields` must be a non-empty list of {target, value}")?;
            let mut pairs = Vec::with_capacity(list.len());
            for (i, f) in list.iter().enumerate() {
                let target = str_arg(f, "target")
                    .ok_or_else(|| format!("browser fill_batch: field {} has no `target`", i + 1))?;
                let value = value_arg(f, action).map_err(|e| format!("{e} (field {})", i + 1))?;
                pairs.push((target, value));
            }
            Action::FillBatch(pairs)
        }
        "select" => Action::Select(need(args, "target", action)?, need(args, "option", action)?),
        "set_checkbox" => match args.get("checked") {
            Some(Value::Bool(b)) => Action::SetCheckbox(need(args, "target", action)?, *b),
            _ => return Err("browser set_checkbox: `checked` (true or false) is required".into()),
        },
        "press" => Action::Press(need(args, "keys", action)?),
        "hover" => Action::Hover(need(args, "target", action)?),
        "scroll" => Action::Scroll(need(args, "to", action)?),
        "wait" => {
            let (selector, ms) = (str_arg(args, "selector"), int_arg(args, "ms"));
            if selector.is_none() && ms.is_none() {
                return Err("browser wait: give `selector` (wait for it to appear) or `ms` (pause)".into());
            }
            Action::Wait(selector, ms)
        }
        "screenshot" => Action::Screenshot(bool_arg(args, "full")),
        "close" => Action::Close,
        other => return Err(format!("browser: unknown action `{other}` (one of: {ACTIONS})")),
    })
}

/// The snapshot as the model reads it, cut at [`MAX_ELEMENTS`] lines.
fn show_snapshot(snap: &Snapshot) -> String {
    let text = snap.render();
    if text.is_empty() {
        return "(no actionable elements on the page; try read_text, or wait and snapshot again)".into();
    }
    let total = text.lines().count();
    if total <= MAX_ELEMENTS {
        return text;
    }
    let head: Vec<&str> = text.lines().take(MAX_ELEMENTS).collect();
    format!(
        "{}\n… {} more elements; scroll and snapshot again, or target one by its text",
        head.join("\n"),
        total - MAX_ELEMENTS
    )
}

fn fill_report(r: &FillReport) -> String {
    let mut out = format!("filled {}: {}", r.filled.len(), r.filled.join(", "));
    if !r.missing.is_empty() {
        out.push_str(&format!("\nNOT FOUND ({}): {}", r.missing.len(), r.missing.join(", ")));
    }
    if !r.failed.is_empty() {
        let items: Vec<String> = r.failed.iter().map(|(t, why)| format!("{t} ({why})")).collect();
        out.push_str(&format!("\nFAILED ({}): {}", r.failed.len(), items.join("; ")));
    }
    out
}

impl BrowserTool {
    /// The current page, for the end of an answer: the snapshot, or why there is none.
    async fn page(&self, b: &Browser) -> String {
        match b.snapshot().await {
            Ok(s) => format!("Page:\n{}", show_snapshot(&s)),
            Err(e) => format!("Page: snapshot failed: {e}"),
        }
    }

    async fn act(&self, b: &Browser, action: Action<'_>, vision: bool) -> ToolOutput {
        // `done` is what happened; a state-changing action then appends the fresh page.
        let done: Result<String, String> = match action {
            Action::Navigate(url) => b.navigate(url).await.map(|_| format!("opened {url}")).map_err(|e| e.to_string()),
            Action::Snapshot => {
                return match b.snapshot().await {
                    Ok(s) => ToolOutput::ok(show_snapshot(&s)),
                    Err(e) => ToolOutput::error(format!("browser snapshot: {e}")),
                }
            }
            Action::ReadText(sel) => {
                return match b.read_text(sel).await {
                    Ok(t) if t.trim().is_empty() => ToolOutput::ok("(no text)"),
                    Ok(t) => ToolOutput::ok(cap_text(&t, MAX_TEXT)),
                    Err(e) => ToolOutput::error(format!("browser read_text: {e}")),
                }
            }
            Action::Screenshot(full) => return screenshot(b, full, vision).await,
            Action::Click(t) => b.click(t).await.map(|_| format!("clicked {t}")).map_err(|e| e.to_string()),
            Action::Fill(t, v) => b.fill(t, v).await.map(|_| format!("filled {t}")).map_err(|e| e.to_string()),
            Action::FillBatch(pairs) => {
                let report = match b.fill_batch(&pairs).await {
                    Ok(r) => r,
                    Err(e) => return ToolOutput::error(format!("browser fill_batch: {e}")),
                };
                let text = fill_report(&report);
                let out = format!("{text}\n\n{}", self.page(b).await);
                return if report.complete() { ToolOutput::ok(out) } else { ToolOutput::error(out) };
            }
            Action::Select(t, o) => {
                b.select(t, o).await.map(|_| format!("selected {o:?} in {t}")).map_err(|e| e.to_string())
            }
            Action::SetCheckbox(t, on) => b
                .set_checkbox(t, on)
                .await
                .map(|_| format!("{} {t}", if on { "checked" } else { "unchecked" }))
                .map_err(|e| e.to_string()),
            Action::Press(k) => b.press(k).await.map(|_| format!("pressed {k}")).map_err(|e| e.to_string()),
            Action::Hover(t) => b.hover(t).await.map(|_| format!("hovered {t}")).map_err(|e| e.to_string()),
            Action::Scroll(to) => b.scroll(to).await.map(|_| format!("scrolled {to}")).map_err(|e| e.to_string()),
            Action::Wait(sel, ms) => {
                let what = match sel {
                    Some(s) => Wait::Selector(s, ms.unwrap_or(WAIT_CAP_MS)),
                    None => Wait::Millis(ms.unwrap_or(0)),
                };
                b.wait(what).await.map(|_| "wait over".to_string()).map_err(|e| e.to_string())
            }
            Action::Close => unreachable!("close is handled before the browser is borrowed"),
        };
        match done {
            Ok(what) => ToolOutput::ok(format!("{what}\n\n{}", self.page(b).await)),
            // The page after a miss is what the model needs to pick a better target.
            Err(e) => ToolOutput::error(format!("browser: {e}\n\n{}", self.page(b).await)),
        }
    }
}

async fn screenshot(b: &Browser, full: bool, vision: bool) -> ToolOutput {
    if !vision {
        return ToolOutput::error("browser screenshot: this model cannot view images; use snapshot or read_text");
    }
    match b.screenshot(full).await {
        Ok(png) if png.len() > MAX_SHOT => ToolOutput::error(format!(
            "browser screenshot: the picture is {} bytes, too large to show; use full=false or read_text",
            png.len()
        )),
        Ok(png) => {
            let data = base64::engine::general_purpose::STANDARD.encode(&png);
            ToolOutput::ok(format!(
                "screenshot of the {} ({} bytes)",
                if full { "full page" } else { "viewport" },
                png.len()
            ))
            .with_images(vec![ImageSource::Base64 { media_type: "image/png".into(), data }])
        }
        Err(e) => ToolOutput::error(format!("browser screenshot: {e}")),
    }
}

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str {
        "browser"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser".into(),
            description: "Drive a real browser (Chrome, Chromium or Edge) for what `web_fetch` cannot do: JavaScript pages, \
                          logins, forms, buttons. One `action` per call; the browser starts on the first call and stays open until `close`.\n\
                          Every action that changes the page returns the new page as a snapshot: one line per element, \
                          `e3 button \"Submit\"`, with a stable ref (e1, e2, ...). Act on the refs of the latest snapshot. A `target` \
                          is a ref (best), a CSS selector or the visible text. Fill a whole form with ONE `fill_batch`, then `click` submit. \
                          `read_text` reads the page, `screenshot` shows it. Never use it to get past a CAPTCHA or bot check: stop and tell the person."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["navigate", "snapshot", "read_text", "click", "fill", "fill_batch",
                        "select", "set_checkbox", "press", "hover", "scroll", "wait", "screenshot", "close"],
                        "description": "What to do. Page-changing actions return the new snapshot."},
                    "url": {"type": "string", "description": "navigate: address to open."},
                    "target": {"type": "string", "description": "click, fill, select, set_checkbox, hover: ref (e3), CSS selector or visible text/label."},
                    "value": {"type": "string", "description": "fill: text to type (\"\" clears)."},
                    "fields": {"type": "array", "description": "fill_batch: every field of the form.",
                        "items": {"type": "object", "properties": {
                            "target": {"type": "string"},
                            "value": {"type": "string"}
                        }, "required": ["target", "value"]}},
                    "option": {"type": "string", "description": "select: option text or value."},
                    "checked": {"type": "boolean", "description": "set_checkbox: tick or untick."},
                    "keys": {"type": "string", "description": "press: \"Enter\", \"Tab Tab Enter\"."},
                    "to": {"type": "string", "description": "scroll: up, down, top, bottom or a target."},
                    "selector": {"type": "string", "description": "read_text: only this part (CSS). wait: what to wait for."},
                    "ms": {"type": "integer", "description": "wait: milliseconds, at most 5000."},
                    "full": {"type": "boolean", "description": "screenshot: whole page."}
                },
                "required": ["action"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: "Drive a real browser, one `action` per call; page changes return a snapshot of refs (`e3 button \"Submit\"`). \
                          A target is a ref, CSS selector or visible text. `fill_batch` fills a whole form.",
            params: &[
                ("action", "navigate{url} snapshot read_text{selector?} click{target} fill{target,value} fill_batch{fields} select{target,option} set_checkbox{target,checked} press{keys} hover{target} scroll{to} wait{selector?,ms?} screenshot{full?} close"),
                ("target", "ref (e3), CSS selector or text"),
                ("fields", "fill_batch: [{target, value}]"),
                ("url", "navigate"),
                ("keys", "e.g. Enter, Tab Tab Enter"),
                ("to", "up, down, top, bottom or target"),
            ],
        })
    }

    fn rare(&self) -> bool {
        true
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Network
    }

    fn summary(&self, args: &Value) -> String {
        let action = str_arg(args, "action").unwrap_or("?");
        let arg = ["url", "target", "keys", "to", "selector"].iter().find_map(|k| str_arg(args, k));
        match arg {
            Some(a) => format!("browser {action} {}", one_line(a, 80)),
            None => format!("browser {action}"),
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let action = match parse(&args) {
            Ok(a) => a,
            Err(e) => return ToolOutput::error(e),
        };
        let mut guard = self.browser.lock().await;
        if action == Action::Close {
            return match guard.take() {
                Some(b) => match b.close().await {
                    Ok(()) => ToolOutput::ok("browser closed"),
                    Err(e) => ToolOutput::ok(format!("browser closed (with a warning: {e})")),
                },
                None => ToolOutput::ok("no browser is open"),
            };
        }
        if guard.is_none() {
            match Browser::launch(LaunchOptions::default()).await {
                Ok(b) => *guard = Some(b),
                Err(e) => return ToolOutput::error(format!("browser: could not start: {e}")),
            }
        }
        match guard.as_ref() {
            Some(b) => self.act(b, action, ctx.vision).await,
            None => ToolOutput::error("browser: could not start"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext::new(std::env::temp_dir())
    }

    #[test]
    fn every_action_parses_from_its_arguments() {
        let cases: Vec<(Value, Action)> = vec![
            (json!({"action": "navigate", "url": "https://a.b"}), Action::Navigate("https://a.b")),
            (json!({"action": "snapshot"}), Action::Snapshot),
            (json!({"action": "read_text"}), Action::ReadText(None)),
            (json!({"action": "read_text", "selector": "main"}), Action::ReadText(Some("main"))),
            (json!({"action": "click", "target": "e3"}), Action::Click("e3")),
            (json!({"action": "fill", "target": "#q", "value": ""}), Action::Fill("#q", "")),
            (
                json!({"action": "fill_batch", "fields": [{"target": "e1", "value": "a"}, {"target": "Email", "value": "b"}]}),
                Action::FillBatch(vec![("e1", "a"), ("Email", "b")]),
            ),
            (json!({"action": "select", "target": "e4", "option": "Spain"}), Action::Select("e4", "Spain")),
            (json!({"action": "set_checkbox", "target": "e5", "checked": false}), Action::SetCheckbox("e5", false)),
            (json!({"action": "press", "keys": "Tab Tab Enter"}), Action::Press("Tab Tab Enter")),
            (json!({"action": "hover", "target": "Menu"}), Action::Hover("Menu")),
            (json!({"action": "scroll", "to": "bottom"}), Action::Scroll("bottom")),
            (json!({"action": "wait", "selector": "e9", "ms": 2000}), Action::Wait(Some("e9"), Some(2000))),
            (json!({"action": "wait", "ms": "300"}), Action::Wait(None, Some(300))),
            (json!({"action": "screenshot", "full": true}), Action::Screenshot(true)),
            (json!({"action": "screenshot"}), Action::Screenshot(false)),
            (json!({"action": "close"}), Action::Close),
        ];
        for (args, want) in cases {
            assert_eq!(parse(&args).unwrap(), want, "{args}");
        }
    }

    #[test]
    fn bad_arguments_name_what_is_missing() {
        let err = |v: Value| parse(&v).unwrap_err();
        assert!(err(json!({})).contains("`action` is required"));
        assert!(err(json!({"action": "dance"})).contains("unknown action `dance`"));
        assert!(err(json!({"action": "navigate"})).contains("`url` is required"));
        assert!(err(json!({"action": "click"})).contains("`target` is required"));
        assert!(err(json!({"action": "fill", "target": "e1"})).contains("`value` is required"));
        assert!(err(json!({"action": "fill_batch", "fields": []})).contains("non-empty list"));
        assert!(err(json!({"action": "fill_batch", "fields": [{"target": "e1"}]})).contains("(field 1)"));
        assert!(err(json!({"action": "set_checkbox", "target": "e1"})).contains("`checked`"));
        assert!(err(json!({"action": "wait"})).contains("`selector`"));
    }

    #[test]
    fn the_schema_lists_every_action_and_its_parameters() {
        let spec = BrowserTool::new().spec();
        assert_eq!(spec.name, "browser");
        assert_eq!(spec.parameters["type"], "object");
        assert_eq!(spec.parameters["required"], json!(["action"]));
        let enum_: Vec<&str> = spec.parameters["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(enum_.join(", "), ACTIONS);
        for p in ["url", "target", "value", "fields", "option", "checked", "keys", "to", "selector", "ms", "full"] {
            assert!(spec.parameters["properties"][p]["description"].is_string(), "{p} is described");
        }
        assert!(spec.description.contains("ref") && spec.description.contains("CSS selector"));
    }

    #[tokio::test]
    async fn bad_calls_and_close_never_start_a_browser() {
        let t = BrowserTool::new();
        let out = t.run(json!({"action": "click"}), &ctx()).await;
        assert!(out.is_error && out.content.contains("`target` is required"));
        let out = t.run(json!({"action": "close"}), &ctx()).await;
        assert!(!out.is_error && out.content.contains("no browser is open"));
        assert!(t.browser.lock().await.is_none());
    }

    #[test]
    fn the_summary_names_the_action_and_its_object() {
        let t = BrowserTool::new();
        assert_eq!(t.summary(&json!({"action": "navigate", "url": "https://a.b"})), "browser navigate https://a.b");
        assert_eq!(t.summary(&json!({"action": "snapshot"})), "browser snapshot");
    }

    #[test]
    fn a_fill_report_says_what_did_not_take() {
        let r = FillReport {
            filled: vec!["e1".into()],
            missing: vec!["Phone".into()],
            failed: vec![("e2".into(), "disabled".into())],
        };
        let text = fill_report(&r);
        assert!(
            text.contains("filled 1: e1")
                && text.contains("NOT FOUND (1): Phone")
                && text.contains("FAILED (1): e2 (disabled)")
        );
    }
}
