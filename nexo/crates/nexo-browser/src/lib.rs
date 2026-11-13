//! Native browser control for nexo: launches the installed Chrome, Chromium or Edge, drives it over the
//! DevTools Protocol and shows the page as a snapshot of actionable elements with stable refs.

mod any;
mod error;
mod events;
mod export;
mod input;
mod overlay;
mod platform;
mod profile;
#[cfg(test)]
mod profile_tests;
#[cfg(test)]
mod readpage_tests;
mod record;
mod recording;
mod relay;
mod snapshot;
mod tabs;
#[cfg(test)]
mod tests;

pub use any::AnyBrowser;
pub use error::{Error, Result};
pub use events::{ConsoleEntry, NetEntry};
pub use export::ExportOptions;
pub use platform::{find_browser, installed_browsers, InstalledBrowser, OVERRIDE_VAR};
pub use profile::{has_display, preflight};
pub use record::{ActionEvent, ActionKind, RecordInfo, RecordOptions, RecordSummary};
pub use relay::{browsers, BrowserEntry, RelayBrowser};
pub use snapshot::{Element, Filter, PageTree, ReadPageOptions, Snapshot, DEFAULT_DEPTH, DEFAULT_MAX_CHARS};
pub use tabs::TabInfo;

use chromiumoxide::browser::{Browser as Chrome, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::dom::SetFileInputFilesParams;
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::input::InsertTextParams;
use chromiumoxide::cdp::browser_protocol::page::{
    CaptureScreenshotFormat, GetNavigationHistoryParams, NavigateToHistoryEntryParams,
};
use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use chromiumoxide::layout::Point;
use chromiumoxide::page::{Page, ScreenshotParams};
use futures::StreamExt;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tabs::Tabs;
use tokio::task::JoinHandle;

/// The window of a browser launched without a size of its own, in pixels.
pub const DEFAULT_WINDOW: (u32, u32) = (1280, 800);

/// Longest a [`Wait`] may last, in milliseconds.
pub const WAIT_CAP_MS: u64 = 5_000;

/// Where the browser keeps cookies, logins and local storage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Profile {
    /// A throwaway profile, deleted on [`Browser::close`].
    #[default]
    Ephemeral,
    /// This `--user-data-dir`, created if missing and kept on close, so a site logged into once stays
    /// logged in on later launches. Only one browser can use a directory at a time.
    Persistent(PathBuf),
}

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    /// Explicit browser binary; `None` searches `PATH`.
    pub executable: Option<PathBuf>,
    pub headless: bool,
    pub profile: Profile,
    /// Window size in pixels; `None`: [`DEFAULT_WINDOW`].
    pub window: Option<(u32, u32)>,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self { executable: None, headless: true, profile: Profile::Ephemeral, window: None }
    }
}

/// What [`Browser::wait`] waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait<'a> {
    /// A ref, CSS selector or visible text to appear, giving up after this many milliseconds.
    Selector(&'a str, u64),
    /// A fixed pause in milliseconds.
    Millis(u64),
}

/// Which targets of a [`Browser::fill_batch`] took their value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FillReport {
    pub filled: Vec<String>,
    /// Targets that matched nothing on the page.
    pub missing: Vec<String>,
    /// Targets that matched but could not take the value, with the reason.
    pub failed: Vec<(String, String)>,
}

impl FillReport {
    /// True when every target took its value.
    pub fn complete(&self) -> bool {
        self.missing.is_empty() && self.failed.is_empty()
    }
}

/// Plain text read from a page, and where on the page it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageText {
    pub text: String,
    /// `selector` for a CSS selector, else `article`, `main`, `densest block <tag#id.class>` or `body`.
    pub source: String,
}

/// A running browser. The verbs act on the active tab ([`Browser::tabs`]). An ephemeral profile is deleted on
/// [`Browser::close`]; a persistent one stays.
pub struct Browser {
    chrome: Chrome,
    tabs: Mutex<Tabs>,
    buffers: events::Buffers,
    handler: JoinHandle<()>,
    recorder: Mutex<Option<record::Recorder>>,
    guard: Option<platform::ProcessGuard>,
    profile: Option<platform::TempProfile>,
    dir: PathBuf,
    headless: bool,
}

impl Browser {
    pub async fn launch(opts: LaunchOptions) -> Result<Browser> {
        let exe = profile::resolve_executable(opts.executable)?;
        if !opts.headless {
            profile::require_display()?;
        }
        let (profile, dir) = profile::open(opts.profile)?;
        let (width, height) = opts.window.unwrap_or(DEFAULT_WINDOW);
        // No viewport emulation: the page fills the window, so a visible window and a headless one look alike.
        let mut cfg = BrowserConfig::builder()
            .chrome_executable(exe)
            .user_data_dir(&dir)
            .no_sandbox()
            .window_size(width, height)
            .viewport(None)
            .launch_timeout(platform::LAUNCH_TIMEOUT);
        cfg = if opts.headless { cfg.new_headless_mode() } else { cfg.with_head() };
        let cfg = cfg.build().map_err(Error::Config)?;
        let (mut chrome, mut events) = Chrome::launch(cfg).await?;
        let guard = platform::ProcessGuard::adopt(chrome.get_mut_child().and_then(|c| c.as_mut_inner().id()));
        // The event stream must be polled before the first command or every command hangs.
        let handler = tokio::spawn(async move { while events.next().await.is_some() {} });
        let buffers = events::Buffers::default();
        // Console and network events reach the buffers through per-page listeners, not through this handler.
        let first = async {
            let page = chrome.new_page("about:blank").await?;
            let tasks = buffers.watch(&page).await?;
            Ok::<_, Error>((page, tasks))
        };
        match first.await {
            Ok((page, tasks)) => {
                // Chrome starts with a blank tab of its own; only the one nexo opened is kept.
                for other in chrome.pages().await.unwrap_or_default() {
                    if other.target_id() != page.target_id() {
                        let _ = other.close().await;
                    }
                }
                Ok(Browser {
                    chrome,
                    tabs: Mutex::new(Tabs::new(page, tasks)),
                    buffers,
                    handler,
                    recorder: Mutex::new(None),
                    guard: Some(guard),
                    profile,
                    dir,
                    headless: opts.headless,
                })
            }
            Err(e) => {
                handler.abort();
                Err(e)
            }
        }
    }

    /// Loads a URL. A local path (`C:\Users\me\page.html`, `/tmp/page.html`) is opened as a `file://` URL.
    pub async fn navigate(&self, url: &str) -> Result<()> {
        self.page().goto(platform::navigable(url).as_ref()).await?;
        Ok(())
    }

    /// PNG bytes of the viewport, or of the whole page.
    pub async fn screenshot(&self, full_page: bool) -> Result<Vec<u8>> {
        let params = ScreenshotParams::builder().format(CaptureScreenshotFormat::Png).full_page(full_page).build();
        Ok(self.page().screenshot(params).await?)
    }

    /// Visible text of the page, or of the first element matching the CSS selector. Without a selector it is the
    /// main content of the page (see [`Browser::page_text`]).
    pub async fn read_text(&self, selector: Option<&str>) -> Result<String> {
        Ok(self.page_text(selector).await?.text)
    }

    /// Plain text of the page. With a CSS selector: the first element matching it. Without one: the main content,
    /// the `<article>` or `<main>` (or `role=main`) holding most of the text, else the densest block of
    /// paragraphs, else the whole body; [`PageText::source`] says which, so a caller can ask for `body` when the
    /// pick was too narrow.
    pub async fn page_text(&self, selector: Option<&str>) -> Result<PageText> {
        #[derive(Deserialize)]
        struct Reply {
            found: bool,
            text: String,
            source: String,
        }
        let arg = serde_json::to_string(&selector)?;
        // The page answers with a string: a bare null does not cross the protocol reliably, and the text may be empty.
        let js = format!("({PAGE_TEXT_JS})({arg})");
        let reply: Reply = serde_json::from_str(&self.eval_as::<String>(&js).await?)?;
        if reply.found {
            Ok(PageText { text: reply.text, source: reply.source })
        } else {
            Err(Error::NotFound(selector.unwrap_or("body").to_string()))
        }
    }

    /// The actionable elements of the page, each carrying a ref that [`Browser::click`] accepts.
    pub async fn snapshot(&self) -> Result<Snapshot> {
        let opts = ReadPageOptions { filter: Filter::Interactive, depth: 1_000, ..Default::default() };
        let reply: snapshot::TreeReply =
            serde_json::from_str(&self.eval_as::<String>(&snapshot::tree_call(&opts)).await?)?;
        Ok(Snapshot { elements: reply.elements() })
    }

    /// The page as a tree: with [`Filter::All`] headings, landmarks, lists, tables, images, text and elements that
    /// are not visible (marked), with [`Filter::Interactive`] only the visible actionable ones. Every element that can
    /// be pointed at carries a ref `e3` that click, fill, hover and the rest accept, the same refs as
    /// [`Browser::snapshot`]. `ref_id` reads the subtree of one element; a ref that matches nothing is
    /// [`Error::NotFound`].
    pub async fn read_page(&self, opts: &ReadPageOptions) -> Result<PageTree> {
        if let Some(r) = &opts.ref_id {
            if !is_ref(r) {
                return Err(Error::Unusable {
                    target: r.clone(),
                    reason: "ref_id must be a ref such as e12".to_string(),
                });
            }
        }
        let reply: snapshot::TreeReply =
            serde_json::from_str(&self.eval_as::<String>(&snapshot::tree_call(opts)).await?)?;
        if !reply.found {
            return Err(Error::NotFound(opts.ref_id.clone().unwrap_or_default()));
        }
        Ok(snapshot::render_tree(&reply, opts))
    }

    /// Clicks the element with this ref (`e3`), or else the first visible element whose text equals the
    /// target, falling back to the first that contains it.
    pub async fn click(&self, target: &str) -> Result<()> {
        let selector = if is_ref(target) {
            format!("[data-nexo-ref=\"{target}\"]")
        } else {
            let text = serde_json::to_string(target)?;
            let js = format!("({MARK_BY_TEXT_JS})({text})");
            if !self.eval_as::<bool>(&js).await? {
                return Err(Error::NotFound(target.to_string()));
            }
            MARKED_SELECTOR.to_string()
        };
        let found = self.page().find_element(selector).await;
        let result = match found {
            Ok(el) => el.click().await.map(drop).map_err(Error::from),
            Err(_) => Err(Error::NotFound(target.to_string())),
        };
        if !is_ref(target) {
            // The mark is only a handle for this click; the page may already be gone.
            let _ = self.eval_as::<bool>(UNMARK_JS).await;
        }
        result
    }

    /// Sets a field and fires `input` and `change` so page scripts notice. The target is a ref (`e3`), a CSS
    /// selector or the label, placeholder or name of the field. Works on inputs, textareas, selects (by value or
    /// visible label), checkboxes (`"true"` ticks) and `contenteditable` elements. The field keeps the focus.
    pub async fn fill(&self, target: &str, value: &str) -> Result<()> {
        self.act_one("fill", target, value).await
    }

    /// Fills a whole form in one round trip. Fields that match nothing or cannot take the value do not stop the
    /// others; the report says which did what.
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

    /// Chooses an `<option>` of a `<select>` by value or visible label and fires `change`.
    pub async fn select(&self, target: &str, option: &str) -> Result<()> {
        self.act_one("select", target, option).await
    }

    /// Ticks or clears a checkbox (or selects a radio button) and fires `change`.
    pub async fn set_checkbox(&self, target: &str, checked: bool) -> Result<()> {
        self.act_one("check", target, if checked { "true" } else { "false" }).await
    }

    /// Presses keys on whatever has the focus, e.g. `"Enter"` or `"Tab Tab Enter"` (space separated). A key with
    /// modifiers is written `Ctrl+A`, `Shift+Tab` or `Ctrl+Shift+Z` (`Ctrl`, `Alt`, `Shift`, `Meta`).
    pub async fn press(&self, keys: &str) -> Result<()> {
        let page = self.page();
        let root = page.find_element("html").await?;
        for key in keys.split_whitespace() {
            if !input::press_combo(&page, key).await? {
                root.press_key(key).await?;
            }
        }
        Ok(())
    }

    /// Moves the mouse over the target (ref, CSS selector or visible text) so hover styles and menus react.
    pub async fn hover(&self, target: &str) -> Result<()> {
        self.act_one("mark", target, "").await?;
        let found = self.page().find_element(MARKED_SELECTOR).await;
        let _ = self.eval_as::<bool>(UNMARK_JS).await;
        match found {
            Ok(el) => el.hover().await.map(drop).map_err(Error::from),
            Err(_) => Err(Error::NotFound(target.to_string())),
        }
    }

    /// Scrolls one screen `"up"` or `"down"`, to the `"top"` or `"bottom"`, or brings the target into view.
    pub async fn scroll(&self, target_or_dir: &str) -> Result<()> {
        self.act_one("scroll", target_or_dir, "").await
    }

    /// Waits for a target to appear or for a fixed time. Both are capped at [`WAIT_CAP_MS`]; a target that
    /// does not appear in time is [`Error::NotFound`].
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

    /// Closes the browser. An ephemeral profile is deleted; a persistent one is kept.
    pub async fn close(mut self) -> Result<()> {
        // A browser that hangs must not hang its caller: ask it to quit, and kill it when it does not.
        let closed = match tokio::time::timeout(platform::CLOSE_GRACE, self.chrome.close()).await {
            Ok(r) => r.map(drop).map_err(Error::from),
            Err(_) => {
                Err(Error::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "the browser did not quit when asked")))
            }
        };
        if tokio::time::timeout(platform::CLOSE_GRACE, self.chrome.wait()).await.is_err() {
            let _ = self.chrome.kill().await;
            let _ = tokio::time::timeout(platform::CLOSE_GRACE, self.chrome.wait()).await;
        }
        lock(&self.tabs).tasks.iter().for_each(JoinHandle::abort);
        drop(lock(&self.recorder).take());
        self.handler.abort();
        // Helper processes can outlive the main one and keep profile files open; end them, then delete the
        // profile, waiting out any lock that is still held.
        self.guard = None;
        if let Some(profile) = self.profile.take() {
            let _ = tokio::task::spawn_blocking(move || profile.remove()).await;
        }
        closed
    }

    /// Goes one page back in the history of the active tab.
    pub async fn back(&self) -> Result<()> {
        self.step_history("back", -1).await
    }

    /// Goes one page forward in the history of the active tab.
    pub async fn forward(&self) -> Result<()> {
        self.step_history("forward", 1).await
    }

    /// Reloads the active tab and waits for it to load.
    pub async fn reload(&self) -> Result<()> {
        self.page().reload().await?;
        Ok(())
    }

    /// Runs JavaScript in the page (promises are awaited) and returns the result as JSON text, `undefined` when
    /// there is none. A script that throws is an error.
    pub async fn eval(&self, js: &str) -> Result<String> {
        let params = EvaluateParams::builder().expression(js).build().map_err(Error::Config)?;
        let result = self.page().evaluate_expression(params).await?;
        Ok(result.value().map_or_else(|| "undefined".to_string(), |v| v.to_string()))
    }

    /// Console messages of every tab since launch (the last 1000), optionally only those whose text or level
    /// matches the regex.
    pub async fn console(&self, filter: Option<&str>) -> Result<Vec<ConsoleEntry>> {
        self.flush_events().await;
        self.buffers.console(filter)
    }

    /// Network requests of every tab since launch (the last 1000), optionally only those whose URL or method
    /// matches the regex.
    pub async fn network(&self, filter: Option<&str>) -> Result<Vec<NetEntry>> {
        self.flush_events().await;
        self.buffers.network(filter)
    }

    /// The open tabs in order, with the active one flagged. Tabs the page opened itself are included.
    pub async fn tabs(&self) -> Result<Vec<TabInfo>> {
        self.sync_tabs().await?;
        let (pages, current) = {
            let t = lock(&self.tabs);
            (t.pages.clone(), t.current)
        };
        let mut out = Vec::with_capacity(pages.len());
        for (index, page) in pages.iter().enumerate() {
            let title = page.get_title().await.ok().flatten().unwrap_or_default();
            let url = page.url().await.ok().flatten().unwrap_or_default();
            out.push(TabInfo { index, title, url, active: index == current });
        }
        Ok(out)
    }

    /// Opens a tab on this URL, makes it the active one and returns its index.
    pub async fn tab_new(&self, url: &str) -> Result<usize> {
        self.sync_tabs().await?;
        let page = self.chrome.new_page("about:blank").await?;
        let tasks = self.buffers.watch(&page).await?;
        let index = {
            let mut t = lock(&self.tabs);
            t.pages.push(page.clone());
            t.tasks.extend(tasks);
            t.current = t.pages.len() - 1;
            t.current
        };
        let target = platform::navigable(url);
        if target != "about:blank" {
            page.goto(target.as_ref()).await?;
        }
        Ok(index)
    }

    /// Makes the tab with this index (as listed by [`Browser::tabs`]) the one the other verbs act on.
    pub async fn tab_switch(&self, index: usize) -> Result<()> {
        self.sync_tabs().await?;
        let page = {
            let mut t = lock(&self.tabs);
            let page = t.pages.get(index).cloned().ok_or_else(|| no_tab(index))?;
            t.current = index;
            page
        };
        page.bring_to_front().await?;
        Ok(())
    }

    /// Closes the tab with this index. The last tab cannot be closed; use [`Browser::close`].
    pub async fn tab_close(&self, index: usize) -> Result<()> {
        self.sync_tabs().await?;
        let (page, next) = {
            let mut t = lock(&self.tabs);
            if index >= t.pages.len() {
                return Err(no_tab(index));
            }
            if t.pages.len() == 1 {
                return Err(Error::Unusable { target: index.to_string(), reason: "it is the last tab".to_string() });
            }
            let page = t.pages.remove(index);
            t.closed.push(page.target_id().clone());
            if index < t.current || t.current >= t.pages.len() {
                t.current = t.current.saturating_sub(1);
            }
            (page, t.active())
        };
        page.close().await?;
        next.bring_to_front().await?;
        Ok(())
    }

    /// Sets the file of an `<input type=file>` (ref, CSS selector or label), as if picked in the dialog.
    pub async fn upload(&self, target: &str, path: impl AsRef<Path>) -> Result<()> {
        let file = platform::existing_file(path.as_ref())?;
        self.act_one("mark", target, "").await?;
        let found = self.page().find_element(MARKED_SELECTOR).await;
        let is_file = self.eval_as::<bool>(MARKED_IS_FILE_JS).await.unwrap_or(false);
        let _ = self.eval_as::<bool>(UNMARK_JS).await;
        let el = found.map_err(|_| Error::NotFound(target.to_string()))?;
        if !is_file {
            return Err(Error::Unusable { target: target.to_string(), reason: "not a file input".to_string() });
        }
        let params = SetFileInputFilesParams::builder()
            .file(file.to_string_lossy())
            .backend_node_id(el.backend_node_id)
            .build()
            .map_err(Error::Config)?;
        self.page().execute(params).await?;
        Ok(())
    }

    /// Resizes the viewport of the active tab, in CSS pixels.
    pub async fn resize(&self, width: u32, height: u32) -> Result<()> {
        self.page().execute(SetDeviceMetricsOverrideParams::new(width, height, 1.0, false)).await?;
        Ok(())
    }

    /// Clicks at viewport coordinates, for canvases and anything without a DOM element to target.
    pub async fn click_xy(&self, x: f64, y: f64) -> Result<()> {
        self.page().click(Point::new(x, y)).await?;
        Ok(())
    }

    /// Types text into whatever has the focus, as the keyboard would.
    pub async fn type_text(&self, text: &str) -> Result<()> {
        self.page().execute(InsertTextParams::new(text)).await?;
        Ok(())
    }

    /// Moves `delta` entries along the history of the active tab and waits until that page has loaded.
    async fn step_history(&self, what: &str, delta: i64) -> Result<()> {
        let page = self.page();
        let history = page.execute(GetNavigationHistoryParams::default()).await?.result;
        let wanted = history.current_index + delta;
        let entry = usize::try_from(wanted).ok().and_then(|i| history.entries.get(i)).ok_or_else(|| {
            Error::Unusable { target: what.to_string(), reason: "no such page in the history".to_string() }
        })?;
        page.execute(NavigateToHistoryEntryParams::new(entry.id)).await?;
        let deadline = Instant::now() + Duration::from_millis(WAIT_CAP_MS);
        while Instant::now() < deadline {
            // The page is briefly detached while a navigation commits; keep polling through that.
            let at = page.execute(GetNavigationHistoryParams::default()).await.ok().map(|h| h.result.current_index);
            let ready =
                page.evaluate_expression("document.readyState").await.ok().and_then(|r| r.into_value::<String>().ok());
            if at == Some(wanted) && ready.as_deref() == Some("complete") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Ok(())
    }

    /// Reconciles the tab list with the browser: tabs the page opened are added, tabs that went away dropped.
    async fn sync_tabs(&self) -> Result<()> {
        let live = self.chrome.pages().await?;
        let fresh: Vec<Page> = {
            let t = lock(&self.tabs);
            let known = |p: &Page| t.pages.iter().any(|k| k.target_id() == p.target_id());
            live.iter().filter(|p| !known(p) && !t.closed.contains(p.target_id())).cloned().collect()
        };
        let mut tasks = Vec::new();
        let mut added = Vec::new();
        for page in fresh {
            if let Ok(t) = self.buffers.watch(&page).await {
                tasks.extend(t);
                added.push(page);
            }
        }
        let mut t = lock(&self.tabs);
        let active = t.active().target_id().clone();
        t.pages.extend(added);
        t.tasks.extend(tasks);
        let gone = |p: &Page| !live.iter().any(|l| l.target_id() == p.target_id());
        if t.pages.iter().any(|p| !gone(p)) {
            t.pages.retain(|p| !gone(p));
        }
        t.current = t.pages.iter().position(|p| p.target_id() == &active).unwrap_or(0);
        Ok(())
    }

    /// Lets events the browser already sent reach the buffers before they are read.
    async fn flush_events(&self) {
        let _ = self.page().evaluate_expression("0").await;
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    }

    /// Runs one verb of `act.js` over `(target, value)` pairs in a single page call.
    async fn act(&self, op: &str, items: &[(&str, &str)]) -> Result<Vec<Outcome>> {
        let js = format!("({ACT_JS})({}, {})", serde_json::to_string(op)?, serde_json::to_string(items)?);
        Ok(serde_json::from_str(&self.eval_as::<String>(&js).await?)?)
    }

    async fn act_one(&self, op: &str, target: &str, value: &str) -> Result<()> {
        let out = self.act(op, &[(target, value)]).await?.into_iter().next();
        match out {
            Some(Outcome { ok: true, .. }) => Ok(()),
            Some(Outcome { error: Some(reason), .. }) => Err(Error::Unusable { target: target.to_string(), reason }),
            _ => Err(Error::NotFound(target.to_string())),
        }
    }

    async fn eval_as<T: DeserializeOwned>(&self, js: &str) -> Result<T> {
        Ok(self.page().evaluate(js).await?.into_value()?)
    }

    /// The active tab.
    fn page(&self) -> Page {
        lock(&self.tabs).active()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn no_tab(index: usize) -> Error {
    Error::Unusable { target: index.to_string(), reason: "no tab with that index".to_string() }
}

fn is_ref(s: &str) -> bool {
    s.strip_prefix('e').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The page-side half of [`Browser::page_text`]: a function of an optional CSS selector.
const PAGE_TEXT_JS: &str = include_str!("page_text.js");

/// The page-side half of the interaction verbs: one function taking a verb and `[target, value]` pairs.
const ACT_JS: &str = include_str!("act.js");

#[derive(Debug, Deserialize)]
struct Outcome {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

const MARKED_SELECTOR: &str = "[data-nexo-click]";

/// Marks the best match for a text with `data-nexo-click` and says whether there was one.
const MARK_BY_TEXT_JS: &str = r#"(text) => {
  const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const want = norm(text);
  const sel = 'a, button, input, select, textarea, [role], [onclick], [contenteditable], label, summary';
  const label = (el) => norm(el.innerText || el.value || el.getAttribute('aria-label') || el.title);
  const shown = [...document.querySelectorAll(sel)].filter((el) => el.getClientRects().length > 0);
  const hit = shown.find((el) => label(el) === want) || shown.find((el) => label(el).includes(want));
  if (!hit) return false;
  hit.setAttribute('data-nexo-click', '1');
  return true;
}"#;

const MARKED_IS_FILE_JS: &str = "(() => { const el = document.querySelector('[data-nexo-click]'); return !!el && el.matches('input[type=file]'); })()";

const UNMARK_JS: &str = "(() => { document.querySelectorAll('[data-nexo-click]').forEach((e) => e.removeAttribute('data-nexo-click')); return true; })()";
