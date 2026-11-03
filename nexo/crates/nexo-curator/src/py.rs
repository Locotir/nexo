//! The string and pattern semantics the curator's rules and learnt model were written for.
//!
//! Patterns use a classic regex dialect and are translated: `\w` is a word character in the Unicode
//! sense (letters, numbers and `_`), `\s` adds the four separators (U+001C to U+001F) counted as space,
//! `\Z` is the end of the text. A pattern with lookaround or a backreference runs on `fancy-regex`, any
//! other on `regex`. That dialect's `\b` differs from the `regex` crate's on a few characters (marks,
//! joiners, connector punctuation and numbers that are not digits), so a pattern with `\b` is compiled
//! twice and the exact (slower) form runs only on a text that has one of those characters.

use std::borrow::Cow;
use std::sync::LazyLock;

pub const I: u8 = 1;
pub const M: u8 = 2;
pub const S: u8 = 4;

const WORD: &str = r"\p{L}\p{N}_";
const SPACE: &str = r"\s\x1c-\x1f";
const EXACT_B: &str = r"(?:(?<=[\p{L}\p{N}_])(?![\p{L}\p{N}_])|(?<![\p{L}\p{N}_])(?=[\p{L}\p{N}_]))";
const EXACT_NB: &str = r"(?:(?<=[\p{L}\p{N}_])(?=[\p{L}\p{N}_])|(?<![\p{L}\p{N}_])(?![\p{L}\p{N}_]))";

/// Characters where the `regex` crate's idea of a word character differs from the dialect's.
static MISMATCH: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[\w&&[^\p{L}\p{N}_]]|[[\p{L}\p{N}_]&&\W]").expect("static pattern"));

fn needs_exact(text: &str) -> bool {
    !text.is_ascii() && MISMATCH.is_match(text)
}

/// A capture: the byte span of each group (0 is the whole match).
pub type Caps = Vec<Option<(usize, usize)>>;

enum Eng {
    Std(regex::Regex),
    Fancy(fancy_regex::Regex),
}

impl Eng {
    fn new(pat: &str, fancy: bool) -> Result<Eng, String> {
        if fancy {
            fancy_regex::Regex::new(pat).map(Eng::Fancy).map_err(|e| e.to_string())
        } else {
            regex::Regex::new(pat).map(Eng::Std).map_err(|e| e.to_string())
        }
    }

    fn is_match(&self, text: &str) -> bool {
        match self {
            Eng::Std(r) => r.is_match(text),
            Eng::Fancy(r) => r.is_match(text).unwrap_or(false),
        }
    }

    fn captures(&self, text: &str, limit: usize) -> Vec<Caps> {
        let mut out = Vec::new();
        match self {
            Eng::Std(r) => {
                for c in r.captures_iter(text) {
                    out.push((0..c.len()).map(|i| c.get(i).map(|m| (m.start(), m.end()))).collect());
                    if out.len() >= limit {
                        break;
                    }
                }
            }
            Eng::Fancy(r) => {
                for c in r.captures_iter(text) {
                    let Ok(c) = c else { break };
                    out.push((0..c.len()).map(|i| c.get(i).map(|m| (m.start(), m.end()))).collect());
                    if out.len() >= limit {
                        break;
                    }
                }
            }
        }
        out
    }

    fn find_iter(&self, text: &str) -> Vec<(usize, usize)> {
        match self {
            Eng::Std(r) => r.find_iter(text).map(|m| (m.start(), m.end())).collect(),
            Eng::Fancy(r) => r.find_iter(text).map_while(|m| m.ok()).map(|m| (m.start(), m.end())).collect(),
        }
    }

    fn names(&self) -> Vec<Option<String>> {
        match self {
            Eng::Std(r) => r.capture_names().map(|n| n.map(str::to_string)).collect(),
            Eng::Fancy(r) => r.capture_names().map(|n| n.map(str::to_string)).collect(),
        }
    }
}

/// A compiled pattern of the dialect.
pub struct Re {
    fast: Eng,
    exact: Option<Eng>,
    names: Vec<Option<String>>,
}

impl Re {
    /// Compiles a pattern that is part of the program (a bad one is a bug, not an input error).
    pub fn new(pat: &str, flags: u8) -> Re {
        Re::try_new(pat, flags).unwrap_or_else(|e| panic!("bad pattern {pat:?}: {e}"))
    }

    pub fn try_new(pat: &str, flags: u8) -> Result<Re, String> {
        let (fast, fancy, has_b) = translate(pat, flags, false)?;
        let fast = Eng::new(&fast, fancy)?;
        let exact = if has_b {
            let (exact, _, _) = translate(pat, flags, true)?;
            Some(Eng::new(&exact, true)?)
        } else {
            None
        };
        let names = fast.names();
        Ok(Re { fast, exact, names })
    }

    fn eng(&self, text: &str) -> &Eng {
        match &self.exact {
            Some(e) if needs_exact(text) => e,
            _ => &self.fast,
        }
    }

    /// The index of a named group.
    pub fn group(&self, name: &str) -> usize {
        self.names.iter().position(|n| n.as_deref() == Some(name)).unwrap_or(usize::MAX)
    }

    /// `re.search(...) is not None`
    pub fn is_match(&self, text: &str) -> bool {
        self.eng(text).is_match(text)
    }

    /// The first match's groups (`re.search`).
    pub fn search(&self, text: &str) -> Option<Caps> {
        self.eng(text).captures(text, 1).into_iter().next()
    }

    /// Every match's groups (`re.finditer`).
    pub fn captures_all(&self, text: &str) -> Vec<Caps> {
        self.eng(text).captures(text, usize::MAX)
    }

    /// Every match's span.
    pub fn find_all(&self, text: &str) -> Vec<(usize, usize)> {
        self.eng(text).find_iter(text)
    }

    /// `re.findall` of a pattern with exactly one group (or none: the whole match).
    pub fn findall<'t>(&self, text: &'t str) -> Vec<&'t str> {
        let g = if self.names.len() > 1 { 1 } else { 0 };
        self.captures_all(text)
            .into_iter()
            .map(|c| c.get(g).copied().flatten().map_or("", |(a, b)| &text[a..b]))
            .collect()
    }

    /// `re.sub(pattern, f, text, count)` (count 0: all).
    pub fn sub_with<'t>(&self, text: &'t str, count: usize, mut f: impl FnMut(&Caps) -> String) -> Cow<'t, str> {
        let limit = if count == 0 { usize::MAX } else { count };
        let all = self.eng(text).captures(text, limit);
        if all.is_empty() {
            return Cow::Borrowed(text);
        }
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for c in &all {
            let Some((a, b)) = c[0] else { continue };
            out.push_str(&text[last..a]);
            out.push_str(&f(c));
            last = b;
        }
        out.push_str(&text[last..]);
        Cow::Owned(out)
    }

    /// `re.sub` with a replacement where `\1` ... `\9` and `\g<name>` are groups.
    pub fn sub<'t>(&self, text: &'t str, repl: &str) -> Cow<'t, str> {
        self.sub_n(text, repl, 0)
    }

    pub fn sub_n<'t>(&self, text: &'t str, repl: &str, count: usize) -> Cow<'t, str> {
        let parts = self.parse_repl(repl);
        self.sub_with(text, count, |c| {
            let mut s = String::new();
            for p in &parts {
                match p {
                    Repl::Lit(l) => s.push_str(l),
                    Repl::Group(g) => {
                        if let Some(Some((a, b))) = c.get(*g) {
                            s.push_str(&text[*a..*b]);
                        }
                    }
                }
            }
            s
        })
    }

    fn parse_repl(&self, repl: &str) -> Vec<Repl> {
        let mut out = Vec::new();
        let mut lit = String::new();
        let mut it = repl.chars().peekable();
        while let Some(c) = it.next() {
            if c != '\\' {
                lit.push(c);
                continue;
            }
            match it.next() {
                Some(d @ '1'..='9') => {
                    out.push(Repl::Lit(std::mem::take(&mut lit)));
                    out.push(Repl::Group(d as usize - '0' as usize));
                }
                Some('g') if it.peek() == Some(&'<') => {
                    it.next();
                    let name: String = it.by_ref().take_while(|&c| c != '>').collect();
                    out.push(Repl::Lit(std::mem::take(&mut lit)));
                    let g = name.parse::<usize>().unwrap_or_else(|_| self.group(&name));
                    out.push(Repl::Group(g));
                }
                Some('n') => lit.push('\n'),
                Some('t') => lit.push('\t'),
                Some(o) => {
                    lit.push('\\');
                    lit.push(o);
                }
                None => lit.push('\\'),
            }
        }
        out.push(Repl::Lit(lit));
        out
    }
}

enum Repl {
    Lit(String),
    Group(usize),
}

fn push_escaped(out: &mut String, c: char) {
    let mut b = [0u8; 4];
    out.push_str(&regex::escape(c.encode_utf8(&mut b)));
}

/// A pattern of the dialect to `regex` syntax: (pattern, needs fancy-regex, has `\b`/`\B`).
fn translate(pat: &str, flags: u8, exact: bool) -> Result<(String, bool, bool), String> {
    let chars: Vec<char> = pat.chars().collect();
    let mut out = String::with_capacity(pat.len() * 2);
    let mut fancy = exact;
    let mut has_b = false;
    let mut m_stack: Vec<bool> = Vec::new();
    let global_m = flags & M != 0;
    if flags & (I | M | S) != 0 {
        out.push_str("(?");
        if flags & I != 0 {
            out.push('i');
        }
        if flags & M != 0 {
            out.push('m');
        }
        if flags & S != 0 {
            out.push('s');
        }
        out.push(')');
    }
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        match c {
            '\\' => {
                let Some(&d) = chars.get(i + 1) else { return Err("trailing backslash".into()) };
                i += 2;
                match d {
                    'w' => out.push_str(&format!("[{WORD}]")),
                    'W' => out.push_str(&format!("[^{WORD}]")),
                    's' => out.push_str(&format!("[{SPACE}]")),
                    'S' => out.push_str(&format!("[^{SPACE}]")),
                    'd' => out.push_str(r"\p{Nd}"),
                    'D' => out.push_str(r"\P{Nd}"),
                    'b' => {
                        has_b = true;
                        out.push_str(if exact { EXACT_B } else { r"\b" });
                    }
                    'B' => {
                        has_b = true;
                        out.push_str(if exact { EXACT_NB } else { r"\B" });
                    }
                    'Z' => out.push_str(r"\z"),
                    'A' => out.push_str(r"\A"),
                    'n' | 't' | 'r' | 'f' | 'v' => {
                        out.push('\\');
                        out.push(d);
                    }
                    'a' => out.push_str(r"\x07"),
                    'x' => {
                        let hex: String = chars.get(i..i + 2).ok_or("bad \\x")?.iter().collect();
                        out.push_str(&format!(r"\x{{{hex}}}"));
                        i += 2;
                    }
                    'u' => {
                        let hex: String = chars.get(i..i + 4).ok_or("bad \\u")?.iter().collect();
                        out.push_str(&format!(r"\x{{{hex}}}"));
                        i += 4;
                    }
                    '1'..='9' => {
                        fancy = true;
                        out.push('\\');
                        out.push(d);
                    }
                    d if d.is_ascii_alphanumeric() => return Err(format!("unsupported escape \\{d}")),
                    d => push_escaped(&mut out, d),
                }
            }
            '[' => {
                i = class(&chars, i + 1, &mut out)?;
            }
            '(' => {
                let rest: String = chars[i..n.min(i + 4)].iter().collect();
                if rest.starts_with("(?P=") {
                    let end = chars[i..].iter().position(|&c| c == ')').ok_or("bad (?P=")? + i;
                    let name: String = chars[i + 4..end].iter().collect();
                    out.push_str(&format!(r"\k<{name}>"));
                    fancy = true;
                    i = end + 1;
                    continue;
                }
                if rest.starts_with("(?=")
                    || rest.starts_with("(?!")
                    || rest.starts_with("(?<=")
                    || rest.starts_with("(?<!")
                {
                    fancy = true;
                }
                let inline_m = rest.starts_with("(?m:");
                m_stack.push(inline_m);
                out.push('(');
                i += 1;
            }
            ')' => {
                m_stack.pop();
                out.push(')');
                i += 1;
            }
            '$' => {
                if global_m || m_stack.iter().any(|&m| m) {
                    out.push('$');
                } else {
                    // only used on texts without a line break, where the dialect's `$` is the end
                    out.push_str(r"\z");
                }
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok((out, fancy, has_b))
}

/// A character class from just after `[`; returns the index after its `]`.
fn class(chars: &[char], mut i: usize, out: &mut String) -> Result<usize, String> {
    out.push('[');
    if chars.get(i) == Some(&'^') {
        out.push('^');
        i += 1;
    }
    let first = i;
    let mut prev_atom = false;
    loop {
        let Some(&c) = chars.get(i) else { return Err("unterminated class".into()) };
        if c == ']' && i > first {
            out.push(']');
            return Ok(i + 1);
        }
        match c {
            '\\' => {
                let Some(&d) = chars.get(i + 1) else { return Err("trailing backslash".into()) };
                i += 2;
                prev_atom = true;
                match d {
                    'w' => {
                        out.push_str(WORD);
                        prev_atom = false;
                    }
                    'W' => {
                        out.push_str(&format!("[^{WORD}]"));
                        prev_atom = false;
                    }
                    's' => {
                        out.push_str(SPACE);
                        prev_atom = false;
                    }
                    'S' => {
                        out.push_str(&format!("[^{SPACE}]"));
                        prev_atom = false;
                    }
                    'd' => {
                        out.push_str(r"\p{Nd}");
                        prev_atom = false;
                    }
                    'D' => {
                        out.push_str(r"\P{Nd}");
                        prev_atom = false;
                    }
                    'n' | 't' | 'r' | 'f' | 'v' => {
                        out.push('\\');
                        out.push(d);
                    }
                    'x' => {
                        let hex: String = chars.get(i..i + 2).ok_or("bad \\x")?.iter().collect();
                        out.push_str(&format!(r"\x{{{hex}}}"));
                        i += 2;
                    }
                    'u' => {
                        let hex: String = chars.get(i..i + 4).ok_or("bad \\u")?.iter().collect();
                        out.push_str(&format!(r"\x{{{hex}}}"));
                        i += 4;
                    }
                    d if d.is_ascii_alphanumeric() => return Err(format!("unsupported escape \\{d}")),
                    d => push_escaped(out, d),
                }
            }
            '-' => {
                let next = chars.get(i + 1).copied();
                if prev_atom && next.is_some_and(|x| x != ']') {
                    out.push('-');
                    prev_atom = false;
                } else {
                    out.push_str(r"\-");
                    prev_atom = true;
                }
                i += 1;
            }
            c => {
                push_escaped(out, c);
                prev_atom = true;
                i += 1;
            }
        }
    }
}

/// `re.escape`
pub fn escape(s: &str) -> String {
    regex::escape(s)
}

// ---------------------------------------------------------------------------------------------------
// str methods

/// `str.isspace` for one character.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

static ND: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^\p{Nd}$").expect("static pattern"));

fn is_nd(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_digit();
    }
    let mut b = [0u8; 4];
    ND.is_match(c.encode_utf8(&mut b))
}

/// `str.isdigit` for one character: decimal digits and the digits that are not decimal (superscripts,
/// circled numbers, ...).
pub fn is_digit(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_digit();
    }
    let u = c as u32;
    matches!(u,
        0xb2 | 0xb3 | 0xb9 | 0x1369..=0x1371 | 0x19da | 0x2070 | 0x2074..=0x2079 | 0x2080..=0x2089
        | 0x2460..=0x2468 | 0x2474..=0x247c | 0x2488..=0x2490 | 0x24ea | 0x24f5..=0x24fd | 0x24ff
        | 0x2776..=0x277e | 0x2780..=0x2788 | 0x278a..=0x2792 | 0x10a40..=0x10a43 | 0x10e60..=0x10e68
        | 0x11052..=0x1105a | 0x1f100..=0x1f10a)
        || is_nd(c)
}

/// `str.isdigit`
pub fn str_isdigit(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_digit)
}

fn is_title(c: char) -> bool {
    matches!(c as u32,
        0x01c5 | 0x01c8 | 0x01cb | 0x01f2 | 0x1f88..=0x1f8f | 0x1f98..=0x1f9f | 0x1fa8..=0x1faf | 0x1fbc | 0x1fcc
        | 0x1ffc)
}

/// `str.islower`
pub fn str_islower(s: &str) -> bool {
    let mut cased = false;
    for c in s.chars() {
        if c.is_uppercase() || is_title(c) {
            return false;
        }
        if c.is_lowercase() {
            cased = true;
        }
    }
    cased
}

/// `str.lower`
pub fn lower(s: &str) -> Cow<'_, str> {
    if s.bytes().all(|b| !b.is_ascii_uppercase() && b.is_ascii()) {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(s.to_lowercase())
    }
}

/// `str.upper`
pub fn upper(s: &str) -> Cow<'_, str> {
    if s.bytes().all(|b| !b.is_ascii_lowercase() && b.is_ascii()) {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(s.to_uppercase())
    }
}

/// `str.split()`
pub fn split_ws(s: &str) -> impl Iterator<Item = &str> {
    s.split(is_space).filter(|p| !p.is_empty())
}

/// `" ".join(s.split())`
pub fn squash_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for p in split_ws(s) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(p);
    }
    out
}

/// `str.strip()`
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `str.lstrip()`
pub fn lstrip(s: &str) -> &str {
    s.trim_start_matches(is_space)
}

/// `str.splitlines()`
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let sep =
            matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}');
        if sep {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(_, '\n')) = it.peek() {
                    it.next();
                    end += 1;
                }
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `len(s)`: code points.
pub fn len(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.chars().count()
    }
}

/// `s[:n]` (code points)
pub fn head(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `s[-n:]` (code points; n > 0)
pub fn tail(s: &str, n: usize) -> &str {
    if n == 0 {
        return "";
    }
    match s.char_indices().rev().nth(n - 1) {
        Some((i, _)) => &s[i..],
        None => s,
    }
}

/// The first character, or `None` for an empty string.
pub fn first(s: &str) -> Option<char> {
    s.chars().next()
}

/// `repr(s)` of a str.
pub fn repr_str(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// `bisect.bisect_left`
pub fn bisect_left<T: PartialOrd>(v: &[T], x: &T) -> usize {
    v.partition_point(|e| e < x)
}

/// `bisect.bisect_right`
pub fn bisect_right<T: PartialOrd>(v: &[T], x: &T) -> usize {
    v.partition_point(|e| e <= x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_and_bounds() {
        let r = Re::new(r"\bfoo\b", 0);
        assert!(r.is_match("a foo b"));
        assert!(!r.is_match("afoo"));
        // a combining mark is a word character for `regex`, not for the dialect
        assert!(r.is_match("\u{301}foo"));
        let w = Re::new(r"[^\W\d_]+", 0);
        assert_eq!(w.findall("ab_1cd é"), vec!["ab", "cd", "é"]);
        let c = Re::new(r"[\w./-]+", 0);
        assert_eq!(c.findall("a/b.c-d e"), vec!["a/b.c-d", "e"]);
        let h = Re::new(r"(?P<head><<-?\s*(?P<q>['\x22]?)(?P<tag>\w+)(?P=q)[^\n]*\n).*?^\s*(?P=tag)\s*$", S | M);
        assert_eq!(h.sub("cat <<EOF > a.txt\nhello\nEOF\nls", r"\g<head>"), "cat <<EOF > a.txt\n\nls");
    }

    #[test]
    fn strings() {
        assert_eq!(splitlines("a\r\nb\n\nc\n"), vec!["a", "b", "", "c"]);
        assert!(str_islower("abc1"));
        assert!(!str_islower("1"));
        assert!(str_isdigit("²3"));
        assert_eq!(head("héllo", 2), "hé");
        assert_eq!(tail("héllo", 4), "éllo");
        assert_eq!(repr_str("nope"), "'nope'");
    }
}
