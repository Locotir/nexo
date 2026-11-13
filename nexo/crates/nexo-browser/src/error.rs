//! The one error type of the crate.

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No Chrome, Chromium or Edge could be found; the message says what to install.
    #[error("no browser found (tried {tried}); install Google Chrome or Chromium, or pass an explicit path")]
    NoBrowser { tried: String },
    #[error("browser executable not found at {0}")]
    BadExecutable(PathBuf),
    #[error("could not configure the browser: {0}")]
    Config(String),
    #[error("browser protocol error: {0}")]
    Cdp(#[from] chromiumoxide::error::CdpError),
    #[error("unexpected reply from the page: {0}")]
    Reply(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// A ref, selector or text that matches nothing on the current page.
    #[error("nothing on the page matches {0:?}")]
    NotFound(String),
    /// The target exists but cannot take this action (an option that is not there, a field that is not a select).
    #[error("cannot act on {target:?}: {reason}")]
    Unusable { target: String, reason: String },
}
