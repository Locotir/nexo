//! The trace of a conversation: its units (blocks), the distinctive words in them, and which of those
//! words the agent copied later (a use of the unit).

use std::sync::{Arc, LazyLock};

use crate::sorted::{FxHashMap, FxHashSet};
use serde_json::Value;

use crate::evidence::{self, Runs};
use crate::json::est_media;
use crate::msg::{blocks, flat, get_str, key_of, text_of};
use crate::textops;

/// An interned string.
pub type Sym = u32;

/// Strings seen by one engine, each stored once.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Interner {
    // (both behind `Arc`: a copy of the engine shares them, and one is copied only when a new string comes
    // while the other copy still lives)
    map: Arc<FxHashMap<Box<str>, Sym>>,
    strs: Arc<Vec<Box<str>>>,
}

impl Interner {
    pub fn intern(&mut self, s: &str) -> Sym {
        if let Some(&k) = self.map.get(s) {
            return k;
        }
        let k = self.strs.len() as Sym;
        Arc::make_mut(&mut self.strs).push(s.into());
        Arc::make_mut(&mut self.map).insert(s.into(), k);
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

/// A token: a number in hex with its prefix (`0x401140`, an AT&T immediate `$0x3039`), an identifier (with
/// the dots, slashes and dashes that join a path or a name), a number with dots (a version), or a number
/// of four digits or more, hex digits included (`401140`, `40114b`, as a disassembly writes addresses).
/// An identifier starts with a letter of any script (`ОбработкаЗаказа`, `Größe_max`), `_` or `$`, then
/// letters, their marks, digits, `_` and `$`. A script written without spaces (`textops::unspaced`) is no part of one:
/// a name written next to it (`修改config.py中的函数`, `config.py를`) is the name alone.
static WORD: LazyLock<regex::Regex> = LazyLock::new(|| {
    let u = textops::unspaced_class();
    let (first, rest) = (format!(r"[\p{{L}}_$--[{u}]]"), format!(r"[\p{{L}}\p{{M}}\p{{N}}_$--[{u}]]"));
    regex::Regex::new(&format!(
        r"\$?0[xX][0-9a-fA-F]+|{first}{rest}*(?:[./-]{rest}+)*|\p{{Nd}}+(?:\.\p{{Nd}}+)+|\p{{Nd}}[0-9a-fA-F]{{3,}}"
    ))
    .expect("static pattern")
});

/// A number with thousands separators, its decimals included.
static GROUPED: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\b\d{1,3}(?:,\d{3})+(?:\.\d+)?\b").unwrap());

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
    if textops::len(w) < 4 {
        return false;
    }
    if textops::str_isdigit(w) {
        if textops::len(w) == 4 {
            // int() of unicode digits: only ASCII years matter here
            if let Ok(y) = w.parse::<u32>() {
                return !(1900..=2099).contains(&y);
            }
        }
        return true;
    }
    if w.contains('_') || w.contains('$') || w.chars().any(textops::is_digit) {
        return true;
    }
    let tail = &w[w.chars().next().map_or(0, char::len_utf8)..];
    tail != textops::lower(tail) && tail != textops::upper(tail)
}

fn has_ext(last: &str, list: &[&str]) -> bool {
    let low = textops::lower(last);
    !low.is_empty() && list.contains(&low.as_ref())
}

/// Distinctive tokens of a text (``hindsight.words``), as a sorted set.
pub fn words(text: &str, int: &mut Interner) -> Set {
    let common = &*GENERAL;
    let mut out = Vec::new();
    // a markdown reply escapes the underscore of a name (`purge\_expired`)
    let unescaped;
    let text = if text.contains("\\_") {
        unescaped = text.replace("\\_", "_");
        unescaped.as_str()
    } else {
        text
    };
    // a number written with thousands separators (`4,821,350`) is the number without them
    if text.contains(',') {
        for m in GROUPED.find_iter(text) {
            let n = m.as_str().replace(',', "");
            let ok = textops::len(&n) >= 4 && (!textops::str_isdigit(&n) || distinctive(&n));
            if ok && !common.contains(n.as_str()) {
                out.push(int.intern(&n));
            }
        }
    }
    for m in WORD.find_iter(text) {
        let w = m.as_str();
        let first = textops::first(w).unwrap_or(' ');
        // a number in hex, as each tool writes it (`0x401140`, `$0x3039`, `0x0000000000401140`, `00401140`,
        // `41C64E6D`): its digits without the prefix and the zeros in front, in lower case, so that every
        // spelling of an address or a value is one word, and the agent's words match the listing it read.
        // A decimal number reads the same way; one of fewer than four digits is too common to be a name.
        let bare = w.strip_prefix('$').unwrap_or(w);
        let hex = bare
            .strip_prefix("0x")
            .or_else(|| bare.strip_prefix("0X"))
            .or_else(|| (textops::is_digit(first) && bare.bytes().all(|c| c.is_ascii_hexdigit())).then_some(bare));
        if let Some(h) = hex {
            let n = h.trim_start_matches('0').to_ascii_lowercase();
            if textops::len(&n) >= 4 && (!textops::str_isdigit(&n) || distinctive(&n)) && !common.contains(n.as_str()) {
                out.push(int.intern(&n));
            }
            continue;
        }
        if w.contains('/') || textops::is_digit(first) {
            let ok = textops::len(w) >= 4 && (!textops::str_isdigit(w) || distinctive(w));
            if ok && !common.contains(w) {
                out.push(int.intern(w));
            }
            continue;
        }
        let parts: Vec<&str> = w.split('.').collect();
        if parts.len() > 1 {
            let last = parts[parts.len() - 1];
            if has_ext(last, &FILE_EXT) || (parts.len() > 2 && has_ext(last, &TLD)) {
                if textops::len(w) >= 4 && !common.contains(w) {
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

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum UKind {
    User,
    Result,
    Text,
    Thinking,
    Call,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Unit {
    pub msg: usize,
    pub block: usize,
    pub kind: UKind,
    pub tool: String,
    pub tokens: i64,
    pub words: Set,
    /// File names and code-shaped names a reply may write instead of the whole word.
    pub alias: Set,
    /// The files the call that made a result named.
    pub src: Vec<String>,
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
        // a list of changes (`edits: [{old_string, new_string}]`) folds each long text inside it too
        UKind::Call => b.get("input").map_or(0, folded),
        _ => 0,
    }
}

/// The tokens folding the long texts of arguments `v` saves, those nested in lists and maps included.
fn folded(v: &Value) -> i64 {
    match v {
        Value::String(s) if s.chars().count() > FOLD_CHARS => (s.chars().count() as i64 / 4 - STUB_TOKENS).max(0),
        Value::Array(a) => a.iter().map(folded).sum(),
        Value::Object(m) => m.values().map(folded).sum(),
        _ => 0,
    }
}

/// How many tool results back a call is looked for in the results of the files it names.
const RESULTS_BACK: usize = 64 * 8;

/// The trace grown one message at a time.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Tracer {
    /// Not saved with the state: taken again from the history it covers (`Engine::take_history`).
    #[serde(skip)]
    pub msgs: Vec<Value>,
    pub units: Vec<Unit>,
    pub steps: Vec<usize>,
    pub used: FxHashMap<usize, Set>,
    pub at: FxHashMap<(usize, usize), usize>,
    latest: FxHashMap<Sym, usize>,
    /// Whether the person or the agent wrote a name before any tool result held it.
    origin: FxHashMap<Sym, bool>,
    runs: Runs,
    tool_names: FxHashMap<String, String>,
    call_paths: FxHashMap<String, Vec<String>>,
    /// The lines of the tool results a call was looked for in (`quoted_in`), squashed, by unit. A session edits
    /// the same files again and again, and each call that names one is looked for in the reads of it: without
    /// this, the same reads are squashed again at every such call, and a resumed session does it all at once.
    /// A unit's text never changes (a rewritten history starts a new tracer), and only the units `quoted_in`
    /// still reaches are kept. Not saved with the state: it is filled in again as calls are looked for.
    #[serde(skip)]
    held: FxHashMap<usize, Arc<[String]>>,
    /// The results squashed for `held`: what the tests bound.
    squashed: usize,
}

/// The file names (behind the folders of a path) and the code-shaped names of a unit, with which a reply that
/// writes only the name still matches.
fn aliases(words: &Set, text: &str, int: &mut Interner) -> Set {
    let mut out = Vec::new();
    for &w in words {
        if let Some(b) = evidence::base_name(int.get(w)).map(str::to_string) {
            if b.rsplit_once('.').is_some_and(|(_, e)| has_ext(e, &FILE_EXT)) {
                out.push(int.intern(&b));
            }
        }
    }
    for n in evidence::code_names(text) {
        if !GENERAL.contains(n) {
            out.push(int.intern(n));
        }
    }
    set_of(out)
}

fn path_ext(e: &str) -> bool {
    has_ext(e, &FILE_EXT)
}

impl Tracer {
    fn units_of(&mut self, mi: usize, m: &Value, int: &mut Interner) -> Vec<(Unit, String)> {
        let mut out = Vec::new();
        let user = m.get("role").and_then(Value::as_str) == Some("user");
        for (bi, b) in blocks(m.get("content")).iter().enumerate() {
            if !b.is_object() {
                continue;
            }
            let t = b.get("type").and_then(Value::as_str);
            let mut src = Vec::new();
            let (kind, tool, text): (UKind, String, String) = if user {
                match t {
                    Some("text") => (UKind::User, String::new(), get_str(b, "text").into()),
                    Some("tool_result") => {
                        let id = key_of(b.get("tool_use_id"));
                        src = self.call_paths.get(&id).cloned().unwrap_or_default();
                        (
                            UKind::Result,
                            self.tool_names.get(&id).cloned().unwrap_or_else(|| "?".into()),
                            text_of(b.get("content")).into_owned(),
                        )
                    }
                    _ => continue,
                }
            } else {
                match t {
                    Some("text") => (UKind::Text, String::new(), get_str(b, "text").into()),
                    Some("thinking") => (UKind::Thinking, String::new(), get_str(b, "thinking").into()),
                    Some("tool_use") => {
                        let name = get_str(b, "name").to_string();
                        let id = key_of(b.get("id"));
                        let text = flat(b.get("input"));
                        self.call_paths.insert(id.clone(), evidence::paths_of(&text, &path_ext));
                        self.tool_names.insert(id, name.clone());
                        (UKind::Call, name, text)
                    }
                    _ => continue,
                }
            };
            let (tokens, media) = est_media(b);
            let opaque = media > 0;
            let words = words(&text, int);
            let alias = aliases(&words, &text, int);
            out.push((
                Unit {
                    msg: mi,
                    block: bi,
                    kind,
                    tool,
                    tokens,
                    words,
                    alias,
                    src,
                    saving: saving(kind, b, tokens, opaque),
                    uses: Vec::new(),
                    word_first: FxHashMap::default(),
                },
                text,
            ));
        }
        out
    }

    /// The unit that a word of a reply is a use of: the one that holds it last, unless it is a tool result and
    /// the person or the agent had written the word before any result held it (a reply repeating what it knew is
    /// no proof that it read the result), or a name every project has.
    fn holder_of(&self, w: Sym, int: &Interner) -> Option<usize> {
        let h = *self.latest.get(&w)?;
        let from_result = self.units[h].kind == UKind::Result;
        if from_result && (self.origin.get(&w) == Some(&true) || evidence::ubiquitous(int.get(w))) {
            return None;
        }
        Some(h)
    }

    /// The most recent tool result of a file `path`, from the end back, that `ok` accepts.
    fn result_of_path(&self, path: &str, mut ok: impl FnMut(usize) -> bool) -> Option<usize> {
        (0..self.units.len())
            .rev()
            .filter(|&i| self.units[i].kind == UKind::Result)
            .take(RESULTS_BACK)
            .find(|&i| self.units[i].src.iter().any(|p| evidence::same_path(p, path)) && ok(i))
    }

    /// The most recent tool result of a file `path`, from the end back as `result_of_path` goes, that holds one of
    /// `lines` (squashed) within one of its lines, its viewer's numbers left out.
    fn quoted_in(&mut self, path: &str, lines: &[String]) -> Option<usize> {
        let mut reached = 0;
        let mut found = Vec::new();
        for i in (0..self.units.len()).rev().filter(|&i| self.units[i].kind == UKind::Result).take(RESULTS_BACK) {
            reached = i;
            if self.units[i].src.iter().any(|p| evidence::same_path(p, path)) {
                found.push(i);
            }
        }
        // what lies before the results this search reached is out of reach of every later one too
        if self.held.len() > RESULTS_BACK {
            self.held.retain(|&i, _| i >= reached);
        }
        for i in found {
            let held = match self.held.get(&i) {
                Some(h) => h.clone(),
                None => {
                    let h: Arc<[String]> =
                        self.unit_text(i).lines().map(|l| evidence::squash(evidence::unnumbered(l))).collect();
                    self.squashed += 1;
                    self.held.insert(i, h.clone());
                    h
                }
            };
            if lines.iter().any(|q| held.iter().any(|l| l.contains(q.as_str()))) {
                return Some(i);
            }
        }
        None
    }

    /// The tool results squashed for `quoted_in` so far.
    #[cfg(test)]
    fn squashed(&self) -> usize {
        self.squashed
    }

    /// The units a reply (its units `mu`) used, each with the word that proves it when a word does.
    fn credits(&mut self, mu: &[(Unit, String)], int: &Interner) -> Vec<(usize, Option<Sym>)> {
        let mut out: Vec<(usize, Option<Sym>)> = Vec::new();
        for (u, text) in mu {
            if !matches!(u.kind, UKind::Text | UKind::Call | UKind::Thinking) {
                continue;
            }
            // a word the unit holds that a unit read earlier holds last
            for &w in u.words.iter().chain(&u.alias) {
                if let Some(h) = self.holder_of(w, int) {
                    out.push((h, Some(w)));
                }
            }
            // a line of an argument found whole in the last read of the file the call is about
            let mut directed = false;
            if u.kind == UKind::Call {
                let lines = evidence::quotable_lines(text);
                for p in evidence::paths_of(text, &path_ext) {
                    if let Some(h) = self.quoted_in(&p, &lines) {
                        out.push((h, None));
                        directed = true;
                    }
                }
            }
            // a run of six words the unit quotes
            if !directed {
                for h in evidence::runs(text, &GENERAL) {
                    if let Some(hold) = self.runs.get(h) {
                        let i = hold.latest as usize;
                        if self.units[i].kind != UKind::Result || hold.authored as usize >= self.units[i].msg {
                            out.push((i, None));
                        }
                    }
                }
            }
            // a line of a file the unit points to
            for (p, n) in evidence::line_refs(text, &path_ext) {
                if let Some(h) = self.result_of_path(&p, |i| self.unit_text(i).lines().count() >= n) {
                    out.push((h, None));
                }
            }
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
            for (u, _) in &mu {
                if matches!(u.kind, UKind::Text | UKind::Call | UKind::Thinking) {
                    copied.extend(u.words.iter().copied().filter(|w| self.latest.contains_key(w)));
                }
            }
            let copied = set_of(copied);
            let mut credits = self.credits(&mu, int);
            credits.sort_unstable();
            credits.dedup();
            for (i, w) in credits {
                let u = &mut self.units[i];
                if u.uses.last() != Some(&mi) {
                    u.uses.push(mi);
                }
                if let Some(w) = w {
                    u.word_first.entry(w).or_insert(mi);
                }
            }
            self.steps.push(mi);
            self.used.insert(mi, copied);
        }
        for (u, text) in mu {
            let idx = self.units.len();
            self.at.insert((u.msg, u.block), idx);
            let authored = u.kind != UKind::Result;
            for &w in u.words.iter().chain(&u.alias) {
                self.latest.insert(w, idx);
                self.origin.entry(w).or_insert(authored);
            }
            self.runs.insert(&evidence::runs(&text, &GENERAL), idx, u.msg, authored);
            self.units.push(u);
        }
    }

    /// The unit that holds word `w` last, so far: the one a reply copying `w` next would be a use of.
    pub fn latest_holder(&self, w: Sym) -> Option<usize> {
        self.latest.get(&w).copied()
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

    fn words_of(text: &str) -> Vec<String> {
        let mut int = Interner::default();
        words(text, &mut int).into_iter().map(|k| int.get(k).to_string()).collect()
    }

    #[test]
    fn separators_and_escapes_do_not_hide_a_word() {
        assert_eq!(words_of("the table holds 4,821,350 rows"), ["4821350"]);
        assert_eq!(words_of("total 1,234.56 and 12,345"), ["1234.56", "12345", "234.56"]);
        assert_eq!(words_of("the method purge\\_expired runs"), ["purge_expired"]);
    }

    fn sorted_words(text: &str) -> Vec<String> {
        let mut v = words_of(text);
        v.sort();
        v
    }

    #[test]
    fn a_name_is_a_word_in_any_script_and_a_script_without_spaces_is_none() {
        // Cyrillic, Greek and accented Latin names start a word as an ASCII one does
        assert_eq!(
            sorted_words("Очередь очищает ОчиститьКольцевойБуфер: голова = МАГИЯ_7f41"),
            ["МАГИЯ_7f41", "ОчиститьКольцевойБуфер"]
        );
        assert_eq!(
            sorted_words("set Größe_max and λόγος_2 in Модули/ОбработкаОчереди.bsl"),
            ["Größe_max", "λόγος_2", "Модули/ОбработкаОчереди.bsl"]
        );
        // plain words of any script are no names
        assert!(words_of("Очередь очищается быстро, größer und schöner").is_empty());
        // a name written next to Chinese, Japanese or Korean is the name alone
        assert_eq!(
            sorted_words("队列由zq_flush_ring_buf清空，它把head设为ZQ_MAGIC_7f41后排空整个队列。"),
            ["ZQ_MAGIC_7f41", "zq_flush_ring_buf"]
        );
        assert_eq!(
            sorted_words("config_loader.pyを修正し、load_settings함수를 고쳤습니다"),
            ["config_loader.py", "load_settings"]
        );
    }

    #[test]
    fn every_spelling_of_an_address_is_one_word() {
        // the same function, as objdump, gdb, radare2, nm, readelf and Ghidra print it, and as the agent writes it
        for line in [
            "  40114b:\t3d 7a 1d b6 2f       \tcmp    $0x2fb61d7a,%eax",
            "   0x40114b <check_key+21>:\tcmp    $0x2fb61d7a,%eax",
            "│           0x0040114b      3d7a1db62f     cmp eax, 0x2fb61d7a",
            "000000000040114b T check_key",
            "     1: 000000000040114b    34 FUNC    GLOBAL DEFAULT   14 check_key",
            "  return param_1 * 0x41c64e6d + 0x3039 == 0x2FB61D7A;",
            "the compare at 0x40114b is against 0x2fb61d7a, so key = (0x2fb61d7a - 0x3039) * inverse",
            "at 40114b the key is compared with 2fb61d7a",
        ] {
            let got = words_of(line);
            assert!(got.iter().any(|w| w == "40114b") || got.iter().any(|w| w == "2fb61d7a"), "{line:?}: {got:?}");
            assert!(got.iter().all(|w| !w.starts_with('$') && !w.starts_with("0x") && !w.starts_with('x')), "{got:?}");
        }
        assert_eq!(words_of("0x40114b 40114b 0x000000000040114b $0x40114B 000000000040114b"), ["40114b"]);
        // a short value, a year and a name's digits are what they were; a decimal number keeps its worth
        assert!(words_of("rax 0x1c8 456 in 2024 for job_0042").iter().all(|w| w == "job_0042"));
        assert_eq!(words_of("pointer 140737488348120 and 0xdeadbeef"), ["140737488348120", "deadbeef"]);
        assert_eq!(words_of("version 1.2.3 of pkg_7f"), ["1.2.3", "pkg_7f"]);
    }

    #[test]
    fn each_read_of_a_file_is_squashed_once_however_many_calls_name_the_file() {
        let mut int = Interner::default();
        let mut tr = Tracer::default();
        let file: String = (0..200).map(|n| format!("{n:>6}\tlet value_{n} = compute_{n}(input, {n});\n")).collect();
        tr.add(serde_json::json!({"role": "user", "content": "Tidy src/app.rs"}), &mut int);
        let mut id = 0;
        let mut step = |tr: &mut Tracer, input: Value, output: &str| {
            id += 1;
            let call = format!("c{id}");
            tr.add(
                serde_json::json!({"role": "assistant", "content": [{"type": "tool_use", "id": call, "name": "edit", "input": input}]}),
                &mut int,
            );
            tr.add(
                serde_json::json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": call, "content": output}]}),
                &mut int,
            );
        };
        for _ in 0..40 {
            step(&mut tr, serde_json::json!({"path": "src/app.rs"}), &file);
        }
        // edits whose text no read holds: each looks through every read of the file within reach
        for n in 0..40 {
            let old = format!("a line written nowhere before number {n}");
            step(&mut tr, serde_json::json!({"path": "src/app.rs", "old_string": old, "new_string": "x"}), "ok");
        }
        let results = tr.units.iter().filter(|u| u.kind == UKind::Result).count();
        assert_eq!(results, 80);
        assert!(tr.squashed() <= results, "{} squashes for {results} results", tr.squashed());
        // an edit that quotes a line of the file is still the use of the latest read that holds it
        step(
            &mut tr,
            serde_json::json!({"path": "src/app.rs", "old_string": "let value_7 = compute_7(input, 7);", "new_string": "y"}),
            "ok",
        );
        let last_read = (0..tr.units.len())
            .rev()
            .find(|&i| tr.units[i].kind == UKind::Result && tr.unit_text(i).contains("value_7"))
            .unwrap();
        let reply = tr.msgs.len() - 2;
        assert!(tr.units[last_read].uses.contains(&reply), "{:?}", tr.units[last_read].uses);
    }

    #[test]
    fn a_listing_is_used_when_the_agent_writes_an_address_it_printed() {
        let mut int = Interner::default();
        let mut tr = Tracer::default();
        let listing = "0000000000401136 <check_key>:\n  401136:\t55                   \tpush   %rbp\n  40114b:\t3d 7a 1d b6 2f       \tcmp    $0x2fb61d7a,%eax\n  401150:\t0f 94 c0             \tsete   %al";
        tr.add(serde_json::json!({"role": "user", "content": "Where is the key compared?"}), &mut int);
        tr.add(
            serde_json::json!({"role": "assistant", "content": [{"type": "tool_use", "id": "c1", "name": "bash", "input": {"command": "objdump -d realm"}}]}),
            &mut int,
        );
        tr.add(
            serde_json::json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "c1", "content": listing}]}),
            &mut int,
        );
        tr.add(
            serde_json::json!({"role": "assistant", "content": "The compare is at 0x40114b, against 0x2fb61d7a."}),
            &mut int,
        );
        let mut used: Vec<&str> = tr.used[&3].iter().map(|&w| int.get(w)).collect();
        used.sort_unstable();
        assert_eq!(used, ["2fb61d7a", "40114b"]);
        let result = tr.units.iter().find(|u| u.kind == UKind::Result).unwrap();
        assert_eq!(result.uses, [3]);
    }
}
