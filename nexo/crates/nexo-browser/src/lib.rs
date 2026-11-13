//! Native browser control for nexo: launches the installed Chrome, Chromium or Edge, drives it over the
//! DevTools Protocol and shows the page as a snapshot of actionable elements with stable refs.

mod error;
mod snapshot;
#[cfg(test)]
mod tests;

pub use error::{Error, Result};
pub use snapshot::{Element, Snapshot};

use chromiumoxide::browser::{Browser as Chrome, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::{Page, ScreenshotParams};
use futures::StreamExt;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::task::JoinHandle;

/// Browser binaries tried, in this order, along `PATH`.
const CANDIDATES: [&str; 5] =
    ["google-chrome-stable", "google-chrome", "chromium", "chromium-browser", "microsoft-edge"];

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
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self { executable: None, headless: true, profile: Profile::Ephemeral }
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

/// The first installed browser found on `PATH`, if any.
pub fn find_browser() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    CANDIDATES.iter().find_map(|name| {
        let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        dirs.iter().map(|d| d.join(&file)).find(|p| p.is_file())
    })
}

/// A running browser with one page. An ephemeral profile is deleted on [`Browser::close`]; a persistent one stays.
pub struct Browser {
    chrome: Chrome,
    page: Page,
    handler: JoinHandle<()>,
    _profile: Option<TempDir>,
}

impl Browser {
    pub async fn launch(opts: LaunchOptions) -> Result<Browser> {
        let exe = match opts.executable {
            Some(p) if Path::new(&p).is_file() => p,
            Some(p) => return Err(Error::BadExecutable(p)),
            None => find_browser().ok_or_else(|| Error::NoBrowser { tried: CANDIDATES.join(", ") })?,
        };
        let (profile, dir) = match opts.profile {
            Profile::Ephemeral => {
                let tmp = tempfile::Builder::new().prefix("nexo-browser-").tempdir()?;
                let dir = tmp.path().to_path_buf();
                (Some(tmp), dir)
            }
            Profile::Persistent(dir) => {
                std::fs::create_dir_all(&dir)?;
                (None, dir)
            }
        };
        let mut cfg = BrowserConfig::builder().chrome_executable(exe).user_data_dir(dir).no_sandbox();
        cfg = if opts.headless { cfg.new_headless_mode() } else { cfg.with_head() };
        let cfg = cfg.build().map_err(Error::Config)?;
        let (chrome, mut events) = Chrome::launch(cfg).await?;
        // The event stream must be polled before the first command or every command hangs.
        let handler = tokio::spawn(async move { while events.next().await.is_some() {} });
        match chrome.new_page("about:blank").await {
            Ok(page) => Ok(Browser { chrome, page, handler, _profile: profile }),
            Err(e) => {
                handler.abort();
                Err(e.into())
            }
        }
    }

    pub async fn navigate(&self, url: &str) -> Result<()> {
        self.page.goto(url).await?;
        Ok(())
    }

    /// PNG bytes of the viewport, or of the whole page.
    pub async fn screenshot(&self, full_page: bool) -> Result<Vec<u8>> {
        let params = ScreenshotParams::builder().format(CaptureScreenshotFormat::Png).full_page(full_page).build();
        Ok(self.page.screenshot(params).await?)
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
        // Always a string: a bare null does not cross the protocol reliably, and the text may be empty.
        let js = format!(
            "(() => {{ const el = document.querySelector({sel}); \
             return JSON.stringify({{ found: !!el, text: el ? (el.innerText ?? el.textContent ?? '') : '' }}); }})()"
        );
        let reply: Reply = serde_json::from_str(&self.eval::<String>(&js).await?)?;
        if reply.found {
            Ok(reply.text)
        } else {
            Err(Error::NotFound(selector.to_string()))
        }
    }

    /// The actionable elements of the page, each carrying a ref that [`Browser::click`] accepts.
    pub async fn snapshot(&self) -> Result<Snapshot> {
        let json = self.eval::<String>(snapshot::SNAPSHOT_JS).await?;
        Ok(Snapshot { elements: serde_json::from_str(&json)? })
    }

    /// Clicks the element with this ref (`e3`), or else the first visible element whose text equals the
    /// target, falling back to the first that contains it.
    pub async fn click(&self, target: &str) -> Result<()> {
        let selector = if is_ref(target) {
            format!("[data-nexo-ref=\"{target}\"]")
        } else {
            let text = serde_json::to_string(target)?;
            let js = format!("({MARK_BY_TEXT_JS})({text})");
            if !self.eval::<bool>(&js).await? {
                return Err(Error::NotFound(target.to_string()));
            }
            MARKED_SELECTOR.to_string()
        };
        let found = self.page.find_element(selector).await;
        let result = match found {
            Ok(el) => el.click().await.map(drop).map_err(Error::from),
            Err(_) => Err(Error::NotFound(target.to_string())),
        };
        if !is_ref(target) {
            // The mark is only a handle for this click; the page may already be gone.
            let _ = self.eval::<bool>(UNMARK_JS).await;
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

    /// Presses keys on whatever has the focus, e.g. `"Enter"` or `"Tab Tab Enter"` (space separated).
    pub async fn press(&self, keys: &str) -> Result<()> {
        let root = self.page.find_element("html").await?;
        for key in keys.split_whitespace() {
            root.press_key(key).await?;
        }
        Ok(())
    }

    /// Moves the mouse over the target (ref, CSS selector or visible text) so hover styles and menus react.
    pub async fn hover(&self, target: &str) -> Result<()> {
        self.act_one("mark", target, "").await?;
        let found = self.page.find_element(MARKED_SELECTOR).await;
        let _ = self.eval::<bool>(UNMARK_JS).await;
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
        let closed = self.chrome.close().await;
        let _ = self.chrome.wait().await;
        self.handler.abort();
        closed?;
        Ok(())
    }

    /// Runs one verb of `act.js` over `(target, value)` pairs in a single page call.
    async fn act(&self, op: &str, items: &[(&str, &str)]) -> Result<Vec<Outcome>> {
        let js = format!("({ACT_JS})({}, {})", serde_json::to_string(op)?, serde_json::to_string(items)?);
        Ok(serde_json::from_str(&self.eval::<String>(&js).await?)?)
    }

    async fn act_one(&self, op: &str, target: &str, value: &str) -> Result<()> {
        let out = self.act(op, &[(target, value)]).await?.into_iter().next();
        match out {
            Some(Outcome { ok: true, .. }) => Ok(()),
            Some(Outcome { error: Some(reason), .. }) => Err(Error::Unusable { target: target.to_string(), reason }),
            _ => Err(Error::NotFound(target.to_string())),
        }
    }

    async fn eval<T: DeserializeOwned>(&self, js: &str) -> Result<T> {
        Ok(self.page.evaluate(js).await?.into_value()?)
    }
}

fn is_ref(s: &str) -> bool {
    s.strip_prefix('e').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

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

const UNMARK_JS: &str = "(() => { document.querySelectorAll('[data-nexo-click]').forEach((e) => e.removeAttribute('data-nexo-click')); return true; })()";
