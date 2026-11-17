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
            // The Basic scheme only with its header prefix: a bare `basic <word>` would eat ordinary prose. The prefix
            // may be quoted, as in a JSON request dump (`"Authorization": "Basic ..."`).
            (r#"(?i)\b(authorization["']?\s*[:=]\s*["']?basic)\s+[A-Za-z0-9+/]{8,}={0,2}"#, "$1 [redacted]"),
            // curl -u user:pass, also --user. Only a value with a colon holds a password; a bare user name is not a secret.
            (r#"(?i)(\bcurl\b[^\r\n]*?\s(?:-u|--user)(?:\s+|=)["']?)[^\s"':]+:[^\s"']+"#, "${1}[redacted]"),
            // Provider key formats that the `sk-` family does not cover.
            (r"\bAIza[0-9A-Za-z_-]{20,}", MASK),
            (r"\bgsk_[0-9A-Za-z]{16,}", MASK),
            (r"\bhf_[A-Za-z0-9]{30,}", MASK),
            (r"\bnpm_[A-Za-z0-9]{36}", MASK),
            (r"\bxai-[A-Za-z0-9]{20,}", MASK),
            (r"\b[\x73r]k_(?:live|test)_[A-Za-z0-9]{16,}", MASK),
            // scheme://user:pass@host: an address carries its credentials before the `@`. HTTP client errors embed the URL.
            // A user name alone (`ssh://git@host`) is not a secret and stays, so notes and mail can name such an address;
            // a lone long token before the `@` (`https://<token>@host`) is a credential and goes.
            (r"(?i)\b([a-z][a-z0-9+.-]*://)(?:[^\s/?#@:]*:[^\s/?#]*|[a-z0-9_-]{24,})@", "${1}[redacted]@"),
            // ?key=..., &sig=..., ;auth=...: query parameters named like a secret (`api_key=` and `token=` are also caught by the assignment rule below).
            (
                r#"(?i)([?&;]\s*[a-z0-9_.-]*(?:key|token|secret|password|passwd|signature|sig|auth)=)[^&\s)"'<>#,`$\[{*][^&\s)"'<>#,`]*"#,
                "${1}[redacted]",
            ),
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
    fn address_credentials_and_secret_query_parameters() {
        hides("GET https://user:hunter2pw@host.example/x failed", "hunter2pw");
        hides("error for url (https://api.example/v1?key=zzz9abc&x=1)", "zzz9abc");
        hides("https://cdn.example/f?sig=q1w2e3r4t5&n=1", "q1w2e3r4t5");
        hides("https://h.example/f?X-Auth=a1b2c3d4", "a1b2c3d4");
        let kept = redact("https://example.org/path?page=2&sort=name");
        assert_eq!(kept, "https://example.org/path?page=2&sort=name");
    }

    #[test]
    fn a_user_name_without_a_password_and_a_placeholder_query_value_are_not_secrets() {
        for plain in [
            "ssh://git@github.com/org/repo",
            "https://user@host.example/x",
            "see ?key=`$KEY` and &sig= in the docs",
            "https://h.example/f?key=${API_KEY}&n=1",
        ] {
            assert_eq!(redact(plain), plain);
        }
        // A lone long token in the user position is a credential, as in a git remote.
        let t = key("tok", "enabcdefghijklmnopqrstuvwxyz0123");
        hides(&format!("origin https://{t}@git.example/org/repo"), &t);
    }

    #[test]
    fn basic_authorization_in_json_and_curl_user_options() {
        hides(r#"{"Authorization": "Basic dXNlcjpwYXNzd29yZA=="}"#, "dXNlcjpwYXNzd29yZA");
        hides("headers = {'authorization': 'Basic dXNlcjpwYXNzd29yZA=='}", "dXNlcjpwYXNzd29yZA");
        hides("curl -s -u admin:hunter2pw https://h.example/api", "hunter2pw");
        hides("curl --user=admin:hunter2pw https://h.example/api", "hunter2pw");
        hides("curl -u 'admin:hunter2pw' https://h.example/api", "hunter2pw");
        let kept = "curl -u admin https://h.example/api && ls -u";
        assert_eq!(redact(kept), kept);
    }

    #[test]
    fn google_and_groq_keys() {
        let g = key("AI", "zaSyabcdefghijklmnopqrstuvwxyz0123");
        hides(&format!("maps {g}"), &g);
        let q = key("gs", "k_abcdefghijklmnopqrstuvwx");
        hides(&format!("groq {q}"), &q);
    }

    #[test]
    fn hugging_face_npm_and_xai_tokens() {
        let h = key("hf", "_abcdefghijklmnopqrstuvwxyzABCDEFGH");
        hides(&format!("hub {h}"), &h);
        let n = key("npm", "_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ");
        hides(&format!("registry {n}"), &n);
        let x = key("xa", "i-abcdefghijklmnopqrstuvwx");
        hides(&format!("grok {x}"), &x);
    }

    #[test]
    fn stripe_live_and_test_keys() {
        let s = key("s", "k_live_abcdefghijklmnop1234");
        hides(&format!("charge with {s}"), &s);
        let r = key("r", "k_test_abcdefghijklmnop1234");
        hides(&format!("restricted {r}"), &r);
    }

    #[test]
    fn basic_authorization_header_only_with_its_prefix() {
        hides("Authorization: Basic dXNlcjpwYXNzd29yZA==", "dXNlcjpwYXNzd29yZA");
        let prose = "set the basic configuration before the first run";
        assert_eq!(redact(prose), prose);
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
