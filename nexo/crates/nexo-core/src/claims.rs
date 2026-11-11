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

    /// Takes every key for `owner` or none: when one is held by another call, the ones this call took are
    /// given back (those `owner` held before stay). Err(index of the refused key, label of its holder).
    pub fn hold_all(&self, keys: &[PathBuf], owner: &str, label: &str) -> Result<(), (usize, String)> {
        let mut held = self.table();
        let mut taken: Vec<&PathBuf> = Vec::new();
        for (i, key) in keys.iter().enumerate() {
            match held.get(key) {
                Some((o, l)) if o != owner => {
                    let refused = (i, l.clone());
                    for k in taken {
                        held.remove(k);
                    }
                    return Err(refused);
                }
                Some(_) => {}
                None => {
                    held.insert(key.clone(), (owner.to_string(), label.to_string()));
                    taken.push(key);
                }
            }
        }
        Ok(())
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

    #[test]
    fn a_call_of_several_files_takes_all_or_none() {
        let c = Claims::default();
        let (a, b, d) = (PathBuf::from("/p/a"), PathBuf::from("/p/b"), PathBuf::from("/p/d"));
        assert!(c.hold(b.clone(), "t1", "first").is_ok());
        // t2 already held `a`; asks for a, b, d: b is refused, d is never taken, a stays t2's
        assert!(c.hold(a.clone(), "t2", "second").is_ok());
        assert_eq!(c.hold_all(&[d.clone(), a.clone(), b.clone()], "t2", "second"), Err((2, "first".to_string())));
        assert!(c.holder(&d).is_none(), "what the refused call took is given back");
        assert_eq!(c.holder(&a).as_deref(), Some("second"), "what it held before stays");
        assert!(c.hold_all(&[a, d.clone()], "t2", "second").is_ok());
        assert_eq!(c.holder(&d).as_deref(), Some("second"));
    }
}
