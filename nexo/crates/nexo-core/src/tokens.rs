//! The token estimate every crate shares: the harness (history size, window, system prompt) and the context
//! curator count with these functions, so both agree. No tokenizer and no allocation: one pass over the bytes.

use serde_json::Value;

/// Cost units in a token: the estimate adds whole numbers, a token is 100 of them.
pub const TOKEN: usize = 100;
/// What a piece of text costs, in hundredths of a token, fitted to the extra input tokens a provider billed
/// for 13 000 messages added to recorded bench requests (prose, code, logs, numeric tables): a run of ASCII
/// letters (a word, a name), each letter of one past the eighth, a run of digits and each digit, a run of
/// punctuation, each whitespace character, a non-ASCII character. JSON syntax around the text costs only
/// through its key words, as the fit has it.
const WORD: usize = 103;
const LONG_WORD: usize = 14;
const NUMBER: usize = 59;
const DIGIT: usize = 39;
const PUNCT: usize = 73;
const SPACE: usize = 37;
const WIDE: usize = 80;
/// A line break.
pub const NEWLINE: usize = SPACE;

/// The cost of `s` in hundredths of a token, in one pass: letters run together for about 4 or 5 characters a
/// token, but digits, symbols and wide characters nearer 1 or 2, so logs, tables, hashes and dense code
/// are not undercounted by 4 characters a token.
pub fn text_cost(s: &str) -> usize {
    #[derive(PartialEq, Clone, Copy)]
    enum Run {
        None,
        Word(usize),
        Digits,
        Punct,
    }
    let mut cost = 0;
    let mut run = Run::None;
    for &b in s.as_bytes() {
        run = match b {
            b'a'..=b'z' | b'A'..=b'Z' => {
                let len = if let Run::Word(n) = run { n + 1 } else { 1 };
                cost += match len {
                    1 => WORD,
                    9.. => LONG_WORD,
                    _ => 0,
                };
                Run::Word(len)
            }
            b'0'..=b'9' => {
                cost += DIGIT + if run == Run::Digits { 0 } else { NUMBER };
                Run::Digits
            }
            b' ' | b'\t' | b'\n' | b'\r' => {
                cost += SPACE;
                Run::None
            }
            0x80..=0xff => {
                if b & 0xc0 != 0x80 {
                    cost += WIDE;
                }
                Run::None
            }
            _ => {
                cost += if run == Run::Punct { 0 } else { PUNCT };
                Run::Punct
            }
        };
    }
    cost
}

/// Estimated tokens of a text: [`text_cost`] in whole tokens.
pub fn text_tokens(s: &str) -> u64 {
    (text_cost(s) / TOKEN) as u64
}

/// The cost of a decoded JSON value written compactly: its keys and its strings (the quotes and the other
/// syntax cost nothing), a number as its digits, `null` and booleans as a word.
pub fn json_cost(v: &Value) -> usize {
    match v {
        Value::Null | Value::Bool(_) => WORD,
        Value::Number(n) => text_cost(&n.to_string()),
        Value::String(s) => text_cost(s),
        Value::Array(a) => a.iter().map(json_cost).sum(),
        Value::Object(m) => m.iter().map(|(k, x)| text_cost(k) + json_cost(x)).sum(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Messages added to recorded requests, with the extra input tokens the provider billed for them: ten
    /// samples of prose, code and numeric tables each, taken at a fixed stride from a larger set (not the
    /// closest fits), and the three rounds of a live chat run that pastes rate cards and stock lists.
    const SAMPLES: &str = include_str!("../tests/token-samples.json");

    fn samples(kind: &str) -> Vec<(f64, f64)> {
        let all: Vec<Value> = serde_json::from_str(SAMPLES).unwrap();
        all.iter()
            .filter(|s| s["kind"] == kind)
            .map(|s| (json_cost(&s["added"]) as f64 / TOKEN as f64, s["tokens"].as_f64().unwrap()))
            .collect()
    }

    fn off(estimate: f64, billed: f64) -> f64 {
        (estimate - billed).abs() / billed
    }

    #[test]
    fn each_kind_is_within_15_percent_of_the_provider() {
        for (kind, n) in [("prose", 14), ("code", 14), ("numeric", 14)] {
            let s = samples(kind);
            assert_eq!(s.len(), n, "{kind}");
            let (est, billed): (f64, f64) = s.iter().fold((0.0, 0.0), |(e, b), (x, y)| (e + x, b + y));
            assert!(off(est, billed) < 0.15, "{kind} as a whole: {est:.0} against {billed:.0}");
            let mut errs: Vec<f64> = s.iter().map(|(e, b)| off(*e, *b)).collect();
            errs.sort_by(f64::total_cmp);
            let median = (errs[errs.len() / 2 - 1] + errs[errs.len() / 2]) / 2.0;
            assert!(median < 0.15, "{kind}, the median sample: {errs:?}");
        }
    }

    #[test]
    fn numeric_tables_of_a_live_run_are_within_15_percent_of_the_provider() {
        let s = samples("chat");
        assert_eq!(s.len(), 3);
        for (est, billed) in s {
            assert!(off(est, billed) < 0.15, "{est:.0} tokens estimated, {billed:.0} billed");
        }
    }

    #[test]
    fn the_old_four_characters_a_token_missed_numbers() {
        // the table the live run pasted: one row per zone, 400 rows
        let card: String = (1..=400)
            .map(|i| {
                format!(
                    "Z{i:03} | base {:.2} | per_kg {:.2}\n",
                    3.0 + (i * 7 % 13) as f64 * 0.25,
                    0.9 + (i % 9) as f64 * 0.05
                )
            })
            .collect();
        let (new, old) = (text_tokens(&card), card.len() as u64 / 4);
        assert!(new * 10 > old * 17, "{new} tokens against {old} by characters");
    }

    #[test]
    fn the_cost_grows_with_the_text() {
        let base = "The quick brown fox, 3 jumps over 12.50 lazy dogs: é\n";
        let mut last = 0;
        for n in 0..6 {
            let c = text_cost(&base.repeat(n));
            assert!(c > last || n == 0, "{n} copies cost {c}, not more than {last}");
            last = c;
        }
        // appending anything never lowers the cost
        let mut s = String::new();
        let mut last = 0;
        for ch in "ab 12,x_é\n\t\"{}".chars().cycle().take(60) {
            s.push(ch);
            let c = text_cost(&s);
            assert!(c >= last, "{s:?}");
            last = c;
        }
        assert_eq!(text_cost(""), 0);
    }

    #[test]
    fn the_classes_cost_what_the_fit_says() {
        assert_eq!(text_cost("word"), WORD);
        assert_eq!(text_cost("abcdefghij"), WORD + 2 * LONG_WORD);
        assert_eq!(text_cost("42"), NUMBER + 2 * DIGIT);
        assert_eq!(text_cost("::"), PUNCT);
        assert_eq!(text_cost("a b"), 2 * WORD + SPACE);
        assert_eq!(text_cost("é€"), 2 * WIDE);
        assert_eq!(json_cost(&json!({"k": "v", "n": 7, "z": null, "t": true})), 7 * WORD + NUMBER + DIGIT);
        assert_eq!(text_tokens("hello world"), 2);
    }
}
