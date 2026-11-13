//! Scratch the tests of one test binary leave to the end of the process, and the sweep that removes it there.
//!
//! A `static OnceLock<TempDir>` is never dropped, so each run would leave its folder behind. The sweep runs
//! from `atexit`, which the test harness reaches when it finishes (`process::exit`).

use std::path::PathBuf;
use std::sync::{Mutex, Once};

static LEFT: Mutex<Vec<Box<dyn FnOnce() + Send>>> = Mutex::new(Vec::new());
static HOOK: Once = Once::new();

extern "C" {
    fn atexit(f: extern "C" fn()) -> i32;
}

extern "C" fn sweep() {
    let left: Vec<_> = LEFT.lock().map(|mut l| l.drain(..).collect()).unwrap_or_default();
    for f in left {
        f();
    }
}

/// Runs `f` when the process exits.
#[allow(dead_code)]
pub fn at_exit(f: impl FnOnce() + Send + 'static) {
    HOOK.call_once(|| {
        // SAFETY: `sweep` is a plain `extern "C"` function that outlives the process.
        unsafe { atexit(sweep) };
    });
    LEFT.lock().unwrap().push(Box::new(f));
}

/// The folder `dir` stands for, deleted at process exit instead of when `dir` is dropped.
#[allow(dead_code)]
pub fn until_exit(dir: tempfile::TempDir) -> PathBuf {
    let path = dir.keep();
    let gone = path.clone();
    at_exit(move || {
        let _ = std::fs::remove_dir_all(gone);
    });
    path
}
