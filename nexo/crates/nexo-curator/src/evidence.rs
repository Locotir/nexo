//! What counts as proof that a reply used an earlier unit, beyond one shared word: a quoted run of words,
//! an exact line of an edit, a `path:line` reference, a code-shaped name, a file name. Everything here is
//! about the text only; `hindsight` decides who gets the credit.

use std::hash::{Hash, Hasher};
use std::sync::LazyLock;

use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};

/// Words in a run that are common to every file and say nothing about which one it came from.
const KEYWORDS: &[&str] = &[
    "import", "return", "function", "const", "class", "static", "public", "private", "void", "super", "while", "async",
    "await", "export", "struct", "using", "include", "package", "define", "println", "print", "false", "print_r",
    "string",
];

/// Names and files every project has, which a listing or a reply mentions without reading one of them.
const UBIQUITOUS: &[&str] = &[
    "__init__",
    "__main__",
    "__name__",
    "__init__.py",
    "__main__.py",
    "user_id",
    "file_name",
    "file_path",
    "Cargo.toml",
    "Cargo.lock",
    "README.md",
    "readme.md",
    "LICENSE.md",
    "CHANGELOG.md",
    "package.json",
    "package-lock.json",
    "tsconfig.json",
    "pyproject.toml",
    "requirements.txt",
    "setup.py",
    "setup.cfg",
    "main.rs",
    "lib.rs",
    "mod.rs",
    "build.rs",
    "main.py",
    "app.py",
    "index.js",
    "index.ts",
    "index.html",
    "main.go",
    "go.mod",
    "go.sum",
    "main.c",
    "main.cpp",
    "Dockerfile",
    "docker-compose.yml",
    ".gitignore",
    "node_modules",
    "yarn.lock",
    "style.css",
    "main.js",
    "app.js",
    "test.py",
];

static UBIQ: LazyLock<FxHashSet<&'static str>> = LazyLock::new(|| UBIQUITOUS.iter().copied().collect());

/// A word too common in projects to show that a reply read one place.
pub fn ubiquitous(w: &str) -> bool {
    UBIQ.contains(w)
}

/// Tokens of a run: letters, digits and underscores, lower-cased.
fn tokens(line: &str) -> impl Iterator<Item = String> + '_ {
    line.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|t| !t.is_empty()).map(str::to_lowercase)
}

/// The line without the number a viewer puts in front of it (`    12<TAB>text`, `12→text`).
pub fn unnumbered(line: &str) -> &str {
    let t = line.trim_start();
    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits > 0 && digits < 8 {
        let rest = &t[digits..];
        if let Some(r) = rest.strip_prefix('\t').or_else(|| rest.strip_prefix('→')) {
            return r;
        }
    }
    line
}

/// Whitespace collapsed to single spaces.
pub fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const RUN: usize = 6;
const RUN_CHARS: usize = 30;

fn hash_of<T: Hash>(v: &T) -> u64 {
    let mut h = FxHasher::default();
    v.hash(&mut h);
    h.finish()
}

/// The runs of six words a text holds that tell it apart (at least two words of five letters or more that
/// are not code keywords, and thirty characters), keeping half of them, chosen by their content, so that a
/// text and its quote pick the same ones.
pub fn runs(text: &str, general: &FxHashSet<&'static str>) -> Vec<u64> {
    let mut out = Vec::new();
    for line in text.lines() {
        let toks: Vec<String> = tokens(unnumbered(line)).collect();
        if toks.len() < RUN {
            continue;
        }
        for w in toks.windows(RUN) {
            let chars: usize = w.iter().map(String::len).sum::<usize>() + RUN - 1;
            if chars < RUN_CHARS {
                continue;
            }
            let mut inf: Vec<&str> = w
                .iter()
                .map(String::as_str)
                .filter(|t| t.len() >= 5 && !KEYWORDS.contains(t) && !general.contains(t))
                .collect();
            inf.sort_unstable();
            inf.dedup();
            if inf.len() < 2 {
                continue;
            }
            let h = hash_of(&w);
            if h & 0x100 == 0 {
                out.push(h);
            }
        }
    }
    out
}

/// Who held a run: the unit that holds it last, and the first message of the person or the agent that held it.
#[derive(Clone, Copy)]
pub struct Hold {
    pub latest: u32,
    pub authored: u32,
}

/// The runs of every unit so far.
#[derive(Default, Clone)]
pub struct Runs {
    held: FxHashMap<u64, Hold>,
}

impl Runs {
    pub fn insert(&mut self, hashes: &[u64], unit: usize, msg: usize, authored: bool) {
        for &h in hashes {
            let e = self.held.entry(h).or_insert(Hold { latest: unit as u32, authored: u32::MAX });
            e.latest = unit as u32;
            if authored {
                e.authored = e.authored.min(msg as u32);
            }
        }
    }

    pub fn get(&self, h: u64) -> Option<Hold> {
        self.held.get(&h).copied()
    }
}

static PATHISH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"[^\s"'`,;()\[\]{}<>=|&]+"#).unwrap());

/// The file paths a text names: a word with a slash or a known extension, without the `./` in front and the
/// `:line` behind.
pub fn paths_of(text: &str, ext_ok: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in PATHISH.find_iter(text) {
        let mut t = m.as_str().trim_end_matches([':', '.', '!', '?']);
        if t.starts_with("http") || t.contains('*') || t.starts_with('-') {
            continue;
        }
        if let Some((head, tail)) = t.rsplit_once(':') {
            if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) {
                t = head;
            }
        }
        let t = t.trim_start_matches("./");
        let last = t.rsplit('/').next().unwrap_or(t);
        let ext = last.rsplit_once('.').map(|(_, e)| e);
        if t.len() >= 3 && ext.is_some_and(|e| !e.is_empty() && ext_ok(e)) && !out.iter().any(|p| p == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// Whether two paths name one file (one may be the tail of the other, from a different folder).
pub fn same_path(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim_start_matches("./"), b.trim_start_matches("./"));
    a == b || a.strip_suffix(b).is_some_and(|h| h.ends_with('/')) || b.strip_suffix(a).is_some_and(|h| h.ends_with('/'))
}

/// The file name behind the folders of a path that has some.
pub fn base_name(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(_, f)| f).filter(|f| f.len() >= 4)
}

static LINE_AFTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?::|\s*[,(]?\s*(?:at |on )?lines? |\s*#?L)(\d+)").unwrap());
static LINE_BEFORE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)lines? (\d+)(?:\s*(?:-|to|–)\s*\d+)?\s*(?:of|in|from)\s+$").unwrap());

/// `(path, line)` for each place a text points to a line of a file: `a/b.rs:12`, `a/b.rs line 12`, `line 12 of a/b.rs`.
pub fn line_refs(text: &str, ext_ok: &dyn Fn(&str) -> bool) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for m in PATHISH.find_iter(text) {
        let raw = m.as_str().trim_end_matches(['.', '!', '?', ',']);
        // `path:12` and `path#L12` are one token
        let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
        let (path, inline) = match (raw.rsplit_once(':'), raw.rsplit_once("#L")) {
            (Some((h, t)), _) | (_, Some((h, t))) if digits(t) => (h, t.parse::<usize>().ok()),
            _ => (raw, None),
        };
        let found = paths_of(path, ext_ok);
        let Some(p) = found.first() else { continue };
        let mut line = inline;
        if line.is_none() {
            let after = &text[m.start() + path.len()..];
            let after: String = after.chars().take(24).collect();
            line = LINE_AFTER.captures(&after).and_then(|c| c[1].parse().ok());
        }
        if line.is_none() {
            let before: String =
                text[..m.start()].chars().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect();
            line = LINE_BEFORE.captures(&before).and_then(|c| c[1].parse().ok());
        }
        if let Some(n) = line.filter(|&n| n > 0) {
            out.push((p.clone(), n));
        }
    }
    out
}

static CODE_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`([a-z][a-z0-9]{7,})`|\b([a-z][a-z0-9]{7,})\(").unwrap());

/// Lower-case names written like code (`name(`, or in backticks), as long as a name is not a word of prose.
pub fn code_names(text: &str) -> Vec<&str> {
    CODE_NAME.captures_iter(text).filter_map(|c| c.get(1).or_else(|| c.get(2)).map(|m| m.as_str())).collect()
}

/// The lines of an argument that can be found whole in a file: trimmed, spaces collapsed, ten characters or more.
pub fn quotable_lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in text.lines() {
        let q = squash(unnumbered(l));
        if q.chars().count() >= 10 && !out.contains(&q) {
            out.push(q);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(e: &str) -> bool {
        matches!(e, "py" | "rs" | "md" | "csv")
    }

    #[test]
    fn line_references_in_every_spelling() {
        assert_eq!(line_refs("see src/store.py:12.", &ext), [("src/store.py".to_string(), 12)]);
        assert_eq!(line_refs("see src/store.py#L12", &ext), [("src/store.py".to_string(), 12)]);
        assert_eq!(line_refs("src/store.py line 12 is wrong", &ext), [("src/store.py".to_string(), 12)]);
        assert_eq!(line_refs("look at lines 12-14 of src/store.py", &ext), [("src/store.py".to_string(), 12)]);
        assert!(line_refs("the file src/store.py is fine, line 12 of the other", &ext).is_empty());
        assert!(line_refs("example.com:8080", &ext).is_empty());
    }

    #[test]
    fn paths_match_by_tail() {
        assert!(same_path("./a/b.rs", "a/b.rs"));
        assert!(same_path("/home/u/a/b.rs", "a/b.rs"));
        assert!(!same_path("a/xb.rs", "b.rs"));
        assert_eq!(paths_of("head reports/invoice_2024_q3.csv && ls", &ext), ["reports/invoice_2024_q3.csv"]);
    }
}
