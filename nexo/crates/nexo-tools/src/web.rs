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
const NO_RETRY: &str = "Web search is not available right now: do not retry it; go on with what you have, or open a URL you already know with web_fetch.";
const BREAKER_SAID: &str = "web_search: not available in this session (the backend refused 2 requests in a row). Do not call it again; go on with what you have, or open a URL you already know with web_fetch.";

/// Counts refused searches in a row. Open while `BREAKER_AFTER` refusals have come in a row and the
/// last one is younger than `open_for`. The first call after it closes is a probe: one more refusal
/// opens it again.
struct Breaker {
    /// (refusals in a row, clock reading at the last one)
    state: Mutex<(u32, Duration)>,
    /// Time since an arbitrary start; a test replaces it.
    now: Arc<dyn Fn() -> Duration + Send + Sync>,
    open_for: Duration,
}

impl Breaker {
    fn new() -> Breaker {
        let start = std::time::Instant::now();
        Breaker {
            state: Mutex::new((0, Duration::ZERO)),
            now: Arc::new(move || start.elapsed()),
            open_for: BREAKER_OPEN,
        }
    }

    fn is_open(&self) -> bool {
        let (n, at) = *self.state.lock().unwrap_or_else(|e| e.into_inner());
        n >= BREAKER_AFTER && (self.now)().saturating_sub(at) < self.open_for
    }

    fn refused(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.0 = s.0.saturating_add(1);
        s.1 = (self.now)();
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
