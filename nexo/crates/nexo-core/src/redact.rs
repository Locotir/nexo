//! Secret redaction. [`redact`] takes the keys, tokens and passwords out of a text before it is shown,
//! audited or sent to a model. It is deliberately greedy: a false hit costs a word, a miss costs a secret.

use std::borrow::Cow;
use std::sync::OnceLock;

use regex::Regex;

/// What a secret is replaced with.
pub const MASK: &str = "[redacted]";

struct Rule {
    re: Regex,
    /// The replacement, with `$1`-style groups kept from the match.
    with: &'static str,
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        // The `\x73` spells the first letter of the OpenAI-style prefix so the pattern text itself never reads as a key.
        let table: &[(&str, &str)] = &[
            // PEM private keys, up to the footer or the end of the text.
            (r"-----BEGIN [A-Z ]*KEY-----[\s\S]*?(?:-----END [A-Z ]*KEY-----|\z)", "[redacted key block]"),
            // OpenAI-style, GitHub, Slack and AWS keys.
            (r"\b\x73k-[A-Za-z0-9_-]{16,}", MASK),
            (r"\bgh[pousr]_[A-Za-z0-9]{20,}", MASK),
            (r"\bgithub_pat_[A-Za-z0-9_]{20,}", MASK),
            (r"\bxox[abeprs]-[A-Za-z0-9-]{10,}", MASK),
            (r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", MASK),
            // JSON web tokens.
            (r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{4,}", MASK),
            // Authorization headers.
            (r"(?i)\b(bearer)\s+[A-Za-z0-9._~+/=-]{8,}", "$1 [redacted]"),
            // Lines of an env file: NAME=value.
            (r"(?m)^([ \t]*(?:export[ \t]+)?[A-Z][A-Z0-9_]*[ \t]*=[ \t]*)[^\r\n]+$", "${1}[redacted]"),
            // password=..., "token": "...", api_key: ...
            (
                r#"(?i)\b([A-Za-z0-9_.-]*(?:password|passwd|secret|token|api[_-]?key)[A-Za-z0-9_.-]*["']?\s*[=:]\s*)("[^"\r\n]*"|'[^'\r\n]*'|[^\s"',;&\[\]{}()]+)"#,
                "${1}[redacted]",
            ),
        ];
        table
            .iter()
            .map(|(p, with)| Rule { re: Regex::new(p).expect("a fixed pattern"), with })
            .collect()
    })
}

/// `text` without its secrets. Borrowed when there was nothing to take out. Running it twice changes nothing.
pub fn redact(text: &str) -> Cow<'_, str> {
    let mut out = Cow::Borrowed(text);
    for rule in rules() {
        if let Cow::Owned(s) = rule.re.replace_all(&out, rule.with) {
            out = Cow::Owned(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic key: the parts are joined at run time so no file holds a literal one.
    fn key(prefix: &str, body: &str) -> String {
        [prefix, body].concat()
    }

    fn hides(text: &str, secret: &str) {
        let out = redact(text);
        assert!(!out.contains(secret), "{secret:?} survived in {out:?}");
        assert!(out.contains("[redacted"), "no mask in {out:?}");
        assert_eq!(redact(&out), out, "not idempotent");
    }

    #[test]
    fn openai_style_key() {
        let k = key("s", "k-abcdefghijklmnopqrstuv1234");
        hides(&format!("use {k} for it"), &k);
    }

    #[test]
    fn github_token() {
        let k = key("gh", "p_abcdefghijklmnopqrstuvwxyz0123456789");
        hides(&format!("GH {k}."), &k);
    }

    #[test]
    fn slack_token() {
        let k = key("xo", "xb-1234567890-abcdefghij");
        hides(&format!("slack {k}"), &k);
    }

    #[test]
    fn aws_key() {
        let k = key("AK", "IAABCDEFGHIJKLMNOP");
        hides(&format!("id {k} here"), &k);
    }

    #[test]
    fn jwt() {
        let k = key("ey", "Jhbgcifake.eyJzdWIfakebody.sigfake1");
        hides(&format!("tok {k}"), &k);
    }

    #[test]
    fn pem_block() {
        let t = "before\n-----BEGIN PRIVATE KEY-----\nAAAABBBBCCCC\nDDDD\n-----END PRIVATE KEY-----\nafter";
        let out = redact(t);
        assert!(!out.contains("AAAABBBB"));
        assert!(out.starts_with("before\n") && out.ends_with("\nafter"));
        // A footer that never comes: the rest of the text goes.
        let cut = redact("x -----BEGIN RSA PRIVATE KEY-----\nAAAABBBB");
        assert!(!cut.contains("AAAABBBB"));
    }

    #[test]
    fn bearer_header() {
        let out = redact("Authorization: Bearer abcdef1234567890");
        assert!(!out.contains("abcdef1234567890"));
        assert!(out.contains("Bearer [redacted]"), "{out}");
    }

    #[test]
    fn assignments() {
        for (text, secret) in [
            ("password=hunter2hunter", "hunter2hunter"),
            ("DB_TOKEN=\"abc def ghi\"", "abc def ghi"),
            ("curl -d secret=topvalue123 url", "topvalue123"),
            ("{\"api_key\": \"zzzz9999\"}", "zzzz9999"),
            ("client-secret: s3cr3tvalue", "s3cr3tvalue"),
        ] {
            hides(text, secret);
        }
    }

    #[test]
    fn env_file_values() {
        let out = redact("# config\nHOST=example.org\nexport PORT=8080\nlower=kept");
        assert!(!out.contains("example.org") && !out.contains("8080"), "{out}");
        assert!(out.contains("HOST=[redacted]") && out.contains("export PORT=[redacted]"));
        assert!(out.contains("# config") && out.contains("lower=kept"));
    }

    #[test]
    fn plain_text_is_borrowed() {
        let t = "cargo test --workspace passed, 12 tests in 3.4s";
        assert!(matches!(redact(t), Cow::Borrowed(_)));
    }
}
