//! The files the model has seen in this session, as they were when it saw them. `write` replaces
//! a whole file, so it does not replace one the model has not read, or one that changed since
//! (the person's editor, a formatter, a command): what is there would be lost unseen.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Size and modification time: what tells a file changed.
type Stamp = (u64, Option<SystemTime>);

/// The files of a session. Cloning shares them.
#[derive(Clone, Default)]
pub struct Seen(Arc<Mutex<HashMap<PathBuf, Stamp>>>);

fn key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    m.is_file().then(|| (m.len(), m.modified().ok()))
}

impl Seen {
    /// The model now knows `path` as it is: it read it, or wrote or edited it.
    pub fn mark(&self, path: &Path) {
        if let Some(s) = stamp(path) {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).insert(key(path), s);
        }
    }

    /// Why replacing `path` would lose content the model has not seen; None when nothing would be
    /// lost (no such file, an empty one, or one unchanged since the model saw it).
    pub fn unseen(&self, path: &Path) -> Option<&'static str> {
        let now = stamp(path)?;
        if now.0 == 0 {
            return None;
        }
        match self.0.lock().unwrap_or_else(|e| e.into_inner()).get(&key(path)) {
            None => Some("you have not read it in this session"),
            Some(s) if *s != now => Some("it changed since you last read or wrote it"),
            Some(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_safe_to_replace_only_as_it_was_seen() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.txt");
        let seen = Seen::default();
        assert_eq!(seen.unseen(&f), None, "a new file loses nothing");
        std::fs::write(&f, "").unwrap();
        assert_eq!(seen.unseen(&f), None, "an empty file loses nothing");
        std::fs::write(&f, "kept").unwrap();
        assert!(seen.unseen(&f).unwrap().contains("not read"));
        seen.clone().mark(&f);
        assert_eq!(seen.unseen(&f), None);
        std::fs::write(&f, "kept, and more").unwrap();
        assert!(seen.unseen(&f).unwrap().contains("changed"));
        // the same file by another spelling
        seen.mark(&f);
        assert_eq!(seen.unseen(&d.path().join(".").join("a.txt")), None);
    }
}
