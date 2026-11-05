//! The version shown to people. The binary sets it at start with the commit it was built from, so two
//! releases read apart; a library or a test that does not set it shows the package version.

use std::sync::OnceLock;

static SHOWN: OnceLock<&'static str> = OnceLock::new();

/// Sets the version shown (once; later calls are ignored).
pub fn set(version: &'static str) {
    let _ = SHOWN.set(version);
}

/// The version shown: what [`set`] gave, else the package version.
pub fn get() -> &'static str {
    SHOWN.get().copied().unwrap_or(env!("CARGO_PKG_VERSION"))
}
