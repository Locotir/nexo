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
pub mod chrome_setup;
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

use args::Action;
mod args;
use dispatch::{read, Step};
mod dispatch;
mod gif;
mod launch;
mod show;
#[cfg(test)]
mod tests;
mod tool;
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
