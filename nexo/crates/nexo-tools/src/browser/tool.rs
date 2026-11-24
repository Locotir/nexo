//! The `Tool` impl: name, schema, permission and the entry point of a call.

use super::args::*;
use super::*;

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str {
        "browser"
    }

    /// The person's own browser, when that is the one this session uses (a call that is running holds the lock:
    /// nothing is said then).
    fn in_use(&self) -> Option<String> {
        let guard = self.browser.try_lock().ok()?;
        guard.as_ref().filter(|b| b.is_real()).map(|b| b.describe())
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser".into(),
            description: "Drive a browser (Chrome, Chromium or Edge) for what `web_fetch` cannot do: JavaScript pages, \
                          logins, forms. One `action` per call; the first page action starts a clean headless browser, which stays open until `close`. \
                          `open{profile=persistent}` keeps logins (window shown to sign in). Page-changing actions return a snapshot, \
                          one line per element, `e3 button \"Submit\"`; act on the refs of the latest one. A `target` is a ref (best), \
                          CSS selector or visible text. Fill a whole form with ONE `fill_batch`. `read_page` reads the page as a tree, \
                          `read_text` its text, `screenshot` shows it, `batch` runs known steps in one call. \
                          Never use it to get past a CAPTCHA or bot check: stop and tell the person. For tasks needing the person's logged-in \
                          pages, first use `browsers` to discover available browsers without granting access; if listed, no /chrome setup is needed. \
                          `select_browser` uses one through the extension's access checks; the result says the rules. \
                          Do not connect just because a browser is installed or listed."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ACTIONS.split(", ").collect::<Vec<_>>()},
                    "url": {"type": "string", "description": "navigate, tab_new, open: URL, host or local file path."},
                    "profile": {"type": "string", "description": "open: persistent keeps logins (default ephemeral)."},
                    "visible": {"type": "boolean", "description": "open: show the window."},
                    "target": {"type": "string", "description": "click, fill, select, set_checkbox, hover, upload: ref (e3), CSS selector or visible text/label. find: words, e.g. \"search box\"."},
                    "value": {"type": "string", "description": "fill: text (\"\" clears). eval: JavaScript. type_text: text. upload: file path. export_gif: .gif path."},
                    "gif": {"type": "object", "description": "export_gif: clicks drags labels progress watermark (bool, default on), quality 1-30."},
                    "fields": {"type": "array", "description": "fill_batch: the fields.",
                        "items": {"type": "object", "properties": {
                            "target": {"type": "string"},
                            "value": {"type": "string"}
                        }, "required": ["target", "value"]}},
                    "option": {"type": "string", "description": "select: option text or value."},
                    "checked": {"type": "boolean", "description": "set_checkbox: tick or untick."},
                    "keys": {"type": "string", "description": "press: \"Enter\", \"Ctrl+A\", \"Tab Tab Enter\"."},
                    "to": {"type": "string", "description": "scroll: up, down, top, bottom or a target. resize: 1280x720. click_xy, double_click, right_click, hover_xy: 120,300. drag: X1,Y1,X2,Y2. scroll_xy: X,Y,DX,DY. tab_switch, tab_close: tab number. network: request number for details."},
                    "modifiers": {"type": "string", "description": "click_xy, double_click, right_click, drag: ctrl, shift, alt, meta joined by +."},
                    "selector": {"type": "string", "description": "read_text: only this part (CSS). wait: what to wait for. console, network: regex filter."},
                    "filter": {"type": "string", "enum": ["all", "interactive"], "description": "read_page: default all."},
                    "depth": {"type": "integer", "description": "read_page: levels (default 15)."},
                    "ref_id": {"type": "string", "description": "read_page: only this ref's subtree."},
                    "max_chars": {"type": "integer", "description": "read_page: default 50000."},
                    "steps": {"type": "array", "description": "batch: [{action, ...fields}] run in order, stops at the first error.",
                        "items": {"type": "object"}},
                    "ms": {"type": "integer", "description": "wait: milliseconds, at most 5000."},
                    "full": {"type": "boolean", "description": "screenshot: whole page."},
                    "path": {"type": "string", "description": "capture_save: .har file to write."},
                    "id": {"type": "string", "description": "select_browser: from `browsers`."}
                },
                "required": ["action"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: "Browser: one action/call; refs (`e3 button`). Headless by default. For the person's logged-in pages, \
                          first `browsers`, then `select_browser`; no /chrome if listed. Discovery grants no access. \
                          Never bypass CAPTCHA/bot checks: stop and tell the person.",
            params: &[
                ("action", "open{url?,profile?,visible?} navigate{url} snapshot read_page{filter?,depth?,ref_id?,max_chars?} read_text{selector?} click{target} fill{target,value} fill_batch{fields} select{target,option} set_checkbox{target,checked} press{keys} hover{target} scroll{to} wait{selector?,ms?} screenshot{full?} back forward reload eval{value} console{selector?} network{selector?,to?} tabs tab_new{url} tab_switch{to} tab_close{to} upload{target,value} resize{to} find{target} click_xy|double_click|right_click|drag|hover_xy|scroll_xy{to,modifiers?} type_text{value} start_recording stop_recording export_gif{value?,gif?} clear_recording capture_start capture_stop capture_save{path} batch{steps} close browsers select_browser{id} switch_browser"),
                ("target", "ref (e3), CSS selector or text"),
                ("profile", "persistent keeps logins"),
                ("keys", "e.g. Enter, Ctrl+A, Tab Tab"),
                ("to", "up, down, top, bottom or target"),
                ("gif", "clicks drags labels progress watermark: bool; quality 1-30"),
                ("steps", "[{action, ...that action's fields}]"),
            ],
        })
    }

    fn rare(&self) -> bool {
        true
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Network
    }

    /// Saving a capture writes a file, so it is asked about and ordered as a write; every other action is as before.
    fn call_kind(&self, args: &Value) -> ToolKind {
        let saves = |a: &Value| str_arg(a, "action") == Some("capture_save");
        let in_batch = args.get("steps").and_then(Value::as_array).is_some_and(|s| s.iter().any(saves));
        if saves(args) || in_batch {
            ToolKind::Write
        } else {
            self.kind()
        }
    }

    fn summary(&self, args: &Value) -> String {
        let action = str_arg(args, "action").unwrap_or("?");
        let arg =
            ["url", "target", "keys", "to", "selector", "ref_id", "path", "id"].iter().find_map(|k| str_arg(args, k));
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
        match action {
            Action::Close => return self.close(&mut guard).await,
            Action::Open(url, want) => return self.open(&mut guard, want, url).await,
            _ => {}
        }
        if let Some(out) = real::run(&action, &mut guard, &ctx.cwd).await {
            return out;
        }
        if let Some(out) = real::lost(&mut guard).await {
            return out;
        }
        let note = match self.ensure(&mut guard, Want::default()).await {
            Ok(note) => note,
            Err(e) => return ToolOutput::error(e),
        };
        let out = match guard.as_ref() {
            Some(b) => self.act(b, action, ctx.vision, &ctx.cwd).await,
            None => ToolOutput::error("browser: could not start"),
        };
        match note {
            Some(note) => ToolOutput { content: format!("{note}\n\n{}", out.content), ..out },
            None => out,
        }
    }
}
