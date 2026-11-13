//! The open tabs of the browser; the active one is where every other verb acts.

use chromiumoxide::cdp::browser_protocol::target::TargetId;
use chromiumoxide::page::Page;
use serde::Serialize;
use tokio::task::JoinHandle;

/// One open tab, as listed by [`crate::Browser::tabs`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TabInfo {
    pub index: usize,
    pub title: String,
    pub url: String,
    /// True for the tab the other verbs act on.
    pub active: bool,
}

/// Never empty: a browser always keeps at least one tab.
pub(crate) struct Tabs {
    pub pages: Vec<Page>,
    pub current: usize,
    /// Event watchers of the tabs, aborted on close.
    pub tasks: Vec<JoinHandle<()>>,
    /// Tabs closed through [`crate::Browser::tab_close`], so a lagging target list does not bring them back.
    pub closed: Vec<TargetId>,
}

impl Tabs {
    pub fn new(first: Page, tasks: Vec<JoinHandle<()>>) -> Self {
        Self { pages: vec![first], current: 0, tasks, closed: Vec::new() }
    }

    pub fn active(&self) -> Page {
        self.pages[self.current].clone()
    }
}
