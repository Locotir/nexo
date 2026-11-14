use super::*;

/// How many of the agent's own text replies in `history` (the text blocks of its messages, whether or not they
/// came with tool calls) the view still shows, whole or as the start of a line the view keeps (the first 60
/// characters, spaces squashed): (kept, all) over every reply, and the same over the latest `recent` of them.
/// The other measures look at results and the words a reply copies; this one asks whether the model can see
/// what it already said, so that it does not say it again (a greeting at each round, a plan told twice).
pub(super) fn own_replies(history: &[Value], view: &[Value], recent: usize) -> ((usize, usize), (usize, usize)) {
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let seen = squash(&view.iter().map(text_of).collect::<Vec<_>>().join("\n"));
    let said: Vec<String> = history
        .iter()
        .filter(|m| m["role"] == "assistant")
        .flat_map(|m| {
            let blocks = m["content"].as_array().cloned().unwrap_or_default();
            let mut texts: Vec<String> =
                blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).map(squash).collect();
            if let Some(s) = m["content"].as_str() {
                texts.push(squash(s));
            }
            texts
        })
        .filter(|t| !t.is_empty())
        .collect();
    let kept = |t: &String| seen.contains(&t.chars().take(60).collect::<String>());
    let all = (said.iter().filter(|t| kept(t)).count(), said.len());
    let tail = &said[said.len().saturating_sub(recent)..];
    (all, (tail.iter().filter(|t| kept(t)).count(), tail.len()))
}
