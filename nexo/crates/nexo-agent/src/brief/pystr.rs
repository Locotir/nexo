//! The text semantics the curated selection is defined with: which characters
//! are alphanumeric, space, digits and decimals, how case folds and what
//! case-insensitive matching equates, from the Unicode tables in
//! [`super::pytables`], and lengths in code points.

use std::cmp::Ordering;

use super::pytables as t;

fn in_ranges(ranges: &[(u32, u32)], c: char) -> bool {
    let c = c as u32;
    ranges
        .binary_search_by(|&(a, b)| {
            if b < c {
                Ordering::Less
            } else if a > c {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        })
        .is_ok()
}

/// `c.isalnum()` (`\w` is this or `_`).
pub fn is_alnum(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphanumeric()
    } else {
        in_ranges(t::ALNUM, c)
    }
}

/// `[\w$]`.
pub fn is_word(c: char) -> bool {
    c == '_' || c == '$' || is_alnum(c)
}

/// `c.isspace()`, which is also `\s`.
pub fn is_space(c: char) -> bool {
    if c.is_ascii() {
        matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r' | '\x1c'..='\x1f')
    } else {
        in_ranges(t::SPACE, c)
    }
}

/// `c.isdigit()` (superscripts included).
pub fn is_digit(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_digit()
    } else {
        in_ranges(t::DIGIT, c)
    }
}

/// `\d`: a decimal digit of any script.
pub fn is_decimal(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_digit()
    } else {
        in_ranges(t::DECIMAL, c)
    }
}

/// The value of a decimal digit (`int()` of it).
fn decimal_value(c: char) -> Option<u32> {
    if !is_decimal(c) {
        return None;
    }
    let cp = c as u32;
    let k = t::DECIMAL_ZEROS.partition_point(|&z| z <= cp);
    let zero = t::DECIMAL_ZEROS[k.checked_sub(1)?];
    (cp - zero < 10).then_some(cp - zero)
}

/// `int(s)` for a string of decimal digits (None for anything else).
pub fn int_value(s: &str) -> Option<u64> {
    let mut v: u64 = 0;
    for c in s.chars() {
        v = v.checked_mul(10)?.checked_add(u64::from(decimal_value(c)?))?;
    }
    (!s.is_empty()).then_some(v)
}

/// Whether `str.lower` changes the character.
pub fn changes_lower(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_uppercase()
    } else {
        in_ranges(t::LOWER_CHANGES, c)
    }
}

/// Whether `str.upper` changes the character.
pub fn changes_upper(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_lowercase()
    } else {
        in_ranges(t::UPPER_CHANGES, c)
    }
}

const CAPITAL_SIGMA: char = '\u{3a3}';

/// `s.lower()`: the full mappings, and a capital sigma that ends a word as final sigma.
pub fn lower(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    let cs: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in cs.iter().enumerate() {
        if c.is_ascii() {
            out.push(c.to_ascii_lowercase());
        } else if c == CAPITAL_SIGMA {
            out.push(if final_sigma(&cs, i) { '\u{3c2}' } else { '\u{3c3}' });
        } else {
            match t::LOWER.binary_search_by_key(&(c as u32), |&(k, _)| k) {
                Ok(k) => out.push_str(t::LOWER[k].1),
                Err(_) => out.push(c),
            }
        }
    }
    out
}

fn cased(c: char) -> bool {
    in_ranges(t::CASED, c)
}

fn ignorable(c: char) -> bool {
    in_ranges(t::CASE_IGNORABLE, c)
}

/// The final-sigma rule: a cased letter before it, none after (case-ignorable characters skipped).
fn final_sigma(cs: &[char], i: usize) -> bool {
    let before = cs[..i].iter().rev().find(|&&c| !ignorable(c));
    if !before.is_some_and(|&c| cased(c)) {
        return false;
    }
    let after = cs[i + 1..].iter().find(|&&c| !ignorable(c));
    !after.is_some_and(|&c| cased(c))
}

/// A character as a case-insensitive pattern of ASCII letters sees it.
pub fn fold(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    match t::FOLD.binary_search_by_key(&(c as u32), |&(k, _)| k) {
        Ok(k) => t::FOLD[k].1,
        Err(_) => c,
    }
}

/// Whether `text` holds one of `needles` (lower-case ASCII), ignoring case as `re.I` does.
pub fn contains_any_folded(text: &str, needles: &[&str]) -> bool {
    let folded: String = text.chars().map(fold).collect();
    needles.iter().any(|n| folded.contains(n))
}

/// `s.strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `s.split()`.
pub fn split_ws(s: &str) -> impl Iterator<Item = &str> {
    s.split(is_space).filter(|p| !p.is_empty())
}

/// `len(s)`: code points.
pub fn len(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.chars().count()
    }
}

/// `s[:n]`.
pub fn prefix(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((b, _)) => &s[..b],
        None => s,
    }
}

/// A float as the shortest digits that read back (`1.0`, `1e-05`, `1e+16`).
pub fn float_repr(x: f64) -> String {
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let e = format!("{:e}", x.abs());
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(char::is_ascii_digit).collect();
    let decpt = exp + 1;
    let sign = if x < 0.0 { "-" } else { "" };
    let n = digits.len() as i32;
    if -4 < decpt && decpt <= 16 {
        if decpt <= 0 {
            format!("{sign}0.{}{digits}", "0".repeat((-decpt) as usize))
        } else if decpt >= n {
            format!("{sign}{digits}{}.0", "0".repeat((decpt - n) as usize))
        } else {
            format!("{sign}{}.{}", &digits[..decpt as usize], &digits[decpt as usize..])
        }
    } else {
        let rest = if n > 1 { format!(".{}", &digits[1..]) } else { String::new() };
        let es = if exp < 0 { '-' } else { '+' };
        format!("{sign}{}{rest}e{es}{:02}", &digits[..1], exp.abs())
    }
}

/// `round(x, n)`: the decimal rounding of the binary value, read back.
pub fn round(x: f64, n: usize) -> f64 {
    format!("{x:.n$}").parse().unwrap_or(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_text_semantics() {
        assert!(is_alnum('é') && is_alnum('²') && !is_alnum('_') && !is_alnum('-'));
        assert!(is_space('\x1c') && is_space('\u{3000}') && !is_space('a'));
        assert!(is_digit('²') && !is_decimal('²') && is_decimal('٣'));
        assert_eq!(int_value("١٩٩٩"), Some(1999));
        assert_eq!(int_value("19²9"), None);
        assert_eq!(lower("İSTANBUL"), "i\u{307}stanbul");
        assert_eq!(lower("ΟΔΟΣ"), "οδος");
        assert_eq!(lower("ΣΑ"), "σα");
        assert_eq!(fold('\u{212a}'), 'k');
        assert!(!contains_any_folded("Not ſound", &["not found"]));
        assert!(contains_any_folded("FAİLED", &["failed"]));
        for (x, s) in [
            (1e-05, "1e-05"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (0.0001, "0.0001"),
            (1.5, "1.5"),
            (-2.0, "-2.0"),
            (123.456, "123.456"),
            (1.2345e-7, "1.2345e-07"),
            (0.1, "0.1"),
        ] {
            assert_eq!(float_repr(x), s);
        }
        assert_eq!(round(0.123456, 4), 0.1235);
        assert_eq!(len("é→x"), 3);
        assert_eq!(prefix("é→xyz", 2), "é→");
    }
}
