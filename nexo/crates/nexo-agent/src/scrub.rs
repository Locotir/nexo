//! Taking secrets out of text that leaves the session: shared files (`nexo export`) and the audit log.
//!
//! [`scrub`] builds on the core redaction (keys, tokens, passwords, env lines) and adds long base64
//! looking strings. Calls that touch a `.env*` file are caught by [`touches_env_file`], so their
//! arguments and results can be dropped whole instead of pattern by pattern.

use std::sync::OnceLock;

use nexo_core::redact::{redact, MASK};
use regex::Regex;
use serde_json::Value;

fn long_secret() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // 40 or more characters of the base64 alphabet in one run
    RE.get_or_init(|| Regex::new(r"[A-Za-z0-9+/_-]{40,}={0,2}").expect("a fixed pattern"))
}

fn env_word() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `.env`, `.env.local`, `dir/.env.production` as a word of a shell command
    RE.get_or_init(|| {
        Regex::new(r#"(?:^|[\s/\\"'=<>])\.env(?:\.[\w.-]*)?(?:$|[\s"'|;&<>)])"#).expect("a fixed pattern")
    })
}

/// A run of letters that looks random: it has a digit, a lower case and an upper case letter, so file
/// paths, words joined by dashes and hex hashes pass.
fn looks_random(s: &str) -> bool {
    let has = |f: fn(&char) -> bool| s.chars().any(|c| f(&c));
    has(char::is_ascii_digit) && has(char::is_ascii_lowercase) && has(char::is_ascii_uppercase)
}

/// `text` without keys, tokens, passwords and long random strings.
pub fn scrub(text: &str) -> String {
    let first = redact(text);
    long_secret()
        .replace_all(&first, |c: &regex::Captures| {
            let m = &c[0];
            if looks_random(m) {
                MASK.to_string()
            } else {
                m.to_string()
            }
        })
        .into_owned()
}

/// [`scrub`] on every string inside a JSON value.
pub fn scrub_value(v: &mut Value) {
    match v {
        Value::String(s) => {
            let t = scrub(s);
            if t != *s {
                *s = t;
            }
        }
        Value::Array(a) => a.iter_mut().for_each(scrub_value),
        Value::Object(o) => o.values_mut().for_each(scrub_value),
        _ => {}
    }
}

/// Whether a file name is a `.env*` file.
pub fn is_env_name(path: &str) -> bool {
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    base == ".env" || base.starts_with(".env.")
}

/// Whether the arguments of a tool call name a `.env*` file, as a path or inside a shell command.
pub fn touches_env_file(args: &Value) -> bool {
    match args {
        Value::String(s) => is_env_name(s) || env_word().is_match(s),
        Value::Array(a) => a.iter().any(touches_env_file),
        Value::Object(o) => o.values().any(touches_env_file),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn long_random_strings_go_but_hashes_and_paths_stay() {
        let secret = ["Zm9vYmFyQmF6", "UXV4MTIzNDU2Nzg5", "MGFiY2RlZmdoaWo"].concat();
        assert!(!scrub(&format!("key {secret} end")).contains(&secret));
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(scrub(sha), sha);
        let path = "crates/nexo-agent/src/some-very-long-file-name-for-a-test.rs";
        assert_eq!(scrub(path), path);
    }

    #[test]
    fn env_files_are_found_in_paths_and_commands() {
        assert!(touches_env_file(&json!({"path": "app/.env.local"})));
        assert!(touches_env_file(&json!({"command": "cat .env | head"})));
        assert!(touches_env_file(&json!({"command": "grep X ./.env"})));
        assert!(!touches_env_file(&json!({"path": "src/environment.rs"})));
        assert!(!touches_env_file(&json!({"command": "echo .environment"})));
    }
}
