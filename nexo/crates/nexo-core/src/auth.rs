//! Credentials kept by `nexo auth login`: `<data dir>/auth.json`, readable
//! only by you. One entry per provider name, either a plain key or the
//! tokens of a subscription login (OAuth), which the providers crate
//! refreshes when they expire.
//!
//! ```json
//! { "deepseek": { "type": "api", "key": "sk-..." },
//!   "copilot":  { "type": "oauth", "access": "...", "refresh": "...", "expires": 0,
//!                 "metadata": { "flow": "copilot", "base_url": "https://api.githubcopilot.com" } } }
//! ```
//!
//! A key in the environment, `api_key_cmd` or `api_key_file` always wins over
//! this file: the store is the last place a key is looked for.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Credential {
    /// An API key.
    Api { key: String },
    /// A subscription login: an access token, the refresh token, when the
    /// access token expires (unix seconds; 0: it does not), and what the
    /// login learned (`flow`, `base_url`, `account_id`, `client_id`, ...).
    Oauth {
        access: String,
        #[serde(default)]
        refresh: String,
        #[serde(default)]
        expires: u64,
        #[serde(default)]
        metadata: BTreeMap<String, String>,
    },
}

impl Credential {
    /// The token sent to the provider.
    pub fn secret(&self) -> &str {
        match self {
            Credential::Api { key } => key,
            Credential::Oauth { access, .. } => access,
        }
    }

    /// True when an OAuth access token expires within `margin` seconds of `now`.
    pub fn expiring(&self, now: u64, margin: u64) -> bool {
        match self {
            Credential::Oauth { expires, .. } => *expires != 0 && *expires <= now + margin,
            Credential::Api { .. } => false,
        }
    }

    pub fn metadata(&self, key: &str) -> Option<&str> {
        match self {
            Credential::Oauth { metadata, .. } => metadata.get(key).map(String::as_str),
            Credential::Api { .. } => None,
        }
    }

    /// `api key` or `<flow> login`, for listings.
    pub fn kind(&self) -> String {
        match self {
            Credential::Api { .. } => "api key".into(),
            Credential::Oauth { metadata, .. } => {
                format!("{} login", metadata.get("flow").map(String::as_str).unwrap_or("oauth"))
            }
        }
    }
}

/// The store's file: `$NEXO_AUTH_FILE`, else `<data dir>/auth.json`.
pub fn file() -> PathBuf {
    std::env::var_os("NEXO_AUTH_FILE").map(PathBuf::from).unwrap_or_else(|| paths::data_dir().join("auth.json"))
}

/// Every entry; an absent or unreadable file is an empty store.
pub fn all() -> BTreeMap<String, Credential> {
    read_from(&file())
}

pub fn get(name: &str) -> Option<Credential> {
    all().remove(name)
}

pub fn set(name: &str, cred: Credential) -> std::io::Result<()> {
    set_in(&file(), name, cred)
}

/// Sets the entry `name` of the store at `f`. Only that entry changes: the others, those of a kind this
/// build does not know included, are written back as they were.
pub fn set_in(f: &Path, name: &str, cred: Credential) -> std::io::Result<()> {
    lock_in(f)?.set(name, cred)
}

/// Removes an entry; true when there was one.
pub fn remove(name: &str) -> std::io::Result<bool> {
    remove_in(&file(), name)
}

pub fn remove_in(f: &Path, name: &str) -> std::io::Result<bool> {
    let _lock = Lock::take(f)?;
    let mut map = read_raw(f)?;
    let had = map.remove(name).is_some();
    if had {
        write_to(f, &map)?;
    }
    Ok(had)
}

/// The store as it is on disk, entry by entry. An absent or empty file is an empty store; a file that
/// does not parse is an error, so nothing is written over the entries it holds.
fn read_raw(path: &Path) -> std::io::Result<BTreeMap<String, serde_json::Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e),
    };
    if text.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    serde_json::from_str(&text).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} cannot be read ({e}); fix or remove it, nothing was written to it", path.display()),
        )
    })
}

/// The store, locked: while this is held no other nexo reads-and-writes it, so a session can read an
/// entry, renew its tokens and write them back without another session doing the same at once.
pub struct Locked {
    _lock: Lock,
    path: PathBuf,
}

impl Locked {
    /// The entry as it is on disk now (another process may have changed it before the lock was taken).
    pub fn get(&self, name: &str) -> Option<Credential> {
        read_from(&self.path).remove(name)
    }

    /// Sets the entry `name`; the others, those of a kind this build does not know included, stay.
    pub fn set(&self, name: &str, cred: Credential) -> std::io::Result<()> {
        let mut map = read_raw(&self.path)?;
        map.insert(name.to_string(), serde_json::to_value(cred).map_err(std::io::Error::other)?);
        write_to(&self.path, &map)
    }
}

/// Locks the store (blocking, for up to a minute while another nexo holds it).
pub fn lock() -> std::io::Result<Locked> {
    lock_in(&file())
}

pub fn lock_in(f: &Path) -> std::io::Result<Locked> {
    Ok(Locked { _lock: Lock::take(f)?, path: f.to_path_buf() })
}

/// An OS lock on `<store>.lock`, held while one writer reads, changes and writes the store (and, for a
/// login renewal, while it asks for the new tokens), so two sessions do not drop each other's entries
/// or renew the same login twice. The system lets it go when the holder exits, even by a crash, so a
/// lock file left behind never blocks anyone; the file itself stays.
struct Lock(#[allow(dead_code)] std::fs::File);

impl Lock {
    /// Longest wait for another holder before giving up with an error.
    const WAIT: std::time::Duration = std::time::Duration::from_secs(60);

    fn take(store: &Path) -> std::io::Result<Lock> {
        Lock::take_within(store, Lock::WAIT)
    }

    fn take_within(store: &Path, wait: std::time::Duration) -> std::io::Result<Lock> {
        let path = store.with_extension("json.lock");
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            o.share_mode(0); // the open itself is the lock: a second one fails while this handle lives
        }
        let start = std::time::Instant::now();
        loop {
            match o.open(&path) {
                Ok(f) => {
                    if try_lock(&f)? {
                        return Ok(Lock(f));
                    }
                }
                // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION: another handle holds it
                Err(e) if cfg!(windows) && matches!(e.raw_os_error(), Some(32 | 33)) => {}
                Err(e) => return Err(e),
            }
            if start.elapsed() >= wait {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("{} is held by another nexo for over {}s", path.display(), wait.as_secs()),
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

/// True when this handle now holds the lock, false when another one does.
#[cfg(unix)]
fn try_lock(f: &std::fs::File) -> std::io::Result<bool> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: flock on a descriptor this File owns; it is released when the File is closed.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        Some(c) if c == libc::EWOULDBLOCK || c == libc::EINTR => Ok(false),
        _ => Err(e),
    }
}

#[cfg(not(unix))]
fn try_lock(_f: &std::fs::File) -> std::io::Result<bool> {
    Ok(true) // on Windows the exclusive open already is the lock
}

pub fn read_from(path: &Path) -> BTreeMap<String, Credential> {
    let Ok(text) = std::fs::read_to_string(path) else { return BTreeMap::new() };
    // entries nexo cannot read (a newer kind) are skipped, not fatal
    let raw: BTreeMap<String, serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();
    raw.into_iter().filter_map(|(k, v)| serde_json::from_value(v).ok().map(|c| (k, c))).collect()
}

/// Writes the whole store, through a file only you can read, renamed into place.
pub fn write_to<T: Serialize>(path: &Path, map: &BTreeMap<String, T>) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    // one temporary name per process: two writing at once never share it
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(map).map_err(std::io::Error::other)?;
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        use std::io::Write;
        let mut f = o.open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.write_all(b"\n")?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)
}

/// Seconds since the epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_round_trip_and_unknown_kinds_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("auth.json");
        let mut map = BTreeMap::new();
        map.insert("a".to_string(), Credential::Api { key: "k1".into() });
        let mut md = BTreeMap::new();
        md.insert("flow".to_string(), "copilot".to_string());
        map.insert(
            "b".to_string(),
            Credential::Oauth { access: "t".into(), refresh: "r".into(), expires: 100, metadata: md },
        );
        write_to(&f, &map).unwrap();
        assert_eq!(read_from(&f), map);
        // another writer's temporary file is not this one
        std::fs::write(dir.path().join("auth.json.tmp"), "other").unwrap();
        write_to(&f, &map).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("auth.json.tmp")).unwrap(), "other");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2, "no temporary file of its own stays");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let text = std::fs::read_to_string(&f).unwrap().replace("\"a\":", "\"z\": {\"type\": \"wellknown\"}, \"a\":");
        std::fs::write(&f, text).unwrap();
        assert_eq!(read_from(&f).len(), 2);
        assert_eq!(map["b"].kind(), "copilot login");
        assert!(map["b"].expiring(50, 60));
        assert!(!map["b"].expiring(10, 60));
        assert!(!map["a"].expiring(u64::MAX / 2, 0));
    }

    #[test]
    fn set_and_remove_keep_what_they_cannot_read_and_never_write_over_a_broken_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("auth.json");
        std::fs::write(&f, r#"{"z": {"type": "wellknown", "x": 1}, "a": {"type": "api", "key": "k"}}"#).unwrap();
        set_in(&f, "b", Credential::Api { key: "k2".into() }).unwrap();
        assert!(remove_in(&f, "a").unwrap());
        let raw: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(raw["z"]["type"], "wellknown", "an entry of a kind this build does not know stays");
        assert_eq!(raw["b"]["key"], "k2");
        assert!(raw.get("a").is_none());
        // a hand edit left a trailing comma: nothing is written over it
        let broken = "{\"a\": {\"type\": \"api\", \"key\": \"k\"},}";
        std::fs::write(&f, broken).unwrap();
        assert!(set_in(&f, "b", Credential::Api { key: "k3".into() }).is_err());
        assert!(remove_in(&f, "a").is_err());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), broken);
    }

    #[test]
    fn writers_at_the_same_time_lose_no_entry() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("auth.json");
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let f = f.clone();
                std::thread::spawn(move || {
                    for j in 0..10 {
                        set_in(&f, &format!("p{i}"), Credential::Api { key: format!("k{j}") }).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let map = read_from(&f);
        assert_eq!(map.len(), 8, "{map:?}");
        assert!(map.values().all(|c| c.secret() == "k9"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2, "only the store and its lock file stay");
    }

    #[test]
    fn a_lock_left_by_a_dead_process_is_taken_at_once_and_a_held_one_is_waited_for_a_bounded_time() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("auth.json");
        std::fs::write(dir.path().join("auth.json.lock"), "").unwrap(); // its taker died just now
        let t = std::time::Instant::now();
        let held = Lock::take_within(&f, std::time::Duration::from_secs(5)).unwrap();
        assert!(t.elapsed() < std::time::Duration::from_secs(2), "{:?}", t.elapsed());
        let t = std::time::Instant::now();
        let e = Lock::take_within(&f, std::time::Duration::from_millis(200)).err().expect("held");
        assert_eq!(e.kind(), std::io::ErrorKind::TimedOut);
        assert!(t.elapsed() >= std::time::Duration::from_millis(200));
        drop(held);
        Lock::take_within(&f, std::time::Duration::from_millis(200)).unwrap();
    }
}
