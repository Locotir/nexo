//! One browser, whichever way it was reached: a headless Chrome nexo launched itself (Backend A, [`Browser`]) or the
//! person's own logged-in Chrome through the extension (Backend B, [`RelayBrowser`]). The verbs are the same; the
//! doc of [`RelayBrowser`] lists what differs on purpose.

use crate::{
    browsers, ActionEvent, Browser, BrowserEntry, ConsoleEntry, Error, ExportOptions, FillReport, LaunchOptions,
    NetEntry, PageText, PageTree, ReadPageOptions, RecordInfo, RecordOptions, RecordSummary, RelayBrowser, Result,
    Snapshot, TabInfo, Wait,
};
use nexo_relay::Dirs;
use std::path::Path;

pub enum AnyBrowser {
    Headless(Box<Browser>),
    Real(RelayBrowser),
}

macro_rules! forward {
    ($($(#[$doc:meta])* $name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
        impl AnyBrowser {
            $(
                $(#[$doc])*
                pub async fn $name(&self $(, $arg: $ty)*) -> Result<$ret> {
                    match self {
                        AnyBrowser::Headless(b) => b.$name($($arg),*).await,
                        AnyBrowser::Real(b) => b.$name($($arg),*).await,
                    }
                }
            )*
        }
    };
}

forward! {
    navigate(url: &str) -> ();
    screenshot(full_page: bool) -> Vec<u8>;
    read_text(selector: Option<&str>) -> String;
    snapshot() -> Snapshot;
    read_page(opts: &ReadPageOptions) -> PageTree;
    page_text(selector: Option<&str>) -> PageText;
    click(target: &str) -> ();
    fill(target: &str, value: &str) -> ();
    fill_batch(pairs: &[(&str, &str)]) -> FillReport;
    select(target: &str, option: &str) -> ();
    set_checkbox(target: &str, checked: bool) -> ();
    press(keys: &str) -> ();
    hover(target: &str) -> ();
    scroll(target_or_dir: &str) -> ();
    wait(what: Wait<'_>) -> ();
    record_start(opts: RecordOptions) -> ();
    record_stop_gif(path: impl AsRef<Path>) -> RecordSummary;
    back() -> ();
    forward() -> ();
    reload() -> ();
    eval(js: &str) -> String;
    console(filter: Option<&str>) -> Vec<ConsoleEntry>;
    network(filter: Option<&str>) -> Vec<NetEntry>;
    tabs() -> Vec<TabInfo>;
    tab_new(url: &str) -> usize;
    tab_switch(index: usize) -> ();
    tab_close(index: usize) -> ();
    upload(target: &str, path: impl AsRef<Path>) -> ();
    resize(width: u32, height: u32) -> ();
    click_xy(x: f64, y: f64) -> ();
    type_text(text: &str) -> ();
}

/// Recording with marks on the actions is nexo's own browser's; the person's own browser records a plain GIF, and
/// `export_gif` stops and saves it.
impl AnyBrowser {
    pub fn recording(&self) -> bool {
        matches!(self, AnyBrowser::Headless(b) if b.recording())
    }

    pub fn record_note(&self, event: ActionEvent) {
        if let AnyBrowser::Headless(b) = self {
            b.record_note(event);
        }
    }

    pub async fn record_note_click(&self, verb: &str, target: &str) {
        if let AnyBrowser::Headless(b) = self {
            b.record_note_click(verb, target).await;
        }
    }

    pub fn record_info(&self) -> Option<RecordInfo> {
        match self {
            AnyBrowser::Headless(b) => b.record_info(),
            AnyBrowser::Real(_) => None,
        }
    }

    pub async fn record_stop(&self) -> Result<RecordInfo> {
        match self {
            AnyBrowser::Headless(b) => b.record_stop().await,
            AnyBrowser::Real(_) => Err(real_recording()),
        }
    }

    pub async fn record_clear(&self) -> Result<RecordInfo> {
        match self {
            AnyBrowser::Headless(b) => b.record_clear().await,
            AnyBrowser::Real(_) => Err(real_recording()),
        }
    }

    pub async fn record_export(&self, path: impl AsRef<Path>, opts: &ExportOptions) -> Result<RecordSummary> {
        match self {
            AnyBrowser::Headless(b) => b.record_export(path, opts).await,
            AnyBrowser::Real(r) => r.record_stop_gif(path).await,
        }
    }
}

fn real_recording() -> Error {
    Error::Unusable {
        target: "recording".into(),
        reason: "on the person's own browser a recording is stopped and saved by export_gif".into(),
    }
}

impl AnyBrowser {
    /// Launches a headless Chrome of nexo's own, with a throwaway profile unless the options say otherwise.
    pub async fn launch(opts: LaunchOptions) -> Result<AnyBrowser> {
        Browser::launch(opts).await.map(|b| AnyBrowser::Headless(Box::new(b)))
    }

    /// Connects to the person's own browser (see [`RelayBrowser::connect`]); waits for them to click Allow.
    pub async fn connect(dirs: &Dirs, id: Option<&str>, name: &str) -> Result<AnyBrowser> {
        RelayBrowser::connect(dirs, id, name).await.map(AnyBrowser::Real)
    }

    /// The browsers with the nexo extension running for the current user, and the ones that are there but did not
    /// answer (id and why). Nothing is asked or controlled by listing.
    pub async fn available() -> (Vec<BrowserEntry>, Vec<(String, String)>) {
        browsers(&Dirs::for_user()).await
    }

    /// [`AnyBrowser::connect`] for the current user's own folders: with an `id` that browser is asked, without one
    /// every running browser is asked and the first to be allowed is used.
    pub async fn connect_user(id: Option<&str>, name: &str) -> Result<AnyBrowser> {
        AnyBrowser::connect(&Dirs::for_user(), id, name).await
    }

    /// `headless` for a browser nexo launched, else the id the extension gave the person's browser (see
    /// [`AnyBrowser::available`]).
    pub fn id(&self) -> &str {
        match self {
            AnyBrowser::Headless(_) => "headless",
            AnyBrowser::Real(r) => &r.info().id,
        }
    }

    /// True when this is the person's own browser, not a headless one nexo launched.
    pub fn is_real(&self) -> bool {
        matches!(self, AnyBrowser::Real(_))
    }

    /// Short description for the model: which browser this is.
    pub fn describe(&self) -> String {
        match self {
            AnyBrowser::Headless(_) => "headless Chrome (nexo's own, throwaway profile)".to_string(),
            AnyBrowser::Real(r) => {
                let i = r.info();
                format!("your own {} ({}) via the nexo extension, id {}", i.name, i.os, i.id)
            }
        }
    }

    /// False once the browser went away: its window was closed, it crashed, or the connection ended.
    pub async fn is_alive(&self) -> bool {
        match self {
            AnyBrowser::Headless(b) => b.is_alive().await,
            AnyBrowser::Real(r) => r.is_alive(),
        }
    }

    /// Ends the browser: a headless one is closed and its profile deleted; a real one is released (the debugger
    /// detaches, the tabs nexo opened stay open for the person).
    pub async fn close(self) -> Result<()> {
        match self {
            AnyBrowser::Headless(b) => (*b).close().await,
            AnyBrowser::Real(r) => r.close().await,
        }
    }
}
