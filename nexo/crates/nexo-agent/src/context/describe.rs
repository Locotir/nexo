//! What the interface shows of the context policy: its label and its line.

use super::ContextPolicy;

/// What the interface shows of the context policy: a short label for the title
/// bar (`curator`, `curator idle`, `plain`, and `curator off` when `auto`
/// or `curator` fell back to everything) and the line `/context` and `/status` print.
pub fn describe(policy: &dyn ContextPolicy, history_tokens: u64, view_tokens: u64) -> (String, String) {
    describe_since(policy, history_tokens, view_tokens, None)
}

/// [`describe`], with the last call's figures (`history_tokens`, `view_tokens`): when the curator moved
/// messages, the label then says by how many tokens that changed the window in use (`curator -12k tok`).
pub fn describe_since(
    policy: &dyn ContextPolicy,
    history_tokens: u64,
    view_tokens: u64,
    last: Option<(u64, u64)>,
) -> (String, String) {
    let name = policy.name();
    if let Some(why) = policy.detail() {
        return ("curator off".to_string(), why);
    }
    let (label, detail) = describe_base(policy, name, history_tokens, view_tokens);
    let detail = match policy.caveat() {
        Some(c) => format!("{detail}. {c}"),
        None => detail,
    };
    match policy.activity() {
        Some((0, 0)) | None => (label, detail),
        Some((a, r)) => {
            // the curator's own doing: this view against the last one plus what the conversation appended
            // since. A history that shrank was rewound or cleared: nothing to compare with.
            let moved = last
                .filter(|&(h, v)| v > 0 && history_tokens >= h)
                .map(|(h, v)| view_tokens as i64 - (v + (history_tokens - h)) as i64)
                .filter(|d| d.unsigned_abs() >= MOVED_LEAST);
            // ASCII only: a sign and a word read the same in every terminal
            let tag = match moved {
                Some(d) => format!(" {}{} tok", if d < 0 { '-' } else { '+' }, fmt_k(d.unsigned_abs())),
                None => String::new(),
            };
            let what = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
            let window = match moved {
                Some(d) => format!(
                    ": the window in use is {} tokens {} for it",
                    fmt_k(d.unsigned_abs()),
                    if d < 0 { "smaller" } else { "larger" }
                ),
                None => String::new(),
            };
            let line = format!(
                "{detail}. This call: {} brought back, {} taken out or cut{window}",
                what(a, "message", "messages"),
                what(r, "message", "messages")
            );
            (format!("{label}{tag}"), line)
        }
    }
}

pub(super) fn describe_base(
    policy: &dyn ContextPolicy,
    name: &str,
    history_tokens: u64,
    view_tokens: u64,
) -> (String, String) {
    let k = |t: u64| format!("{} tokens", fmt_k(t));
    match name {
        "curator" => {
            let saved = saving(history_tokens, view_tokens);
            if saved > 0 {
                (
                    // the share saved is in the detail; on the bar it reads as noise once the history is huge
                    "curator".to_string(),
                    format!(
                        "curator: this call sends {} of the {} kept ({saved}% less)",
                        k(view_tokens),
                        k(history_tokens)
                    ),
                )
            } else {
                (
                    "curator idle".to_string(),
                    format!(
                        "curator idle: the whole history ({}) still costs less to send than to rewrite, since a \
                         rewrite pays the provider's cache again; it steps in as the history grows (in recorded \
                         sessions, from 30k to 70k tokens)",
                        k(history_tokens)
                    ),
                )
            }
        }
        "compact" => (
            name.to_string(),
            match policy.compact_at() {
                Some(at) => format!("compact: the whole history, summarised past {}", k(at)),
                None => "compact: the whole history".to_string(),
            },
        ),
        _ => (name.to_string(), format!("{name}: every call sends the whole history")),
    }
}

/// The least change of the window, in tokens, the label names: under it the figure is noise.
pub(super) const MOVED_LEAST: u64 = 100;

/// 950, 12k, 1.2M.
pub(super) fn fmt_k(t: u64) -> String {
    match t {
        0..=999 => t.to_string(),
        1_000..=9_999 => format!("{:.1}k", t as f64 / 1e3),
        10_000..=999_999 => format!("{}k", (t as f64 / 1e3).round()),
        _ => format!("{:.1}M", t as f64 / 1e6),
    }
}

/// Percent of the history a view leaves out (0 below 3 %: the curator's
/// squeezing of each message as it arrives is not worth a label).
pub fn saving(history_tokens: u64, view_tokens: u64) -> u64 {
    if history_tokens == 0 || view_tokens >= history_tokens {
        return 0;
    }
    let p = ((history_tokens - view_tokens) as f64 / history_tokens as f64 * 100.0).round() as u64;
    if p < 3 {
        0
    } else {
        p
    }
}
