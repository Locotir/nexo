//! The files that running sub-agents are changing. A file one of them changes is closed to everyone else
//! (the other sub-agents and the main agent) until that sub-agent ends.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Path, then (the call that holds it, its label). Cloning shares the table.
#[derive(Clone, Default)]
pub struct Claims(Arc<Mutex<HashMap<PathBuf, (String, String)>>>);

impl std::fmt::Debug for Claims {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Claims({} held)", self.len())
    }
}

impl Claims {
    fn table(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, (String, String)>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Takes `key` for `owner`. Err(label of the holder) when another call holds it.
    pub fn hold(&self, key: PathBuf, owner: &str, label: &str) -> Result<(), String> {
        let mut held = self.table();
        match held.get(&key) {
            Some((o, l)) if o != owner => Err(l.clone()),
            Some(_) => Ok(()),
            None => {
                held.insert(key, (owner.to_string(), label.to_string()));
                Ok(())
            }
        }
    }

    /// The label of the sub-agent that holds `key`, if any.
    pub fn holder(&self, key: &Path) -> Option<String> {
        self.table().get(key).map(|(_, l)| l.clone())
    }

    /// Gives back everything `owner` holds.
    pub fn release(&self, owner: &str) {
        self.table().retain(|_, (o, _)| o != owner);
    }

    pub fn len(&self) -> usize {
        self.table().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The path as written, made absolute and without `.` and `..`, so two spellings of one file meet.
pub fn plain_path(p: &str, cwd: &Path) -> PathBuf {
    let path = if Path::new(p).is_absolute() { PathBuf::from(p) } else { cwd.join(p) };
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_file_is_closed_to_others_until_released() {
        let c = Claims::default();
        let key = plain_path("src/../a.rs", Path::new("/p"));
        assert_eq!(key, PathBuf::from("/p/a.rs"));
        assert!(c.hold(key.clone(), "t1", "first").is_ok());
        assert!(c.hold(key.clone(), "t1", "first").is_ok(), "the holder may go on");
        assert_eq!(c.hold(key.clone(), "t2", "second"), Err("first".to_string()));
        assert_eq!(c.holder(&key).as_deref(), Some("first"));
        c.release("t1");
        assert!(c.holder(&key).is_none());
        assert!(c.hold(key, "t2", "second").is_ok());
    }
}
