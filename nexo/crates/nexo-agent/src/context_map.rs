//! The map `/context live` draws: each message of the history and how much of
//! it the next model call carries; with the curator, also the zone of its
//! memory (anchor or topic) the message is in.

use nexo_core::message::{Message, Part, Role};
use nexo_core::{CallMark, CellKind, CellState, ContextCell, ContextZone};

use crate::context::Memory;
use crate::history::History;

fn kind(m: &Message) -> CellKind {
    let has = |f: fn(&Part) -> bool| m.content.iter().any(f);
    match m.role {
        Role::User if has(|p| matches!(p, Part::ToolResult { .. })) => CellKind::Output,
        Role::User => CellKind::Request,
        Role::Assistant if has(|p| matches!(p, Part::ToolCall { .. })) => CellKind::Call,
        Role::Assistant => CellKind::Reply,
    }
}

/// What ties a message to a shortened copy of it: its call ids, or the start of its text.
fn key(m: &Message) -> Vec<String> {
    let ids: Vec<String> = m
        .content
        .iter()
        .filter_map(|p| match p {
            Part::ToolCall { id, .. } => Some(id.clone()),
            Part::ToolResult { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect();
    if !ids.is_empty() {
        return ids;
    }
    m.content
        .iter()
        .find_map(|p| match p {
            Part::Text { text } if !text.trim().is_empty() => Some(vec![text.trim().chars().take(40).collect()]),
            _ => None,
        })
        .unwrap_or_default()
}

// Hash every equality-relevant field; fingerprints only select a bucket. Exact
// Message equality still resolves collisions before assigning a shared identity.
fn fingerprint(m: &Message) -> u64 {
    use std::hash::{Hash, Hasher};
    fn part(p: &Part, h: &mut impl Hasher) {
        std::mem::discriminant(p).hash(h);
        match p {
            Part::Text { text } => text.hash(h),
            Part::Reasoning { text, signature, encrypted, model } => {
                text.hash(h);
                signature.hash(h);
                encrypted.hash(h);
                model.hash(h);
            }
            Part::RedactedReasoning { data, model } => {
                data.hash(h);
                model.hash(h);
            }
            Part::ToolCall { id, name, args } => {
                id.hash(h);
                name.hash(h);
                args.hash(h);
            }
            Part::ToolResult { call_id, name, content, is_error } => {
                call_id.hash(h);
                name.hash(h);
                content.len().hash(h);
                for p in content {
                    part(p, h);
                }
                is_error.hash(h);
            }
            Part::Image { source } => match source {
                nexo_core::message::ImageSource::Base64 { media_type, data } => {
                    0u8.hash(h);
                    media_type.hash(h);
                    data.hash(h);
                }
                nexo_core::message::ImageSource::Url { url } => {
                    1u8.hash(h);
                    url.hash(h);
                }
            },
        }
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    m.role.hash(&mut h);
    m.content.len().hash(&mut h);
    for p in &m.content {
        part(p, &mut h);
    }
    h.finish()
}

/// Align in order, reporting a match only when every maximum alignment needs
/// that history position and its possible partners agree on its state. Ambiguous
/// duplicates or differing possible states are shown as unknown.
pub fn map(history: &History, view: &[Message]) -> Vec<ContextCell> {
    let mut cells: Vec<_> = history
        .iter()
        .enumerate()
        .map(|(i, h)| ContextCell {
            kind: kind(h),
            tokens: history.message_tokens_at(i).max(1),
            state: CellState::Dropped,
            zone: 0,
        })
        .collect();
    if view.is_empty() {
        return cells;
    }
    let h: Vec<_> = history.iter().collect();
    let (n, m) = (h.len(), view.len());
    // Avoid unbounded quadratic memory for unusually large histories.
    let Some(size) = (n + 1).checked_mul(m + 1).filter(|&s| s <= 2_000_000) else {
        if m > 0 {
            for cell in &mut cells {
                cell.state = CellState::Unknown;
            }
        }
        return cells;
    };
    // Equal messages necessarily share a key. Avoid hashing large payloads for
    // keys occurring at most twice, especially tool results with distinct IDs.
    let keys: Vec<_> = h.iter().copied().chain(view.iter()).map(key).collect();
    let mut key_counts = std::collections::HashMap::new();
    for k in &keys {
        *key_counts.entry(k).or_insert(0usize) += 1;
    }
    // Intern messages once. A fingerprint collision never creates a false match.
    let mut buckets: std::collections::HashMap<u64, Vec<usize>> = std::collections::HashMap::new();
    let mut small_buckets: std::collections::HashMap<&Vec<String>, usize> = std::collections::HashMap::new();
    let mut representatives: Vec<&Message> = Vec::new();
    let mut identity = Vec::with_capacity(n + m);
    for (index, message) in h.iter().copied().chain(view.iter()).enumerate() {
        if key_counts[&keys[index]] <= 2 {
            if let Some(&id) = small_buckets.get(&keys[index]) {
                if representatives[id] == message {
                    identity.push(id);
                    continue;
                }
            }
            let id = representatives.len();
            representatives.push(message);
            small_buckets.insert(&keys[index], id);
            identity.push(id);
            continue;
        }
        let bucket = buckets.entry(fingerprint(message)).or_default();
        let id = bucket.iter().copied().find(|&id| representatives[id] == message).unwrap_or_else(|| {
            let id = representatives.len();
            representatives.push(message);
            bucket.push(id);
            id
        });
        identity.push(id);
    }
    let h_keys: Vec<_> = h
        .iter()
        .map(|message| {
            message
                .content
                .iter()
                .any(|p| matches!(p, Part::ToolCall { .. } | Part::ToolResult { .. }))
                .then(|| key(message))
        })
        .collect();
    let v_keys = &keys[n..];
    // Memoize pair classification: the alignment makes three passes over it.
    let mut states = vec![0u8; n * m];
    for i in 0..n {
        for j in 0..m {
            states[i * m + j] = if identity[i] == identity[n + j] {
                1
            } else if h[i].role == view[j].role && h_keys[i].as_ref().is_some_and(|k| *k == v_keys[j]) {
                2
            } else {
                0
            };
        }
    }
    let state = |i: usize, j: usize| match states[i * m + j] {
        1 => Some(CellState::Whole),
        2 => Some(CellState::Shrunk),
        _ => None,
    };
    let idx = |i: usize, j: usize| i * (m + 1) + j;
    let mut pre = vec![0u32; size];
    for i in 0..n {
        for j in 0..m {
            pre[idx(i + 1, j + 1)] =
                pre[idx(i, j + 1)].max(pre[idx(i + 1, j)]).max(pre[idx(i, j)] + u32::from(state(i, j).is_some()));
        }
    }
    let mut post = vec![0u32; size];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            post[idx(i, j)] = post[idx(i + 1, j)]
                .max(post[idx(i, j + 1)])
                .max(post[idx(i + 1, j + 1)] + u32::from(state(i, j).is_some()));
        }
    }
    let best = pre[idx(n, m)];
    // Any view message that can be skipped in a maximum alignment may instead
    // be transformed text, even if it matches another history message. Do not
    // assert that an unmatched history message of the same role was dropped.
    let unidentified: Vec<bool> =
        (0..m).map(|j| (0..=n).any(|i| pre[idx(i, j)] + post[idx(i, j + 1)] == best)).collect();
    for i in 0..n {
        let can_skip = (0..=m).any(|j| pre[idx(i, j)] + post[idx(i + 1, j)] == best);
        let mut matched = None;
        let mut conflict = false;
        for j in 0..m {
            if let Some(s) = state(i, j) {
                if pre[idx(i, j)] + 1 + post[idx(i + 1, j + 1)] == best {
                    if matched.is_some_and(|prior| prior != s) {
                        conflict = true;
                    }
                    matched = Some(s);
                }
            }
        }
        cells[i].state = match (can_skip, matched, conflict) {
            (_, None, _) if (0..m).any(|j| unidentified[j] && h[i].role == view[j].role) => CellState::Unknown,
            (_, None, _) => CellState::Dropped,
            (false, Some(s), false) => s,
            _ => CellState::Unknown,
        };
    }
    cells
}

/// Where the output of each tool call in `history` stands, from its message's cell: sent whole within
/// the first `same` messages of `view` (messages equal to the request before) is `Cached`, sent whole
/// after it `Sent`. `Cached` is a legacy name for a repeated-prefix candidate, not evidence of a
/// provider cache hit: system/tools, cache lifetime and provider confirmation are not checked here.
pub fn calls(history: &[Message], cells: &[ContextCell], view: &[Message], same: usize) -> Vec<(String, CallMark)> {
    let ids = |m: &Message| -> Vec<String> {
        m.content
            .iter()
            .filter_map(|p| match p {
                Part::ToolResult { call_id, .. } => Some(call_id.clone()),
                _ => None,
            })
            .collect()
    };
    let repeated: std::collections::HashSet<String> = view[..same.min(view.len())].iter().flat_map(ids).collect();
    history
        .iter()
        .zip(cells)
        .flat_map(|(h, c)| {
            h.content.iter().filter_map(|part| {
                let Part::ToolResult { call_id: id, .. } = part else { return None };
                let mark = match c.state {
                    CellState::Whole if repeated.contains(id) => CallMark::Cached,
                    CellState::Whole => CallMark::Sent,
                    CellState::Shrunk => {
                        // The cell doesn't retain its alignment. Only infer a per-result
                        // state when its message and ID have unambiguous counterparts.
                        let k = key(h);
                        let mut hs = history.iter().filter(|x| x.role == h.role && key(x) == k);
                        let mut vs = view.iter().enumerate().filter(|(_, x)| x.role == h.role && key(x) == k);
                        let unique_history = hs.next().is_some() && hs.next().is_none();
                        let candidate = vs.next();
                        if !unique_history || vs.next().is_some() {
                            CallMark::Unknown
                        } else if let Some((j, v)) = candidate {
                            let mut originals = h
                                .content
                                .iter()
                                .filter(|p| matches!(p, Part::ToolResult { call_id, .. } if call_id == id));
                            let mut results = v
                                .content
                                .iter()
                                .filter(|p| matches!(p, Part::ToolResult { call_id, .. } if call_id == id));
                            match (originals.next(), originals.next(), results.next(), results.next()) {
                                (Some(a), None, Some(b), None) if a == b && j < same.min(view.len()) => {
                                    CallMark::Cached
                                }
                                (Some(a), None, Some(b), None) if a == b => CallMark::Sent,
                                (Some(_), None, Some(_), None) => CallMark::Cut,
                                _ => CallMark::Unknown,
                            }
                        } else {
                            CallMark::Unknown
                        }
                    }
                    CellState::Dropped => CallMark::Out,
                    CellState::Recalled => CallMark::Back,
                    CellState::Unknown => CallMark::Unknown,
                };
                Some((id.clone(), mark))
            })
        })
        .collect()
}

/// The tokens of the output of each tool call in `history`, for the messages that carry just that one
/// output (several outputs in a message share one count, so none is given for them).
pub fn call_tokens(history: &[Message], cells: &[ContextCell]) -> Vec<(String, u64)> {
    history
        .iter()
        .zip(cells)
        .filter_map(|(h, c)| {
            let mut ids = h.content.iter().filter_map(|p| match p {
                Part::ToolResult { call_id, .. } => Some(call_id.clone()),
                _ => None,
            });
            let id = ids.next()?;
            ids.next().is_none().then_some((id, c.tokens))
        })
        .collect()
}

/// The curator's own map of its memory, with the kinds taken from `history`;
/// `None` when it does not cover the history message for message.
pub fn from_memory(history: &[Message], mem: &Memory) -> Option<(Vec<ContextCell>, Vec<ContextZone>)> {
    if mem.cells.len() != history.len() || mem.cells.iter().any(|c| c.zone as usize >= mem.zones.len()) {
        return None;
    }
    let cells = history
        .iter()
        .zip(&mem.cells)
        .map(|(h, c)| {
            let state = match c.state.as_str() {
                "whole" => CellState::Whole,
                "cut" => CellState::Shrunk,
                "recalled" => CellState::Recalled,
                _ => CellState::Dropped,
            };
            ContextCell { kind: kind(h), tokens: c.tokens.max(1), state, zone: c.zone }
        })
        .collect();
    Some((cells, mem.zones.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference implementation retained to verify exact alignment semantics.
    fn legacy_map(history: &History, view: &[Message]) -> Vec<ContextCell> {
        let mut cells: Vec<_> = history
            .iter()
            .enumerate()
            .map(|(i, h)| ContextCell {
                kind: kind(h),
                tokens: history.message_tokens_at(i).max(1),
                state: CellState::Dropped,
                zone: 0,
            })
            .collect();
        let h: Vec<_> = history.iter().collect();
        let (n, m) = (h.len(), view.len());
        // Avoid unbounded quadratic memory for unusually large histories.
        let Some(size) = (n + 1).checked_mul(m + 1).filter(|&s| s <= 2_000_000) else {
            if m > 0 {
                for cell in &mut cells {
                    cell.state = CellState::Unknown;
                }
            }
            return cells;
        };
        let state = |i: usize, j: usize| {
            if h[i] == &view[j] {
                Some(CellState::Whole)
            } else if h[i].role == view[j].role
                && h[i].content.iter().any(|p| matches!(p, Part::ToolCall { .. } | Part::ToolResult { .. }))
                && key(h[i]) == key(&view[j])
            {
                Some(CellState::Shrunk)
            } else {
                None
            }
        };
        let idx = |i: usize, j: usize| i * (m + 1) + j;
        let mut pre = vec![0u32; size];
        for i in 0..n {
            for j in 0..m {
                pre[idx(i + 1, j + 1)] =
                    pre[idx(i, j + 1)].max(pre[idx(i + 1, j)]).max(pre[idx(i, j)] + u32::from(state(i, j).is_some()));
            }
        }
        let mut post = vec![0u32; size];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                post[idx(i, j)] = post[idx(i + 1, j)]
                    .max(post[idx(i, j + 1)])
                    .max(post[idx(i + 1, j + 1)] + u32::from(state(i, j).is_some()));
            }
        }
        let best = pre[idx(n, m)];
        let unidentified: Vec<bool> =
            (0..m).map(|j| (0..=n).any(|i| pre[idx(i, j)] + post[idx(i, j + 1)] == best)).collect();
        for i in 0..n {
            let can_skip = (0..=m).any(|j| pre[idx(i, j)] + post[idx(i + 1, j)] == best);
            let mut matched = None;
            let mut conflict = false;
            for j in 0..m {
                if let Some(s) = state(i, j) {
                    if pre[idx(i, j)] + 1 + post[idx(i + 1, j + 1)] == best {
                        if matched.is_some_and(|prior| prior != s) {
                            conflict = true;
                        }
                        matched = Some(s);
                    }
                }
            }
            cells[i].state = match (can_skip, matched, conflict) {
                (_, None, _) if (0..m).any(|j| unidentified[j] && h[i].role == view[j].role) => CellState::Unknown,
                (_, None, _) => CellState::Dropped,
                (false, Some(s), false) => s,
                _ => CellState::Unknown,
            };
        }
        cells
    }

    #[test]
    fn optimized_matches_reference_for_mixed_and_ambiguous_messages() {
        let messages = [
            Message::user("same"),
            Message::assistant("same"),
            result("a", "long"),
            result("a", "short"),
            result("b", "long"),
            Message::new(Role::User, vec![Part::text("same"), Part::tool_result("a", "nested", false)]),
        ];
        for seed in 0..200 {
            let h: Vec<_> = (0..6).map(|i| messages[(seed * 7 + i * i + i * seed) % messages.len()].clone()).collect();
            let v: Vec<_> = (0..5).map(|i| messages[(seed * 3 + i * 5 + i * seed) % messages.len()].clone()).collect();
            let history = History::new(h);
            assert_eq!(
                map(&history, &v).iter().map(|c| c.state).collect::<Vec<_>>(),
                legacy_map(&history, &v).iter().map(|c| c.state).collect::<Vec<_>>(),
                "seed {seed}"
            );
        }
    }

    #[test]
    #[ignore = "manual performance comparison"]
    fn measure_alignment_against_reference() {
        let payload = "x".repeat(128 * 1024);
        let messages: Vec<_> = (0..250).map(|i| result("shared-id", &format!("{payload}{i}"))).collect();
        let view = messages.iter().rev().cloned().collect::<Vec<_>>();
        let history = History::new(messages);
        let start = std::time::Instant::now();
        let old = legacy_map(&history, &view);
        let old_time = start.elapsed();
        let start = std::time::Instant::now();
        let new = map(&history, &view);
        let new_time = start.elapsed();
        assert_eq!(old.iter().map(|c| c.state).collect::<Vec<_>>(), new.iter().map(|c| c.state).collect::<Vec<_>>());
        eprintln!("legacy={old_time:?} optimized={new_time:?}");
    }

    fn result(id: &str, text: &str) -> Message {
        Message::tool_results(vec![Part::ToolResult {
            call_id: id.into(),
            name: Some("read".into()),
            content: vec![Part::text(text)],
            is_error: false,
        }])
    }

    #[test]
    fn whole_shrunk_and_dropped() {
        let call = Message {
            role: Role::Assistant,
            content: vec![Part::tool_call("c1", "read", serde_json::json!({"path": "a"}))],
        };
        let history = vec![
            Message::user("first question"),
            Message::assistant("an answer"),
            call.clone(),
            result("c1", "a long file ".repeat(50).as_str()),
            Message::user("second question"),
        ];
        let view = vec![call, result("c1", "[curator] read output left out"), Message::user("second question")];
        let states: Vec<_> = map(&History::new(history.clone()), &view).iter().map(|c| (c.kind, c.state)).collect();
        assert_eq!(
            states,
            [
                (CellKind::Request, CellState::Dropped),
                (CellKind::Reply, CellState::Dropped),
                (CellKind::Call, CellState::Whole),
                (CellKind::Output, CellState::Shrunk),
                (CellKind::Request, CellState::Whole),
            ]
        );
    }

    #[test]
    fn the_curators_memory_brings_its_zones() {
        use crate::context::MemoryCell;
        let history = vec![Message::user("a"), Message::assistant("b"), Message::user("c")];
        let zone = |label: &str, active| ContextZone { label: label.into(), active, card: false };
        let cell = |zone, state: &str| MemoryCell { zone, tokens: 5, state: state.into() };
        let mem = Memory {
            zones: vec![zone("anchor", true), zone("other", false), zone("this", true)],
            cells: vec![cell(0, "whole"), cell(1, "recalled"), cell(2, "cut")],
            curated: Vec::new(),
            relation_quotes: None,
        };
        let (cells, zones) = from_memory(&history, &mem).unwrap();
        assert_eq!(zones.len(), 3);
        let got: Vec<_> = cells.iter().map(|c| (c.kind, c.state, c.zone)).collect();
        assert_eq!(
            got,
            [
                (CellKind::Request, CellState::Whole, 0),
                (CellKind::Reply, CellState::Recalled, 1),
                (CellKind::Request, CellState::Shrunk, 2),
            ]
        );
        // a map of another history is not used
        assert!(from_memory(&history[..2], &mem).is_none());
    }

    #[test]
    fn repeated_tail_and_common_prefix_are_not_confused() {
        let prefix = "x".repeat(40);
        let h = vec![Message::user("again"), Message::assistant("reply"), Message::user("again")];
        let states: Vec<_> = map(&History::new(h.clone()), &h[2..]).iter().map(|c| c.state).collect();
        assert_eq!(states, [CellState::Unknown, CellState::Dropped, CellState::Unknown]);
        let states: Vec<_> = map(&History::new(h.clone()), &h[..2]).iter().map(|c| c.state).collect();
        assert_eq!(states, [CellState::Whole, CellState::Whole, CellState::Dropped]);
        let h = vec![Message::user(format!("{prefix} one"))];
        let view = vec![Message::user(format!("{prefix} two"))];
        assert_eq!(map(&History::new(h), &view)[0].state, CellState::Unknown);
        let h = History::new(vec![Message::user("original")]);
        assert_eq!(map(&h, &[Message::user("completely rewritten")])[0].state, CellState::Unknown);
        assert_eq!(map(&h, &[])[0].state, CellState::Dropped);
    }

    #[test]
    fn excess_and_ambiguous_view_matches_do_not_imply_dropped() {
        let history = History::new(vec![Message::user("A"), Message::user("B")]);
        let view = vec![Message::user("B"), Message::user("B")];
        let states: Vec<_> = map(&history, &view).iter().map(|c| c.state).collect();
        assert_eq!(states, [CellState::Unknown, CellState::Whole]);

        // A view message can also be skipped because matching it would violate order.
        let view = vec![Message::user("B"), Message::user("A")];
        let history = History::new(vec![Message::user("X"), Message::user("A"), Message::user("B")]);
        assert_eq!(map(&history, &view)[0].state, CellState::Unknown);

        // An excess assistant message cannot explain a missing user message.
        let history = History::new(vec![Message::user("A"), Message::assistant("B")]);
        let view = vec![Message::assistant("B"), Message::assistant("B")];
        assert_eq!(map(&history, &view)[0].state, CellState::Dropped);
    }

    #[test]
    fn ambiguous_tool_results_and_size_limit_are_unknown() {
        let h = vec![result("c", "a"), result("c", "a")];
        let view = vec![result("c", "a")];
        let cells = map(&History::new(h.clone()), &view);
        assert!(cells.iter().all(|c| c.state == CellState::Unknown));
        assert!(calls(&h, &cells, &view, 1).iter().all(|(_, m)| *m == CallMark::Unknown));
        let many = vec![Message::user("a"); 1414];
        assert!(map(&History::new(many.clone()), &many).iter().all(|c| c.state == CellState::Unknown));
    }

    #[test]
    fn grouped_results_distinguish_cut_whole_and_ambiguous() {
        let grouped = |a: &str, b: &str| {
            Message::tool_results(vec![Part::tool_result("a", a, false), Part::tool_result("b", b, false)])
        };
        let h = vec![grouped("long", "intact")];
        let view = vec![grouped("short", "intact")];
        let cells = map(&History::new(h.clone()), &view);
        assert_eq!(cells[0].state, CellState::Shrunk);
        assert_eq!(calls(&h, &cells, &view, 0), vec![("a".into(), CallMark::Cut), ("b".into(), CallMark::Sent)]);
        assert_eq!(calls(&h, &cells, &view, 1)[1].1, CallMark::Cached);
        let ambiguous_view = vec![grouped("short", "intact"), grouped("other", "changed")];
        let cells = map(&History::new(h.clone()), &ambiguous_view);
        assert!(calls(&h, &cells, &ambiguous_view, 0).iter().all(|(_, mark)| *mark == CallMark::Unknown));
        let duplicate =
            Message::tool_results(vec![Part::tool_result("a", "long", false), Part::tool_result("a", "intact", false)]);
        let changed = Message::tool_results(vec![
            Part::tool_result("a", "short", false),
            Part::tool_result("a", "intact", false),
        ]);
        let cells = map(&History::new(vec![duplicate.clone()]), std::slice::from_ref(&changed));
        assert!(calls(&[duplicate], &cells, &[changed], 0).iter().all(|(_, mark)| *mark == CallMark::Unknown));
    }

    #[test]
    fn everything_sent_is_all_whole() {
        let h = vec![Message::user("a"), Message::assistant("b")];
        assert!(map(&History::new(h.clone()), &h).iter().all(|c| c.state == CellState::Whole));
    }

    #[test]
    fn each_call_says_where_its_output_stands() {
        let call = |id: &str| Message {
            role: Role::Assistant,
            content: vec![Part::tool_call(id, "read", serde_json::json!({"path": id}))],
        };
        let history = vec![
            Message::user("q"),
            call("c1"),
            result("c1", "one"),
            call("c2"),
            result("c2", "two"),
            call("c3"),
            result("c3", "three"),
        ];
        // the first three messages repeat the request before, c2 is paid in full, c3 left out
        let view = history[..5].to_vec();
        let cells = map(&History::new(history.clone()), &view);
        let got = calls(&history, &cells, &view, 3);
        assert_eq!(
            got,
            vec![("c1".into(), CallMark::Cached), ("c2".into(), CallMark::Sent), ("c3".into(), CallMark::Out)]
        );
        // nothing repeated (a first request, or one after a change at the start): all paid
        assert_eq!(calls(&history, &cells, &view, 0)[0].1, CallMark::Sent);
        let tokens = call_tokens(&history, &cells);
        assert_eq!(tokens.iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), ["c1", "c2", "c3"]);
        assert!(tokens.iter().all(|t| t.1 > 0));
    }
}
