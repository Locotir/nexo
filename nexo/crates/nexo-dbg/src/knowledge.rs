//! What a debugging session learns about a binary and keeps for the next one: names and notes in the
//! per-binary file of nexo-bin (`<data dir>/bin/<sha256>.json`), the same ones `nexo bin` reads.
//!
//! The data dir is set once by whoever hosts the debugger. Without it nothing is kept, and saying so is
//! the answer to `name` and `note`.

use std::path::PathBuf;
use std::sync::Mutex;

static DATA_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Where names and notes are kept (the nexo data dir).
pub fn set_data_dir(dir: PathBuf) {
    if let Ok(mut d) = DATA_DIR.lock() {
        *d = Some(dir);
    }
}

pub fn data_dir() -> Option<PathBuf> {
    DATA_DIR.lock().ok().and_then(|d| d.clone())
}
