//! The files and commands that calls name, and the agent's own writes of files.

use indexmap::IndexMap;
use rustc_hash::FxHashSet;
use serde_json::Value;

use crate::hindsight::Set;
use crate::json::{est_str, str_chars};
use crate::msg::py_str;
use crate::textops::{self};

/// Tokens of one file the agent wrote kept in the anchor; a longer one keeps its head and tail.
pub(super) const WROTE_TOKENS: i64 = 1500;

/// A file the agent wrote whole: the message and call that wrote it, its text (capped at
/// [`WROTE_TOKENS`]) and the message of a later edit that made the text stale.
#[derive(Clone, Debug)]
pub(super) struct Wrote {
    pub(super) mi: usize,
    pub(super) call: String,
    pub(super) path: String,
    pub(super) text: String,
    pub(super) edited: Option<usize>,
}

/// ``text`` within ``tokens``: its head and tail lines, and how many lines were cut between them. Lines and
/// characters are weighed as the budget is (`est_str`), so a text dense in quotes or control bytes keeps less.
pub(super) fn head_and_tail(text: &str, tokens: i64) -> String {
    if est_str(text) <= tokens {
        return text.to_string();
    }
    let half = (tokens.max(2) * 4 / 2) as usize;
    // what a piece adds to `str_chars`, without the two quotes of a string
    let weight = |s: &str| str_chars(s) - 2;
    let fit = |pieces: &mut dyn Iterator<Item = usize>| {
        let (mut used, mut n) = (0, 0);
        for w in pieces {
            used += w;
            if used > half {
                break;
            }
            n += 1;
        }
        n
    };
    let lines: Vec<&str> = text.lines().collect();
    // at least one line goes, or the cut would only add its note
    let head = fit(&mut lines.iter().map(|l| weight(l) + 1)).min(lines.len().saturating_sub(1));
    let tail = fit(&mut lines.iter().rev().map(|l| weight(l) + 1)).min(lines.len().saturating_sub(head + 1));
    if head == 0 && tail == 0 {
        // one long line, or lines each longer than half the budget: the first and last characters that fit
        let chars: Vec<char> = text.chars().collect();
        let mut buf = [0u8; 4];
        let mut w = |c: &char| weight(c.encode_utf8(&mut buf));
        let a = fit(&mut chars.iter().map(&mut w)).min(chars.len().saturating_sub(1));
        let b = fit(&mut chars.iter().rev().map(&mut w)).min(chars.len().saturating_sub(a + 1));
        let n = chars.len();
        let (a_s, b_s): (String, String) = (chars[..a].iter().collect(), chars[n - b..].iter().collect());
        return format!("{a_s}\n… {} characters cut …\n{b_s}", n - a - b);
    }
    format!(
        "{}\n… {} lines cut …\n{}",
        lines[..head].join("\n"),
        lines.len() - head - tail,
        lines[lines.len() - tail..].join("\n")
    )
}

/// Whether ``path`` looks like a scratch file ("tmp_out.txt", "_try.rs", "scratch.md", anything under
/// "/tmp/"): the first to go for the budget. A leading underscore alone marks a private, partial or
/// framework file ("_app.tsx", "__init__.py", "_variables.scss"), not a scratch one.
pub(super) fn scratch(path: &str) -> bool {
    let name = basename(path).to_ascii_lowercase();
    let path = slashes(path).to_ascii_lowercase();
    ["tmp", "_tmp", "_try", "_scratch", "scratch"].iter().any(|m| name.starts_with(m))
        || path.starts_with("/tmp/")
        || path.starts_with("/var/tmp/")
        || ["%temp%/", "%tmp%/", "$env:temp/", "$env:tmp/"].iter().any(|t| path.starts_with(t))
        || path.contains("/appdata/local/temp/")
        || path.contains(":/windows/temp/")
}

/// A Windows path (`C:\proj\a.rs`, `\\host\share\a.rs`, or one with `\` and no `/`, as `src\a.rs`) with `/`
/// for `\`, so the path helpers read it as they read any other; a path with `/` in it as it is.
fn slashes(path: &str) -> std::borrow::Cow<'_, str> {
    let b = path.as_bytes();
    let drive = b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    if path.contains('\\') && (drive || !path.contains('/')) {
        std::borrow::Cow::Owned(path.replace('\\', "/"))
    } else {
        std::borrow::Cow::Borrowed(path)
    }
}

/// Whether a word with `\` in it is a Windows path (`\proj\a.rs`, `src\a.rs`, `..\a.rs`) rather than a
/// pattern with escapes (`a\|b`, `\d+\.rs`, `foo\.txt`): parts of plain name characters, none of which past
/// the first starts with a dot unless it is `.` or `..`.
fn windows_path(w: &str) -> bool {
    w.strip_prefix("\\\\").unwrap_or(w).split('\\').enumerate().all(|(k, s)| {
        (k == 0 || !s.is_empty())
            && s.chars().all(|c| c.is_alphanumeric() || "._-~@".contains(c))
            && (k == 0 || !s.starts_with('.') || s.chars().all(|c| c == '.'))
    })
}

/// The key of a file in the tracked files: its path as written (a Windows one with `/`), with "//" as "/" and
/// without a leading "./".
pub(super) fn file_key(path: &str) -> String {
    let path = slashes(path);
    let mut key = String::with_capacity(path.len());
    for c in path.chars() {
        if c != '/' || !key.ends_with('/') {
            key.push(c);
        }
    }
    let mut p = key.as_str();
    while let Some(rest) = p.strip_prefix("./") {
        p = rest;
    }
    p.to_string()
}

/// File ``path`` changed in message ``mi`` by an edit of ``size`` characters: the whole writes in ``wrote``
/// of every file it may name (a bare file name names each file of that name), from before, are marked
/// stale, or go when the edit is as big as half of one.
pub(super) fn edited(wrote: &mut IndexMap<String, Wrote>, path: &str, mi: usize, size: usize) {
    wrote.retain(|key, w| {
        if w.mi >= mi || !same_file(key, path) {
            return true;
        }
        if 2 * size >= w.text.chars().count() {
            return false;
        }
        w.edited = Some(mi);
        true
    });
}

/// The lines of what ``old`` wrote, out of the ledger's pool: the file was written again since, and its old text
/// would read as what the file holds.
pub(super) fn forget(pool: &mut Vec<(usize, String, Set, f64)>, old: &Wrote) {
    let lines: FxHashSet<String> = old.text.split('\n').map(textops::squash_ws).collect();
    pool.retain(|(mi, ln, _, _)| *mi != old.mi || !lines.contains(ln));
}

/// Whether two paths name the same file: equal, or one is the other with more directories in front
/// (`src/a.rs` and `/work/src/a.rs`).
pub(super) fn same_file(a: &str, b: &str) -> bool {
    let (a, b) = (slashes(a), slashes(b));
    let a = a.strip_prefix("./").unwrap_or(&a);
    let b = b.strip_prefix("./").unwrap_or(&b);
    let under = |long: &str, short: &str| long.strip_suffix(short).is_some_and(|h| h.ends_with('/'));
    a == b || under(a, b) || under(b, a)
}

/// The last component of a path, POSIX or Windows ("" for one that ends in a separator).
pub(super) fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or("")
}

pub(super) fn command_of(b: &Value) -> String {
    match b.get("input") {
        Some(Value::Object(o)) => o.get("command").map_or(String::new(), py_str),
        _ => String::new(),
    }
}

/// The parts of a text's words (split at anything not a letter or a digit): long ones in lower case, the
/// short ones (one or two characters) as written, where case is all that tells a name from a word.
pub(super) fn name_parts(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|p| !p.is_empty())
        .map(|p| if p.chars().count() > 2 { p.to_lowercase() } else { p.to_string() })
        .collect()
}

/// The files a call names, each as the parts of its name without the extension (`material/assembly-B.txt`
/// → `assembly`, `B`). A file is a word whose last part has a short extension (`.txt`, `.rs`, `.json`) and
/// that is not an option or a pattern: a folder, a range or a regex names no file.
pub(super) fn files_in(call: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for w in call.split(|c: char| c.is_whitespace() || ";|&<>'\"`(),=".contains(c)) {
        // a drive in front (`C:\`, `C:/`) is no part of the name
        let b = w.as_bytes();
        let w = if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/') {
            &w[2..]
        } else {
            w
        };
        if w.starts_with('-') || (w.contains('\\') && !windows_path(w)) || w.contains(['*', '?', '$', ':']) {
            continue;
        }
        let base = basename(w);
        let stem = match base.rsplit_once('.') {
            Some((s, ext))
                if !s.is_empty() && (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
            {
                s
            }
            _ => continue,
        };
        let parts = name_parts(stem);
        if !parts.is_empty() && parts.iter().any(|p| p.chars().any(|c| c.is_alphabetic())) && !out.contains(&parts) {
            out.push(parts);
        }
    }
    out
}
