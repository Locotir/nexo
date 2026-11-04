//! The trace of a conversation: its units (blocks), the distinctive words in them, and which of those
//! words the agent copied later (a use of the unit).

use std::sync::LazyLock;

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;

use crate::json::est_media;
use crate::msg::{blocks, flat, get_str, key_of, text_of};
use crate::py::{self, Re};

/// An interned string.
pub type Sym = u32;

/// Strings seen by one engine, each stored once.
#[derive(Default, Clone)]
pub struct Interner {
    map: FxHashMap<Box<str>, Sym>,
    strs: Vec<Box<str>>,
}

impl Interner {
    pub fn intern(&mut self, s: &str) -> Sym {
        if let Some(&k) = self.map.get(s) {
            return k;
        }
        let k = self.strs.len() as Sym;
        self.strs.push(s.into());
        self.map.insert(s.into(), k);
        k
    }

    /// The symbol of a string already seen (without storing a new one).
    pub fn find(&self, s: &str) -> Option<Sym> {
        self.map.get(s).copied()
    }

    pub fn get(&self, k: Sym) -> &str {
        &self.strs[k as usize]
    }
}

/// A sorted set of symbols without duplicates.
pub type Set = Vec<Sym>;

pub fn set_of(mut v: Vec<Sym>) -> Set {
    v.sort_unstable();
    v.dedup();
    v
}

static WORD: LazyLock<Re> = LazyLock::new(|| Re::new(r"[A-Za-z_$][\w$]*(?:[./-][\w$]+)*|\d+(?:\.\d+)+|\d{4,}", 0));

const FILE_EXT: [&str; 92] = [
    "py", "js", "mjs", "ts", "tsx", "jsx", "md", "json", "jsonl", "toml", "yaml", "yml", "txt", "cfg", "ini", "sh",
    "rs", "go", "java", "c", "h", "cc", "cpp", "hpp", "cs", "css", "scss", "html", "sql", "lock", "log", "csv", "xml",
    "env", "db", "rb", "php", "swift", "kt", "conf", "docx", "pdf", "lua", "vue", "svelte", "ipynb", "proto", "tf",
    "gradle", "mod", "sum", "zig", "ex", "exs", "dart", "", "tsv", "xls", "xlsx", "ods", "parquet", "feather", "arrow",
    "avro", "pkl", "pickle", "npy", "npz", "h5", "hdf5", "sqlite", "rds", "rmd", "dta", "pptx", "odt", "rst", "tex",
    "astro", "mdx", "svg", "png", "jpg", "jpeg", "gif", "webp", "graphql", "gql", "less", "sass", "erb", "hbs",
];

const TLD: [&str; 28] = [
    "com",
    "net",
    "org",
    "io",
    "dev",
    "ai",
    "co",
    "app",
    "cloud",
    "internal",
    "local",
    "localhost",
    "lan",
    "edu",
    "gov",
    "info",
    "xyz",
    "tech",
    "sh",
    "es",
    "uk",
    "de",
    "fr",
    "eu",
    "us",
    "corp",
    "test",
    "example",
];

/// Names any model knows without reading them here.
static GENERAL: LazyLock<FxHashSet<&'static str>> = LazyLock::new(|| crate::GENERAL_WORDS.split_whitespace().collect());

fn distinctive(w: &str) -> bool {
    if py::len(w) < 4 {
        return false;
    }
    if py::str_isdigit(w) {
        if py::len(w) == 4 {
            // int() of unicode digits: only ASCII years matter here
            if let Ok(y) = w.parse::<u32>() {
                return !(1900..=2099).contains(&y);
            }
        }
        return true;
    }
    if w.contains('_') || w.contains('$') || w.chars().any(py::is_digit) {
        return true;
    }
    let tail = &w[w.chars().next().map_or(0, char::len_utf8)..];
    tail != py::lower(tail) && tail != py::upper(tail)
}

fn has_ext(last: &str, list: &[&str]) -> bool {
    let low = py::lower(last);
    !low.is_empty() && list.contains(&low.as_ref())
}

/// Distinctive tokens of a text (``hindsight.words``), as a sorted set.
pub fn words(text: &str, int: &mut Interner) -> Set {
    let common = &*GENERAL;
    let mut out = Vec::new();
    for (a, b) in WORD.find_all(text) {
        let w = &text[a..b];
        let first = py::first(w).unwrap_or(' ');
        if w.contains('/') || py::is_digit(first) {
            let ok = py::len(w) >= 4 && (!py::str_isdigit(w) || distinctive(w));
            if ok && !common.contains(w) {
                out.push(int.intern(w));
            }
            continue;
        }
        let parts: Vec<&str> = w.split('.').collect();
        if parts.len() > 1 {
            let last = parts[parts.len() - 1];
            if has_ext(last, &FILE_EXT) || (parts.len() > 2 && has_ext(last, &TLD)) {
                if py::len(w) >= 4 && !common.contains(w) {
                    out.push(int.intern(w));
                }
                continue;
            }
        }
        for p in parts {
            if distinctive(p) && !common.contains(p) {
                out.push(int.intern(p));
            }
        }
    }
    set_of(out)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UKind {
    User,
    Result,
    Text,
    Thinking,
    Call,
}

#[derive(Clone)]
pub struct Unit {
    pub msg: usize,
    pub block: usize,
    pub kind: UKind,
    pub tool: String,
    pub tokens: i64,
    pub words: Set,
    pub saving: i64,
    pub uses: Vec<usize>,
    pub word_first: FxHashMap<Sym, usize>,
}

const STUB_TOKENS: i64 = 25;
const FOLD_CHARS: usize = 200;

fn saving(kind: UKind, b: &Value, tokens: i64, opaque: bool) -> i64 {
    if opaque {
        return 0;
    }
    match kind {
        UKind::Result => (tokens - STUB_TOKENS).max(0),
        UKind::Thinking => tokens,
        UKind::Call => match b.get("input") {
            Some(Value::Object(m)) => m
                .values()
                .filter_map(Value::as_str)
                .map(|v| (v.chars().count(), v))
                .filter(|(n, _)| *n > FOLD_CHARS)
                .map(|(n, _)| (n as i64 / 4 - STUB_TOKENS).max(0))
                .sum(),
            _ => 0,
        },
        _ => 0,
    }
}

/// The trace grown one message at a time.
#[derive(Default, Clone)]
pub struct Tracer {
    pub msgs: Vec<Value>,
    pub units: Vec<Unit>,
    pub steps: Vec<usize>,
    pub used: FxHashMap<usize, Set>,
    pub at: FxHashMap<(usize, usize), usize>,
    latest: FxHashMap<Sym, usize>,
    tool_names: FxHashMap<String, String>,
}

impl Tracer {
    fn units_of(&mut self, mi: usize, m: &Value, int: &mut Interner) -> Vec<Unit> {
        let mut out = Vec::new();
        let user = m.get("role").and_then(Value::as_str) == Some("user");
        for (bi, b) in blocks(m.get("content")).iter().enumerate() {
            if !b.is_object() {
                continue;
            }
            let t = b.get("type").and_then(Value::as_str);
            let (kind, tool, text): (UKind, String, std::borrow::Cow<'_, str>) = if user {
                match t {
                    Some("text") => (UKind::User, String::new(), get_str(b, "text").into()),
                    Some("tool_result") => (
                        UKind::Result,
                        self.tool_names.get(&key_of(b.get("tool_use_id"))).cloned().unwrap_or_else(|| "?".into()),
                        text_of(b.get("content")).into_owned().into(),
                    ),
                    _ => continue,
                }
            } else {
                match t {
                    Some("text") => (UKind::Text, String::new(), get_str(b, "text").into()),
                    Some("thinking") => (UKind::Thinking, String::new(), get_str(b, "thinking").into()),
                    Some("tool_use") => {
                        let name = get_str(b, "name").to_string();
                        self.tool_names.insert(key_of(b.get("id")), name.clone());
                        (UKind::Call, name, flat(b.get("input")).into())
                    }
                    _ => continue,
                }
            };
            let (tokens, media) = est_media(b);
            let opaque = media > 0;
            out.push(Unit {
                msg: mi,
                block: bi,
                kind,
                tool,
                tokens,
                words: words(&text, int),
                saving: saving(kind, b, tokens, opaque),
                uses: Vec::new(),
                word_first: FxHashMap::default(),
            });
        }
        out
    }

    pub fn add(&mut self, m: Value, int: &mut Interner) {
        let mi = self.msgs.len();
        let mu = self.units_of(mi, &m, int);
        let assistant = m.get("role").and_then(Value::as_str) == Some("assistant");
        self.msgs.push(m);
        if assistant && mi > 0 {
            let mut copied: Vec<Sym> = Vec::new();
            for u in &mu {
                if matches!(u.kind, UKind::Text | UKind::Call) {
                    copied.extend(u.words.iter().copied().filter(|w| self.latest.contains_key(w)));
                }
            }
            let copied = set_of(copied);
            for &w in &copied {
                let u = &mut self.units[self.latest[&w]];
                if u.uses.last() != Some(&mi) {
                    u.uses.push(mi);
                }
                u.word_first.entry(w).or_insert(mi);
            }
            self.steps.push(mi);
            self.used.insert(mi, copied);
        }
        for u in mu {
            let idx = self.units.len();
            self.at.insert((u.msg, u.block), idx);
            for &w in &u.words {
                self.latest.insert(w, idx);
            }
            self.units.push(u);
        }
    }

    /// The block a unit was made from.
    pub fn block(&self, i: usize) -> &Value {
        let u = &self.units[i];
        match self.msgs[u.msg].get("content") {
            Some(Value::Array(a)) => &a[u.block],
            _ => &NULL_TEXT,
        }
    }

    /// The text of a unit (``learn.unit_text``).
    pub fn unit_text(&self, i: usize) -> std::borrow::Cow<'_, str> {
        let u = &self.units[i];
        if let Some(Value::String(s)) = self.msgs[u.msg].get("content") {
            // a string content is one text block
            return s.as_str().into();
        }
        let b = self.block(i);
        match u.kind {
            UKind::Result => text_of(b.get("content")),
            UKind::Call => flat(b.get("input")).into(),
            _ => {
                let t = get_str(b, "text");
                if !t.is_empty() {
                    t.into()
                } else {
                    get_str(b, "thinking").into()
                }
            }
        }
    }
}

static NULL_TEXT: Value = Value::Null;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_and_web_file_names_are_names() {
        let mut int = Interner::default();
        let text = "load sales.xlsx, fix schema.graphql, index.astro, logo.svg and model.parquet";
        let got: Vec<String> = words(text, &mut int).into_iter().map(|k| int.get(k).to_string()).collect();
        for name in ["sales.xlsx", "schema.graphql", "index.astro", "logo.svg", "model.parquet"] {
            assert!(got.iter().any(|w| w == name), "{name}: {got:?}");
        }
    }
}
