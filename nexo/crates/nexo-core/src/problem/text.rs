//! Small text helpers the cards use: hosts, clipping, wait words and cleaning provider text.

use std::time::Duration;

pub(super) fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit('@').next().unwrap_or("");
    if let Some(v6) = auth.strip_prefix('[') {
        return v6.split(']').next().unwrap_or("").to_string();
    }
    auth.split(':').next().unwrap_or("").to_ascii_lowercase()
}

pub(super) use crate::error::shorten;

/// "45 seconds", "3 minutes".
pub(super) fn wait_words(d: Duration) -> String {
    let s = d.as_secs().max(1);
    if s < 120 {
        format!("{s} s")
    } else if s < 7200 {
        format!("{} min", s.div_ceil(60))
    } else {
        format!("{} h", s.div_ceil(3600))
    }
}

pub(super) fn thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A title longer than this many characters is cut on the closed card, so its whole text goes in the detail.
pub(super) const TITLE_ROW: usize = 60;

pub(super) fn has(text: &str, words: &[&str]) -> bool {
    words.iter().any(|w| text.contains(w))
}

pub(super) fn skip_csi(it: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for d in it.by_ref() {
        if ('@'..='~').contains(&d) {
            break;
        }
    }
}

pub(super) fn skip_string(it: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(d) = it.next() {
        if d == '\x07' || d == '\u{9c}' {
            break;
        }
        if d == '\x1b' {
            it.next_if_eq(&'\\');
            break;
        }
    }
}

/// `s` as text for a screen: escape sequences (colours, links, title changes) and control bytes taken out,
/// a tab as a space; line breaks stay. What a server sends is never trusted to be plain.
pub fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\x1b' => match it.next() {
                Some('[') => skip_csi(&mut it),
                Some(']' | 'P' | '_' | '^' | 'X') => skip_string(&mut it),
                _ => {}
            },
            '\u{9b}' => skip_csi(&mut it),
            '\u{9d}' | '\u{90}' | '\u{9e}' | '\u{9f}' | '\u{98}' => skip_string(&mut it),
            '\t' => out.push(' '),
            // a carriage return alone is a line break; before a line feed it is nothing
            '\r' => {
                if it.peek() != Some(&'\n') {
                    out.push('\n');
                }
            }
            c if c.is_control() && c != '\n' => {}
            c => out.push(c),
        }
    }
    out
}

/// What a text carries beyond the core redaction: the credentials of an address (`user:pass@host`), the
/// value of a `key=` style query parameter, and the key formats of the providers.
pub(super) fn extra_rules() -> &'static [(regex::Regex, &'static str)] {
    use std::sync::OnceLock;
    static RULES: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    RULES.get_or_init(|| {
        let table: &[(&str, &str)] = &[
            // scheme://user:pass@host
            (r"(?i)\b([a-z][a-z0-9+.-]*://)[^\s/?#]*@", "${1}[redacted]@"),
            // ?key=..., &api_key=..., &access_token=...
            (
                r#"(?i)([?&;]\s*[a-z0-9_.-]*(?:key|token|secret|password|passwd|signature|sig|auth)=)[^&\s)"'<>#,]+"#,
                "${1}[redacted]",
            ),
            // Google and Groq keys
            (r"\bAIza[0-9A-Za-z_-]{20,}", crate::redact::MASK),
            (r"\bgsk_[0-9A-Za-z]{16,}", crate::redact::MASK),
        ];
        table.iter().map(|(p, w)| (regex::Regex::new(p).expect("a fixed pattern"), *w)).collect()
    })
}

/// `s` for a card: [`tidy`], then the secrets taken out. Done when the card is built, so nothing downstream
/// (the transcript, a saved session, headless output, a log) ever holds them.
pub(super) fn safe(s: &str) -> String {
    let mut out = crate::redact::redact(&tidy(s)).into_owned();
    for (re, with) in extra_rules() {
        if let std::borrow::Cow::Owned(t) = re.replace_all(&out, *with) {
            out = t;
        }
    }
    out
}

/// `m` without the `for url (...)` part reqwest puts in its message: the address is not a cause.
pub(super) fn without_url(m: &str) -> String {
    match m.find("for url (") {
        Some(i) => {
            let rest = &m[i..];
            match rest.find("): ").or_else(|| rest.find(')')) {
                Some(j) if rest[j..].starts_with("): ") => format!("{}{}", &m[..i], &rest[j + 3..]),
                Some(j) => format!("{}{}", &m[..i], &rest[j + 1..]),
                None => m[..i].to_string(),
            }
        }
        None => m.to_string(),
    }
}
