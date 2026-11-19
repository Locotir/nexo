//! Small JSON helpers of the wire format: numbers, arguments and usage.

use nexo_core::Usage;
use serde_json::{json, Value};

/// A temperature as the JSON number the person typed (an `f32` widened
/// to `f64` would carry noise: 0.2 becomes 0.20000000298).
pub(crate) fn temp(t: f32) -> Value {
    json!((t as f64 * 1000.0).round() / 1000.0)
}

/// Arguments as JSON; an empty or broken fragment gives an empty object.
pub(crate) fn parse_args(s: &str) -> Value {
    let s = s.trim();
    if s.is_empty() {
        return json!({});
    }
    match serde_json::from_str(s) {
        // a call with no arguments, written as `null` by some servers
        Ok(Value::Null) => json!({}),
        Ok(v) => v,
        Err(_) => json!({"_raw": s}),
    }
}

/// A token count, whether the server sent `12` or `12.0`.
pub(crate) fn count(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f.round() as u64))
}

/// Arguments of a call as text: a string as is, a JSON object (some local servers) serialised.
pub(super) fn args_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(_) | Value::Array(_) => v.to_string(),
        _ => String::new(),
    }
}

/// Usage from the protocol's counts, with every vendor's cache field.
pub fn usage(u: &Value) -> Usage {
    let prompt = count(&u["prompt_tokens"]).unwrap_or(0);
    let cached = count(&u["prompt_tokens_details"]["cached_tokens"])
        .or_else(|| count(&u["prompt_cache_hit_tokens"]))
        .or_else(|| count(&u["cached_tokens"]))
        .unwrap_or(0);
    // OpenRouter reports cache writes inside `prompt_tokens_details`; they are part of `prompt_tokens`, like
    // reads, so they leave the plain input count and are priced at the write rate.
    let written = count(&u["prompt_tokens_details"]["cache_write_tokens"])
        .or_else(|| count(&u["cache_creation_input_tokens"]))
        .unwrap_or(0);
    let completion = count(&u["completion_tokens"]).unwrap_or(0);
    let reasoning = count(&u["completion_tokens_details"]["reasoning_tokens"]).unwrap_or(0);
    // xAI reports `completion_tokens` without its reasoning tokens and a `total_tokens` that adds them back, and
    // bills them as output; OpenAI's `completion_tokens` already holds them (total == prompt + completion).
    // (a server-sent count can be u64::MAX, so the sum is checked)
    let reasoning_apart = reasoning > 0
        && count(&u["total_tokens"])
            .is_some_and(|t| prompt.checked_add(completion).and_then(|s| s.checked_add(reasoning)) == Some(t));
    Usage {
        input: prompt.saturating_sub(cached).saturating_sub(written),
        output: if reasoning_apart { completion.saturating_add(reasoning) } else { completion },
        cache_read: cached,
        cache_write: written,
        cache_write_1h: 0,
        transformations: 0,
        reasoning,
    }
}
