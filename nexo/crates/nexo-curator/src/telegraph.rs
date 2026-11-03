//! Filler words removed from the prose of user messages; code and quoted text untouched (`compress`).

use std::borrow::Cow;
use std::sync::LazyLock;

use rustc_hash::FxHashSet;

use crate::py::{self, Re, I, S};

static DROP: LazyLock<FxHashSet<&'static str>> = LazyLock::new(|| {
    "the a an is are was were am \
     just really actually basically simply literally please kindly okay ok hey hi hello um uh \
     el la los las un una unos unas \
     pues bueno digamos vale okey eh porfa básicamente simplemente literalmente realmente"
        .split_whitespace()
        .collect()
});
const DROP_PHRASES: [&str; 15] = [
    "you know",
    "i mean",
    "kind of",
    "sort of",
    "let's say",
    "o sea",
    "en plan",
    "la verdad",
    "por favor",
    "sabes",
    "entiendes",
    "comprendes",
    "me entiendes",
    "o algo así",
    "o algo",
];

static PROTECT: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        concat!(
            r"```.*?(?:```|\Z)",
            r"|`[^`\n]+`",
            r"|<[^<>\n]{1,200}>",
            r#"|"[^"\n]{1,300}"|“[^”\n]{1,300}”"#,
            r"|https?://\S+",
            r"|(?m:^(?:    |\t)[^\n]*$)",
            r"|(?m:^[^\n]*[{};=<>\[\]|\\$][^\n]*$)"
        ),
        S,
    )
});
static WORDISH: LazyLock<Re> = LazyLock::new(|| Re::new(r"[^\W\d_](?:[^\W\d_]|['’][^\W\d_])*", 0));
static PHRASE: LazyLock<Re> = LazyLock::new(|| {
    let alts: Vec<String> = DROP_PHRASES.iter().map(|p| py::escape(p)).collect();
    Re::new(&format!(r"(?<![\w/.-])(?:{})(?![\w/-])", alts.join("|")), I)
});
static SPACES: LazyLock<Re> = LazyLock::new(|| Re::new(r"[ \t]{2,}", 0));
static BEFORE_PUNCT: LazyLock<Re> = LazyLock::new(|| Re::new(r"[ \t]+([,.;:!?)])", 0));
static AFTER_OPEN: LazyLock<Re> = LazyLock::new(|| Re::new(r"([(¿¡]) +", 0));
static COMMAS: LazyLock<Re> = LazyLock::new(|| Re::new(r",\s*,", 0));
static INDENT: LazyLock<Re> = LazyLock::new(|| Re::new(r"\n[ \t]+(?=\S)", 0));
static TRAILING: LazyLock<Re> = LazyLock::new(|| Re::new(r"[ \t]+\n", 0));
static BLANKS: LazyLock<Re> = LazyLock::new(|| Re::new(r"\n{3,}", 0));

fn glued(b: u8) -> bool {
    matches!(b, b'_' | b'.' | b'/' | b'-')
}

fn prose(text: &str) -> String {
    let text = PHRASE.sub(text, "");
    let text: &str = &text;
    let lead = if text.is_empty() || text.starts_with([' ', '\t']) { " " } else { "" };
    let tail = if text.is_empty() || text.ends_with([' ', '\t']) { " " } else { "" };
    let bytes = text.as_bytes();
    let text = WORDISH.sub_with(text, 0, |c| {
        let Some((a, b)) = c[0] else { return String::new() };
        let w = &text[a..b];
        if !DROP.contains(py::lower(w).as_ref()) {
            return w.to_string();
        }
        if (a > 0 && glued(bytes[a - 1])) || (b < bytes.len() && glued(bytes[b])) {
            return w.to_string();
        }
        String::new()
    });
    let text = SPACES.sub(&text, " ").into_owned();
    let text = BEFORE_PUNCT.sub(&text, r"\1").into_owned();
    let text = AFTER_OPEN.sub(&text, r"\1").into_owned();
    let text = COMMAS.sub(&text, ",").into_owned();
    let text = INDENT.sub(&text, "\n").into_owned();
    let text = TRAILING.sub(&text, "\n").into_owned();
    let core = text.trim_matches([' ', '\t']);
    if core.is_empty() {
        if lead.is_empty() { tail } else { lead }.to_string()
    } else {
        format!("{lead}{core}{tail}")
    }
}

/// `text` with filler words removed from its prose.
pub fn compress(text: &str) -> Cow<'_, str> {
    if text.is_empty() || py::strip(text).is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for (a, b) in PROTECT.find_all(text) {
        if a > pos {
            out.push_str(&prose(&text[pos..a]));
        }
        out.push_str(&text[a..b]);
        pos = b;
    }
    if pos < text.len() {
        out.push_str(&prose(&text[pos..]));
    }
    let res = BLANKS.sub(&out, "\n\n").into_owned();
    if py::len(&res) < py::len(text) {
        Cow::Owned(res)
    } else {
        Cow::Borrowed(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_filler() {
        assert_eq!(compress("Fix the failing test in src/lib.rs"), "Fix failing test in src/lib.rs");
        assert_eq!(compress("please, you know, run `the tests`"), ", run `the tests`");
    }
}
