//! `browser`: drives a real Chrome, Chromium or Edge. One tool with an `action` parameter; the browser is
//! launched on the first action that needs a page (clean and headless unless `[browser]` or an `open` call says
//! otherwise, see `launch`), stays up for the session and is closed by the `close` action (or killed when the
//! session ends).
//!
//! Every action that changes the page answers with a fresh snapshot of it (one line per actionable element,
//! `e3 button "Submit"`), so the model sees the new state in the same round-trip and never has to ask for it.

use std::path::Path;

use async_trait::async_trait;
use base64::Engine;
mod real;

use nexo_browser::{
    AnyBrowser as Browser, ConsoleEntry, FillReport, Filter, Modifiers, NetDetail, NetEntry, PageText, ReadPageOptions,
    Snapshot, TabInfo, Wait, WAIT_CAP_MS,
};
use nexo_core::config::{BrowserConfig, BrowserProfile};
use nexo_core::message::ImageSource;
use nexo_core::{Compact, Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::util::{bool_arg, cap_text, int_arg, one_line, resolve, str_arg};

mod gif;
mod launch;
use launch::Want;

/// Elements of a snapshot shown at most.
const MAX_ELEMENTS: usize = 400;
/// Characters of `read_text` shown at most.
const MAX_TEXT: usize = 30_000;
/// Most characters `read_page` may be asked for with `max_chars`.
const MAX_PAGE_CHARS: u64 = 200_000;
/// Bytes of a screenshot shown at most.
const MAX_SHOT: usize = 5_000_000;
/// Characters of an `eval` result shown at most.
const MAX_EVAL: usize = 10_000;
/// Console or network entries shown at most (the latest), and the characters of the list.
const MAX_ENTRIES: usize = 50;
const MAX_LIST: usize = 8_000;
/// Steps of one `batch`, and characters of the answer of each.
const MAX_STEPS: usize = 30;
const MAX_STEP_TEXT: usize = 1_500;
/// Matches of `find` shown at most.
const MAX_FOUND: usize = 20;
/// Characters of one header value, and of a whole request, in `network` detail.
const MAX_HEADER: usize = 300;
const MAX_DETAIL: usize = 12_000;

/// The `browser` tool. The browser is started on first use and kept in the tool, like `todo` keeps its list.
#[derive(Default)]
pub struct BrowserTool {
    browser: Mutex<Option<Browser>>,
    /// The `[browser]` settings: what a launch without an `open` call uses.
    config: BrowserConfig,
    /// How the running browser was started.
    mode: std::sync::Mutex<Option<launch::Mode>>,
    /// Where a kept profile lives (`<this>/browser/profile`); `None`: nexo's data folder.
    data_dir: Option<std::path::PathBuf>,
}

impl BrowserTool {
    pub fn new() -> BrowserTool {
        BrowserTool::default()
    }

    /// The tool with the person's `[browser]` settings.
    pub fn with_config(config: BrowserConfig) -> BrowserTool {
        BrowserTool { config, ..BrowserTool::default() }
    }

    /// nexo's data folder, or the one this tool was given.
    fn data_dir(&self) -> std::path::PathBuf {
        self.data_dir.clone().unwrap_or_else(nexo_core::paths::data_dir)
    }
}

/// One call, read from the arguments. Borrows from them.
#[derive(Debug, PartialEq)]
enum Action<'a> {
    /// Starts the browser (or restarts it) the way asked, then goes to the address.
    Open(Option<&'a str>, Want),
    Navigate(&'a str),
    Snapshot,
    ReadPage(ReadPageArgs<'a>),
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
    Back,
    Forward,
    Reload,
    Eval(&'a str),
    Console(Option<&'a str>),
    /// A regex filter, or the number of one request to show in full.
    Network(Option<&'a str>, Option<u64>),
    /// Elements by what a person would call them.
    Find(&'a str),
    Tabs,
    TabNew(&'a str),
    TabSwitch(usize),
    TabClose(usize),
    Upload(&'a str, &'a str),
    Resize(u32, u32),
    ClickXy(f64, f64, Modifiers),
    DoubleClick(f64, f64, Modifiers),
    RightClick(f64, f64, Modifiers),
    /// From x1,y1 to x2,y2.
    Drag([f64; 4], Modifiers),
    HoverXy(f64, f64),
    /// At x,y by dx,dy.
    ScrollXy([f64; 4]),
    TypeText(&'a str),
    /// start_recording, stop_recording, export_gif, clear_recording.
    Gif(gif::GifCall<'a>),
    /// Steps in order, each with the name of its action.
    Batch(Vec<(&'a str, Action<'a>)>),
    Close,
    /// The browsers there are to drive, and which is in use.
    Browsers,
    /// Use the browser with this id (the person's own, or `headless`).
    SelectBrowser(&'a str),
    /// Ask every running browser of the person's at once.
    SwitchBrowser,
}

/// The arguments of `read_page`; what they leave out the browser's defaults fill in.
#[derive(Debug, PartialEq, Default)]
struct ReadPageArgs<'a> {
    interactive: bool,
    depth: Option<usize>,
    ref_id: Option<&'a str>,
    max_chars: Option<usize>,
}

impl ReadPageArgs<'_> {
    fn options(&self) -> ReadPageOptions {
        let mut o = ReadPageOptions::default();
        if self.interactive {
            o.filter = Filter::Interactive;
        }
        o.depth = self.depth.unwrap_or(o.depth);
        o.max_chars = self.max_chars.unwrap_or(o.max_chars);
        o.ref_id = self.ref_id.map(str::to_string);
        o
    }
}

const ACTIONS: &str = "open, navigate, snapshot, read_page, read_text, click, fill, fill_batch, select, set_checkbox, press, hover, scroll, \
                       wait, screenshot, back, forward, reload, eval, console, network, tabs, tab_new, tab_switch, tab_close, \
                       upload, resize, find, click_xy, double_click, right_click, drag, hover_xy, scroll_xy, type_text, start_recording, stop_recording, export_gif, clear_recording, batch, close, browsers, select_browser, switch_browser";

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

/// Exactly `N` finite numbers in one text: `1280x720`, `120,300` or `10,20 30,40`.
fn numbers<const N: usize>(text: &str) -> Option<[f64; N]> {
    let parts: Vec<&str> =
        text.split(|c: char| c == ',' || c == 'x' || c == 'X' || c.is_whitespace()).filter(|p| !p.is_empty()).collect();
    if parts.len() != N {
        return None;
    }
    let mut out = [0.0; N];
    for (slot, part) in out.iter_mut().zip(parts) {
        *slot = part.parse().ok().filter(|n: &f64| n.is_finite())?;
    }
    Some(out)
}

/// Two numbers in one text: `1280x720` or `120,300`.
fn pair(text: &str) -> Option<(f64, f64)> {
    numbers::<2>(text).map(|[a, b]| (a, b))
}

/// The point (`to`) of a mouse action, and the modifier keys held (`modifiers`).
fn point_args(args: &Value, action: &str) -> Result<(f64, f64, Modifiers), String> {
    let (x, y) = pair(need(args, "to", action)?)
        .ok_or_else(|| format!("browser {action}: `to` must be X,Y in pixels, e.g. 120,300"))?;
    Ok((x, y, modifiers_arg(args, action)?))
}

fn modifiers_arg(args: &Value, action: &str) -> Result<Modifiers, String> {
    Modifiers::parse(str_arg(args, "modifiers").unwrap_or("")).map_err(|e| format!("browser {action}: {e}"))
}

/// What `open` asks for: `profile` (`persistent` or `ephemeral`) and `visible` (show the window).
fn open_want(args: &Value) -> Result<Want, String> {
    let profile = match str_arg(args, "profile") {
        None => None,
        Some(p) if p.eq_ignore_ascii_case("persistent") => Some(BrowserProfile::Persistent),
        Some(p) if p.eq_ignore_ascii_case("ephemeral") => Some(BrowserProfile::Ephemeral),
        Some(p) => return Err(format!("browser open: `profile` must be persistent or ephemeral, not {p:?}")),
    };
    let visible = match args.get("visible") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        // models send booleans as text now and then; `bool_arg` takes the same words
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => return Err("browser open: `visible` must be true or false".into()),
        },
        Some(_) => return Err("browser open: `visible` must be true or false".into()),
    };
    Ok(Want { profile, visible })
}

/// The tab number (in `to`), as listed by `tabs`.
fn index_arg(args: &Value, action: &str) -> Result<usize, String> {
    int_arg(args, "to")
        .map(|i| i as usize)
        .ok_or_else(|| format!("browser {action}: `to` (the tab number, from `tabs`) is required"))
}

/// A positive whole number that may be left out; present but unusable is an error that says what it must be.
fn count_arg(args: &Value, key: &str, action: &str, max: u64) -> Result<Option<usize>, String> {
    if args.get(key).is_none_or(Value::is_null) {
        return Ok(None);
    }
    match int_arg(args, key) {
        Some(n) if (1..=max).contains(&n) => Ok(Some(n as usize)),
        _ => Err(format!("browser {action}: `{key}` must be a whole number from 1 to {max}")),
    }
}

fn read_page_args<'a>(args: &'a Value, action: &str) -> Result<ReadPageArgs<'a>, String> {
    let interactive = match str_arg(args, "filter") {
        None | Some("all") => false,
        Some("interactive") => true,
        Some(other) => {
            return Err(format!("browser {action}: `filter` must be `all` or `interactive`, not `{other}`"));
        }
    };
    Ok(ReadPageArgs {
        interactive,
        depth: count_arg(args, "depth", action, 1_000)?,
        ref_id: str_arg(args, "ref_id").or_else(|| str_arg(args, "target")),
        max_chars: count_arg(args, "max_chars", action, MAX_PAGE_CHARS)?,
    })
}

fn parse(args: &Value) -> Result<Action<'_>, String> {
    let action = str_arg(args, "action").ok_or_else(|| format!("browser: `action` is required (one of: {ACTIONS})"))?;
    Ok(match action {
        "open" => Action::Open(str_arg(args, "url"), open_want(args)?),
        "navigate" => Action::Navigate(need(args, "url", action)?),
        "snapshot" => Action::Snapshot,
        "read_page" => Action::ReadPage(read_page_args(args, action)?),
        // `get_page_text` is the name in Claude's Chrome extension: accepted, not advertised (the schema is budgeted).
        "read_text" | "get_page_text" => Action::ReadText(str_arg(args, "selector")),
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
        "back" => Action::Back,
        "forward" => Action::Forward,
        "reload" => Action::Reload,
        "eval" => match value_arg(args, action)? {
            js if js.trim().is_empty() => return Err("browser eval: `value` (the JavaScript) is required".into()),
            js => Action::Eval(js),
        },
        "console" => Action::Console(str_arg(args, "selector")),
        "network" => Action::Network(str_arg(args, "selector"), int_arg(args, "to")),
        "find" => Action::Find(need(args, "target", action)?),
        "tabs" => Action::Tabs,
        "tab_new" => Action::TabNew(need(args, "url", action)?),
        "tab_switch" => Action::TabSwitch(index_arg(args, action)?),
        "tab_close" => Action::TabClose(index_arg(args, action)?),
        "upload" => Action::Upload(need(args, "target", action)?, need(args, "value", action)?),
        "resize" => match pair(need(args, "to", action)?) {
            Some((w, h)) if (1.0..=65535.0).contains(&w) && (1.0..=65535.0).contains(&h) => {
                Action::Resize(w as u32, h as u32)
            }
            _ => return Err("browser resize: `to` must be WIDTHxHEIGHT in pixels, e.g. 1280x720".into()),
        },
        "click_xy" => point_args(args, action).map(|(x, y, m)| Action::ClickXy(x, y, m))?,
        "double_click" => point_args(args, action).map(|(x, y, m)| Action::DoubleClick(x, y, m))?,
        "right_click" => point_args(args, action).map(|(x, y, m)| Action::RightClick(x, y, m))?,
        "hover_xy" => point_args(args, action).map(|(x, y, _)| Action::HoverXy(x, y))?,
        "drag" => match numbers::<4>(need(args, "to", action)?) {
            Some(p) => Action::Drag(p, modifiers_arg(args, action)?),
            None => return Err("browser drag: `to` must be X1,Y1,X2,Y2 (from, to), e.g. 100,200,300,200".into()),
        },
        "scroll_xy" => match numbers::<4>(need(args, "to", action)?) {
            Some(p) => Action::ScrollXy(p),
            None => {
                return Err("browser scroll_xy: `to` must be X,Y,DX,DY (where, and how far), e.g. 400,300,0,500".into())
            }
        },
        "type_text" => Action::TypeText(value_arg(args, action)?),
        name if gif::NAMES.contains(&name) => match gif::parse(name, args) {
            Some(call) => Action::Gif(call?),
            None => return Err(format!("browser: unknown action `{name}`")),
        },
        "batch" => {
            let list = args
                .get("steps")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
                .ok_or("browser batch: `steps` must be a non-empty list of {action, ...}")?;
            if list.len() > MAX_STEPS {
                return Err(format!("browser batch: at most {MAX_STEPS} steps per call"));
            }
            let mut steps = Vec::with_capacity(list.len());
            for (i, s) in list.iter().enumerate() {
                let name = str_arg(s, "action").unwrap_or("?");
                let step = parse(s).map_err(|e| format!("{e} (batch step {})", i + 1))?;
                if matches!(
                    step,
                    Action::Batch(_)
                        | Action::Close
                        | Action::Open(..)
                        | Action::Screenshot(_)
                        | Action::Browsers
                        | Action::SelectBrowser(_)
                        | Action::SwitchBrowser
                ) {
                    return Err(format!("browser batch: step {} `{name}` cannot be part of a batch", i + 1));
                }
                // A batch shows each step's answer in a few lines, which would cut a page read to pieces.
                if matches!(step, Action::ReadPage(_) | Action::ReadText(_)) {
                    return Err(format!(
                        "browser batch: step {} `{name}` cannot be part of a batch (its answer is too long to keep whole): call it on its own",
                        i + 1
                    ));
                }
                steps.push((name, step));
            }
            Action::Batch(steps)
        }
        "close" => Action::Close,
        "browsers" => Action::Browsers,
        "select_browser" => Action::SelectBrowser(need(args, "id", action)?),
        "switch_browser" => Action::SwitchBrowser,
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

/// The tree of `read_page`; an empty one says so, so the model does not take it for a failure.
fn show_tree(text: &str) -> String {
    if text.is_empty() {
        "(nothing on the page; it may still be loading: wait, then read_page again)".into()
    } else {
        text.to_string()
    }
}

/// The text of `read_text`; when the page's main part was picked, a line says so and how to get the rest.
fn show_text(t: &PageText) -> String {
    if t.text.trim().is_empty() {
        return "(no text)".into();
    }
    let text = cap_text(&t.text, MAX_TEXT);
    match t.source.as_str() {
        "selector" | "body" => text,
        main => format!("{text}\n\n[main content only ({main}); selector \"body\" reads the whole page]"),
    }
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

    async fn act(&self, b: &Browser, action: Action<'_>, vision: bool, cwd: &Path) -> ToolOutput {
        match action {
            Action::Screenshot(full) => screenshot(b, full, vision).await,
            Action::Batch(steps) => self.batch(b, steps, cwd).await,
            action => match self.step(b, action, cwd).await {
                Ok(s) if s.page => ToolOutput::ok(format!("{}\n\n{}", s.text, self.page(b).await)),
                Ok(s) => ToolOutput::ok(s.text),
                // The page after a miss is what the model needs to pick a better target.
                Err(s) if s.page => ToolOutput::error(format!("browser: {}\n\n{}", s.text, self.page(b).await)),
                Err(s) => ToolOutput::error(s.text),
            },
        }
    }

    /// The steps of a `batch` in order, stopping at the first one that fails; a short log, then the page.
    async fn batch(&self, b: &Browser, steps: Vec<(&str, Action<'_>)>, cwd: &Path) -> ToolOutput {
        let total = steps.len();
        let mut log = Vec::with_capacity(total + 1);
        for (i, (name, action)) in steps.into_iter().enumerate() {
            match self.step(b, action, cwd).await {
                Ok(s) => log.push(format!("{}. {name}: {}", i + 1, cap_text(&s.text, MAX_STEP_TEXT))),
                Err(s) => {
                    log.push(format!("{}. {name} FAILED: {}", i + 1, cap_text(&s.text, MAX_STEP_TEXT)));
                    log.push(format!("stopped at step {} of {total}; the rest did not run", i + 1));
                    return ToolOutput::error(format!("{}\n\n{}", log.join("\n"), self.page(b).await));
                }
            }
        }
        ToolOutput::ok(format!("{}\n\n{}", log.join("\n"), self.page(b).await))
    }

    /// One action. The text is what happened or what was read; `page` says whether the fresh page follows it.
    async fn step(&self, b: &Browser, action: Action<'_>, cwd: &Path) -> Result<Step, Step> {
        gif::note(b, &action).await;
        match action {
            Action::Gif(call) => gif::run(b, call, cwd).await,
            Action::Navigate(url) => changed(b.navigate(url).await.map(|_| format!("opened {url}"))),
            Action::Snapshot => read("snapshot", b.snapshot().await.map(|s| show_snapshot(&s))),
            Action::ReadPage(a) => read("read_page", b.read_page(&a.options()).await.map(|t| show_tree(&t.text))),
            Action::ReadText(sel) => read("read_text", b.page_text(sel).await.map(|t| show_text(&t))),
            Action::Click(t) => changed(b.click(t).await.map(|_| format!("clicked {t}"))),
            Action::Fill(t, v) => changed(b.fill(t, v).await.map(|_| format!("filled {t}"))),
            Action::FillBatch(pairs) => {
                let report =
                    b.fill_batch(&pairs).await.map_err(|e| Step { text: format!("fill_batch: {e}"), page: false })?;
                let step = Step { text: fill_report(&report), page: true };
                if report.complete() {
                    Ok(step)
                } else {
                    Err(step)
                }
            }
            Action::Select(t, o) => changed(b.select(t, o).await.map(|_| format!("selected {o:?} in {t}"))),
            Action::SetCheckbox(t, on) => changed(
                b.set_checkbox(t, on).await.map(|_| format!("{} {t}", if on { "checked" } else { "unchecked" })),
            ),
            Action::Press(k) => changed(b.press(k).await.map(|_| format!("pressed {k}"))),
            Action::Hover(t) => changed(b.hover(t).await.map(|_| format!("hovered {t}"))),
            Action::Scroll(to) => changed(b.scroll(to).await.map(|_| format!("scrolled {to}"))),
            Action::Wait(sel, ms) => {
                let what = match sel {
                    Some(s) => Wait::Selector(s, ms.unwrap_or(WAIT_CAP_MS)),
                    None => Wait::Millis(ms.unwrap_or(0)),
                };
                changed(b.wait(what).await.map(|_| "wait over".to_string()))
            }
            Action::Back => changed(b.back().await.map(|_| "went back".to_string())),
            Action::Forward => changed(b.forward().await.map(|_| "went forward".to_string())),
            Action::Reload => changed(b.reload().await.map(|_| "reloaded".to_string())),
            Action::Eval(js) => read("eval", b.eval(js).await.map(|r| cap_text(&r, MAX_EVAL))),
            Action::Console(filter) => read("console", b.console(filter).await.map(|l| show_console(&l))),
            Action::Network(_, Some(n)) => read("network", b.network_detail(n).await.map(|d| show_request(&d))),
            Action::Network(filter, None) => read("network", b.network(filter).await.map(|l| show_network(&l))),
            Action::Find(query) => {
                read("find", b.find(query, MAX_FOUND).await.map(|(hits, n)| show_found(query, &hits, n)))
            }
            Action::Tabs => read("tabs", b.tabs().await.map(|t| show_tabs(&t))),
            Action::TabNew(url) => {
                let index = b.tab_new(url).await.map_err(|e| Step { text: e.to_string(), page: true })?;
                let tabs = b.tabs().await.map(|t| show_tabs(&t)).unwrap_or_default();
                Ok(Step { text: format!("opened {url} in tab {index}\n{tabs}"), page: true })
            }
            Action::TabSwitch(i) => changed(b.tab_switch(i).await.map(|_| format!("switched to tab {i}"))),
            Action::TabClose(i) => {
                b.tab_close(i).await.map_err(|e| Step { text: e.to_string(), page: true })?;
                let tabs = b.tabs().await.map(|t| show_tabs(&t)).unwrap_or_default();
                Ok(Step { text: format!("closed tab {i}\n{tabs}"), page: true })
            }
            Action::Upload(t, path) => {
                changed(b.upload(t, resolve(cwd, path)).await.map(|_| format!("uploaded {path} to {t}")))
            }
            Action::Resize(w, h) => changed(b.resize(w, h).await.map(|_| format!("resized to {w}x{h}"))),
            Action::ClickXy(x, y, m) => {
                changed(b.click_xy(x, y, m).await.map(|_| format!("clicked at {x},{y}{}", held(m))))
            }
            Action::DoubleClick(x, y, m) => {
                changed(b.double_click(x, y, m).await.map(|_| format!("double-clicked at {x},{y}{}", held(m))))
            }
            Action::RightClick(x, y, m) => {
                changed(b.right_click(x, y, m).await.map(|_| format!("right-clicked at {x},{y}{}", held(m))))
            }
            Action::Drag([x1, y1, x2, y2], m) => changed(b.drag((x1, y1), (x2, y2), m).await.map(|dropped| {
                let how = if dropped { ", dropped as a drag and drop" } else { "" };
                format!("dragged from {x1},{y1} to {x2},{y2}{}{how}", held(m))
            })),
            Action::HoverXy(x, y) => changed(b.hover_xy(x, y).await.map(|_| format!("moved the mouse to {x},{y}"))),
            Action::ScrollXy([x, y, dx, dy]) => {
                changed(b.scroll_xy(x, y, dx, dy).await.map(|_| format!("scrolled by {dx},{dy} at {x},{y}")))
            }
            Action::TypeText(t) => {
                changed(b.type_text(t).await.map(|_| format!("typed {} characters", t.chars().count())))
            }
            Action::Screenshot(_)
            | Action::Batch(_)
            | Action::Close
            | Action::Open(..)
            | Action::Browsers
            | Action::SelectBrowser(_)
            | Action::SwitchBrowser => Err(Step { text: "browser: this action cannot run here".into(), page: false }),
        }
    }
}

/// What one action answered: its text, and whether the fresh page follows it (every action that changes the page).
struct Step {
    text: String,
    page: bool,
}

/// A page-changing action: its message, or why it failed (then the page shows what is there instead).
fn changed(done: Result<String, nexo_browser::Error>) -> Result<Step, Step> {
    done.map(|text| Step { text, page: true }).map_err(|e| Step { text: e.to_string(), page: true })
}

/// A read-only action: its data alone, no page after it.
fn read(name: &str, done: Result<String, nexo_browser::Error>) -> Result<Step, Step> {
    done.map(|text| Step { text, page: false }).map_err(|e| Step { text: format!("browser {name}: {e}"), page: false })
}

/// `level  text`, the latest [`MAX_ENTRIES`].
fn show_console(list: &[ConsoleEntry]) -> String {
    show_lines(list, |e| format!("{}  {}", e.level, one_line(&e.text, 300)))
}

/// ` with ctrl+shift held`, or nothing without modifiers.
fn held(m: Modifiers) -> String {
    if m.is_none() {
        String::new()
    } else {
        format!(" with {m} held")
    }
}

/// `number method status url`, the latest [`MAX_ENTRIES`]; `-` while no response has come. The number is
/// what `network` takes in `to` for the full request.
fn show_network(list: &[NetEntry]) -> String {
    show_lines(list, |e| {
        let status = e.status.map_or_else(|| "-".to_string(), |s| s.to_string());
        format!("{} {} {status} {}", e.seq, e.method, one_line(&e.url, 300))
    })
}

/// One request in full: status, timing, headers both ways and the start of the body.
fn show_request(d: &NetDetail) -> String {
    let e = &d.entry;
    let status = e.status.map_or_else(|| "no response".to_string(), |s| format!("{s} {}", e.status_text));
    let mut out = format!("{} {}\n{}", e.method, one_line(&e.url, 600), status.trim());
    if !e.mime.is_empty() {
        out.push_str(&format!("  {}", e.mime));
    }
    let ms = |v: f64| format!("{} ms", v.round());
    match (e.first_byte_ms, e.duration_ms) {
        (Some(first), Some(total)) => {
            out.push_str(&format!("\ntime: {} total, first byte after {}", ms(total), ms(first)))
        }
        (None, Some(total)) => out.push_str(&format!("\ntime: {}", ms(total))),
        (Some(first), None) => out.push_str(&format!("\ntime: first byte after {}, still loading", ms(first))),
        (None, None) => {}
    }
    if let Some(err) = &e.error {
        out.push_str(&format!("\nFAILED: {err}"));
    }
    for (title, list) in [("request headers", &e.request_headers), ("response headers", &e.response_headers)] {
        if !list.is_empty() {
            out.push_str(&format!("\n{title}:"));
            for (k, v) in list {
                out.push_str(&format!("\n  {k}: {}", one_line(v, MAX_HEADER)));
            }
        }
    }
    match (&d.body, &d.body_note) {
        (Some(body), note) => {
            let note = note.as_ref().map(|n| format!(" ({n})")).unwrap_or_default();
            out.push_str(&format!("\nbody{note}:\n{body}"));
        }
        (None, Some(note)) => out.push_str(&format!("\nbody: {note}")),
        (None, None) => out.push_str("\nbody: (none)"),
    }
    cap_text(&out, MAX_DETAIL)
}

/// The matches of `find` as snapshot lines, best first.
fn show_found(query: &str, hits: &[nexo_browser::Element], total: usize) -> String {
    if hits.is_empty() {
        return format!("(nothing on the page matches {query:?}; try other words, snapshot, or read_text)");
    }
    let snap = Snapshot { elements: hits.to_vec() };
    match total.saturating_sub(hits.len()) {
        0 => snap.render(),
        more => format!("{}\n… {more} weaker matches; use more specific words", snap.render()),
    }
}

fn show_lines<T>(list: &[T], line: impl Fn(&T) -> String) -> String {
    if list.is_empty() {
        return "(none)".into();
    }
    let skip = list.len().saturating_sub(MAX_ENTRIES);
    let mut lines: Vec<String> = list[skip..].iter().map(line).collect();
    if skip > 0 {
        lines.insert(0, format!("… {skip} earlier entries left out"));
    }
    cap_text(&lines.join("\n"), MAX_LIST)
}

/// `index* title  url`; the star marks the tab the actions work on.
fn show_tabs(tabs: &[TabInfo]) -> String {
    let lines: Vec<String> = tabs
        .iter()
        .map(|t| {
            format!(
                "{}{} {}  {}",
                t.index,
                if t.active { "*" } else { "" },
                one_line(&t.title, 80),
                one_line(&t.url, 200)
            )
        })
        .collect();
    lines.join("\n")
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
            description: "Drive a browser (Chrome, Chromium or Edge) for what `web_fetch` cannot do: JavaScript pages, \
                          logins, forms. One `action` per call; the first call starts a clean headless browser, which stays open until `close`. \
                          `open{profile=persistent}` keeps logins (window shown to sign in). Page-changing actions return a snapshot, \
                          one line per element, `e3 button \"Submit\"`; act on the refs of the latest one. A `target` is a ref (best), \
                          CSS selector or visible text. Fill a whole form with ONE `fill_batch`. `read_page` reads the page as a tree, \
                          `read_text` its text, `screenshot` shows it, `batch` runs known steps in one call. \
                          Never use it to get past a CAPTCHA or bot check: stop and tell the person. `browsers` lists the person's own \
                          browsers (`select_browser` uses one; the result says the rules)."
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
                    "id": {"type": "string", "description": "select_browser: from `browsers`."}
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
                ("action", "open{url?,profile?,visible?} navigate{url} snapshot read_page{filter?,depth?,ref_id?,max_chars?} read_text{selector?} click{target} fill{target,value} fill_batch{fields} select{target,option} set_checkbox{target,checked} press{keys} hover{target} scroll{to} wait{selector?,ms?} screenshot{full?} back forward reload eval{value} console{selector?} network{selector?,to?} tabs tab_new{url} tab_switch{to} tab_close{to} upload{target,value} resize{to} find{target} click_xy|double_click|right_click|drag|hover_xy|scroll_xy{to,modifiers?} type_text{value} start_recording stop_recording export_gif{value?,gif?} clear_recording batch{steps} close browsers select_browser{id} switch_browser"),
                ("target", "ref (e3), CSS selector or text"),
                ("profile", "persistent keeps logins"),
                ("keys", "e.g. Enter, Ctrl+A, Tab Tab"),
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
        let arg = ["url", "target", "keys", "to", "selector", "ref_id", "id"].iter().find_map(|k| str_arg(args, k));
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
            (json!({"action": "open"}), Action::Open(None, Want::default())),
            (
                json!({"action": "open", "url": "https://a.b", "profile": "Persistent", "visible": true}),
                Action::Open(
                    Some("https://a.b"),
                    Want { profile: Some(BrowserProfile::Persistent), visible: Some(true) },
                ),
            ),
            (
                json!({"action": "open", "profile": "ephemeral", "visible": false}),
                Action::Open(None, Want { profile: Some(BrowserProfile::Ephemeral), visible: Some(false) }),
            ),
            (json!({"action": "snapshot"}), Action::Snapshot),
            (json!({"action": "read_page"}), Action::ReadPage(ReadPageArgs::default())),
            (
                json!({"action": "read_page", "filter": "interactive", "depth": 3, "ref_id": "e12", "max_chars": "900"}),
                Action::ReadPage(ReadPageArgs {
                    interactive: true,
                    depth: Some(3),
                    ref_id: Some("e12"),
                    max_chars: Some(900),
                }),
            ),
            (
                json!({"action": "read_page", "filter": "all", "target": "e4"}),
                Action::ReadPage(ReadPageArgs { ref_id: Some("e4"), ..Default::default() }),
            ),
            (json!({"action": "get_page_text"}), Action::ReadText(None)),
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
            (json!({"action": "back"}), Action::Back),
            (json!({"action": "forward"}), Action::Forward),
            (json!({"action": "reload"}), Action::Reload),
            (json!({"action": "eval", "value": "document.title"}), Action::Eval("document.title")),
            (json!({"action": "console"}), Action::Console(None)),
            (json!({"action": "console", "selector": "error"}), Action::Console(Some("error"))),
            (json!({"action": "network", "selector": "api"}), Action::Network(Some("api"), None)),
            (json!({"action": "network", "to": "4"}), Action::Network(None, Some(4))),
            (json!({"action": "find", "target": "search box"}), Action::Find("search box")),
            (json!({"action": "double_click", "to": "3,4"}), Action::DoubleClick(3.0, 4.0, Modifiers::default())),
            (
                json!({"action": "right_click", "to": "3,4", "modifiers": "alt"}),
                Action::RightClick(3.0, 4.0, Modifiers::parse("alt").unwrap()),
            ),
            (
                json!({"action": "click_xy", "to": "1,2", "modifiers": "Ctrl+Shift"}),
                Action::ClickXy(1.0, 2.0, Modifiers::parse("ctrl shift").unwrap()),
            ),
            (json!({"action": "hover_xy", "to": "5,6"}), Action::HoverXy(5.0, 6.0)),
            (
                json!({"action": "drag", "to": "10,20,30,-40", "modifiers": "shift"}),
                Action::Drag([10.0, 20.0, 30.0, -40.0], Modifiers::parse("shift").unwrap()),
            ),
            (
                json!({"action": "drag", "to": "10,20 30,40"}),
                Action::Drag([10.0, 20.0, 30.0, 40.0], Modifiers::default()),
            ),
            (json!({"action": "scroll_xy", "to": "400,300,0,500"}), Action::ScrollXy([400.0, 300.0, 0.0, 500.0])),
            (json!({"action": "tabs"}), Action::Tabs),
            (json!({"action": "tab_new", "url": "https://a.b"}), Action::TabNew("https://a.b")),
            (json!({"action": "tab_switch", "to": 1}), Action::TabSwitch(1)),
            (json!({"action": "tab_close", "to": "2"}), Action::TabClose(2)),
            (json!({"action": "upload", "target": "e7", "value": "cv.pdf"}), Action::Upload("e7", "cv.pdf")),
            (json!({"action": "resize", "to": "390x844"}), Action::Resize(390, 844)),
            (json!({"action": "click_xy", "to": "10, 20.5"}), Action::ClickXy(10.0, 20.5, Modifiers::default())),
            (json!({"action": "type_text", "value": "hi"}), Action::TypeText("hi")),
            (json!({"action": "press", "keys": "Ctrl+A"}), Action::Press("Ctrl+A")),
            (
                json!({"action": "batch", "steps": [{"action": "click", "target": "e1"}, {"action": "tabs"}]}),
                Action::Batch(vec![("click", Action::Click("e1")), ("tabs", Action::Tabs)]),
            ),
            (json!({"action": "close"}), Action::Close),
            (json!({"action": "start_recording"}), Action::Gif(gif::GifCall::Start)),
            (json!({"action": "stop_recording"}), Action::Gif(gif::GifCall::Stop)),
            (json!({"action": "clear_recording"}), Action::Gif(gif::GifCall::Clear)),
            (
                json!({"action": "export_gif", "value": "a.gif", "gif": {"watermark": false}}),
                Action::Gif(gif::GifCall::Export {
                    path: Some("a.gif"),
                    opts: nexo_browser::ExportOptions { watermark: false, ..Default::default() },
                }),
            ),
            (json!({"action": "browsers"}), Action::Browsers),
            (json!({"action": "select_browser", "id": "chrome-1"}), Action::SelectBrowser("chrome-1")),
            (json!({"action": "switch_browser"}), Action::SwitchBrowser),
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
        assert!(err(json!({"action": "read_page", "filter": "some"})).contains("`all` or `interactive`"));
        assert!(err(json!({"action": "read_page", "depth": 0})).contains("`depth` must be a whole number"));
        assert!(err(json!({"action": "read_page", "max_chars": "lots"})).contains("`max_chars`"));
        assert!(err(json!({"action": "read_page", "max_chars": 9_999_999})).contains("from 1 to 200000"));
        assert!(err(json!({"action": "eval"})).contains("`value`"));
        assert!(err(json!({"action": "eval", "value": " "})).contains("JavaScript"));
        assert!(err(json!({"action": "tab_switch"})).contains("`to`"));
        assert!(err(json!({"action": "resize", "to": "10"})).contains("WIDTHxHEIGHT"));
        assert!(err(json!({"action": "click_xy", "to": "1,2,3"})).contains("X,Y"));
        assert!(err(json!({"action": "click_xy", "to": "1,2", "modifiers": "hyper"})).contains("unknown modifier"));
        assert!(err(json!({"action": "double_click", "to": "1"})).contains("X,Y"));
        assert!(err(json!({"action": "drag", "to": "1,2,3"})).contains("X1,Y1,X2,Y2"));
        assert!(err(json!({"action": "drag", "to": "1,2,3,NaN"})).contains("X1,Y1,X2,Y2"));
        assert!(err(json!({"action": "scroll_xy", "to": "1,2"})).contains("X,Y,DX,DY"));
        assert!(err(json!({"action": "find"})).contains("`target` is required"));
        assert!(err(json!({"action": "batch"})).contains("`steps`"));
        assert!(err(json!({"action": "batch", "steps": [{"action": "click"}]})).contains("(batch step 1)"));
        assert!(err(json!({"action": "batch", "steps": [{"action": "tabs"}, {"action": "close"}]}))
            .contains("step 2 `close`"));
        assert!(err(json!({"action": "batch", "steps": [{"action": "batch", "steps": [{"action": "tabs"}]}]}))
            .contains("cannot be part"));
        for read in ["read_page", "read_text", "get_page_text"] {
            let e = err(json!({"action": "batch", "steps": [{"action": "tabs"}, {"action": read}]}));
            assert!(e.contains(&format!("step 2 `{read}`")) && e.contains("on its own"), "{e}");
        }
        assert!(err(json!({"action": "select_browser"})).contains("`id` is required"));
        for a in ["browsers", "switch_browser"] {
            assert!(err(json!({"action": "batch", "steps": [{"action": a}]})).contains("cannot be part"), "{a}");
        }
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
        for p in [
            "url",
            "target",
            "value",
            "fields",
            "option",
            "checked",
            "keys",
            "to",
            "selector",
            "ms",
            "full",
            "steps",
            "filter",
            "depth",
            "ref_id",
            "max_chars",
            "profile",
            "visible",
            "id",
        ] {
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
        for bad in [json!({"action": "open", "profile": "forever"}), json!({"action": "open", "visible": "maybe"})] {
            let out = t.run(bad, &ctx()).await;
            assert!(out.is_error && out.content.contains("browser open"), "{}", out.content);
        }
        let out = t.run(json!({"action": "batch", "steps": [{"action": "open"}]}), &ctx()).await;
        assert!(out.is_error, "open cannot run inside a batch: {}", out.content);
        assert!(t.browser.lock().await.is_none());
    }

    /// A page on 127.0.0.1 (cookies and localStorage need a real origin), answering every request the same way.
    fn site() -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let _ = s.read(&mut [0u8; 2048]);
                let body = "<!doctype html><title>site</title><body>page</body>";
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(reply.as_bytes());
            }
        });
        origin
    }

    fn tool_in(data: &std::path::Path) -> BrowserTool {
        BrowserTool { data_dir: Some(data.to_path_buf()), ..BrowserTool::default() }
    }

    /// Every test below passes `visible: false`: a window would open on the desk of whoever runs the tests.
    #[tokio::test]
    async fn a_kept_profile_keeps_a_login_across_close_and_reopen_and_a_changed_mode_restarts() {
        if nexo_browser::find_browser().is_none() {
            eprintln!("skipped: no chrome");
            return;
        }
        let (data, origin) = (tempfile::tempdir().unwrap(), site());
        let t = tool_in(data.path());
        // clean first: the way the model starts by navigating
        let out = t.run(json!({"action": "navigate", "url": origin}), &ctx()).await;
        assert!(!out.is_error && !out.content.contains("browser started"), "{}", out.content);
        // a change of profile restarts it, with the note
        let out =
            t.run(json!({"action": "open", "profile": "persistent", "visible": "false", "url": origin}), &ctx()).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.contains("restarted the browser") && out.content.contains("persistent profile"),
            "{}",
            out.content
        );
        let out = t
            .run(
                json!({"action": "eval", "value": "localStorage.setItem('k', 'kept-1'); document.cookie = 'c=kept-2; max-age=3600'; 1"}),
                &ctx(),
            )
            .await;
        assert!(!out.is_error, "{}", out.content);
        // the same call again is a no-op
        let out = t.run(json!({"action": "open", "profile": "persistent", "visible": false}), &ctx()).await;
        assert!(out.content.contains("already open"), "{}", out.content);
        let out = t.run(json!({"action": "close"}), &ctx()).await;
        assert!(out.content.contains("closed"), "{}", out.content);
        assert!(data.path().join("browser").join("profile").is_dir(), "a kept profile stays after close");

        let out =
            t.run(json!({"action": "open", "profile": "persistent", "visible": false, "url": origin}), &ctx()).await;
        assert!(!out.is_error && !out.content.contains("restarted"), "{}", out.content);
        let out = t
            .run(json!({"action": "eval", "value": "localStorage.getItem('k') + '|' + document.cookie"}), &ctx())
            .await;
        assert!(out.content.contains("kept-1|c=kept-2"), "{}", out.content);
        // a visible flag alone changes how it runs, so it restarts with the kept profile's own lock released first
        let out = t.run(json!({"action": "open", "visible": false}), &ctx()).await;
        assert!(out.content.contains("already open"), "{}", out.content);
        t.run(json!({"action": "close"}), &ctx()).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_browser_killed_behind_the_tools_back_comes_back_the_way_it_was() {
        if nexo_browser::find_browser().is_none() {
            eprintln!("skipped: no chrome");
            return;
        }
        let data = tempfile::tempdir().unwrap();
        let t = tool_in(data.path());
        let out = t.run(json!({"action": "open", "profile": "persistent", "visible": false}), &ctx()).await;
        assert!(!out.is_error, "{}", out.content);
        let profile = data.path().join("browser").join("profile");
        let killed = std::process::Command::new("pkill")
            .arg("-9")
            .arg("-f")
            .arg(format!("--user-data-dir={}", profile.display()))
            .status();
        if !killed.is_ok_and(|s| s.success()) {
            eprintln!("skipped: pkill could not stop the browser");
            t.run(json!({"action": "close"}), &ctx()).await;
            return;
        }
        let started = std::time::Instant::now();
        let out = t.run(json!({"action": "snapshot"}), &ctx()).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("the browser had been closed"), "{}", out.content);
        assert!(started.elapsed() < std::time::Duration::from_secs(40), "the dead browser must not hang the call");
        let mode = *t.mode.lock().unwrap();
        assert_eq!(
            mode,
            Some(launch::Mode { profile: BrowserProfile::Persistent, headless: true }),
            "it comes back kept and hidden, as it was"
        );
        t.run(json!({"action": "close"}), &ctx()).await;
    }

    #[tokio::test]
    async fn a_failed_open_leaves_the_running_browser_alone() {
        if nexo_browser::find_browser().is_none() {
            eprintln!("skipped: no chrome");
            return;
        }
        let (data, origin) = (tempfile::tempdir().unwrap(), site());
        // another browser holds the kept profile
        let holder = nexo_browser::Browser::launch(nexo_browser::LaunchOptions {
            profile: nexo_browser::Profile::Persistent(data.path().join("browser").join("profile")),
            ..Default::default()
        })
        .await
        .unwrap();
        let t = tool_in(data.path());
        let out = t.run(json!({"action": "navigate", "url": origin}), &ctx()).await;
        assert!(!out.is_error, "{}", out.content);
        let out = t.run(json!({"action": "open", "profile": "persistent", "visible": false}), &ctx()).await;
        assert!(out.is_error && out.content.contains("already in use"), "{}", out.content);
        assert!(out.content.contains("untouched"), "{}", out.content);
        // still the clean browser, on its page
        let out = t.run(json!({"action": "read_text"}), &ctx()).await;
        assert!(!out.is_error && out.content.contains("page"), "{}", out.content);
        assert_eq!(*t.mode.lock().unwrap(), Some(launch::Mode { profile: BrowserProfile::Ephemeral, headless: true }));
        t.run(json!({"action": "close"}), &ctx()).await;
        holder.close().await.unwrap();
    }

    #[tokio::test]
    async fn open_starts_a_clean_headless_browser_on_the_address_and_close_ends_it() {
        if nexo_browser::find_browser().is_none() {
            eprintln!("skipped: no chrome");
            return;
        }
        let t = BrowserTool::new();
        let out = t
            .run(json!({"action": "open", "url": "data:text/html,<title>Hello</title><button>Go</button>"}), &ctx())
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Go"), "{}", out.content);
        let out = t.run(json!({"action": "open", "profile": "ephemeral", "visible": false}), &ctx()).await;
        assert!(!out.is_error, "{}", out.content);
        let out = t.run(json!({"action": "close"}), &ctx()).await;
        assert!(out.content.contains("closed"), "{}", out.content);
        assert!(t.browser.lock().await.is_none());
    }

    #[tokio::test]
    async fn records_a_session_and_exports_a_gif_with_overlays() {
        if nexo_browser::find_browser().is_none() {
            eprintln!("skipped: no chrome");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let page = dir.path().join("p.html");
        std::fs::write(
            &page,
            r#"<!doctype html><body style="margin:0"><button style="position:absolute;left:300px;top:200px;width:120px;height:40px" onclick="document.body.style.background='#ddf'">Press me</button>"#,
        )
        .unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let t = BrowserTool::new();
        let run = |args: Value| t.run(args, &ctx);

        let out = run(json!({"action": "navigate", "url": format!("file://{}", page.display())})).await;
        assert!(!out.is_error, "{}", out.content);
        // exporting before any recording says what to do
        let out = run(json!({"action": "export_gif"})).await;
        assert!(out.is_error, "{}", out.content);

        let out = run(json!({"action": "start_recording"})).await;
        assert!(!out.is_error && out.content.contains("stop_recording"), "{}", out.content);
        run(json!({"action": "wait", "ms": 250})).await;
        let out = run(json!({"action": "click", "target": "Press me"})).await;
        assert!(!out.is_error, "{}", out.content);
        run(json!({"action": "wait", "ms": 300})).await;
        let out = run(json!({"action": "stop_recording"})).await;
        assert!(!out.is_error && out.content.contains("1 actions logged"), "{}", out.content);

        let out = run(json!({"action": "export_gif", "value": "out/demo.gif", "gif": {"quality": 20}})).await;
        assert!(!out.is_error, "{}", out.content);
        let file = dir.path().join("out").join("demo.gif");
        let bytes = std::fs::read(&file).unwrap();
        assert!(bytes.starts_with(b"GIF8"));
        assert!(
            out.content.contains(&file.display().to_string())
                && out.content.contains(&format!("{} bytes", bytes.len())),
            "{}",
            out.content
        );
        // the default name is recording-<n>.gif in the session directory
        let out = run(json!({"action": "export_gif"})).await;
        assert!(!out.is_error && dir.path().join("recording-1.gif").is_file(), "{}", out.content);

        let out = run(json!({"action": "clear_recording"})).await;
        assert!(!out.is_error && out.content.contains("discarded"), "{}", out.content);
        run(json!({"action": "close"})).await;
    }

    #[test]
    fn the_lists_are_compact_and_capped() {
        let tabs = vec![
            TabInfo { index: 0, title: "A".into(), url: "https://a.b".into(), active: false },
            TabInfo { index: 1, title: "B".into(), url: "https://c.d".into(), active: true },
        ];
        assert_eq!(show_tabs(&tabs), "0 A  https://a.b\n1* B  https://c.d");
        assert_eq!(show_console(&[]), "(none)");
        let many: Vec<ConsoleEntry> =
            (0..60).map(|i| ConsoleEntry { level: "error".into(), text: format!("boom {i}"), ts: 0.0 }).collect();
        let shown = show_console(&many);
        assert!(
            shown.starts_with("… 10 earlier entries left out\nerror  boom 10") && shown.ends_with("error  boom 59")
        );
        assert_eq!(shown.lines().count(), 51);
    }

    #[test]
    fn the_summary_names_the_action_and_its_object() {
        let t = BrowserTool::new();
        assert_eq!(t.summary(&json!({"action": "navigate", "url": "https://a.b"})), "browser navigate https://a.b");
        assert_eq!(t.summary(&json!({"action": "snapshot"})), "browser snapshot");
        assert_eq!(t.summary(&json!({"action": "read_page", "ref_id": "e3"})), "browser read_page e3");
        assert_eq!(
            t.summary(&json!({"action": "select_browser", "id": "chrome-1"})),
            "browser select_browser chrome-1"
        );
    }

    /// The whole path in a real headless Chrome: the full tree on request, the compact snapshot after an action.
    #[tokio::test]
    async fn read_page_is_opt_in_and_actions_still_answer_with_the_compact_snapshot() {
        if nexo_browser::find_browser().is_none() {
            eprintln!("skipped: no chrome");
            return;
        }
        let html =
            "<main><h1>Shop</h1><p>Fresh bread daily.</p><button onclick=\"document.title='bought'\">Buy</button>\
                    <div style='display:none'>secret offer</div></main><footer>Imprint</footer>";
        let t = BrowserTool::new();
        let c = ctx();
        let out = t.run(json!({"action": "navigate", "url": format!("data:text/html,{html}")}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        // after a page-changing action: the compact list only
        let page = out.content.split_once("Page:\n").expect("the page follows").1;
        assert_eq!(page, "e1 button \"Buy\"", "{}", out.content);

        let out = t.run(json!({"action": "read_page"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        for want in ["heading \"Shop\" level=1", "text \"Fresh bread daily.\"", "button \"Buy\"", "(not visible)"] {
            assert!(out.content.contains(want), "{want}: {}", out.content);
        }
        assert!(
            !out.content.starts_with("Page:") && !out.content.contains("\n\nPage:"),
            "read-only: no snapshot after it"
        );
        let buy = out
            .content
            .lines()
            .find(|l| l.contains("button \"Buy\""))
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_string();

        let out = t.run(json!({"action": "read_page", "filter": "interactive"}), &c).await;
        assert_eq!(out.content, format!("{buy} button \"Buy\""));
        let out = t.run(json!({"action": "read_page", "ref_id": "e9999"}), &c).await;
        assert!(out.is_error && out.content.contains("e9999"), "{}", out.content);

        let out = t.run(json!({"action": "click", "target": buy}), &c).await;
        assert!(!out.is_error && !out.content.contains("heading"), "{}", out.content);
        let out = t.run(json!({"action": "get_page_text"}), &c).await;
        assert!(out.content.contains("Fresh bread daily.") && !out.content.contains("Imprint"), "{}", out.content);
        assert!(out.content.contains("[main content only (main)"), "{}", out.content);
        let out = t.run(json!({"action": "read_text", "selector": "body"}), &c).await;
        assert!(out.content.contains("Imprint") && !out.content.contains("main content only"), "{}", out.content);
        let out = t.run(json!({"action": "close"}), &c).await;
        assert!(!out.is_error);
    }

    #[test]
    fn read_text_says_when_it_read_only_the_main_part() {
        let page = |text: &str, source: &str| PageText { text: text.into(), source: source.into() };
        assert_eq!(show_text(&page("all of it", "body")), "all of it");
        assert_eq!(show_text(&page("part", "selector")), "part");
        assert_eq!(show_text(&page("  ", "article")), "(no text)");
        let main = show_text(&page("story", "article"));
        assert!(
            main.starts_with("story\n\n[main content only (article)") && main.contains("selector \"body\""),
            "{main}"
        );
        assert!(show_tree("").contains("nothing on the page") && show_tree("e1 button") == "e1 button");
    }

    #[test]
    fn read_page_args_become_browser_options() {
        let o = ReadPageArgs::default().options();
        assert_eq!(o, ReadPageOptions::default());
        assert_eq!((o.depth, o.max_chars, o.filter), (15, 50_000, Filter::All));
        let o = ReadPageArgs { interactive: true, depth: Some(2), ref_id: Some("e1"), max_chars: Some(10) }.options();
        assert_eq!((o.depth, o.max_chars, o.filter, o.ref_id.as_deref()), (2, 10, Filter::Interactive, Some("e1")));
    }

    #[test]
    fn a_request_is_shown_with_its_headers_timing_and_body() {
        let d = NetDetail { entry: NetEntry::sample(3), body: Some("{\"ok\":true}".into()), body_note: None };
        let text = show_request(&d);
        assert!(text.starts_with("POST https://a.b/api\n201 Created  application/json\n"), "{text}");
        assert!(text.contains("time: 120 ms total, first byte after 80 ms"), "{text}");
        assert!(text.contains("request headers:\n  Accept: */*"), "{text}");
        assert!(text.contains("response headers:\n  Content-Type: application/json"), "{text}");
        assert!(text.ends_with("body:\n{\"ok\":true}"), "{text}");
        let cut = NetDetail { body_note: Some("cut: 8000 of 9000 characters".into()), ..d.clone() };
        assert!(show_request(&cut).contains("body (cut: 8000 of 9000 characters):"));
        let none = NetDetail { body: None, body_note: Some("binary (image/png, about 3 bytes)".into()), ..d };
        assert!(show_request(&none).ends_with("body: binary (image/png, about 3 bytes)"));
        assert_eq!(show_network(&[NetEntry::sample(3)]), "3 POST 201 https://a.b/api");
        assert_eq!(held(Modifiers::parse("shift ctrl").unwrap()), " with ctrl+shift held");
        assert_eq!(held(Modifiers::default()), "");
    }

    #[test]
    fn find_answers_with_lines_or_a_hint() {
        use nexo_browser::Element;
        let el =
            |r: &str, role: &str, name: &str| Element { reference: r.into(), role: role.into(), name: name.into() };
        let hits = vec![el("e2", "searchbox", "Search"), el("e5", "button", "Search")];
        assert_eq!(show_found("search box", &hits, 2), "e2 searchbox \"Search\"\ne5 button \"Search\"");
        assert!(show_found("search box", &hits, 5).ends_with("… 3 weaker matches; use more specific words"));
        assert!(show_found("nope", &[], 0).starts_with("(nothing on the page matches \"nope\""));
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
