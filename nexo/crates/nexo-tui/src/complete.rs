//! Word completion for the draft: every word of the session (what was typed, said, run and
//! printed) goes into a sorted index once, and the word being typed is finished from it as grey
//! ghost text.
//!
//! The index is fed block by block when a block is final, never per keystroke. A lookup is a
//! range walk over a `BTreeMap` from the typed prefix, bounded by `SCAN_CAP`, so it costs
//! microseconds however long the session is. Big blocks (a `/resume` of megabytes) are queued and
//! indexed a slice per frame by `pump`.
//!
//! The tokenizer keeps whole units (a path, a URL, a hash, a `--flag`, `file:line`, an identifier
//! with `_ . - ::`) and also their pieces. Secrets are filtered with a heuristic: the value after a
//! key/token/password word, well-known key prefixes, query values with such keys and long mixed
//! tokens never enter. A secret with no hint before it can still get in.
//!
//! It lives in nexo-tui and not on the curator's word extractors: those sit on the curator's own
//! pattern engine and keep only distinctive names, and this needs ordinary words too, in any
//! language, without a new crate dependency.

use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ops::Bound;

/// Shortest word indexed: a completion of it always adds at least two characters.
pub const MIN_LEN: usize = 5;
/// Fewest typed characters that get a suggestion.
pub const MIN_TYPED: usize = 3;
/// Words kept; past a tenth over it the oldest tenth goes.
pub const CAP: usize = 200_000;
/// Entries a lookup looks at at most.
pub const SCAN_CAP: usize = 2048;
/// A word seen in the last this many blocks ranks like one the person wrote.
pub const RECENT: u32 = 64;
/// Longest unit kept; longer ones are blobs.
pub const MAX_UNIT: usize = 120;
/// Most of a block that is indexed.
pub const MAX_FEED: usize = 1 << 20;
/// Texts up to this size are indexed on the spot.
pub const INLINE: usize = 64 * 1024;
/// Bytes `pump` indexes per call.
pub const PUMP: usize = 256 * 1024;
/// A word the person wrote outlives others this many blocks when making room.
const USER_GRACE: u32 = 4096;
/// Units after a key/token/password word that are never indexed (same line).
const SKIP_UNITS: u8 = 3;
/// Newest words kept in the set a lookup walks first, so the scan cap cannot hide them.
const FRESH: usize = 1024;

/// Exact case, the person's or recent, frequency, shorter, newer: higher is better.
type Rank = (u8, u8, u16, Reverse<usize>, u32);

const JOIN: &str = "_-./\\:@~+%#=?&";

/// Where a text came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Src {
    /// The person's own messages.
    User,
    /// Replies, tool arguments and tool output.
    Other,
}

struct Entry {
    /// The commonest spelling.
    form: Box<str>,
    alt: Option<Box<str>>,
    n_form: u16,
    n_alt: u16,
    /// Block number it was last seen in.
    last: u32,
    /// Block number it was last written by the person in (0: never).
    user: u32,
}

/// What the index has done, for the tests.
#[derive(Default)]
pub struct Stats {
    pub bytes_scanned: u64,
    pub lookups: Cell<u64>,
    pub lookups_scanned: Cell<u64>,
}

pub struct WordIndex {
    enabled: bool,
    map: BTreeMap<Box<str>, Entry>,
    /// Keys of the words the person wrote: always looked at first, whatever `SCAN_CAP` cuts.
    hot: BTreeSet<Box<str>>,
    /// Keys of the newest words (about `FRESH`): looked at right after `hot`.
    fresh: BTreeSet<Box<str>>,
    seq: u32,
    pending: VecDeque<(String, Src, u32)>,
    buf: String,
    pub stats: Stats,
}

impl WordIndex {
    pub fn new(enabled: bool) -> WordIndex {
        WordIndex {
            enabled,
            map: BTreeMap::new(),
            hot: BTreeSet::new(),
            fresh: BTreeSet::new(),
            seq: 0,
            pending: VecDeque::new(),
            buf: String::new(),
            stats: Stats::default(),
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Texts waiting for `pump`.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.hot.clear();
        self.fresh.clear();
        self.pending.clear();
        self.seq = 0;
    }

    /// Indexes the text of a block that is final.
    pub fn feed(&mut self, text: &str, src: Src) {
        if !self.enabled || text.len() < MIN_LEN {
            return;
        }
        let mut end = text.len().min(MAX_FEED);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let text = &text[..end];
        self.seq = self.seq.saturating_add(1);
        if text.len() <= INLINE && self.pending.is_empty() {
            self.index(text, src, self.seq);
        } else {
            self.pending.push_back((text.to_string(), src, self.seq));
        }
    }

    /// Indexes the strings of a JSON value (tool arguments); the keys are not words of the person.
    pub fn feed_json(&mut self, v: &serde_json::Value, src: Src) {
        if !self.enabled {
            return;
        }
        fn walk(v: &serde_json::Value, out: &mut String) {
            match v {
                serde_json::Value::String(s) => {
                    out.push_str(s);
                    out.push('\n');
                }
                serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
                serde_json::Value::Object(o) => o.values().for_each(|x| walk(x, out)),
                _ => {}
            }
        }
        let mut s = String::new();
        walk(v, &mut s);
        self.feed(&s, src);
    }

    /// Indexes up to `budget` bytes of what is queued; returns the bytes done.
    pub fn pump(&mut self, budget: usize) -> usize {
        let mut done = 0;
        while done < budget {
            let Some((text, src, seq)) = self.pending.pop_front() else { break };
            let room = budget - done;
            if text.len() <= room {
                done += text.len();
                self.index(&text, src, seq);
                continue;
            }
            // cut at a blank so no word is split; the rest waits
            let mut cut = room;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            // no blank inside the slice: run on to the next one rather than split the word
            let cut = match text[..cut].rfind(char::is_whitespace) {
                Some(i) => i + 1,
                None => text[cut..].find(char::is_whitespace).map_or(text.len(), |i| cut + i),
            };
            if cut == 0 {
                self.pending.push_front((text, src, seq));
                break;
            }
            if cut >= text.len() {
                done += text.len();
                self.index(&text, src, seq);
                break;
            }
            done += cut;
            self.index(&text[..cut], src, seq);
            self.pending.push_front((text[cut..].to_string(), src, seq));
            break;
        }
        done
    }

    /// Everything queued, at once.
    pub fn flush(&mut self) {
        while self.pump(usize::MAX / 2) > 0 {}
    }

    fn index(&mut self, text: &str, src: Src, seq: u32) {
        self.stats.bytes_scanned += text.len() as u64;
        let mut found: Vec<String> = Vec::new();
        tokens(text, |w| found.push(w.to_string()));
        for w in &found {
            self.insert(w, src, seq);
        }
        if self.map.len() > CAP + CAP / 10 {
            self.evict();
        }
    }

    fn insert(&mut self, word: &str, src: Src, seq: u32) {
        let n = word.chars().count();
        self.buf.clear();
        self.buf.extend(word.chars().flat_map(char::to_lowercase));
        // a fold that changes the length (a few odd letters) would break the prefix arithmetic
        if self.buf.chars().count() != n {
            return;
        }
        if let Some(e) = self.map.get_mut(self.buf.as_str()) {
            if *e.form == *word {
                e.n_form = e.n_form.saturating_add(1);
            } else if e.alt.as_deref() == Some(word) {
                e.n_alt = e.n_alt.saturating_add(1);
            } else {
                e.alt = Some(word.into());
                e.n_alt = 1;
            }
            if e.n_alt > e.n_form {
                if let Some(a) = e.alt.take() {
                    e.alt = Some(std::mem::replace(&mut e.form, a));
                    std::mem::swap(&mut e.n_form, &mut e.n_alt);
                }
            }
            e.last = e.last.max(seq);
            if src == Src::User {
                e.user = seq;
                if !self.hot.contains(self.buf.as_str()) {
                    self.hot.insert(self.buf.as_str().into());
                }
            } else {
                self.touch();
            }
            return;
        }
        let key: Box<str> = self.buf.as_str().into();
        if src == Src::User {
            self.hot.insert(key.clone());
        } else {
            self.fresh.insert(key.clone());
            self.prune_fresh();
        }
        self.map.insert(
            key,
            Entry {
                form: word.into(),
                alt: None,
                n_form: 1,
                n_alt: 0,
                last: seq,
                user: if src == Src::User { seq } else { 0 },
            },
        );
    }

    /// Notes a word of the session as new.
    fn touch(&mut self) {
        if !self.fresh.contains(self.buf.as_str()) {
            self.fresh.insert(self.buf.as_str().into());
            self.prune_fresh();
        }
    }

    /// Keeps the newest `FRESH` keys once the set is twice that.
    fn prune_fresh(&mut self) {
        if self.fresh.len() <= 2 * FRESH {
            return;
        }
        let mut v: Vec<(u32, Box<str>)> =
            self.fresh.iter().map(|k| (self.map.get(k).map_or(0, |e| e.last), k.clone())).collect();
        v.sort_unstable_by_key(|x| Reverse(x.0));
        v.truncate(FRESH);
        self.fresh = v.into_iter().map(|x| x.1).collect();
    }

    /// Drops the oldest words (the person's last longer) down to nine tenths of `CAP`.
    fn evict(&mut self) {
        let keep = CAP - CAP / 10;
        let drop_n = self.map.len().saturating_sub(keep);
        if drop_n == 0 {
            return;
        }
        let mut v: Vec<(u32, Box<str>)> = self
            .map
            .iter()
            .map(|(k, e)| (if e.user > 0 { e.user.saturating_add(USER_GRACE) } else { e.last }, k.clone()))
            .collect();
        v.select_nth_unstable_by_key(drop_n - 1, |x| x.0);
        for (_, k) in v.into_iter().take(drop_n) {
            self.map.remove(&k);
            self.hot.remove(&k);
            self.fresh.remove(&k);
        }
    }

    /// What follows the typed text to finish a word of the session: the whole typed unit first,
    /// then what is left after each joiner (`src/configu` falls back to `configu`).
    pub fn suggest(&self, typed: &str) -> Option<String> {
        if !self.enabled || self.map.is_empty() {
            return None;
        }
        if let Some(s) = self.lookup(typed) {
            return Some(s);
        }
        for (i, c) in typed.char_indices() {
            if JOIN.contains(c) {
                let tail = &typed[i + c.len_utf8()..];
                if let Some(s) = self.lookup(tail) {
                    return Some(s);
                }
            }
        }
        None
    }

    fn lookup(&self, typed: &str) -> Option<String> {
        let tn = typed.chars().count();
        if tn < MIN_TYPED {
            return None;
        }
        let key: String = typed.chars().flat_map(char::to_lowercase).collect();
        if key.chars().count() != tn {
            return None;
        }
        self.stats.lookups.set(self.stats.lookups.get() + 1);
        let upper_inside = typed.chars().skip(1).any(char::is_uppercase);
        let seq = self.seq;
        let mut scanned = 0u64;
        let mut best: Option<(Rank, String)> = None;
        let mut see = |k: &str, e: &Entry| {
            let klen = if k.is_ascii() { k.len() } else { k.chars().count() };
            // never a suggestion that adds nothing
            if klen <= tn {
                return;
            }
            let exact = [Some(&*e.form), e.alt.as_deref()].into_iter().flatten().find(|f| f.starts_with(typed));
            if upper_inside && exact.is_none() {
                return;
            }
            let chosen = exact.unwrap_or(&e.form);
            let tier = u8::from(e.user > 0 || e.last.saturating_add(RECENT) >= seq);
            let freq = e.n_form.saturating_add(e.n_alt);
            let rank = (u8::from(exact.is_some()), tier, freq, Reverse(klen), e.last);
            if best.as_ref().is_none_or(|(b, _)| rank > *b) {
                best = Some((rank, chosen.to_string()));
            }
        };
        let from = (Bound::Included(key.as_str()), Bound::Unbounded);
        let mut hot_n = 0usize;
        for k in self.hot.range::<str, _>(from).take_while(|k| k.starts_with(&key)).take(SCAN_CAP) {
            hot_n += 1;
            if let Some(e) = self.map.get(k) {
                see(k, e);
            }
        }
        scanned += hot_n as u64;
        for k in self.fresh.range::<str, _>(from).take_while(|k| k.starts_with(&key)).take(SCAN_CAP) {
            scanned += 1;
            if let Some(e) = self.map.get(k) {
                see(k, e);
            }
        }
        // the person's words were all seen above unless that scan was cut
        let seen_hot = hot_n < SCAN_CAP;
        for (k, e) in self.map.range::<str, _>(from).take_while(|(k, _)| k.starts_with(&key)).take(SCAN_CAP) {
            scanned += 1;
            if !(seen_hot && e.user > 0) {
                see(k, e);
            }
        }
        self.stats.lookups_scanned.set(self.stats.lookups_scanned.get() + scanned);
        let (_, chosen) = best?;
        Some(chosen.chars().skip(tn).collect())
    }
}

fn is_unit(c: char) -> bool {
    c.is_alphanumeric() || JOIN.contains(c)
}

/// Pieces of a unit: runs of letters and digits, split again at camelCase boundaries.
fn pieces(unit: &str, mut emit: impl FnMut(&str)) {
    let cs: Vec<(usize, char)> = unit.char_indices().collect();
    let mut from: Option<usize> = None;
    for i in 0..=cs.len() {
        let here = cs.get(i).map(|x| x.1);
        let alnum = here.is_some_and(char::is_alphanumeric);
        let boundary = alnum && i > 0 && from.is_some() && {
            let prev = cs[i - 1].1;
            let next = cs.get(i + 1).map(|x| x.1);
            let c = here.unwrap_or(' ');
            (prev.is_lowercase() && c.is_uppercase())
                || (prev.is_uppercase() && c.is_uppercase() && next.is_some_and(char::is_lowercase))
        };
        if (!alnum || boundary) && from.is_some() {
            let s = from.take().unwrap_or(0);
            let e = cs.get(i).map(|x| x.0).unwrap_or(unit.len());
            let p = &unit[s..e];
            if p.chars().count() >= MIN_LEN && p.len() < unit.len() {
                emit(p);
            }
        }
        if alnum && from.is_none() {
            from = Some(cs[i].0);
        }
    }
}

/// A word that says the next value is a secret.
fn secret_word(unit: &str, sep_after: bool) -> bool {
    let low: String = unit.chars().flat_map(char::to_lowercase).collect();
    if low == "key" {
        return sep_after;
    }
    let mut hit = false;
    let mut parts: Vec<String> = Vec::new();
    pieces_all(unit, |p| parts.push(p.chars().flat_map(char::to_lowercase).collect()));
    for p in &parts {
        if p.starts_with("passw")
            || p.starts_with("secret")
            || p.starts_with("credential")
            || p.starts_with("authorization")
            || matches!(p.as_str(), "token" | "bearer" | "apikey" | "key" | "pwd" | "passwd")
        {
            hit = true;
        }
    }
    hit
}

/// Every piece of a unit, whatever its length.
fn pieces_all(unit: &str, mut emit: impl FnMut(&str)) {
    let mut from: Option<usize> = None;
    let cs: Vec<(usize, char)> = unit.char_indices().collect();
    for i in 0..=cs.len() {
        let c = cs.get(i).map(|x| x.1);
        let alnum = c.is_some_and(char::is_alphanumeric);
        let camel = alnum && i > 0 && from.is_some() && cs[i - 1].1.is_lowercase() && c.is_some_and(char::is_uppercase);
        if (!alnum || camel) && from.is_some() {
            let s = from.take().unwrap_or(0);
            emit(&unit[s..cs.get(i).map(|x| x.0).unwrap_or(unit.len())]);
        }
        if alnum && from.is_none() {
            from = Some(cs[i].0);
        }
    }
}

fn secret_value(unit: &str) -> bool {
    let low: String = unit.chars().take(12).flat_map(char::to_lowercase).collect();
    if ["sk-", "sk_", "ghp_", "gho_", "ghs_", "ghu_", "github_pat_", "xoxb", "xoxp", "xoxa", "akia", "eyj", "-----"]
        .iter()
        .any(|p| low.starts_with(p))
    {
        return true;
    }
    // a long unit of mixed case and digits that is not a hash: base64, a key, a session id
    let n = unit.chars().count();
    if n >= 32 && !unit.chars().all(|c| c.is_ascii_hexdigit()) {
        let seps = unit.chars().filter(|c| JOIN.contains(*c) && *c != '+' && *c != '=').count();
        let up = unit.chars().any(char::is_uppercase);
        let lo = unit.chars().any(char::is_lowercase);
        let dg = unit.chars().any(|c| c.is_ascii_digit());
        if up && lo && dg && seps <= 2 {
            return true;
        }
    }
    // https://x/?api_key=...: a query with a secret key
    if let Some((_, q)) = unit.split_once('?') {
        for kv in q.split('&') {
            if let Some((k, _)) = kv.split_once('=') {
                if secret_word(k, true) {
                    return true;
                }
            }
        }
    }
    false
}

fn keep_lead(lead: &str) -> bool {
    matches!(lead, "-" | "--" | "/" | "//" | "~/" | "./" | "../" | "~")
}

fn unit_tokens(raw: &str, sep_after: bool, skip: &mut u8, emit: &mut impl FnMut(&str)) {
    let Some(first) = raw.find(char::is_alphanumeric) else { return };
    let lead = &raw[..first];
    let start = if keep_lead(lead) { 0 } else { first };
    let unit = raw[start..].trim_end_matches(|c: char| c != '/' && !c.is_alphanumeric());
    let n = unit.chars().count();
    if unit.is_empty() || n > MAX_UNIT {
        *skip = 0;
        return;
    }
    // key=value: the value of a secret key is not a word
    if let Some((k, _)) = unit.split_once('=') {
        if secret_word(k, true) {
            *skip = 0;
            return;
        }
    }
    let kw = secret_word(unit, sep_after || raw.len() > unit.len());
    if *skip > 0 {
        *skip = if kw { SKIP_UNITS } else { *skip - 1 };
        return;
    }
    if kw {
        *skip = SKIP_UNITS;
    }
    if secret_value(unit) {
        return;
    }
    if n >= MIN_LEN {
        emit(unit);
    }
    // path:line and path:line:col also as the bare path
    let mut bare = unit;
    for _ in 0..2 {
        if let Some((head, tail)) = bare.rsplit_once(':') {
            if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
                bare = head;
            }
        }
    }
    if bare.len() < unit.len() && bare.chars().count() >= MIN_LEN {
        emit(bare);
    }
    // what follows each slash, so a path is findable from any directory in it
    for (i, c) in unit.char_indices() {
        if c == '/' {
            let tail = &unit[i + 1..];
            if !tail.starts_with('/') && tail.chars().count() >= MIN_LEN && tail.len() < unit.len() {
                emit(tail);
            }
        }
    }
    if unit.chars().any(|c| !c.is_alphanumeric()) || unit.chars().skip(1).any(char::is_uppercase) {
        pieces(unit, |p| emit(p));
    }
}

/// Every word of a text worth keeping, whole units first and then their pieces.
pub fn tokens(text: &str, mut emit: impl FnMut(&str)) {
    let mut skip = 0u8;
    let mut last_end = 0usize;
    let mut start: Option<usize> = None;
    let run = |s: usize, e: usize, last_end: &mut usize, skip: &mut u8, emit: &mut dyn FnMut(&str)| {
        if text[*last_end..s].contains('\n') {
            *skip = 0;
        }
        let rest = text[e..].trim_start_matches(['"', '\'', ' ']);
        let sep_after = rest.starts_with(':') || rest.starts_with('=');
        unit_tokens(&text[s..e], sep_after, skip, &mut |w| emit(w));
        *last_end = e;
    };
    for (i, c) in text.char_indices() {
        if is_unit(c) {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            run(s, i, &mut last_end, &mut skip, &mut emit);
        }
    }
    if let Some(s) = start {
        run(s, text.len(), &mut last_end, &mut skip, &mut emit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx(text: &str) -> WordIndex {
        let mut w = WordIndex::new(true);
        w.feed(text, Src::Other);
        w
    }

    fn all(text: &str) -> Vec<String> {
        let mut v = Vec::new();
        tokens(text, |w| v.push(w.to_string()));
        v
    }

    #[test]
    fn plain_english_and_spanish_words() {
        let w = idx("the configuración of the deployment in Madrid, el español y el niño pequeño");
        assert_eq!(w.suggest("configu").as_deref(), Some("ración"));
        assert_eq!(w.suggest("deploy").as_deref(), Some("ment"));
        assert_eq!(w.suggest("madr").as_deref(), Some("id"), "typed case kept, the tail from the word");
        assert_eq!(w.suggest("Madr").as_deref(), Some("id"));
        assert_eq!(w.suggest("espa").as_deref(), Some("ñol"));
        assert_eq!(w.suggest("pequ").as_deref(), Some("eño"));
        assert_eq!(w.suggest("CONFIGU"), None, "capitals inside the typed word must match exactly");
        assert_eq!(w.suggest("niñ"), None, "nino has 4 letters: below the minimum");
    }

    #[test]
    fn hash_completes_from_three_chars() {
        let h = "3a7bd3e2360a3d29eea436fcfb7e44c735d117c42d1c1835420b6b9942dd4f1b";
        let w = idx(&format!("sha256: {h}\nok"));
        assert_eq!(w.suggest("3a7").as_deref(), Some(&h[3..]));
        assert_eq!(w.suggest("3a7bd3e").as_deref(), Some(&h[7..]));
        assert_eq!(w.suggest("3a8"), None);
    }

    #[test]
    fn path_and_segments() {
        let w = idx("see crates/nexo-tui/src/app.rs:120 and /etc/hostname");
        assert_eq!(w.suggest("crates/ne").as_deref(), Some("xo-tui/src/app.rs"), "tie: the shorter, the bare path");
        assert_eq!(w.suggest("nexo-t").as_deref(), Some("ui/src/app.rs:120"));
        assert_eq!(w.suggest("crates/nexo-tui/src/app.rs:1").as_deref(), Some("20"));
        assert_eq!(w.suggest("crates/nexo-tui/src/app.rs").as_deref(), Some(":120"));
        // the bare path is a word too, and the segments
        assert!(all("crates/nexo-tui/src/app.rs:120").contains(&"crates/nexo-tui/src/app.rs".to_string()));
        assert_eq!(w.suggest("src/ap").as_deref(), Some("p.rs:120"));
        assert_eq!(w.suggest("crate").as_deref(), Some("s"));
        assert_eq!(w.suggest("/etc/host").as_deref(), Some("name"));
        assert_eq!(w.suggest("host").as_deref(), Some("name"), "four typed letters of a segment finish it");
    }

    #[test]
    fn identifier_parts() {
        let t = all("call parse_header_value then ParseHeaderValue and foo_bar::bazqux_thing now");
        for want in ["parse_header_value", "parse_header_value", "ParseHeaderValue", "foo_bar::bazqux_thing"] {
            assert!(t.contains(&want.to_string()), "{want} in {t:?}");
        }
        for want in ["parse", "header", "value", "Parse", "Header", "Value", "bazqux", "thing"] {
            if want.len() >= MIN_LEN {
                assert!(t.contains(&want.to_string()), "piece {want} in {t:?}");
            }
        }
        let w = idx("call parse_header_value then ParseHeaderValue");
        assert_eq!(w.suggest("heade").as_deref(), Some("r"));
        assert_eq!(w.suggest("parse_h").as_deref(), Some("eader_value"));
    }

    #[test]
    fn url_and_flag_stay_whole() {
        let w = idx("open https://x.dev/a/b?c=1 and git commit --no-verify now");
        assert_eq!(w.suggest("https://x.d").as_deref(), Some("ev/a/b?c=1"));
        assert_eq!(w.suggest("--no-v").as_deref(), Some("erify"));
        assert_eq!(w.suggest("no-ver").as_deref(), Some("ify"));
    }

    #[test]
    fn case_rules() {
        let mut w = WordIndex::new(true);
        w.feed("Config Config Config config", Src::Other);
        w.feed("configuration", Src::Other);
        assert_eq!(w.suggest("Conf").as_deref(), Some("ig"), "exact case first");
        assert_eq!(w.suggest("conf").as_deref(), Some("ig"), "the commoner one");
        let w3 = idx("config config config Configuration");
        assert_eq!(w3.suggest("Conf").as_deref(), Some("iguration"), "exact case beats a commoner word");
        let w2 = idx("HashMap hashmap");
        assert_eq!(w2.suggest("HashM").as_deref(), Some("ap"));
        assert_eq!(w2.suggest("hashM"), None, "a capital inside must match exactly");
    }

    #[test]
    fn recency_and_user_words_first() {
        let mut w = WordIndex::new(true);
        w.feed("alphabet alphabet alphabet alphabet", Src::Other);
        for _ in 0..(RECENT + 5) {
            w.feed("filler words here", Src::Other);
        }
        w.feed("alphanumeric", Src::User);
        assert_eq!(w.suggest("alph").as_deref(), Some("anumeric"), "the person's word beats a commoner one");
        let mut w = WordIndex::new(true);
        w.feed("oldest oldest", Src::Other);
        for _ in 0..(RECENT + 5) {
            w.feed("filler words here", Src::Other);
        }
        w.feed("olderly", Src::Other);
        assert_eq!(w.suggest("olde").as_deref(), Some("rly"), "recent beats frequent and old");
        let t = idx("zebra zebras");
        assert_eq!(t.suggest("zeb").as_deref(), Some("ra"), "tie: the shorter completion");
    }

    #[test]
    fn never_shorter_or_equal() {
        let w = idx("deployment");
        assert_eq!(w.suggest("deployment"), None);
        assert_eq!(w.suggest("deploymen").as_deref(), Some("t"));
        assert_eq!(w.suggest("deploymentx"), None);
    }

    #[test]
    fn secrets_excluded() {
        let h = "3a7bd3e2360a3d29eea436fcfb7e44c735d117c42d1c1835420b6b9942dd4f1b";
        let w = idx(&format!(
            "token=abc12345xyz\n\"password\": \"hunter2000\"\nAuthorization: Bearer eyJhbGciOiJIUzI1NiJ9abc\n\
             key sk-proj-abcdefghijk ghp_abcdefghijklmnop\nAPI_SECRET=zzzsecretvalue\n\
             url https://x.io/p?api_key=qqqqvalue1&x=2 hash {h}\nsession Zk3Jd9Qw7Lm2Xv8Rt5Yb1Nc4Hs6Pa0Ge9Uf\n"
        ));
        for typed in ["abc1", "hunt", "eyJh", "sk-p", "ghp_", "zzzs", "qqqq", "https://x.io", "Zk3J"] {
            assert_eq!(w.suggest(typed), None, "{typed}");
        }
        assert_eq!(w.suggest("3a7").as_deref(), Some(&h[3..]), "a hash next to secrets stays");
        let w = idx("my password reset page");
        assert_eq!(w.suggest("page"), None);
    }

    #[test]
    fn min_length_and_min_typed() {
        let w = idx("tiny word words configuration");
        assert_eq!(w.suggest("tin"), None, "4 letters are not worth a ghost");
        assert_eq!(w.suggest("con"), Some("figuration".into()));
        assert_eq!(w.suggest("co"), None, "two typed characters: nothing");
        assert!(!all("tiny word").iter().any(|x| x == "tiny"));
    }

    #[test]
    fn eviction_keeps_the_cap() {
        let mut w = WordIndex::new(true);
        w.feed("myusername wrote this", Src::User);
        for chunk in 0..50 {
            let s: String = (0..5000).map(|i| format!("w{chunk:03}x{i:05} ")).collect();
            w.feed(&s, Src::Other);
        }
        assert!(w.len() <= CAP + CAP / 10, "{}", w.len());
        assert_eq!(w.suggest("myuser").as_deref(), Some("name"), "the person's word outlives the rest");
    }

    #[test]
    fn big_text_is_queued_and_pumped_in_slices() {
        let mut w = WordIndex::new(true);
        let s = "configuración deployment ".repeat(20_000);
        w.feed(&s, Src::Other);
        assert!(w.pending() > 0 || s.len() <= INLINE);
        let done = w.pump(100_000);
        assert!(done <= 100_000 && done > 0);
        w.flush();
        assert_eq!(w.pending(), 0);
        assert_eq!(w.stats.bytes_scanned, s.len() as u64);
        assert_eq!(w.suggest("configu").as_deref(), Some("ración"));
    }

    #[test]
    fn secret_values_a_few_units_after_the_keyword_stay_out() {
        let w = idx("the password is hunter2xyz\nmy token was abcdef123456 ok\nsecret = the value zxcvbn99");
        for t in ["hunter", "hunter2", "abcdef", "zxcvbn"] {
            assert_eq!(w.suggest(t), None, "{t}");
        }
        // the next line is ordinary again
        let w = idx("the password is hunter2xyz\ndeployment finished");
        assert_eq!(w.suggest("deploy").as_deref(), Some("ment"));
    }

    #[test]
    fn a_new_word_wins_over_an_alphabetical_flood() {
        let mut w = WordIndex::new(true);
        let flood: String = (0..20_000).map(|i| format!("con{i:016} ")).collect();
        w.feed(&flood, Src::Other);
        for i in 0..70 {
            w.feed(&format!("filler{i:04} text"), Src::Other);
        }
        w.feed("la configuración nueva", Src::Other);
        w.flush();
        assert_eq!(w.suggest("con").as_deref(), Some("figuración"));
    }

    #[test]
    fn pump_never_splits_a_word() {
        let mut w = WordIndex::new(true);
        let first = "x ".repeat(40_000);
        w.feed(&first, Src::Other);
        w.feed("configuración deployment", Src::Other);
        w.pump(first.len() + 5);
        w.flush();
        assert_eq!(w.suggest("configu").as_deref(), Some("ración"));
        assert_eq!(w.suggest("confi").as_deref(), Some("guración"));
        assert_eq!(w.suggest("guraci"), None, "no stray half word");
    }

    #[test]
    fn off_means_nothing() {
        let mut w = WordIndex::new(false);
        w.feed("configuración", Src::User);
        assert!(w.is_empty());
        assert_eq!(w.suggest("configu"), None);
    }

    /// Noise of the 3-letter threshold: how often the first three letters of a word of ordinary
    /// text give a ghost that is another word.
    #[test]
    fn noise_is_low() {
        let en = "the deployment pipeline builds every commit and runs the integration tests before \
                  the release candidate goes to staging where the team reviews the changes together \
                  with the product owner and decides whether the feature is ready for production";
        let es = "la configuración del servidor se guarda en el directorio principal y cada usuario \
                  puede cambiar las opciones desde la pantalla de ajustes antes de reiniciar el \
                  programa para que los cambios tengan efecto inmediatamente después";
        let text = format!("{en} {es}").repeat(3);
        let w = idx(&text);
        let words: Vec<&str> = text.split_whitespace().filter(|x| x.chars().count() >= 6).collect();
        let (mut shown, mut right, mut total) = (0, 0, 0);
        for word in &words {
            total += 1;
            let typed: String = word.chars().take(MIN_TYPED).collect();
            if let Some(s) = w.suggest(&typed) {
                shown += 1;
                let full = format!("{typed}{s}");
                if *word == full || word.starts_with(&full) {
                    right += 1;
                }
            }
        }
        let wrong = shown - right;
        eprintln!("noise: {total} words, {shown} ghosts at {MIN_TYPED} letters, {right} right, {wrong} wrong");
        assert!(shown * 10 >= total * 5, "most 3-letter prefixes find a word");
        assert!(wrong * 100 <= shown * 25, "{wrong} of {shown} ghosts were another word");
    }
}
