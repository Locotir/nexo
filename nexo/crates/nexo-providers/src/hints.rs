//! What a provider's error message says about the model: the context
//! window it names when a request is too long, the largest reply it allows
//! when `max_tokens` is too high, whether the model does not exist. Every
//! vendor words these differently; the extractors here are tolerant and
//! tested on the real wordings of the four protocols.

use nexo_core::{ErrorKind, ProviderError};

/// Numbers in the message, in order, with `k`/`K` suffixes and thousands
/// separators understood (`256k` → 256000, `131,072` → 131072). Each comes
/// with the lower-cased text before it and the text after it up to the next
/// number. Decimals and version numbers (`1.25`, `v3.1`) are not counts.
struct Num {
    n: u64,
    before: String,
    after: String,
}

fn numbers(message: &str) -> Vec<Num> {
    let lower = message.to_ascii_lowercase();
    let b = lower.as_bytes();
    let mut spans: Vec<(u64, usize, usize)> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let after_word =
            i > 0 && (b[i - 1].is_ascii_alphanumeric() || (b[i - 1] == b'.' && i >= 2 && b[i - 2].is_ascii_digit()));
        if b[i].is_ascii_digit() && !after_word {
            let start = i;
            let mut digits = String::new();
            while i < b.len()
                && (b[i].is_ascii_digit() || (b[i] == b',' && i + 1 < b.len() && b[i + 1].is_ascii_digit()))
            {
                if b[i] != b',' {
                    digits.push(b[i] as char);
                }
                i += 1;
            }
            if i < b.len() && b[i] == b'.' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                    i += 1;
                }
                continue;
            }
            let mut n: u64 = digits.parse().unwrap_or(0);
            if i < b.len() && b[i] == b'k' && (i + 1 >= b.len() || !b[i + 1].is_ascii_alphanumeric()) {
                n *= 1000;
                i += 1;
            }
            spans.push((n, start, i));
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (k, (n, start, end)) in spans.iter().enumerate() {
        let next = spans.get(k + 1).map(|s| s.1).unwrap_or(lower.len());
        out.push(Num { n: *n, before: lower[..*start].to_string(), after: lower[*end..next].to_string() });
    }
    out
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_').filter(|w| !w.is_empty()).map(String::from).collect()
}

/// Words that announce a limit when they come just before or after a number.
const LIMIT_WORDS: &[&str] =
    &["maximum", "max", "allowed", "limit", "most", "equal", "window", "length", "supports", "up", "below", "under"];
/// Words that announce what was sent, never a limit.
const SENT_WORDS: &[&str] = &["requested", "contains", "sent", "got", "provided", "has", "count", "you"];

/// Among numbers that look like token counts, the one a limit word
/// announces, else the smallest (a request that is too long names what it
/// sent and what fits; what fits is the smaller).
fn limit_among(nums: &[Num], at_least: u64) -> Option<u64> {
    let counts: Vec<&Num> = nums.iter().filter(|x| x.n >= at_least).collect();
    if counts.is_empty() {
        return None;
    }
    let announced = counts.iter().find(|x| {
        let before: Vec<String> = words(&x.before).into_iter().rev().take(4).collect();
        // "100000 exceeds the limit (65536)": what follows "exceeds" belongs to the next number
        let after: Vec<String> = words(&x.after)
            .into_iter()
            .take_while(|w| {
                !["exceeds", "exceed", "exceeded", "but", "than", "while", "whereas", "however"].contains(&w.as_str())
            })
            .take(4)
            .collect();
        let sent = before.iter().take(2).any(|w| SENT_WORDS.contains(&w.as_str()));
        let sign = x.before.trim_end().ends_with("<=") || x.before.trim_end().ends_with('≤');
        !sent
            && (sign
                || before.iter().any(|w| LIMIT_WORDS.contains(&w.as_str()))
                || after.iter().any(|w| ["maximum", "max", "allowed", "limit"].contains(&w.as_str())))
    });
    announced.map(|x| x.n).or_else(|| counts.iter().map(|x| x.n).min())
}

/// Whether the message is about the request being longer than the window.
pub fn is_context_message(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    // the shared test, and looser wordings that only matter once the message is known to be an error
    (nexo_core::is_context_error(None, "", message)
        || ["too long", "too large", "input token count", "reduce the length", "prompt length", "maximum prompt"]
            .iter()
            .any(|k| m.contains(k)))
        && !is_output_message(message)
}

/// Whether the message is about `max_tokens` (the reply) being too large.
pub fn is_output_message(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    [
        "max_tokens",
        "max_output_tokens",
        "max_completion_tokens",
        "maxoutputtokens",
        "output tokens",
        "completion tokens",
        "output limit",
    ]
    .iter()
    .any(|k| m.contains(k))
        && [
            "maximum",
            "at most",
            "<=",
            "less than",
            "exceed",
            "above",
            "too large",
            "too high",
            "allowed",
            "up to",
            "limit",
            "greater",
        ]
        .iter()
        .any(|k| m.contains(k))
}

/// Whether the message names a rate limit (tokens or requests per minute or day): its number is a
/// throttle that can change with the plan, not the model's window.
pub fn is_rate_message(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    ["per minute", "per day", "per hour", "per min", "tpm", "rpm", "tpd", "rpd", "rate limit"].iter().any(|k| {
        // short codes only as whole words ("tpm" is not in "attempt")
        if k.len() <= 3 {
            m.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| w == *k)
        } else {
            m.contains(k)
        }
    })
}

/// The context window the message names, when a request was too long.
pub fn context_window(message: &str) -> Option<u32> {
    if !is_context_message(message) || is_rate_message(message) {
        return None;
    }
    let nums = numbers(message);
    // "requested N (A in the messages, B in the completion)": the breakdown is not a limit
    let n = limit_among(&nums, 1_000)?;
    u32::try_from(n).ok()
}

/// The largest reply the message names, when `max_tokens` was too high.
pub fn max_output(message: &str) -> Option<u32> {
    if !is_output_message(message) {
        return None;
    }
    let nums = numbers(message);
    let n = limit_among(&nums, 256)?;
    u32::try_from(n).ok()
}

/// Whether the error says the model does not exist for this provider.
pub fn unknown_model(e: &ProviderError) -> bool {
    let m = e.message.to_ascii_lowercase();
    let says = m.contains("model")
        && [
            "not found",
            "does not exist",
            "not exist",
            "unknown model",
            "invalid model",
            "no such model",
            "is not supported",
            "not available",
            "unsupported model",
            "could not find",
            "doesn't exist",
        ]
        .iter()
        .any(|k| m.contains(k));
    says || (e.status == Some(404) && m.contains("model"))
}

/// Models by closeness of name to `wanted` (shared prefix, then edit distance).
pub fn closest<'a>(wanted: &str, candidates: impl IntoIterator<Item = &'a str>, n: usize) -> Vec<String> {
    let w = wanted.to_ascii_lowercase();
    let mut scored: Vec<(usize, usize, String)> = candidates
        .into_iter()
        .map(|c| {
            let l = c.to_ascii_lowercase();
            let prefix = w.chars().zip(l.chars()).take_while(|(a, b)| a == b).count();
            (usize::MAX - prefix, edit_distance(&w, &l), c.to_string())
        })
        .collect();
    scored.sort();
    scored.dedup_by(|a, b| a.2 == b.2);
    scored.into_iter().take(n).map(|(_, _, c)| c).collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// A one-line explanation of the wait a rate limit asked for.
pub fn wait_hint(e: &ProviderError) -> Option<String> {
    if e.kind != ErrorKind::RateLimit {
        return None;
    }
    e.retry_after.map(|d| format!("the provider asked to wait {} s", d.as_secs().max(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_context_window_from_every_vendors_wording() {
        let cases = [
            ("This model's maximum context length is 131072 tokens. However, you requested 140000 tokens (130000 in the messages, 10000 in the completion). Please reduce the length of the messages or completion.", 131072),
            ("prompt is too long: 213000 tokens > 200000 maximum", 200000),
            ("The input token count (300000) exceeds the maximum number of tokens allowed (262144).", 262144),
            ("Prompt contains 150000 tokens and 0 draft tokens, too large for model with 131072 maximum context length", 131072),
            ("This model's maximum prompt length is 131072 but the request contains 140000 tokens.", 131072),
            ("Your input exceeds the context window of this model (256k). Please adjust your input and try again.", 256000),
            ("{\"error\":{\"message\":\"Input is too long for requested model. maximum context length is 1,048,576 tokens\",\"type\":\"invalid_request_error\"}}", 1048576),
            ("context_length_exceeded: the request has 120500 tokens but the limit is 120000", 120000),
            ("the request (9000 tokens) exceeds the available context size (8192 tokens)", 8192),
        ];
        for (msg, want) in cases {
            assert_eq!(context_window(msg), Some(want), "{msg}");
        }
        assert_eq!(context_window("Your input exceeds the context window of this model."), None);
        assert_eq!(context_window("invalid api key"), None);
        // a throttle is not a window
        assert_eq!(
            context_window("Request too large for model `llama-3.3-70b-versatile` in organization `org` on tokens per minute (TPM): Limit 6000, Requested 9000, please reduce your message size"),
            None
        );
    }

    #[test]
    fn reads_the_largest_reply() {
        let cases = [
            ("max_tokens: 65536 > 8192, which is the maximum allowed number of output tokens for claude-3-5-haiku-20241022", 8192),
            ("Invalid 'max_tokens': integer above maximum value. Expected a value <= 16384, but got 65536 instead.", 16384),
            ("max_output_tokens must be at most 32768", 32768),
            ("max_completion_tokens is too large: 200000. This model supports at most 128000 completion tokens, whereas you provided 200000.", 128000),
            ("The maxOutputTokens value (100000) exceeds the limit (65536)", 65536),
            ("max_tokens must be less than or equal to 8192", 8192),
        ];
        for (msg, want) in cases {
            assert_eq!(max_output(msg), Some(want), "{msg}");
            assert_eq!(context_window(msg), None, "not a context message: {msg}");
        }
        assert_eq!(max_output("max_tokens is required"), None);
    }

    #[test]
    fn knows_a_missing_model_and_suggests_names() {
        let e = ProviderError::new(
            ErrorKind::BadRequest,
            "The model `gpt-5-turbo` does not exist or you do not have access to it.",
        );
        assert!(unknown_model(&e));
        let e = ProviderError::new(ErrorKind::BadRequest, "Model Not Exist").with_status(400);
        assert!(unknown_model(&e));
        assert!(!unknown_model(&ProviderError::new(ErrorKind::BadRequest, "bad json")));
        let s = closest("deepseek-chta", ["deepseek-chat", "deepseek-reasoner", "gpt-5"], 2);
        assert_eq!(s, vec!["deepseek-chat", "deepseek-reasoner"]);
    }

    #[test]
    fn numbers_understand_suffixes_and_separators() {
        let n: Vec<u64> =
            numbers("256k tokens, 131,072 max, v3.1 and 1.25 dollars, abc123").into_iter().map(|x| x.n).collect();
        assert_eq!(n, vec![256000, 131072]);
    }
}
