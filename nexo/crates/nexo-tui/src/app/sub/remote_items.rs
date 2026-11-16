//! Turning a remote session's snapshot into transcript items, and making tool text safe to draw.

#[cfg(any(unix, test))]
use super::*;

/// Never pass terminal control characters from a tool to the terminal renderer.
pub(super) fn safe_tool_text(text: &str) -> String {
    text.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect()
}

pub(super) fn safe_tool_args(text: &str) -> serde_json::Value {
    fn clean(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(s) => *s = safe_tool_text(s),
            serde_json::Value::Array(a) => a.iter_mut().for_each(clean),
            serde_json::Value::Object(o) => {
                let old = std::mem::take(o);
                for (key, mut value) in old {
                    clean(&mut value);
                    o.insert(safe_tool_text(&key), value);
                }
            }
            _ => {}
        }
    }
    let mut value = serde_json::from_str(text).unwrap_or_else(|_| serde_json::Value::String(text.to_owned()));
    clean(&mut value);
    value
}

/// Rebuild from the retained atomic event suffix. History is indexed, whereas live
/// assistant completion and tool events overlap their streamed representations.
#[cfg(any(unix, test))]
pub(super) fn remote_items(snapshot: &nexo_core::agent::ObservationSnapshot) -> (Vec<SubItem>, String) {
    use std::collections::HashSet;
    let mut items = Vec::new();
    let mut partial = String::new();
    let mut flushed = String::new();
    let mut history = HashSet::new();
    let mut calls = HashSet::new();
    let mut results = HashSet::new();
    let mut expected = 1u64;
    if snapshot.dropped > 0 {
        items.push(SubItem::Text {
            text: format!("[remote observation: {} earlier events no longer kept]", snapshot.dropped),
        });
        expected = snapshot.dropped.saturating_add(1);
    }
    for event in &snapshot.events {
        if event.sequence != expected {
            items.push(SubItem::Text {
                text: format!("[remote observation sequence gap: expected {expected}, got {}]", event.sequence),
            });
        }
        expected = event.sequence.saturating_add(1);
        if event.truncated {
            items.push(SubItem::Text { text: format!("[remote event {} truncated or parts omitted]", event.sequence) });
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&event.json) else {
            if !event.truncated {
                items.push(SubItem::Text { text: format!("[remote event {} invalid]", event.sequence) });
            }
            continue;
        };
        match event.kind.as_str() {
            "history" | "assistant_done" => {
                if event.kind == "history" && !history.insert(v[0].as_u64()) {
                    continue;
                }
                let msg = if event.kind == "history" { &v[1] } else { &v };
                let role = msg[0].as_str().unwrap_or("").to_ascii_lowercase();
                if event.kind == "assistant_done" {
                    partial.clear();
                }
                if let Some(parts) = msg[1].as_array() {
                    let full_text: String = parts
                        .iter()
                        .filter(|p| p["type"].as_str() == Some("text"))
                        .map(|p| safe_tool_text(p["text"].as_str().unwrap_or("")))
                        .collect();
                    let mut skip = if event.kind == "assistant_done" && full_text.starts_with(&flushed) {
                        flushed.len()
                    } else {
                        0
                    };
                    for part in parts {
                        match part["type"].as_str().unwrap_or("") {
                            "text" if role == "user" => items
                                .push(SubItem::Person { text: safe_tool_text(part["text"].as_str().unwrap_or("")) }),
                            "text" if role == "assistant" => {
                                let text = safe_tool_text(part["text"].as_str().unwrap_or(""));
                                let n = skip.min(text.len());
                                skip -= n;
                                if n < text.len() {
                                    items.push(SubItem::Text { text: text[n..].to_owned() });
                                }
                            }
                            "reasoning" => items.push(SubItem::Text {
                                text: format!(
                                    "[remote reasoning] {}",
                                    safe_tool_text(part["text"].as_str().unwrap_or(""))
                                ),
                            }),
                            "tool_call" => {
                                remote_call(&mut items, &mut calls, &part["id"], &part["name"], &part["args"], "")
                            }
                            "tool_result" => remote_result(
                                &mut items,
                                &mut calls,
                                &mut results,
                                &part["call_id"],
                                part["is_error"].as_bool().unwrap_or(false),
                                &remote_result_text(&part["content"]),
                            ),
                            "omitted" => items.push(SubItem::Text { text: "[remote message part omitted]".into() }),
                            _ => {}
                        }
                    }
                }
                if event.kind == "assistant_done" {
                    flushed.clear();
                }
            }
            "text_delta" => {
                if let Some(t) = v.as_str() {
                    partial.push_str(&safe_tool_text(t));
                }
            }
            "assistant_start" | "turn_end" => {
                if !partial.is_empty() {
                    items.push(SubItem::Text { text: std::mem::take(&mut partial) });
                }
                flushed.clear();
            }
            "tool_call" => {
                if !partial.is_empty() {
                    flushed.push_str(&partial);
                    items.push(SubItem::Text { text: std::mem::take(&mut partial) });
                }
                remote_call(&mut items, &mut calls, &v[0], &v[1], &v[2], v[3].as_str().unwrap_or(""));
            }
            "tool_done" => remote_result(
                &mut items,
                &mut calls,
                &mut results,
                &v[0],
                v[2].as_bool().unwrap_or(false),
                v[1].as_str().unwrap_or(""),
            ),
            "tool_progress" => {
                if let (Some(id), Some(line)) = (v[0].as_str(), v[1].as_str()) {
                    items.push(SubItem::Text {
                        text: format!("[remote tool {} progress] {}", safe_tool_text(id), safe_tool_text(line)),
                    });
                }
            }
            "preparing" | "notice" | "shell_output" => items
                .push(SubItem::Text { text: format!("[remote {}] {}", event.kind, safe_tool_text(&v.to_string())) }),
            _ => {}
        }
    }
    if expected <= snapshot.sequence {
        items.push(SubItem::Text {
            text: format!("[remote observation sequence gap: events {expected}–{} unavailable]", snapshot.sequence),
        });
    }
    if snapshot.permission_waiting {
        items.push(SubItem::Text { text: "[remote session waiting for permission]".into() });
    }
    if snapshot.question_waiting {
        items.push(SubItem::Text { text: "[remote session waiting for an answer]".into() });
    }
    (items, partial)
}

#[cfg(any(unix, test))]
fn remote_call(
    items: &mut Vec<SubItem>,
    calls: &mut std::collections::HashSet<String>,
    id: &serde_json::Value,
    name: &serde_json::Value,
    args: &serde_json::Value,
    summary: &str,
) {
    if let (Some(id), Some(name)) = (id.as_str(), name.as_str()) {
        if calls.insert(id.to_owned()) {
            items.push(SubItem::Call {
                id: id.into(),
                name: safe_tool_text(name),
                args: args.to_string(),
                summary: safe_tool_text(summary),
            });
        }
    }
}

#[cfg(any(unix, test))]
fn remote_result(
    items: &mut Vec<SubItem>,
    calls: &mut std::collections::HashSet<String>,
    results: &mut std::collections::HashSet<String>,
    id: &serde_json::Value,
    error: bool,
    output: &str,
) {
    if let Some(id) = id.as_str() {
        if results.insert(id.to_owned()) {
            if calls.insert(id.to_owned()) {
                items.push(SubItem::Call {
                    id: id.into(),
                    name: "unknown remote tool (call not retained)".into(),
                    args: "null".into(),
                    summary: String::new(),
                });
            }
            items.push(SubItem::Result {
                id: id.into(),
                ok: !error,
                bytes: output.len(),
                output: safe_tool_text(output),
            });
        }
    }
}

#[cfg(any(unix, test))]
fn remote_result_text(parts: &serde_json::Value) -> String {
    parts
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .map(|p| match p["type"].as_str() {
                    Some("text") | Some("reasoning") => p["text"].as_str().unwrap_or("").to_owned(),
                    _ => "[remote result part omitted]".into(),
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| "[remote result unavailable]".into())
}

#[cfg(test)]
mod remote_tests {
    use super::*;
    use crate::app::sub::remote::merge_remote;
    use nexo_core::agent::{channels, AgentEvent};
    use nexo_core::message::{Message, Part, Role};
    use nexo_core::tool::ToolOutput;

    #[test]
    fn rejected_drafts_preserve_extensions_and_independent_edits() {
        assert_eq!(recover_rejected("hola mundo", "hola"), "hola mundo");
        assert_eq!(recover_rejected("hola", "hola"), "hola");
        assert_eq!(recover_rejected("", "hola"), "hola");
        assert_eq!(recover_rejected("otra cosa", "hola"), "otra cosa\n\nhola");
    }

    #[test]
    fn remote_rejection_keeps_retyped_extension() {
        let (commands, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(crate::TuiOptions::default(), commands);
        let (_tx, rx) = std::sync::mpsc::sync_channel(1);
        app.open_remote_receiver("remote", rx);
        app.input.set_text("hola mundo");
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        app.sub_view.as_mut().unwrap().remote.as_mut().unwrap().delivery =
            RemoteDelivery::Sending { text: "hola".into(), request_id: "id".into(), rx };
        tx.send(RemoteReceipt::Rejected("offline".into())).unwrap();
        app.remote_delivery_tick();
        assert_eq!(app.input.full_text(), "hola mundo");
    }

    #[test]
    fn empty_remote_polls_do_not_rebuild_or_accumulate_notices() {
        let (commands, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(crate::TuiOptions::default(), commands);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        app.open_remote_receiver("remote", rx);
        let (observer, ends) = channels();
        ends.events.send(AgentEvent::SessionChanged { session_id: "remote".into(), title: String::new() }).unwrap();
        let first = observer.observation.snapshot();
        tx.send(Ok(RemoteUpdate::Initial(first.clone()))).unwrap();
        app.remote_tick();
        let initial = format!("{:?}", app.blocks);
        app.dirty = false;
        for _ in 0..100 {
            tx.send(Ok(RemoteUpdate::Delta(observer.observation.snapshot_since(&first).unwrap()))).unwrap();
            app.remote_tick();
        }
        assert_eq!(format!("{:?}", app.blocks), initial);
        assert!(!app.dirty);
        tx.send(Err("offline".into())).unwrap();
        app.remote_tick();
        assert!(format!("{:?}", app.blocks).contains("offline"));
        tx.send(Ok(RemoteUpdate::Delta(observer.observation.snapshot_since(&first).unwrap()))).unwrap();
        app.remote_tick();
        assert!(!format!("{:?}", app.blocks).contains("offline"));
    }

    #[test]
    fn incremental_merge_rejects_duplicates_and_resyncs_after_eviction() {
        let (handle, ends) = channels();
        ends.events.send(AgentEvent::SessionChanged { session_id: "remote".into(), title: String::new() }).unwrap();
        ends.events.send(AgentEvent::TextDelta { text: "one".into() }).unwrap();
        let first = handle.observation.snapshot();
        let mut state = None;
        assert!(!merge_remote(&mut state, RemoteUpdate::Initial(first.clone())).unwrap());
        ends.events.send(AgentEvent::TextDelta { text: "two".into() }).unwrap();
        let delta = handle.observation.snapshot_since(&first).unwrap();
        assert!(!merge_remote(&mut state, RemoteUpdate::Delta(delta.clone())).unwrap());
        assert_eq!(remote_items(state.as_ref().unwrap()).1, "onetwo");
        assert!(merge_remote(&mut state, RemoteUpdate::Delta(delta)).is_err());
        for _ in 0..520 {
            ends.events.send(AgentEvent::TextDelta { text: "x".into() }).unwrap();
        }
        let replacement = handle.observation.snapshot_since(&first).unwrap_err();
        assert!(merge_remote(&mut state, RemoteUpdate::Resync(replacement)).unwrap());
        assert!(state.as_ref().unwrap().dropped > 0);
        assert!(format!("{:?}", remote_items(state.as_ref().unwrap()).0).contains("earlier events"));
    }

    #[test]
    fn replay_and_live_tool_events_do_not_duplicate() {
        let (handle, ends) = channels();
        let tx = ends.events;
        tx.send(AgentEvent::SessionChanged { session_id: "remote".into(), title: String::new() }).unwrap();
        let msg = Message::new(
            Role::Assistant,
            vec![
                Part::text("hello"),
                Part::ToolCall { id: "call".into(), name: "bash".into(), args: serde_json::json!({"cmd":"ls"}) },
            ],
        );
        tx.send(AgentEvent::History { index: 0, message: msg }).unwrap();
        tx.send(AgentEvent::ToolCall {
            id: "call".into(),
            name: "bash".into(),
            args: serde_json::json!({"cmd":"ls"}),
            summary: "ls".into(),
        })
        .unwrap();
        tx.send(AgentEvent::ToolDone { id: "call".into(), output: ToolOutput::ok("done"), elapsed_ms: 0 }).unwrap();
        tx.send(AgentEvent::TextDelta { text: "live".into() }).unwrap();
        let (items, partial) = remote_items(&handle.observation.snapshot());
        assert_eq!(items.iter().filter(|i| matches!(i, SubItem::Call { .. })).count(), 1);
        assert_eq!(items.iter().filter(|i| matches!(i, SubItem::Result { output, .. } if output == "done")).count(), 1);
        assert_eq!(partial, "live");
        tx.send(AgentEvent::AssistantDone { message: Message::new(Role::Assistant, vec![Part::text("live reply")]) })
            .unwrap();
        let (items, partial) = remote_items(&handle.observation.snapshot());
        assert!(partial.is_empty());
        assert_eq!(items.iter().filter(|i| matches!(i, SubItem::Text { text } if text == "live reply")).count(), 1);
    }

    #[test]
    fn streamed_text_before_tool_is_not_repeated_on_completion() {
        let (handle, ends) = channels();
        let tx = ends.events;
        tx.send(AgentEvent::SessionChanged { session_id: "remote".into(), title: String::new() }).unwrap();
        tx.send(AgentEvent::TextDelta { text: "before ".into() }).unwrap();
        tx.send(AgentEvent::ToolCall {
            id: "c".into(),
            name: "bash".into(),
            args: serde_json::json!({}),
            summary: String::new(),
        })
        .unwrap();
        tx.send(AgentEvent::TextDelta { text: "after".into() }).unwrap();
        tx.send(AgentEvent::AssistantDone {
            message: Message::new(
                Role::Assistant,
                vec![
                    Part::text("before "),
                    Part::ToolCall { id: "c".into(), name: "bash".into(), args: serde_json::json!({}) },
                    Part::text("after"),
                ],
            ),
        })
        .unwrap();
        let (items, partial) = remote_items(&handle.observation.snapshot());
        assert!(partial.is_empty());
        assert!(
            matches!(&items[..], [SubItem::Text { text: a }, SubItem::Call { .. }, SubItem::Text { text: b }] if a == "before " && b == "after"),
            "{items:?}"
        );
    }

    #[test]
    fn gaps_truncation_and_orphan_result_are_visible() {
        let (handle, ends) = channels();
        ends.events.send(AgentEvent::SessionChanged { session_id: "remote".into(), title: String::new() }).unwrap();
        ends.events
            .send(AgentEvent::ToolDone { id: "lost".into(), output: ToolOutput::error("bad"), elapsed_ms: 0 })
            .unwrap();
        let mut snap = handle.observation.snapshot();
        snap.dropped = 3;
        snap.events[0].sequence = 5;
        snap.sequence = 6;
        let (items, _) = remote_items(&snap);
        let text = format!("{items:?}");
        assert!(text.contains("earlier events no longer kept"));
        assert!(text.contains("sequence gap"));
        assert!(text.contains("call not retained"));
        assert!(text.contains("bad"));
        snap.events[0].truncated = true;
        assert!(format!("{:?}", remote_items(&snap).0).contains("truncated"));
    }
}
