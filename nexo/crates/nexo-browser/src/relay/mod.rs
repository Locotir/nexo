//! Backend B: the person's own, logged-in Chrome, reached through the nexo extension and the native-messaging host
//! (see the `nexo-relay` and `nexo-browser-host` crates). The verbs are the ones of [`crate::Browser`], done with raw
//! protocol commands that travel the relay to `chrome.debugger`; the page-side scripts (`act.js`, the snapshot) are
//! the same.
//!
//! What is different from the headless browser, on purpose:
//! - nothing happens before the person clicks Allow in the browser ([`RelayBrowser::connect`] waits for it);
//! - only tabs nexo opened itself (or that those tabs opened) can be driven, never the person's other tabs;
//! - [`RelayBrowser::resize`] sizes the whole window (a viewport override would show on the person's screen);
//! - closing ends the session and detaches the debugger; the tabs stay open for the person to look at;
//! - a dialog (`alert`, `confirm`, `prompt`) is closed at once (cancelled, except `alert`) and noted in the console.
//! - [`RelayBrowser::drag`] sends the mouse events only (no HTML5 drag and drop), and [`RelayBrowser::network_detail`]
//!   has no headers or body: the extension does not pass them on.

mod connect;
mod keys;

pub use connect::{browsers, BrowserEntry};

use crate::events::Buffers;
use crate::export::{self, ExportOptions};
use crate::pointer::{self, Modifiers};
use crate::record::Frame;
use crate::{
    Error, FillReport, Filter, PageText, PageTree, ReadPageOptions, RecordOptions, RecordSummary, Result, Snapshot,
    TabInfo, Wait, ACT_JS, WAIT_CAP_MS,
};
use base64::Engine;
use nexo_relay::client::{Client, Error as WireError, Event};
use nexo_relay::BrowserInfo;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

/// Longest the browser gets to answer one protocol command.
const CALL: Duration = Duration::from_secs(30);
/// Longest a navigation is waited for.
const NAV: Duration = Duration::from_secs(30);
/// After this long without a `load` event the page's `readyState` is looked at as well.
const NAV_POLL_AFTER: Duration = Duration::from_secs(2);
/// A page taller than this is cut in a full-page screenshot (Chrome cannot render more in one image).
const MAX_SHOT_HEIGHT: f64 = 16_000.0;

/// Where to click or hover, and what is there.
const LOCATE_JS: &str = r#"(act, target) => {
  const out = JSON.parse(act('mark', [[target, '']]));
  if (!out[0] || !out[0].ok) return JSON.stringify({ found: false });
  const el = document.querySelector('[data-nexo-click]');
  if (!el) return JSON.stringify({ found: false });
  el.removeAttribute('data-nexo-click');
  el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
  const rect = [...el.getClientRects()].find((r) => r.width > 0 && r.height > 0) || el.getBoundingClientRect();
  const x = Math.min(Math.max(rect.left + rect.width / 2, 1), window.innerWidth - 1);
  const y = Math.min(Math.max(rect.top + rect.height / 2, 1), window.innerHeight - 1);
  return JSON.stringify({ found: true, x, y, file: el.matches('input[type=file]') });
}"#;

/// The element itself (not its value), for commands that take an object: the upload.
const FIND_JS: &str = r#"(act, target) => {
  const out = JSON.parse(act('mark', [[target, '']]));
  if (!out[0] || !out[0].ok) return null;
  const el = document.querySelector('[data-nexo-click]');
  if (el) el.removeAttribute('data-nexo-click');
  return el;
}"#;

#[derive(Debug, Deserialize)]
struct Located {
    found: bool,
    #[serde(default)]
    x: f64,
    #[serde(default)]
    y: f64,
    #[serde(default)]
    file: bool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// What the page said when a script threw: the error's own description first.
fn exception_text(details: &Value) -> String {
    let described = details.get("exception").map(|e| str_of(e, "description")).filter(|t| !t.is_empty());
    described.map_or_else(|| str_of(details, "text").to_string(), str::to_string)
}

/// Screencast frames being collected.
struct Sink {
    tab: i64,
    cap: usize,
    began: Instant,
    frames: Vec<Frame>,
}

#[derive(Default)]
struct State {
    /// Tab ids in the order nexo learnt of them; the index of a tab for the verbs is its place here.
    tabs: Vec<i64>,
    current: Option<i64>,
    /// `load` events seen per tab, so a navigation can wait for the next one.
    loads: HashMap<i64, u64>,
    /// Tabs the person stopped nexo from controlling (the debugger bar was dismissed), with why.
    detached: HashMap<i64, String>,
    /// The person pressed Cancel on Chrome's debugging bar: this session is over (others and the extension are not).
    cancelled: bool,
    sink: Option<Sink>,
}

struct Inner {
    client: Client,
    buffers: Buffers,
    state: Mutex<State>,
}

/// The person's own browser, with the verbs of [`crate::Browser`]. Opened with [`RelayBrowser::connect`].
pub struct RelayBrowser {
    inner: Arc<Inner>,
    pump: JoinHandle<()>,
}

impl Drop for RelayBrowser {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

impl Inner {
    async fn req(&self, method: &str, params: Value) -> Result<Value> {
        match self.client.request(method, params, CALL).await {
            Ok(v) => Ok(v),
            Err(WireError::Timeout(d)) => Err(Error::Page(format!(
                "the browser did not answer {method} within {d:?}; a dialog may be open or the tab stuck"
            ))),
            Err(e) => Err(e.into()),
        }
    }

    /// One protocol command on a tab.
    async fn cdp(&self, tab: i64, method: &str, params: Value) -> Result<Value> {
        if let Some(why) = lock(&self.state).detached.get(&tab) {
            return Err(Error::Page(format!(
                "nexo no longer controls this tab ({why}); the person stopped it. Connect again to continue"
            )));
        }
        self.req("cdp", json!({"tab": tab, "method": method, "params": params})).await
    }

    /// Runs a script in the tab. `by_value` returns the result as JSON, else as a remote object (an id).
    async fn evaluate(&self, tab: i64, expression: &str, by_value: bool, repl: bool) -> Result<Value> {
        let params = json!({
            "expression": expression, "returnByValue": by_value, "awaitPromise": true,
            "userGesture": true, "replMode": repl,
        });
        let reply = self.cdp(tab, "Runtime.evaluate", params).await?;
        if let Some(details) = reply.get("exceptionDetails") {
            return Err(Error::Page(exception_text(details)));
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }

    /// A script that answers with a string (the page-side helpers all do, so the reply crosses as one value).
    async fn eval_string(&self, tab: i64, js: &str) -> Result<String> {
        let result = self.evaluate(tab, js, true, false).await?;
        result
            .get("value")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::Page("the page script did not answer with text".into()))
    }

    fn loads(&self, tab: i64) -> u64 {
        lock(&self.state).loads.get(&tab).copied().unwrap_or(0)
    }

    /// Turns on the event domains nexo reads (console, log, network, page lifecycle) for a tab.
    async fn enable(&self, tab: i64) -> Result<()> {
        for domain in ["Page.enable", "Runtime.enable", "Log.enable", "Network.enable"] {
            self.cdp(tab, domain, json!({})).await?;
        }
        Ok(())
    }

    /// Waits for the load event after `before` (`readyState` is trusted too once it took a while).
    async fn settled(&self, tab: i64, before: u64) {
        let began = Instant::now();
        while began.elapsed() < NAV {
            if self.loads(tab) > before {
                return;
            }
            if began.elapsed() > NAV_POLL_AFTER {
                let ready = self.evaluate(tab, "document.readyState", true, false).await;
                if ready.is_ok_and(|r| str_of(&r, "value") == "complete") {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

/// What the browser does with an event the extension forwards.
async fn handle(inner: &Arc<Inner>, ev: Event) {
    let tab = ev.tab;
    match ev.method.as_str() {
        "Page.loadEventFired" => *lock(&inner.state).loads.entry(tab).or_default() += 1,
        "Page.screencastFrame" => {
            // Chrome sends no further frame until this one is acknowledged.
            if let Some(id) = ev.params.get("sessionId").and_then(Value::as_i64) {
                let _ = inner.cdp(tab, "Page.screencastFrameAck", json!({"sessionId": id})).await;
            }
            let png = base64::engine::general_purpose::STANDARD.decode(str_of(&ev.params, "data")).ok();
            let mut st = lock(&inner.state);
            if let (Some(png), Some(sink)) = (png, st.sink.as_mut().filter(|s| s.tab == tab)) {
                let at = ev.params["metadata"]
                    .get("timestamp")
                    .and_then(Value::as_f64)
                    .unwrap_or_else(|| sink.began.elapsed().as_secs_f64());
                if sink.frames.len() < sink.cap {
                    sink.frames.push(Frame::new(png, at));
                }
            }
        }
        "Page.javascriptDialogOpening" => {
            inner.buffers.ingest(tab, &ev.method, &ev.params);
            // A dialog blocks the page until somebody answers; nobody is watching, so it is closed the safe way:
            // an alert only has OK, anything else is cancelled.
            let accept = str_of(&ev.params, "type") == "alert";
            let inner = Arc::clone(inner);
            tokio::spawn(async move {
                let _ = inner.cdp(tab, "Page.handleJavaScriptDialog", json!({"accept": accept})).await;
            });
        }
        "Nexo.tabOpened" => {
            let new = {
                let mut st = lock(&inner.state);
                let new = !st.tabs.contains(&tab);
                if new {
                    st.tabs.push(tab);
                }
                new
            };
            if new {
                let inner = Arc::clone(inner);
                tokio::spawn(async move {
                    let _ = inner.enable(tab).await;
                });
            }
        }
        "Nexo.tabClosed" => forget(&mut lock(&inner.state), tab),
        "Nexo.detached" => {
            let why = str_of(&ev.params, "reason");
            let mut st = lock(&inner.state);
            st.cancelled |= why == "canceled_by_user";
            st.detached.insert(tab, if why.is_empty() { "detached".to_string() } else { why.to_string() });
        }
        method => {
            inner.buffers.ingest(tab, method, &ev.params);
        }
    }
}

fn forget(st: &mut State, tab: i64) {
    st.tabs.retain(|t| *t != tab);
    st.loads.remove(&tab);
    st.detached.remove(&tab);
    if st.current == Some(tab) {
        st.current = st.tabs.last().copied();
    }
    if st.sink.as_ref().is_some_and(|s| s.tab == tab) {
        st.sink = None;
    }
}

async fn pump(inner: Arc<Inner>, mut events: UnboundedReceiver<Event>) {
    while let Some(ev) = events.recv().await {
        handle(&inner, ev).await;
    }
}

/// An address without a scheme is a web address: `example.com` is `https://example.com`. Anything that already has
/// one (`https:`, `about:`, `chrome:`, `file:`, `data:`) is left alone.
fn normalize(url: &str) -> String {
    let url = url.trim();
    let scheme = url.split_once(':').map(|(s, _)| s).filter(|s| {
        s.len() > 1
            && s.starts_with(|c: char| c.is_ascii_alphabetic())
            && s.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    });
    // `localhost:3000` and `host:8080/path` look like a scheme but are a host and a port.
    let port = url.split_once(':').is_some_and(|(_, rest)| {
        rest.split('/').next().is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    });
    if scheme.is_some() && !port {
        url.to_string()
    } else {
        format!("https://{url}")
    }
}

fn no_tab() -> Error {
    Error::Unusable { target: "tab".into(), reason: "no tab is open; open one with tab_new".into() }
}

fn bad_index(index: usize) -> Error {
    Error::Unusable { target: index.to_string(), reason: "no tab with that index".to_string() }
}

impl RelayBrowser {
    /// Takes a session that already has the person's consent and opens its first tab.
    pub(crate) async fn start(client: Client) -> Result<RelayBrowser> {
        let events = client.take_events().ok_or_else(|| Error::Page("the connection has no event stream".into()))?;
        let inner = Arc::new(Inner { client, buffers: Buffers::default(), state: Mutex::new(State::default()) });
        let pump = tokio::spawn(pump(Arc::clone(&inner), events));
        let browser = RelayBrowser { inner, pump };
        browser.adopt_existing().await?;
        if lock(&browser.inner.state).tabs.is_empty() {
            browser.open_tab("about:blank").await?;
        }
        Ok(browser)
    }

    /// Who the browser says it is (name, system, version).
    pub fn info(&self) -> &BrowserInfo {
        self.inner.client.info()
    }

    /// Other agents that were using this browser when the session began.
    pub fn others(&self) -> &[String] {
        self.inner.client.others()
    }

    /// False once the browser or the host went away, or the person pressed Cancel on Chrome's debugging bar. Only
    /// this session ends then: a new one can be asked for at once.
    pub fn is_alive(&self) -> bool {
        self.inner.client.is_alive() && !lock(&self.inner.state).cancelled
    }

    async fn adopt_existing(&self) -> Result<()> {
        let listed = self.listed().await?;
        for tab in listed.iter().map(|t| t.0) {
            self.inner.enable(tab).await?;
        }
        self.reconcile(&listed);
        Ok(())
    }

    /// The session's tabs as the extension lists them: id, title, url, whether it is the window's selected tab.
    async fn listed(&self) -> Result<Vec<(i64, String, String)>> {
        let reply = self.inner.req("tabs.list", json!({})).await?;
        Ok(reply
            .get("tabs")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|t| {
                        let id = t.get("tab").and_then(Value::as_i64)?;
                        Some((id, str_of(t, "title").to_string(), str_of(t, "url").to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Brings the local tab order in line with the browser's list: new ones are appended, gone ones dropped.
    fn reconcile(&self, listed: &[(i64, String, String)]) {
        let mut st = lock(&self.inner.state);
        let live: HashSet<i64> = listed.iter().map(|t| t.0).collect();
        let gone: Vec<i64> = st.tabs.iter().copied().filter(|t| !live.contains(t)).collect();
        for tab in gone {
            forget(&mut st, tab);
        }
        for (tab, _, _) in listed {
            if !st.tabs.contains(tab) {
                st.tabs.push(*tab);
            }
        }
        if st.current.is_none_or(|c| !st.tabs.contains(&c)) {
            st.current = st.tabs.first().copied();
        }
    }

    /// A new tab in nexo's tab group, made the active one; returns its index.
    async fn open_tab(&self, url: &str) -> Result<usize> {
        let reply = self.inner.req("tabs.create", json!({"url": "about:blank"})).await?;
        let tab = reply
            .get("tab")
            .and_then(Value::as_i64)
            .ok_or_else(|| Error::Page("the browser did not say which tab it opened".into()))?;
        let index = {
            let mut st = lock(&self.inner.state);
            if !st.tabs.contains(&tab) {
                st.tabs.push(tab);
            }
            st.current = Some(tab);
            st.tabs.iter().position(|t| *t == tab).unwrap_or(0)
        };
        self.inner.enable(tab).await?;
        if url != "about:blank" {
            self.go(tab, &normalize(url)).await?;
        }
        Ok(index)
    }

    /// The active tab.
    fn tab(&self) -> Result<i64> {
        lock(&self.inner.state).current.ok_or_else(no_tab)
    }

    async fn go(&self, tab: i64, url: &str) -> Result<()> {
        let before = self.inner.loads(tab);
        let reply = self.inner.cdp(tab, "Page.navigate", json!({"url": url})).await?;
        let failed = str_of(&reply, "errorText");
        if !failed.is_empty() {
            return Err(Error::Page(format!("could not open {url}: {failed}")));
        }
        // A same-document navigation (a #fragment) has no loader and fires no load event.
        if reply.get("loaderId").is_some() {
            self.inner.settled(tab, before).await;
        }
        Ok(())
    }

    pub async fn navigate(&self, url: &str) -> Result<()> {
        self.go(self.tab()?, &normalize(url)).await
    }

    /// PNG bytes of the viewport, or of the whole page (up to 16000 px tall).
    pub async fn screenshot(&self, full_page: bool) -> Result<Vec<u8>> {
        let tab = self.tab()?;
        let mut params = json!({"format": "png"});
        if full_page {
            let m = self.inner.cdp(tab, "Page.getLayoutMetrics", json!({})).await?;
            let size = m.get("cssContentSize").or_else(|| m.get("contentSize")).cloned().unwrap_or(Value::Null);
            let (w, h) = (size["width"].as_f64().unwrap_or(0.0), size["height"].as_f64().unwrap_or(0.0));
            if w >= 1.0 && h >= 1.0 {
                params["captureBeyondViewport"] = json!(true);
                params["clip"] =
                    json!({"x": 0, "y": 0, "width": w.ceil(), "height": h.min(MAX_SHOT_HEIGHT).ceil(), "scale": 1});
            }
        }
        let reply = self.inner.cdp(tab, "Page.captureScreenshot", params).await?;
        base64::engine::general_purpose::STANDARD
            .decode(str_of(&reply, "data"))
            .map_err(|e| Error::Page(format!("the screenshot was not valid image data: {e}")))
    }

    /// Visible text of the page, or of the first element matching the CSS selector.
    pub async fn read_text(&self, selector: Option<&str>) -> Result<String> {
        #[derive(Deserialize)]
        struct Reply {
            found: bool,
            text: String,
        }
        let selector = selector.unwrap_or("body");
        let sel = serde_json::to_string(selector)?;
        let js = format!(
            "(() => {{ const el = document.querySelector({sel}); \
             return JSON.stringify({{ found: !!el, text: el ? (el.innerText ?? el.textContent ?? '') : '' }}); }})()"
        );
        let reply: Reply = serde_json::from_str(&self.inner.eval_string(self.tab()?, &js).await?)?;
        if reply.found {
            Ok(reply.text)
        } else {
            Err(Error::NotFound(selector.to_string()))
        }
    }

    /// The actionable elements of the page, each carrying a ref the other verbs accept.
    pub async fn snapshot(&self) -> Result<Snapshot> {
        let opts = ReadPageOptions { filter: Filter::Interactive, depth: 1_000, ..Default::default() };
        let json = self.inner.eval_string(self.tab()?, &crate::snapshot::tree_call(&opts)).await?;
        let reply: crate::snapshot::TreeReply = serde_json::from_str(&json)?;
        Ok(Snapshot { elements: reply.elements() })
    }

    /// The page as an indented tree (see [`crate::Browser::read_page`]).
    pub async fn read_page(&self, opts: &ReadPageOptions) -> Result<PageTree> {
        let json = self.inner.eval_string(self.tab()?, &crate::snapshot::tree_call(opts)).await?;
        let reply: crate::snapshot::TreeReply = serde_json::from_str(&json)?;
        if !reply.found {
            return Err(Error::NotFound(opts.ref_id.clone().unwrap_or_default()));
        }
        Ok(crate::snapshot::render_tree(&reply, opts))
    }

    /// Plain text of the page or of the first element matching `selector` (see [`crate::Browser::page_text`]).
    pub async fn page_text(&self, selector: Option<&str>) -> Result<PageText> {
        #[derive(Deserialize)]
        struct Reply {
            found: bool,
            text: String,
            source: String,
        }
        let arg = serde_json::to_string(&selector)?;
        let js = format!("({})({arg})", crate::PAGE_TEXT_JS);
        let reply: Reply = serde_json::from_str(&self.inner.eval_string(self.tab()?, &js).await?)?;
        if reply.found {
            Ok(PageText { text: reply.text, source: reply.source })
        } else {
            Err(Error::NotFound(selector.unwrap_or("body").to_string()))
        }
    }

    /// Where the target is on screen after scrolling it into view; `NotFound` when nothing matches.
    async fn locate(&self, tab: i64, target: &str) -> Result<Located> {
        let js = format!("({LOCATE_JS})({ACT_JS}, {})", serde_json::to_string(target)?);
        let found: Located = serde_json::from_str(&self.inner.eval_string(tab, &js).await?)?;
        if found.found {
            Ok(found)
        } else {
            Err(Error::NotFound(target.to_string()))
        }
    }

    async fn mouse(&self, tab: i64, kind: &str, x: f64, y: f64, pressed: bool) -> Result<()> {
        let mut p = json!({"type": kind, "x": x, "y": y});
        if pressed {
            p["button"] = json!("left");
            p["clickCount"] = json!(1);
        }
        self.inner.cdp(tab, "Input.dispatchMouseEvent", p).await.map(drop)
    }

    async fn click_at(&self, tab: i64, x: f64, y: f64) -> Result<()> {
        self.mouse(tab, "mouseMoved", x, y, false).await?;
        self.mouse(tab, "mousePressed", x, y, true).await?;
        self.mouse(tab, "mouseReleased", x, y, true).await
    }

    /// Clicks the element: a ref (`e3`), a CSS selector or its visible text.
    pub async fn click(&self, target: &str) -> Result<()> {
        let tab = self.tab()?;
        let at = self.locate(tab, target).await?;
        if at.file {
            // A click would open the system file picker on the person's screen, which nobody can answer.
            return Err(Error::Unusable {
                target: target.to_string(),
                reason: "a file input; use upload to give it a file".to_string(),
            });
        }
        self.click_at(tab, at.x, at.y).await
    }

    pub async fn fill(&self, target: &str, value: &str) -> Result<()> {
        self.act_one("fill", target, value).await
    }

    /// Fills a whole form in one round trip; the report says which fields took their value.
    pub async fn fill_batch(&self, pairs: &[(&str, &str)]) -> Result<FillReport> {
        let outcomes = self.act("fill", pairs).await?;
        let mut report = FillReport::default();
        for ((target, _), out) in pairs.iter().zip(outcomes) {
            let target = target.to_string();
            match (out.ok, out.error) {
                (true, _) => report.filled.push(target),
                (false, None) => report.missing.push(target),
                (false, Some(reason)) => report.failed.push((target, reason)),
            }
        }
        Ok(report)
    }

    pub async fn select(&self, target: &str, option: &str) -> Result<()> {
        self.act_one("select", target, option).await
    }

    pub async fn set_checkbox(&self, target: &str, checked: bool) -> Result<()> {
        self.act_one("check", target, if checked { "true" } else { "false" }).await
    }

    /// Presses keys on whatever has the focus: `"Enter"`, `"Tab Tab Enter"`, `"Ctrl+A"`.
    pub async fn press(&self, keys: &str) -> Result<()> {
        let tab = self.tab()?;
        // Every token is checked first, so a typo does not leave half the keys pressed.
        let mut events = Vec::new();
        for token in keys.split_whitespace() {
            events.extend(keys::events_for(token)?);
        }
        for e in events {
            self.inner.cdp(tab, "Input.dispatchKeyEvent", e).await?;
        }
        Ok(())
    }

    pub async fn hover(&self, target: &str) -> Result<()> {
        let tab = self.tab()?;
        let at = self.locate(tab, target).await?;
        self.mouse(tab, "mouseMoved", at.x, at.y, false).await
    }

    pub async fn scroll(&self, target_or_dir: &str) -> Result<()> {
        self.act_one("scroll", target_or_dir, "").await
    }

    /// Waits for a target to appear or for a fixed time, both capped at [`WAIT_CAP_MS`].
    pub async fn wait(&self, what: Wait<'_>) -> Result<()> {
        match what {
            Wait::Millis(ms) => {
                tokio::time::sleep(Duration::from_millis(ms.min(WAIT_CAP_MS))).await;
                Ok(())
            }
            Wait::Selector(target, ms) => {
                let deadline = Instant::now() + Duration::from_millis(ms.min(WAIT_CAP_MS));
                loop {
                    if self.act("exists", &[(target, "")]).await?[0].ok {
                        return Ok(());
                    }
                    if Instant::now() >= deadline {
                        return Err(Error::NotFound(target.to_string()));
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }

    /// Starts recording the active tab; frames are captured as the page changes until [`Self::record_stop_gif`].
    pub async fn record_start(&self, opts: RecordOptions) -> Result<()> {
        let tab = self.tab()?;
        {
            let mut st = lock(&self.inner.state);
            if st.sink.is_some() {
                return Err(Error::Unusable { target: "recording".into(), reason: "already recording".into() });
            }
            st.sink = Some(Sink { tab, cap: opts.max_frames.max(1), began: Instant::now(), frames: Vec::new() });
        }
        let params = json!({
            "format": "png", "maxWidth": opts.max_width.max(1), "maxHeight": opts.max_height.max(1),
            "everyNthFrame": opts.every_nth_frame.max(1),
        });
        if let Err(e) = self.inner.cdp(tab, "Page.startScreencast", params).await {
            lock(&self.inner.state).sink = None;
            return Err(e);
        }
        Ok(())
    }

    /// Stops the recording and writes it to `path` as a looping animated GIF.
    pub async fn record_stop_gif(&self, path: impl AsRef<Path>) -> Result<RecordSummary> {
        let (tab, sink) = {
            let mut st = lock(&self.inner.state);
            let sink = st.sink.take().ok_or_else(|| Error::Unusable {
                target: "recording".into(),
                reason: "no recording in progress".into(),
            })?;
            (sink.tab, sink)
        };
        let stopped = self.inner.cdp(tab, "Page.stopScreencast", json!({})).await;
        // Frames already on their way still count; the pump keeps no sink, so they are dropped here.
        let frames = sink.frames;
        let path = path.as_ref().to_path_buf();
        let summary =
            tokio::task::spawn_blocking(move || export::render_gif(&frames, &[], None, &ExportOptions::plain(), &path))
                .await
                .map_err(|e| Error::Encode(e.to_string()))?;
        stopped.and(summary)
    }

    /// Ends the session: the debugger is detached and the tabs nexo opened stay open in the browser.
    pub async fn close(self) -> Result<()> {
        let recording = lock(&self.inner.state).sink.take();
        if let (Some(sink), Ok(tab)) = (recording, self.tab()) {
            let _ = self.inner.cdp(tab, "Page.stopScreencast", json!({})).await;
            drop(sink);
        }
        Ok(())
    }

    /// Goes one page back in the history of the active tab.
    pub async fn back(&self) -> Result<()> {
        self.step_history("back", -1).await
    }

    pub async fn forward(&self) -> Result<()> {
        self.step_history("forward", 1).await
    }

    pub async fn reload(&self) -> Result<()> {
        let tab = self.tab()?;
        let before = self.inner.loads(tab);
        self.inner.cdp(tab, "Page.reload", json!({})).await?;
        self.inner.settled(tab, before).await;
        Ok(())
    }

    /// Runs JavaScript in the page (top-level `await` works, promises are awaited) and returns the result as JSON
    /// text, `undefined` when there is none. A script that throws is an error.
    pub async fn eval(&self, js: &str) -> Result<String> {
        let result = self.inner.evaluate(self.tab()?, js, true, true).await?;
        Ok(match (result.get("value"), result.get("unserializableValue").and_then(Value::as_str)) {
            (Some(v), _) => v.to_string(),
            (None, Some(u)) => u.to_string(),
            (None, None) => "undefined".to_string(),
        })
    }

    pub async fn console(&self, filter: Option<&str>) -> Result<Vec<crate::ConsoleEntry>> {
        self.flush().await;
        self.inner.buffers.console(filter)
    }

    pub async fn network(&self, filter: Option<&str>) -> Result<Vec<crate::NetEntry>> {
        self.flush().await;
        self.inner.buffers.network(filter)
    }

    /// Lets events the browser already sent reach the buffers before they are read.
    async fn flush(&self) {
        if let Ok(tab) = self.tab() {
            let _ = self.inner.evaluate(tab, "0", true, false).await;
        }
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    /// The tabs nexo opened, in order, with the active one flagged.
    pub async fn tabs(&self) -> Result<Vec<TabInfo>> {
        let listed = self.listed().await?;
        self.reconcile(&listed);
        let (order, current) = {
            let st = lock(&self.inner.state);
            (st.tabs.clone(), st.current)
        };
        Ok(order
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let (title, url) =
                    listed.iter().find(|t| t.0 == *id).map(|t| (t.1.clone(), t.2.clone())).unwrap_or_default();
                TabInfo { index, title, url, active: Some(*id) == current }
            })
            .collect())
    }

    pub async fn tab_new(&self, url: &str) -> Result<usize> {
        self.open_tab(url).await
    }

    pub async fn tab_switch(&self, index: usize) -> Result<()> {
        let tab = lock(&self.inner.state).tabs.get(index).copied().ok_or_else(|| bad_index(index))?;
        self.inner.req("tabs.activate", json!({"tab": tab})).await?;
        lock(&self.inner.state).current = Some(tab);
        Ok(())
    }

    /// Closes a tab nexo opened. Closing the last one leaves the session without a tab (`tab_new` opens another).
    pub async fn tab_close(&self, index: usize) -> Result<()> {
        let tab = lock(&self.inner.state).tabs.get(index).copied().ok_or_else(|| bad_index(index))?;
        self.inner.req("tabs.close", json!({"tab": tab})).await?;
        let mut st = lock(&self.inner.state);
        let was = st.tabs.iter().position(|t| *t == tab).unwrap_or(0);
        forget(&mut st, tab);
        if st.current.is_some() && st.tabs.get(was.saturating_sub(1)).is_some() && was > 0 {
            // The tab before the closed one is the natural neighbour.
            st.current = st.tabs.get(was - 1).copied();
        }
        Ok(())
    }

    /// Sets the file of an `<input type=file>` (ref, CSS selector or label) as if picked in the dialog; the
    /// dialog never opens.
    pub async fn upload(&self, target: &str, path: impl AsRef<Path>) -> Result<()> {
        let file = std::fs::canonicalize(path)?;
        let tab = self.tab()?;
        let js = format!("({FIND_JS})({ACT_JS}, {})", serde_json::to_string(target)?);
        let el = self.inner.evaluate(tab, &js, false, false).await?;
        let object =
            el.get("objectId").and_then(Value::as_str).ok_or_else(|| Error::NotFound(target.to_string()))?.to_string();
        let check = json!({
            "objectId": object, "returnByValue": true,
            "functionDeclaration": "function() { return this.matches('input[type=file]'); }",
        });
        let is_file = self.inner.cdp(tab, "Runtime.callFunctionOn", check).await?;
        if is_file["result"]["value"] != json!(true) {
            return Err(Error::Unusable { target: target.to_string(), reason: "not a file input".to_string() });
        }
        let params = json!({"files": [file.to_string_lossy()], "objectId": object});
        self.inner.cdp(tab, "DOM.setFileInputFiles", params).await?;
        Ok(())
    }

    /// Resizes the browser window (outer size, in pixels) the active tab is in.
    pub async fn resize(&self, width: u32, height: u32) -> Result<()> {
        self.inner.req("window.resize", json!({"tab": self.tab()?, "width": width, "height": height})).await.map(drop)
    }

    /// Sends mouse events in order to the active tab.
    async fn pointer(&self, events: &[pointer::Mouse]) -> Result<()> {
        let tab = self.tab()?;
        for e in events {
            self.inner.cdp(tab, "Input.dispatchMouseEvent", e.params()).await?;
        }
        Ok(())
    }

    /// Clicks at viewport coordinates, for canvases and anything without an element to target, with these
    /// modifier keys held.
    pub async fn click_xy(&self, x: f64, y: f64, mods: Modifiers) -> Result<()> {
        self.pointer(&pointer::click((x, y), pointer::Button::Left, 1, mods)?).await
    }

    /// Double-clicks at viewport coordinates.
    pub async fn double_click(&self, x: f64, y: f64, mods: Modifiers) -> Result<()> {
        self.pointer(&pointer::click((x, y), pointer::Button::Left, 2, mods)?).await
    }

    /// Right-clicks at viewport coordinates.
    pub async fn right_click(&self, x: f64, y: f64, mods: Modifiers) -> Result<()> {
        self.pointer(&pointer::click((x, y), pointer::Button::Right, 1, mods)?).await
    }

    /// Drags with the mouse events only (sliders, canvases, sortable lists that follow the pointer); the answer is
    /// always `false`: the extension does not pass on the browser's own drag-and-drop event, so an HTML5 drag and
    /// drop is not carried out here. The browser's own drags are held back meanwhile, so one started by the press
    /// cannot swallow the rest of the moves.
    pub async fn drag(&self, from: (f64, f64), to: (f64, f64), mods: Modifiers) -> Result<bool> {
        let mut events = pointer::drag_moves(from, to, mods)?;
        events.push(pointer::drag_release(to, mods));
        let tab = self.tab()?;
        self.inner.cdp(tab, "Input.setInterceptDrags", json!({"enabled": true})).await?;
        let done = self.pointer(&events).await;
        // If the press started a drag of the browser's own (selected text, a link), it is called off, not left
        // hanging in the person's browser; with none going on this is refused, which is fine.
        let cancel =
            json!({"type": "dragCancel", "x": to.0, "y": to.1, "data": {"items": [], "dragOperationsMask": 0}});
        let _ = self.inner.cdp(tab, "Input.dispatchDragEvent", cancel).await;
        let off = self.inner.cdp(tab, "Input.setInterceptDrags", json!({"enabled": false})).await;
        done?;
        off.map(|_| false)
    }

    /// Moves the mouse to viewport coordinates.
    pub async fn hover_xy(&self, x: f64, y: f64) -> Result<()> {
        self.pointer(&pointer::hover((x, y), Modifiers::default())?).await
    }

    /// Turns the mouse wheel at viewport coordinates by `dx`, `dy` CSS pixels.
    pub async fn scroll_xy(&self, x: f64, y: f64, dx: f64, dy: f64) -> Result<()> {
        self.pointer(&pointer::wheel((x, y), (dx, dy), Modifiers::default())?).await
    }

    /// Elements by what a person would call them, as [`crate::Browser::find`].
    pub async fn find(&self, query: &str, limit: usize) -> Result<(Vec<crate::Element>, usize)> {
        let snap = self.snapshot().await?;
        let (hits, total) = snap.search(query, limit);
        Ok((hits.into_iter().cloned().collect(), total))
    }

    /// One request of [`RelayBrowser::network`]: what the extension passes on (method, address, status, type,
    /// failure). Its headers are never sent from the person's browser and its body is not read from it.
    pub async fn network_detail(&self, seq: u64) -> Result<crate::NetDetail> {
        self.flush().await;
        let entry = self.inner.buffers.request(seq).ok_or_else(|| Error::NotFound(format!("request {seq}")))?;
        let note = "not read from the person's own browser (headers neither)".to_string();
        Ok(crate::NetDetail { entry, body: None, body_note: Some(note) })
    }

    /// Types text into whatever has the focus.
    pub async fn type_text(&self, text: &str) -> Result<()> {
        self.inner.cdp(self.tab()?, "Input.insertText", json!({"text": text})).await.map(drop)
    }

    async fn step_history(&self, what: &str, delta: i64) -> Result<()> {
        let tab = self.tab()?;
        let history = self.inner.cdp(tab, "Page.getNavigationHistory", json!({})).await?;
        let wanted = history["currentIndex"].as_i64().unwrap_or(0) + delta;
        let entry = usize::try_from(wanted)
            .ok()
            .and_then(|i| history["entries"].get(i))
            .and_then(|e| e.get("id").and_then(Value::as_i64))
            .ok_or_else(|| Error::Unusable {
                target: what.to_string(),
                reason: "no such page in the history".to_string(),
            })?;
        let before = self.inner.loads(tab);
        self.inner.cdp(tab, "Page.navigateToHistoryEntry", json!({"entryId": entry})).await?;
        let deadline = Instant::now() + Duration::from_millis(WAIT_CAP_MS);
        while Instant::now() < deadline {
            let at = self
                .inner
                .cdp(tab, "Page.getNavigationHistory", json!({}))
                .await
                .ok()
                .and_then(|h| h["currentIndex"].as_i64());
            let ready = self.inner.evaluate(tab, "document.readyState", true, false).await.ok();
            let loaded = ready.is_some_and(|r| str_of(&r, "value") == "complete");
            // A restored page may come from the back-forward cache and fire no load event.
            if at == Some(wanted) && (loaded || self.inner.loads(tab) > before) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Ok(())
    }

    /// Runs one verb of `act.js` over `(target, value)` pairs in a single page call.
    async fn act(&self, op: &str, items: &[(&str, &str)]) -> Result<Vec<crate::Outcome>> {
        let js = format!("({ACT_JS})({}, {})", serde_json::to_string(op)?, serde_json::to_string(items)?);
        Ok(serde_json::from_str(&self.inner.eval_string(self.tab()?, &js).await?)?)
    }

    async fn act_one(&self, op: &str, target: &str, value: &str) -> Result<()> {
        let out = self.act(op, &[(target, value)]).await?.into_iter().next();
        match out {
            Some(crate::Outcome { ok: true, .. }) => Ok(()),
            Some(crate::Outcome { error: Some(reason), .. }) => {
                Err(Error::Unusable { target: target.to_string(), reason })
            }
            _ => Err(Error::NotFound(target.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_address_is_https() {
        assert_eq!(normalize("example.com"), "https://example.com");
        assert_eq!(normalize(" example.com/a?b=c "), "https://example.com/a?b=c");
        assert_eq!(normalize("localhost:3000/x"), "https://localhost:3000/x");
        assert_eq!(normalize("https://a.b"), "https://a.b");
        assert_eq!(normalize("http://a.b"), "http://a.b");
        assert_eq!(normalize("about:blank"), "about:blank");
        assert_eq!(normalize("file:///tmp/a.html"), "file:///tmp/a.html");
        assert_eq!(normalize("data:text/html,hi"), "data:text/html,hi");
    }

    #[test]
    fn exceptions_read_the_description_first() {
        let d = json!({"text": "Uncaught", "exception": {"description": "Error: boom\n    at x"}});
        assert_eq!(exception_text(&d), "Error: boom\n    at x");
        assert_eq!(exception_text(&json!({"text": "Uncaught"})), "Uncaught");
    }
}
