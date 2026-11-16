//! The curator's state saved next to a session, so that a resumed session starts from it instead of taking every
//! reply of its history again. The history is written as it always was: a build that cannot use the saved state
//! (another engine, a history that is not the one the state covers, a cut or damaged file) rebuilds the state from
//! the history as before, and an older build never reads the file.
//!
//! The file is a fixed header and then the engine, in `bincode`. The header holds the engine's format (a hash of
//! its source, `build.rs`), the settings, how many messages and views the state covers, a hash of those messages,
//! and a hash of the rest of the file. The history itself is not in it: the engine's copy of the history is taken
//! from the history again, with the messages it shortened when they came as they became.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{LazyLock, Mutex};

use bincode::Options;
use indexmap::IndexMap;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::curator::Curator;

/// The engine this build runs (`build.rs`): a state another build saved is not read.
pub const FORMAT: &str = env!("NEXO_CURATOR_FORMAT");

const MAGIC: [u8; 8] = *b"NXCURST1";
/// The magic and seven numbers.
const HEAD: usize = 8 + 7 * 8;

/// A hash of one message as the host gives it to the curator. The host keeps one per message as the history
/// grows, so that saving a state never goes over the history again.
pub fn message_hash(v: &Value) -> u64 {
    let mut h = DefaultHasher::new();
    feed(v, &mut h);
    h.finish()
}

fn feed(v: &Value, h: &mut DefaultHasher) {
    match v {
        Value::Null => 0u8.hash(h),
        Value::Bool(b) => {
            1u8.hash(h);
            b.hash(h);
        }
        Value::Number(n) => {
            2u8.hash(h);
            match (n.as_i64(), n.as_u64(), n.as_f64()) {
                (Some(i), _, _) => i.hash(h),
                (_, Some(u), _) => u.hash(h),
                (_, _, f) => f.map(f64::to_bits).hash(h),
            }
        }
        Value::String(s) => {
            3u8.hash(h);
            s.hash(h);
        }
        Value::Array(a) => {
            4u8.hash(h);
            a.len().hash(h);
            for x in a {
                feed(x, h);
            }
        }
        Value::Object(m) => {
            5u8.hash(h);
            m.len().hash(h);
            for (k, x) in m {
                k.hash(h);
                feed(x, h);
            }
        }
    }
}

/// A hash of the first messages of a history, from the hash of each (`message_hash`).
pub fn history_hash(hashes: &[u64]) -> u64 {
    let mut h = DefaultHasher::new();
    hashes.len().hash(&mut h);
    hashes.hash(&mut h);
    h.finish()
}

fn hash_of<T: Hash + ?Sized>(x: &T) -> u64 {
    let mut h = DefaultHasher::new();
    x.hash(&mut h);
    h.finish()
}

pub(crate) fn options() -> impl Options {
    bincode::DefaultOptions::new()
}

/// What a saved state says of itself, read before the engine is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// The engine that saved it (`FORMAT`).
    pub format: u64,
    /// A hash of the curator's settings.
    pub spec: u64,
    /// The messages of the history the engine had taken in.
    pub covered: u64,
    /// The views the host had asked for when it was saved (for the record; nothing depends on it).
    pub views: u64,
    /// `history_hash` of the covered messages.
    pub history: u64,
    /// The bytes after the header, and a hash of them.
    pub len: u64,
    pub check: u64,
}

impl Header {
    /// The header at the start of `bytes`, when they hold one this build can read.
    pub fn read(bytes: &[u8]) -> Result<Header, String> {
        if bytes.len() < HEAD || bytes[..8] != MAGIC {
            return Err("not a saved curator state".into());
        }
        let n = |k: usize| u64::from_le_bytes(bytes[8 + 8 * k..16 + 8 * k].try_into().expect("8 bytes"));
        let h = Header { format: n(0), spec: n(1), covered: n(2), views: n(3), history: n(4), len: n(5), check: n(6) };
        if h.format != format_id() {
            return Err("saved by another build of the curator".into());
        }
        Ok(h)
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&MAGIC);
        for n in [self.format, self.spec, self.covered, self.views, self.history, self.len, self.check] {
            out.extend_from_slice(&n.to_le_bytes());
        }
    }
}

fn format_id() -> u64 {
    u64::from_str_radix(FORMAT, 16).unwrap_or(0)
}

/// The state of a curator at one point, to be written out (`encode`) on another thread: the large parts of the
/// engine are shared with the curator that goes on, not copied.
pub struct Saved {
    cur: Curator,
    views: u64,
    history: u64,
}

impl Saved {
    /// The messages of the history the state covers.
    pub fn covered(&self) -> usize {
        self.cur.eng.covered()
    }

    /// The file: the header, then the engine.
    pub fn encode(&self) -> Vec<u8> {
        let born = self.cur.eng.born_messages();
        let payload = options().serialize(&(&self.cur, &born)).expect("an engine always encodes");
        let h = Header {
            format: format_id(),
            spec: hash_of(self.cur.spec.as_str()),
            covered: self.covered() as u64,
            views: self.views,
            history: self.history,
            len: payload.len() as u64,
            check: hash_of(&payload[..]),
        };
        let mut out = Vec::with_capacity(HEAD + payload.len());
        h.write(&mut out);
        out.extend_from_slice(&payload);
        out
    }
}

impl Curator {
    /// The state as it is now, with the views the host asked for so far and `history_hash` of the messages the
    /// engine took in (`Curator::covered` of them).
    pub fn save(&self, views: u64, history: u64) -> Saved {
        Saved { cur: self.clone(), views, history }
    }

    /// The settings it was made with (`Curator::new`).
    pub fn spec(&self) -> &str {
        &self.spec
    }

    /// The messages of the history the engine has taken in: the last view was over these.
    pub fn covered(&self) -> usize {
        self.eng.covered()
    }

    /// The curator `bytes` saved (`Saved::encode`), for the curator of settings `spec` over `msgs`, the history
    /// now, whose messages hash to `hashes` (`message_hash`). The views to take after it are the ones over the
    /// messages past `Header::covered`. Any difference from what was saved is an error, and the caller rebuilds
    /// the state from the history.
    pub fn restore(spec: &str, bytes: &[u8], msgs: &[Value], hashes: &[u64]) -> Result<(Curator, Header), String> {
        let h = Header::read(bytes)?;
        if h.spec != hash_of(spec) {
            return Err("saved with other curator settings".into());
        }
        let covered = usize::try_from(h.covered).map_err(|_| "covers more messages than there are")?;
        if covered > msgs.len() || covered > hashes.len() {
            return Err(format!("covers {covered} messages, the history has {}", msgs.len()));
        }
        if history_hash(&hashes[..covered]) != h.history {
            return Err("the history differs from the one it covers".into());
        }
        let payload = &bytes[HEAD..];
        if payload.len() as u64 != h.len || hash_of(payload) != h.check {
            return Err("the file is cut or damaged".into());
        }
        let (mut cur, born): (Curator, Vec<(usize, String)>) =
            options().with_limit(h.len).deserialize(payload).map_err(|e| format!("cannot read the engine: {e}"))?;
        if cur.spec != spec {
            return Err("saved with other curator settings".into());
        }
        cur.eng.take_history(&msgs[..covered], &born)?;
        Ok((cur, h))
    }
}

/// The names of rules and counters, which the engine keeps as `&'static str`: each distinct name read back is
/// stored once for the life of the process.
pub(crate) fn name(s: &str) -> &'static str {
    static NAMES: LazyLock<Mutex<FxHashSet<&'static str>>> = LazyLock::new(Default::default);
    let mut t = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&n) = t.get(s) {
        return n;
    }
    let n: &'static str = Box::leak(s.into());
    t.insert(n);
    n
}

/// Reads back the counters by name (`Engine::stats`).
pub(crate) fn names_map<'de, D: Deserializer<'de>>(d: D) -> Result<IndexMap<&'static str, i64>, D::Error> {
    let m: IndexMap<String, i64> = IndexMap::deserialize(d)?;
    Ok(m.into_iter().map(|(k, v)| (name(&k), v)).collect())
}

/// Reads back what took each thing out of the view, by the name of its rule (`Own`).
pub(crate) fn named_map<'de, D, K>(d: D) -> Result<FxHashMap<K, (u64, &'static str)>, D::Error>
where
    D: Deserializer<'de>,
    K: Deserialize<'de> + Eq + std::hash::Hash,
{
    let m: Vec<(K, (u64, String))> = Vec::deserialize(d)?;
    Ok(m.into_iter().map(|(k, (r, n))| (k, (r, name(&n)))).collect())
}

/// Reads back the record of what left the view and came back, by rule (`Own`).
pub(crate) fn named_events<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<(&'static str, u64, i64)>, D::Error> {
    let v: Vec<(String, u64, i64)> = Vec::deserialize(d)?;
    Ok(v.into_iter().map(|(n, r, t)| (name(&n), r, t)).collect())
}

/// `serde(with)` for the line index's postings (`Index`): each list holds units in the order they came, so it is
/// written as the step from the one before, which mostly takes one byte where the unit takes three.
pub(crate) mod posts {
    use rustc_hash::FxHashMap;
    use serde::ser::SerializeSeq;
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::index::Post;

    pub fn serialize<S: Serializer>(m: &FxHashMap<u64, Vec<Post>>, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(m.len()))?;
        let mut steps: Vec<Post> = Vec::new();
        for (k, v) in m {
            steps.clear();
            let mut prev = 0u32;
            for &(unit, line) in v {
                steps.push((unit.wrapping_sub(prev), line));
                prev = unit;
            }
            seq.serialize_element(&(k, &steps))?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<FxHashMap<u64, Vec<Post>>, D::Error> {
        let lists: Vec<(u64, Vec<Post>)> = Vec::deserialize(d)?;
        let mut m = FxHashMap::with_capacity_and_hasher(lists.len(), Default::default());
        for (k, mut v) in lists {
            let mut prev = 0u32;
            for p in &mut v {
                prev = prev.wrapping_add(p.0);
                p.0 = prev;
            }
            m.insert(k, v);
        }
        Ok(m)
    }
}

/// `serde(with)` for a message the engine keeps as JSON: written as its text, since the binary format cannot
/// read JSON's self-describing values back.
pub(crate) mod json_opt {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::Value;

    pub fn serialize<S: Serializer>(v: &Option<Value>, s: S) -> Result<S::Ok, S::Error> {
        v.as_ref().map(Value::to_string).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
        let t: Option<String> = Option::deserialize(d)?;
        t.map(|t| serde_json::from_str(&t).map_err(serde::de::Error::custom)).transpose()
    }
}
