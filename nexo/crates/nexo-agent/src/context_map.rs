//! The map `/context live` draws: each message of the history and how much of
//! it the next model call carries; with the curator, also the zone of its
//! memory (anchor or topic) the message is in.

use nexo_core::message::{estimate_tokens, Message, Part, Role};
use nexo_core::{CallMark, CellKind, CellState, ContextCell, ContextZone};

use crate::context::Memory;

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

/// Each message of `history` as whole, shrunk or dropped in `view`. Both keep
/// the history's order, so a message is looked for after the last one found.
pub fn map(history: &[Message], view: &[Message]) -> Vec<ContextCell> {
    let mut from = 0;
    history
        .iter()
        .map(|h| {
            let tokens = estimate_tokens(std::slice::from_ref(h)).max(1);
            let k = key(h);
            let state = match view[from..].iter().position(|v| v == h) {
                Some(i) => {
                    from += i + 1;
                    CellState::Whole
                }
                None => match view[from..].iter().position(|v| v.role == h.role && !k.is_empty() && key(v) == k) {
                    Some(i) => {
                        from += i + 1;
                        CellState::Shrunk
                    }
                    None => CellState::Dropped,
                },
            };
            ContextCell { kind: kind(h), tokens, state, zone: 0 }
        })
        .collect()
}

/// Where the output of each tool call in `history` stands, from its message's cell: sent whole within
/// the first `same` messages of `view` (the part equal to the request before, which the provider's cache
/// serves) is `Cached`, sent whole after it `Sent`.
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
    let cached: std::collections::HashSet<String> = view[..same.min(view.len())].iter().flat_map(ids).collect();
    history
        .iter()
        .zip(cells)
        .flat_map(|(h, c)| {
            ids(h).into_iter().map(|id| {
                let mark = match c.state {
                    CellState::Whole if cached.contains(&id) => CallMark::Cached,
                    CellState::Whole => CallMark::Sent,
                    CellState::Shrunk => CallMark::Cut,
                    CellState::Dropped => CallMark::Out,
                    CellState::Recalled => CallMark::Back,
                };
                (id, mark)
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
        let states: Vec<_> = map(&history, &view).iter().map(|c| (c.kind, c.state)).collect();
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
    fn everything_sent_is_all_whole() {
        let h = vec![Message::user("a"), Message::assistant("b")];
        assert!(map(&h, &h).iter().all(|c| c.state == CellState::Whole));
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
        let cells = map(&history, &view);
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
