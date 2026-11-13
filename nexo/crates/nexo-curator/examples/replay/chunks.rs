use super::*;

/// Whether every result line of `m` (8 characters or more, trimmed) is already in a result of `before`:
/// leaving `m` out loses nothing while that earlier result stays.
pub(super) fn repeats(before: &[Value], m: &Value) -> bool {
    let lines = |m: &Value| -> Vec<String> {
        let Some(Value::Array(blocks)) = m.get("content") else { return Vec::new() };
        blocks
            .iter()
            .filter(|b| b["type"] == "tool_result")
            .flat_map(|b| {
                let text = match &b["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(inner) => {
                        inner.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n")
                    }
                    _ => String::new(),
                };
                text.lines().map(str::trim).filter(|l| l.len() >= 8).map(str::to_string).collect::<Vec<_>>()
            })
            .collect()
    };
    let mine = lines(m);
    if mine.is_empty() {
        return false;
    }
    let seen: HashSet<String> = before.iter().flat_map(lines).collect();
    mine.iter().all(|l| seen.contains(l))
}

/// Whether a message carries tool results.
pub(super) fn has_result(m: &Value) -> bool {
    m["content"].as_array().is_some_and(|b| b.iter().any(|x| x["type"] == "tool_result"))
}

/// The signals a curator could read at each request about each run of 8 result lines seen before, with
/// whether a reply from that request on copies one of its distinctive words (the run is used later), one
/// tab-separated row per run and request: label, age in requests, share of its words in the open request,
/// in the agent's last three messages, in anything the agent wrote since it arrived, share of digits in
/// it, whether it came in the open request, its tokens, and keys to the run's text and to the text it is
/// weighed against (the open request and the agent's last three messages), both in the returned texts.
/// A study of what predicts use, not a rule.
pub(super) fn chunk_rows(requests: &[Request]) -> (Vec<String>, Vec<(u64, String)>) {
    let key = |s: &str| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut h);
        h.finish()
    };
    let mut texts: HashMap<u64, String> = HashMap::new();
    let said: Vec<HashSet<String>> = (0..requests.len())
        .map(|i| reply_to(requests, i).map(|m| said_words(m).into_iter().collect()).unwrap_or_default())
        .collect();
    let mut ahead: Vec<HashSet<String>> = vec![HashSet::new(); requests.len() + 1];
    for i in (0..requests.len()).rev() {
        ahead[i] = ahead[i + 1].union(&said[i]).cloned().collect();
    }
    let words = |s: &str| -> HashSet<String> { distinctive_words(s).into_iter().collect() };
    let share = |ws: &HashSet<String>, of: &HashSet<String>| -> f64 {
        if ws.is_empty() {
            0.0
        } else {
            ws.iter().filter(|w| of.contains(*w)).count() as f64 / ws.len() as f64
        }
    };
    let mut born: HashMap<String, usize> = HashMap::new();
    let mut rows = Vec::new();
    for (i, r) in requests.iter().enumerate() {
        let new = i.checked_sub(1).map_or(0, |p| requests[p].messages.len()).min(r.messages.len());
        let start = latest_request(&r.messages);
        let asked = words(&text_of(&r.messages[start]));
        let mine: Vec<usize> = (0..r.messages.len()).filter(|&j| r.messages[j]["role"] == "assistant").collect();
        let recent: HashSet<String> =
            mine.iter().rev().take(3).flat_map(|&j| distinctive_words(&text_of(&r.messages[j]))).collect();
        let context: String = std::iter::once(start)
            .chain(mine.iter().rev().take(3).copied())
            .map(|j| text_of(&r.messages[j]))
            .collect::<Vec<_>>()
            .join("\n");
        let ck = key(&context);
        texts.entry(ck).or_insert(context);
        for (j, m) in r.messages.iter().enumerate() {
            let born_at = *born.entry(m.to_string()).or_insert(i);
            if j >= new || !has_result(m) {
                continue;
            }
            let since: HashSet<String> =
                mine.iter().filter(|&&k| k > j).flat_map(|&k| distinctive_words(&text_of(&r.messages[k]))).collect();
            let text = text_of(m);
            let lines: Vec<&str> = text.lines().collect();
            for c in lines.chunks(8) {
                let run = c.join("\n");
                let ws = words(&run);
                if ws.is_empty() {
                    continue;
                }
                let used = ws.iter().any(|w| ahead[i].contains(w));
                let digits = run.chars().filter(char::is_ascii_digit).count() as f64 / run.len().max(1) as f64;
                rows.push(format!(
                    "{}\t{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{}\t{}\t{}\t{ck}",
                    used as u8,
                    i - born_at,
                    share(&ws, &asked),
                    share(&ws, &recent),
                    share(&ws, &since),
                    digits,
                    (j >= start) as u8,
                    run.len().div_ceil(4),
                    {
                        let rk = key(&run);
                        texts.entry(rk).or_insert_with(|| run.clone());
                        rk
                    }
                ));
            }
        }
    }
    (rows, texts.into_iter().collect())
}
