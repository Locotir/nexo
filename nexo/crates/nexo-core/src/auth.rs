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
    let f = file();
    let mut map = read_from(&f);
    map.insert(name.to_string(), cred);
    write_to(&f, &map)
}

/// Removes an entry; true when there was one.
pub fn remove(name: &str) -> std::io::Result<bool> {
    let f = file();
    let mut map = read_from(&f);
    let had = map.remove(name).is_some();
    if had {
        write_to(&f, &map)?;
    }
    Ok(had)
}

pub fn read_from(path: &Path) -> BTreeMap<String, Credential> {
    let Ok(text) = std::fs::read_to_string(path) else { return BTreeMap::new() };
    // entries nexo cannot read (a newer kind) are skipped, not fatal
    let raw: BTreeMap<String, serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();
    raw.into_iter().filter_map(|(k, v)| serde_json::from_value(v).ok().map(|c| (k, c))).collect()
}

/// Writes the whole store, through a file only you can read, renamed into place.
pub fn write_to(path: &Path, map: &BTreeMap<String, Credential>) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.tmp");
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
}
