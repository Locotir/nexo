//! The throwaway profile directory of an ephemeral browser. On Windows the browser's helper processes keep files
//! in it open for a moment after the browser closes, so deleting it once is not enough.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Waits between attempts to delete the directory, about five seconds in all.
const RETRY_DELAYS_MS: [u64; 8] = [25, 50, 100, 200, 400, 800, 1_500, 2_000];

/// Waits of the background retry that takes over when [`RETRY_DELAYS_MS`] was not enough: about a minute more.
const LATE_RETRIES: usize = 30;
const LATE_DELAY: Duration = Duration::from_secs(2);

/// A temporary `--user-data-dir`, deleted by [`TempProfile::remove`] or, failing that, on drop.
#[derive(Debug)]
pub struct TempProfile {
    path: Option<PathBuf>,
}

impl TempProfile {
    pub fn create() -> io::Result<TempProfile> {
        let path = tempfile::Builder::new().prefix("nexo-browser-").tempdir()?.keep();
        Ok(TempProfile { path: Some(path) })
    }

    pub fn path(&self) -> &Path {
        self.path.as_deref().unwrap_or(Path::new(""))
    }

    /// Deletes the directory, waiting for the browser's processes to let go of their files. Blocks for up to
    /// about five seconds; call it from a blocking thread. A directory that is still locked after that is handed to
    /// a background thread that keeps trying for about a minute, so it is not left behind silently; the error says
    /// which path it is.
    pub fn remove(mut self) -> io::Result<()> {
        let Some(path) = self.path.take() else { return Ok(()) };
        let first = remove_dir_retrying(&path, &delays(), |p| std::fs::remove_dir_all(p));
        if let Err(e) = first {
            retry_later(path.clone());
            return Err(io::Error::new(e.kind(), format!("{} is still in use: {e}", path.display())));
        }
        Ok(())
    }
}

impl Drop for TempProfile {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else { return };
        if std::fs::remove_dir_all(&path).is_ok() || !path.exists() {
            return;
        }
        // Still locked: keep trying off this thread so dropping never waits.
        retry_later(path);
    }
}

/// Keeps trying to delete `path` on a thread of its own, so nothing waits for it.
fn retry_later(path: PathBuf) {
    let _ = std::thread::Builder::new().name("nexo-browser-profile-cleanup".into()).spawn(move || {
        let late = vec![LATE_DELAY; LATE_RETRIES];
        let _ = remove_dir_retrying(&path, &late, |p| std::fs::remove_dir_all(p));
    });
}

fn delays() -> Vec<Duration> {
    RETRY_DELAYS_MS.iter().map(|ms| Duration::from_millis(*ms)).collect()
}

/// Calls `remove` until it succeeds or the directory is gone, sleeping `delays[i]` before retry `i`. The error
/// is the last one seen.
fn remove_dir_retrying(
    path: &Path,
    delays: &[Duration],
    mut remove: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let mut last = match remove(path) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    for pause in delays {
        if last.kind() == io::ErrorKind::NotFound || !path.exists() {
            return Ok(());
        }
        std::thread::sleep(*pause);
        match remove(path) {
            Ok(()) => return Ok(()),
            Err(e) => last = e,
        }
    }
    if last.kind() == io::ErrorKind::NotFound || !path.exists() {
        Ok(())
    } else {
        Err(last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn quick() -> Vec<Duration> {
        vec![Duration::from_millis(1); 4]
    }

    #[test]
    fn retries_while_the_directory_is_locked() {
        let dir = tempfile::tempdir().unwrap();
        let tries = Cell::new(0);
        let got = remove_dir_retrying(dir.path(), &quick(), |p| {
            tries.set(tries.get() + 1);
            if tries.get() < 3 {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "in use"))
            } else {
                std::fs::remove_dir_all(p)
            }
        });
        assert!(got.is_ok());
        assert_eq!(tries.get(), 3);
        assert!(!dir.path().exists());
    }

    #[test]
    fn gives_up_with_the_last_error() {
        let dir = tempfile::tempdir().unwrap();
        let tries = Cell::new(0);
        let got = remove_dir_retrying(dir.path(), &quick(), |_| {
            tries.set(tries.get() + 1);
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "in use"))
        });
        assert_eq!(got.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(tries.get(), 5, "one try plus one per delay");
        assert!(dir.path().exists());
    }

    #[test]
    fn a_directory_that_is_already_gone_is_success() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("x");
        let got = remove_dir_retrying(&gone, &quick(), |_| Err(io::Error::new(io::ErrorKind::NotFound, "gone")));
        assert!(got.is_ok());
    }

    #[test]
    fn a_profile_is_created_and_removed() {
        let p = TempProfile::create().unwrap();
        let path = p.path().to_path_buf();
        assert!(path.is_dir());
        assert!(path.file_name().unwrap().to_string_lossy().starts_with("nexo-browser-"));
        std::fs::write(path.join("Local State"), "{}").unwrap();
        p.remove().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn dropping_a_profile_removes_it() {
        let p = TempProfile::create().unwrap();
        let path = p.path().to_path_buf();
        drop(p);
        assert!(!path.exists());
    }
}
