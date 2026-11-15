//! Where the managed policy file is looked for.

use std::path::PathBuf;

/// The source named in `/status`, `nexo doctor` and the hook list for everything this file forces.
pub const SOURCE: &str = "managed";

/// Where the file lives on this system.
pub fn default_path() -> PathBuf {
    if cfg!(windows) {
        let base =
            std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        base.join("nexo").join("managed.toml")
    } else if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/nexo/managed.toml")
    } else {
        PathBuf::from("/etc/nexo/managed.toml")
    }
}

/// `NEXO_MANAGED_FILE` moves the file, in tests and debug builds only: in a release build the
/// variable is not read, so a user cannot point nexo at a file of their own.
#[cfg(any(test, debug_assertions))]
pub(super) fn override_path() -> Option<PathBuf> {
    std::env::var_os("NEXO_MANAGED_FILE").filter(|v| !v.is_empty()).map(PathBuf::from)
}

#[cfg(not(any(test, debug_assertions)))]
pub(super) fn override_path() -> Option<PathBuf> {
    None
}

/// The file nexo reads (whether or not it exists).
pub fn path() -> PathBuf {
    override_path().unwrap_or_else(default_path)
}
