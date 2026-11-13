//! The one error type of the crate.

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No Chrome, Chromium or Edge could be found. The text is read by the model, so it names what the person can do;
    /// `note` says when `NEXO_BROWSER` was set but unusable (empty or starting with `; `).
    #[error("no browser found (tried {tried}{note}); ask the person to install Google Chrome or Edge, or to set NEXO_BROWSER to a browser executable")]
    NoBrowser { tried: String, note: String },
    #[error("browser executable not found at {0}")]
    BadExecutable(PathBuf),
    #[error("could not configure the browser: {0}")]
    Config(String),
    /// Another Chrome (or another nexo session) holds the kept profile; two browsers cannot share one profile folder.
    #[error(
        "the browser profile {} is already in use{} by another Chrome; close that browser (or the other nexo \
         session using it), or open the browser with an ephemeral profile instead{}",
        .dir.display(),
        .pid.map(|p| format!(" (process {p})")).unwrap_or_default(),
        .pid.map_or(if cfg!(windows) {
            " (if none is running, delete the lockfile file inside the profile folder)"
        } else {
            " (if none is running, delete the SingletonLock file inside the profile folder)"
        }, |_| "")
    )]
    ProfileInUse { dir: PathBuf, pid: Option<u32> },
    /// A visible window was asked for where there is no screen (no `DISPLAY` or `WAYLAND_DISPLAY`).
    #[error("no screen to show a browser window on (no DISPLAY or WAYLAND_DISPLAY); open it headless instead")]
    NoDisplay,
    #[error("browser protocol error: {0}")]
    Cdp(#[from] chromiumoxide::error::CdpError),
    #[error("unexpected reply from the page: {0}")]
    Reply(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not encode the recording: {0}")]
    Encode(String),
    /// A ref, selector or text that matches nothing on the current page.
    #[error("nothing on the page matches {0:?}")]
    NotFound(String),
    /// The target exists but cannot take this action (an option that is not there, a field that is not a select).
    #[error("cannot act on {target:?}: {reason}")]
    Unusable { target: String, reason: String },
    /// The connection to the real browser (see [`crate::RelayBrowser`]) failed, was refused or closed.
    #[error("{0}")]
    Relay(#[from] nexo_relay::client::Error),
    /// A script run in the page threw, or the browser refused a protocol command.
    #[error("the page reported: {0}")]
    Page(String),
    /// No browser with the extension is running, or none answered.
    #[error("{0}")]
    NotConnected(String),
    /// Something the headless browser does that the person's own browser, reached through the extension, cannot.
    #[error("{what} is not available in your own browser: {why}")]
    Unsupported { what: String, why: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_no_browser_message_tells_the_reader_what_to_ask_for() {
        let plain = Error::NoBrowser { tried: "Google Chrome, Chromium".into(), note: String::new() }.to_string();
        assert!(plain.contains("tried Google Chrome, Chromium)"), "{plain}");
        assert!(plain.contains("ask the person to install Google Chrome or Edge, or to set NEXO_BROWSER"), "{plain}");
        assert!(!plain.contains("explicit path"), "{plain}");
        let bad = Error::NoBrowser { tried: "Google Chrome".into(), note: "; NEXO_BROWSER=/x is not a file".into() };
        assert!(bad.to_string().contains("(tried Google Chrome; NEXO_BROWSER=/x is not a file)"), "{bad}");
    }
}
