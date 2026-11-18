//! Words of a task or a passage, as the selections weigh them.

use super::*;

pub(super) const STOP: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "any", "can", "had", "her", "was", "one", "our", "out",
    "has", "him", "his", "how", "its", "may", "new", "now", "old", "see", "two", "way", "who", "did", "get", "let",
    "put", "say", "she", "too", "use", "that", "with", "have", "this", "will", "your", "from", "they", "been", "were",
    "what", "when", "where", "which", "while", "there", "their", "them", "then", "than", "into", "only", "also",
    "some", "such", "each", "more", "most", "very", "here", "these", "those", "being", "does", "done", "over", "under",
    "please", "need", "needs", "want", "like", "make", "sure", "okay", "yes", "why", "should", "would", "could",
    "about", "after", "before", "just", "find", "look", "check", "tell", "show", "file", "files",
];

/// Topic words: words of at least 3 letters (identifiers split at `_` and
/// camelCase), lower-cased and cut to 5 characters, plus [`names`].
pub fn terms(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for tok in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        for part in tok.split('_') {
            for piece in humps(part) {
                if piece.chars().count() < 3 || piece.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                let low = piece.to_lowercase();
                if STOP.contains(&low.as_str()) {
                    continue;
                }
                out.insert(low.chars().take(5).collect());
            }
        }
    }
    out.extend(names(text));
    out
}

/// `parseLine` -> `parse`, `Line`.
pub(super) fn humps(s: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut start = 0;
    let b: Vec<(usize, char)> = s.char_indices().collect();
    for k in 1..b.len() {
        if b[k].1.is_uppercase() && b[k - 1].1.is_lowercase() {
            out.push(&s[start..b[k].0]);
            start = b[k].0;
        }
    }
    out.push(&s[start..]);
    out
}

/// Distinctive tokens: paths, file names and code-like identifiers (with `_`,
/// digits or camelCase), at least 4 characters.
pub fn names(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for raw in text.split(|c: char| !(c.is_alphanumeric() || "_./-".contains(c))) {
        let w = raw.trim_matches(|c: char| ".-/".contains(c));
        if w.chars().count() < 4 || w.chars().all(|c| c.is_ascii_digit() || c == '.') {
            continue;
        }
        let letters = w.chars().any(char::is_alphabetic);
        let file = w.rsplit_once('.').is_some_and(|(a, e)| {
            a.len() >= 2 && (1..=4).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric())
        });
        let code = w.contains('_')
            || (w.chars().any(|c| c.is_ascii_digit()) && letters && !w.contains('.'))
            || humps(w).len() > 1;
        if letters && (w.contains('/') || file || (code && !w.contains('.'))) {
            out.insert(w.to_string());
        } else if w.contains('.') {
            // `cfg.epoch_tokens`: the attribute carries the meaning
            out.extend(names(&w.replace('.', " ")));
        }
    }
    out
}

pub(super) const NOT_REQUEST: &[&str] =
    &["<system-reminder>", "<local-command", "<command-", "Caveat:", "[Request interrupted", "[background jobs ended]"];

pub(super) fn is_request(text: &str) -> bool {
    let t = text.trim_start();
    !t.is_empty() && !NOT_REQUEST.iter().any(|p| t.starts_with(p))
}
