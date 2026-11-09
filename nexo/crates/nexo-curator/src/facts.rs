//! The keys of a line: every word, number and name it holds, and its paths, versions and identifiers whole.
//!
//! The index (`index.rs`) finds a line by these. Nothing is dropped for being short or made of digits ("412" is
//! the key of "topic 412"), and nothing is listed by domain: the only words left out are single letters. A key is
//! the hash of the lower-cased text, so the index keeps no copy of the line.

use std::hash::Hasher;

use rustc_hash::FxHasher;

/// Characters of a line that are read; the rest of a longer one is a paste, not a fact.
pub const LINE_CHARS: usize = 400;

/// Keys one line gives, at most.
const LINE_KEYS: usize = 48;

/// Longest word kept whole as a path, version or identifier.
const WORD_CHARS: usize = 200;

/// The key of a word.
pub fn key(word: &str) -> u64 {
    let mut h = FxHasher::default();
    h.write(word.to_lowercase().as_bytes());
    h.finish()
}

/// Whether ``w`` is a number with dots: a version or a decimal (`1.4.2`, `0.75`), kept whole.
fn dotted(w: &str) -> bool {
    w.contains('.') && w.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// The keys of ``line`` into ``out`` (sorted, without repeats): its words, its numbers of any length (and a number
/// with the word before it), and each path (and its last part), version or identifier written with `/ . _ - : =` whole.
pub fn keys(line: &str, out: &mut Vec<u64>) {
    out.clear();
    let line = match line.char_indices().nth(LINE_CHARS) {
        Some((i, _)) => &line[..i],
        None => line,
    };
    let mut prev = "";
    for raw in line.split_whitespace() {
        let w = raw
            .trim_start_matches(|c: char| !(c.is_alphanumeric() || matches!(c, '/' | '~' | '.' | '_')))
            .trim_end_matches(|c: char| !(c.is_alphanumeric() || c == '_' || c == '/'));
        if w.is_empty() {
            continue;
        }
        // a number after a word (`topic 500`, `port 8080`) is also one key with it: the number alone is in many places
        if !prev.is_empty() && w.bytes().all(|b| b.is_ascii_digit()) {
            out.push(key(&format!("{prev} {w}")));
        }
        prev = if w.bytes().all(|b| b.is_ascii_alphabetic()) && w.len() > 1 { w } else { "" };
        if dotted(w) {
            out.push(key(w));
            continue;
        }
        let mut parts = 0;
        for run in w.split(|c: char| !c.is_alphanumeric()).filter(|r| !r.is_empty()) {
            parts += 1;
            if run.chars().count() > 1 || run.bytes().all(|b| b.is_ascii_digit()) {
                out.push(key(run));
            }
        }
        if (parts > 1 || w.contains(|c: char| !c.is_alphanumeric())) && w.len() <= WORD_CHARS {
            out.push(key(w));
            if let Some(last) = w.trim_end_matches('/').rsplit('/').next().filter(|l| *l != w && l.len() > 1) {
                out.push(key(last));
            }
        }
        // `name=value` and `host:port` keep the value whole as well as the name
        if let Some((_, v)) = w.split_once(['=', ':']).filter(|(a, v)| !a.is_empty() && !v.is_empty()) {
            out.push(key(v));
        }
        if out.len() >= 4 * LINE_KEYS {
            break;
        }
    }
    out.sort_unstable();
    out.dedup();
    out.truncate(LINE_KEYS);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(line: &str, word: &str) -> bool {
        let mut v = Vec::new();
        keys(line, &mut v);
        v.contains(&key(word))
    }

    #[test]
    fn numbers_of_any_length_are_keys() {
        assert!(has("[topic 412] port = 54321", "412"));
        assert!(has("[topic 4] port = 54321", "4"));
        assert!(has("[topic 412] port = 54321", "54321"));
        assert!(has("[topic 412] port = 54321", "port"));
        assert!(has("it was 1999 then", "1999"));
    }

    #[test]
    fn paths_versions_and_identifiers_are_kept_whole_and_by_part() {
        let l = "cat /srv/t412/service.conf, then 3.17.42 of shell_rc = ~/.bashrc";
        for k in [
            "/srv/t412/service.conf",
            "service.conf",
            "srv",
            "t412",
            "conf",
            "3.17.42",
            "shell_rc",
            "shell",
            "rc",
            "~/.bashrc",
            ".bashrc",
            "bashrc",
        ] {
            assert!(has(l, k), "{k}");
        }
        assert!(!has(l, "3"), "a version is one key");
        assert!(has("staging at stage371.internal:8797", "8797"));
        assert!(has("id=0000abcd in 12 ms", "0000abcd"));
    }

    #[test]
    fn a_number_is_also_a_key_with_the_word_before_it() {
        assert!(has("[topic 500] owner = kestrel317", "topic 500"));
        assert!(has("What is the owner of the service in topic 500?", "Topic 500"));
        assert!(!has("handled request in 500 ms", "topic 500"));
    }

    #[test]
    fn single_letters_and_case_do_not_count() {
        assert!(!has("a b c", "a"));
        assert!(has("Dunmore042", "dunmore042"));
        let mut v = Vec::new();
        keys(&"word ".repeat(1000), &mut v);
        assert_eq!(v, vec![key("word")]);
    }
}
