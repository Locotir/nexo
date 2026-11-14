use super::*;

/// The short numbers of `text` that `distinctive_words` does not see: three-digit numbers and decimals
/// (coins, counts, line numbers, prices), standing alone (not inside a word or a version string).
pub(super) fn short_numbers(text: &str) -> HashSet<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut found = HashSet::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
            i += 1;
        }
        let run: String = chars[start..i].iter().collect();
        let run = run.trim_end_matches('.');
        let alone = (start == 0 || !chars[start - 1].is_alphanumeric())
            && chars.get(start + run.chars().count()).is_none_or(|c| !c.is_alphanumeric());
        let decimal = run.matches('.').count() == 1 && !run.starts_with('.');
        if alone && ((run.len() == 3 && run.bytes().all(|b| b.is_ascii_digit())) || decimal) {
            found.insert(run.to_string());
        }
    }
    found
}

/// The short numbers of `reply` that `history` holds but neither `view` nor `system` does, and how many of
/// its short numbers `history` holds.
pub(super) fn forgotten_short(reply: &Value, history: &[Value], view: &[Value], system: &str) -> (usize, usize) {
    let numbers_of =
        |msgs: &[Value]| -> HashSet<String> { msgs.iter().flat_map(|m| short_numbers(&text_of(m))).collect() };
    let mut seen = numbers_of(view);
    seen.extend(short_numbers(system));
    let held = numbers_of(history);
    let copied: Vec<String> = short_numbers(&said_of(reply)).into_iter().filter(|n| held.contains(n)).collect();
    (copied.iter().filter(|n| !seen.contains(*n)).count(), copied.len())
}

/// Whether the agent has written about a result whose distinctive words are `result` in `later`: a later
/// assistant message with text or a `note` call, or a call whose arguments draw on a word of the result
/// (it read the result and acted on it, as a model that writes no text between calls does).
pub(super) fn wrote_about(later: &[Value], result: &HashSet<String>) -> bool {
    later.iter().any(|m| {
        wrote(m)
            || (m["role"] == "assistant"
                && match &m["content"] {
                    Value::Array(blocks) => blocks
                        .iter()
                        .filter(|b| b["type"] == "tool_use")
                        .any(|b| distinctive_words(&b["input"].to_string()).iter().any(|w| result.contains(w))),
                    _ => false,
                })
    })
}

/// The tokens of results of the latest request in `history` (after the person's last message) that the agent
/// has not written about yet (no text, no `note` call, and no call that draws on a word of the result, in a
/// later assistant message) that `view` leaves out, and the tokens of all such results. A result counts for
/// the share of its lines with a value (a word, number, path or hash) the view lacks anywhere, so a copy
/// elsewhere in the view, or the line written again by a fold, shows the line.
/// The agent is still reading those: what it writes next draws on them, and a reply that paraphrases them
/// shows no copied word for `forgotten` to catch. Results under `SMALL_RESULT` tokens are left out of both
/// counts, as the engine leaves them in place.
pub(super) fn blind(history: &[Value], view: &[Value]) -> (i64, i64) {
    blind_parts(history, view).iter().fold((0, 0), |(m, o), p| (m + p.3, o + p.2))
}

/// The results `blind` counts, one by one: (message, block, tokens, tokens the view lacks).
pub(super) fn blind_parts(history: &[Value], view: &[Value]) -> Vec<(usize, usize, i64, i64)> {
    const SMALL_RESULT: i64 = 100;
    let shown: HashSet<String> = view.iter().flat_map(values).collect();
    let start = latest_request(history);
    let mut out = Vec::new();
    for (k, m) in history.iter().enumerate().skip(start) {
        let Some(Value::Array(blocks)) = m.get("content") else { continue };
        for (bi, b) in blocks.iter().enumerate().filter(|(_, b)| b["type"] == "tool_result") {
            let one = json!({"role": "user", "content": [b]});
            let tokens = tokens_of(&one);
            if tokens < SMALL_RESULT {
                continue;
            }
            let words: HashSet<String> = distinctive_words(&text_of(&one)).into_iter().collect();
            if wrote_about(&history[k + 1..], &words) {
                continue;
            }
            let (lacking, lines) = lacking_lines(&one, &shown);
            out.push((k, bi, tokens, if lines == 0 { 0 } else { tokens * lacking as i64 / lines as i64 }));
        }
    }
    out
}
