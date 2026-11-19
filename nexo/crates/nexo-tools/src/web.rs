//! `web_search` and `web_fetch`: the web, for what the repository does not hold.
//!
//! `web_search` asks one of several backends and answers with a numbered list
//! of titles, URLs and snippets. With `search = "auto"` (the default) it takes
//! the first backend whose key is present (Tavily, Brave, Exa), else a SearXNG
//! instance when `url` is set, else DuckDuckGo, which needs no key; when a
//! keyed backend fails it falls back to DuckDuckGo and says so.
//!
//! `web_fetch` reads one URL: HTML becomes readable Markdown (title first,
//! scripts, styles, navigation and forms dropped, links made absolute); text,
//! Markdown, JSON and XML come back as they are; anything else is refused. Long
//! pages come in pieces: the output ends with the `start` of the next one.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use nexo_core::config::WebConfig;
use nexo_core::tool::Check;
use nexo_core::{Compact, Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use regex::Regex;
use reqwest::Url;
use serde_json::{json, Value};

use crate::util::{int_arg, str_arg};

const DEFAULT_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) nexo/0.1";
const DDG_BASE: &str = "https://html.duckduckgo.com";
const MAX_BODY: usize = 5 * 1024 * 1024;
const DEFAULT_RESULTS: u64 = 8;
const MAX_RESULTS: u64 = 20;
const DEFAULT_CHARS: u64 = 20_000;
const MAX_CHARS: u64 = 100_000;

/// Where keys are read from: the process environment, or a fixed map in tests.
type Env = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Why a search backend failed. `refused` is set when the backend itself said no (a block, a rate
/// limit, a bad key, a server error, no connection, a timeout): asking again will not help, and the
/// query is not at fault.
struct Fail {
    text: String,
    refused: bool,
}

impl Fail {
    fn refused(text: String) -> Fail {
        Fail { text, refused: true }
    }
}

impl From<String> for Fail {
    fn from(text: String) -> Fail {
        Fail { text, refused: false }
    }
}

/// Refusals in a row that open the breaker, and how long it stays open after the last one.
const BREAKER_AFTER: u32 = 2;
const BREAKER_OPEN: Duration = Duration::from_secs(120);

/// What the model is told when the backend refused this call: the refusals in a row, and what the breaker does next.
fn refusal_said(e: &str, n: u32, open_for: Duration) -> String {
    let secs = open_for.as_secs();
    let e = e.trim_end_matches('.');
    if n < BREAKER_AFTER {
        format!(
            "web_search: {e}. Refusal {n} in a row; at {BREAKER_AFTER} in a row web_search stops asking the backend for {secs} s. web_fetch still opens a URL you have."
        )
    } else {
        format!(
            "web_search: {e}. Refusal {n} in a row; web_search will not ask the backend for the next {secs} s, then asks it again. web_fetch still opens a URL you have."
        )
    }
}

/// What the model is told when the breaker is open: the count, the last refusal, and when the backend is asked again.
fn breaker_said(n: u32, last: &str, left_secs: u64) -> String {
    format!(
        "web_search: not asked: the backend refused {n} requests in a row (last: {last}); calls in the next {left_secs} s get this answer without asking it, and the first call after that asks it again. web_fetch still opens a URL you have."
    )
}

/// Counts refused searches in a row. Open while `BREAKER_AFTER` refusals have come in a row and the
/// last one is younger than `open_for`. The first call after it closes is a probe: one more refusal
/// opens it again.
struct Breaker {
    /// (refusals in a row, clock reading at the last one, the text of the last one)
    state: Mutex<(u32, Duration, String)>,
    /// Time since an arbitrary start; a test replaces it.
    now: Arc<dyn Fn() -> Duration + Send + Sync>,
    open_for: Duration,
}

impl Breaker {
    fn new() -> Breaker {
        let start = std::time::Instant::now();
        Breaker {
            state: Mutex::new((0, Duration::ZERO, String::new())),
            now: Arc::new(move || start.elapsed()),
            open_for: BREAKER_OPEN,
        }
    }

    /// When open: the refusals in a row, the last refusal's text, and the whole seconds left (rounded up).
    fn is_open(&self) -> Option<(u32, String, u64)> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let age = (self.now)().saturating_sub(s.1);
        if s.0 < BREAKER_AFTER || age >= self.open_for {
            return None;
        }
        let left = self.open_for - age;
        Some((s.0, s.2.clone(), left.as_secs() + u64::from(left.subsec_nanos() > 0)))
    }

    /// Records a refusal and returns how many are in a row now.
    fn refused(&self, text: &str) -> u32 {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.0 = s.0.saturating_add(1);
        s.1 = (self.now)();
        s.2 = crate::util::one_line(text.trim_end_matches('.'), 200);
        s.0
    }

    fn reset(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).0 = 0;
    }
}

/// The two web tools, or none when `[web] enabled = false`.
pub fn tools(cfg: &WebConfig) -> Vec<Arc<dyn Tool>> {
    if !cfg.enabled {
        return Vec::new();
    }
    vec![Arc::new(WebSearch::new(cfg)), Arc::new(WebFetch::new(cfg))]
}

/// Keys in a map, for tests.
#[cfg(test)]
fn env_of(pairs: &[(&str, &str)]) -> Env {
    let m: std::collections::BTreeMap<String, String> =
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    Arc::new(move |k: &str| m.get(k).cloned())
}

#[cfg(test)]
mod tests;

mod net_guard;
use net_guard::*;
mod search_backends;
pub use search_backends::*;
mod search_parse;
pub use search_parse::*;
mod fetch;
mod search_tool;
pub use fetch::*;
