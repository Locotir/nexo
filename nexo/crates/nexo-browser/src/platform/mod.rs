//! Everything that differs per OS, kept out of the browser code: where browsers are installed, how a launched
//! one is tied to nexo's lifetime, how its throwaway profile is deleted, and how paths reach it.

mod discover;
#[cfg(test)]
mod live;
mod paths;
mod process;
mod profile;
mod registry;

use std::time::Duration;

pub use discover::{find_browser, installed_browsers, override_problem, tried_names, InstalledBrowser, OVERRIDE_VAR};
#[cfg(test)]
pub(crate) use paths::file_url;
pub(crate) use paths::{existing_file, navigable};
pub(crate) use process::ProcessGuard;
pub(crate) use profile::TempProfile;

/// How long to wait for the browser to open its debugging port. A cold start on Windows (antivirus scanning a
/// fresh profile) or on macOS (Gatekeeper checking a browser that never ran) can take far longer than on Linux.
/// Same on every platform: four Chromes starting at once on a busy two-core Linux runner took more than 20 s.
pub const LAUNCH_TIMEOUT: Duration = Duration::from_secs(45);

/// How long `close` waits for the browser process to exit before killing it.
pub const CLOSE_GRACE: Duration = Duration::from_secs(5);
