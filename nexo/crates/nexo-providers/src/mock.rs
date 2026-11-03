//! The `mock` protocol: scripted replies for tests and demos, no network.
//!
//! Without a script it echoes the last user text (`echo: …`); a user text
//! that starts with `tool:` makes it call that tool once
//! (`tool:read {"path":"a.rs"}`) and, on the next request, answer with the
//! first line of the result. With `extra.script` (a JSON file) it plays one
//! reply per request: a list of replies, each a list of items
//! `{"text": "…"}`, `{"reasoning": "…"}`, `{"tool": {"id", "name", "args"}}`,
//! `{"usage": {...}}`, `{"finish": "stop"|"length"|"tool_use"}`,
//! `{"error": "…"}`. `extra.delay_ms` spaces the words out, for a live feel.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use nexo_core::{
    ErrorKind, EventStream, FinishReason, ModelInfo, ModelRequest, Part, Protocol, ProviderConfig, ProviderError, Role,
    StreamEvent, Usage,
};
use serde_json::Value;

pub const ID: &str = "mock";

/// What the mock says to a request whose tool calls and results do not pair up.
pub const UNPAIRED: &str =
    "messages: each tool call needs its result in the next message, and each result its call in the one before";

#[derive(Debug, Default)]
pub struct Mock {
    /// Replies already played per script file.
    played: Mutex<HashMap<PathBuf, usize>>,
}

impl Mock {
    pub fn new() -> Mock {
        Mock::default()
    }

    /// The events of one reply, from the script or the echo rules.
    pub fn events(&self, cfg: &ProviderConfig, req: &ModelRequest) -> Vec<StreamEvent> {
        // a real provider refuses a call without its results, or results without their call:
        // so does the mock, or tests would pass with requests the network turns down
        if !nexo_core::message::is_well_formed(&req.messages) {
            return error(UNPAIRED.into());
        }
        if let Some(path) = cfg.extra.get("script").and_then(|v| v.as_str()) {
            let path = PathBuf::from(path);
            match std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| serde_json::from_str::<Vec<Vec<Value>>>(&t).map_err(|e| e.to_string()))
            {
                Ok(replies) => {
                    let mut played = self.played.lock().unwrap_or_else(|e| e.into_inner());
                    let n = played.entry(path).or_insert(0);
                    let reply = replies.get(*n).cloned().unwrap_or_default();
                    *n += 1;
                    return scripted(&reply, req);
                }
                Err(e) => return error(format!("mock script: {e}")),
            }
        }
        echo(req)
    }
}

fn scripted(items: &[Value], req: &ModelRequest) -> Vec<StreamEvent> {
    let mut out = vec![StreamEvent::Start { response_id: None, model: Some(req.model.clone()) }];
    let mut saw_tool = false;
    let mut finish = None;
    let mut usage_given = false;
    for it in items {
        if let Some(t) = it["text"].as_str() {
            out.extend(words(t).into_iter().map(StreamEvent::TextDelta));
        } else if let Some(t) = it["reasoning"].as_str() {
            out.extend(words(t).into_iter().map(StreamEvent::ReasoningDelta));
        } else if let Some(t) = it.get("tool") {
            saw_tool = true;
            let id = t["id"].as_str().map(String::from).unwrap_or_else(|| format!("mock_{}", out.len()));
            let name = t["name"].as_str().unwrap_or("").to_string();
            let args = t.get("args").cloned().unwrap_or(Value::Object(Default::default()));
            out.push(StreamEvent::ToolCallStart { id: id.clone(), name: name.clone() });
            out.push(StreamEvent::ToolCallDelta { id: id.clone(), partial_json: args.to_string() });
            out.push(StreamEvent::ToolCallEnd { id, name, args });
        } else if let Some(u) = it.get("usage") {
            usage_given = true;
            out.push(StreamEvent::Usage(serde_json::from_value(u.clone()).unwrap_or_default()));
        } else if let Some(f) = it["finish"].as_str() {
            finish = Some(match f {
                "length" => FinishReason::Length,
                "tool_use" => FinishReason::ToolUse,
                "content_filter" => FinishReason::ContentFilter,
                _ => FinishReason::Stop,
            });
        } else if let Some(e) = it["error"].as_str() {
            out.push(StreamEvent::Error(ProviderError::new(ErrorKind::Server, e)));
            out.push(StreamEvent::Done);
            return out;
        }
    }
    if !usage_given {
        out.push(StreamEvent::Usage(estimate(req, &out)));
    }
    out.push(StreamEvent::Finish(finish.unwrap_or(if saw_tool { FinishReason::ToolUse } else { FinishReason::Stop })));
    out.push(StreamEvent::Done);
    out
}

fn echo(req: &ModelRequest) -> Vec<StreamEvent> {
    let mut out = vec![StreamEvent::Start { response_id: None, model: Some(req.model.clone()) }];
    let last = req.messages.iter().rev().find(|m| m.role == Role::User);
    let mut saw_tool = false;
    match last {
        Some(m) if m.is_tool_results() => {
            let first = m
                .content
                .iter()
                .find_map(|p| match p {
                    Part::ToolResult { content, .. } => Some(nexo_core::message::result_text(content)),
                    _ => None,
                })
                .unwrap_or_default();
            let line = first.lines().next().unwrap_or("").to_string();
            out.extend(words(&format!("result: {line}")).into_iter().map(StreamEvent::TextDelta));
        }
        Some(m) => {
            let text = m.text();
            if let Some(rest) = text.strip_prefix("tool:") {
                let (name, args) = rest.trim().split_once(' ').unwrap_or((rest.trim(), "{}"));
                let args: Value = serde_json::from_str(args.trim()).unwrap_or(Value::Object(Default::default()));
                let id = format!("mock_{}", req.messages.len());
                saw_tool = true;
                out.push(StreamEvent::ToolCallStart { id: id.clone(), name: name.to_string() });
                out.push(StreamEvent::ToolCallDelta { id: id.clone(), partial_json: args.to_string() });
                out.push(StreamEvent::ToolCallEnd { id, name: name.to_string(), args });
            } else {
                out.extend(words(&format!("echo: {text}")).into_iter().map(StreamEvent::TextDelta));
            }
        }
        None => out.push(StreamEvent::TextDelta("echo: (nothing)".into())),
    }
    out.push(StreamEvent::Usage(estimate(req, &out)));
    out.push(StreamEvent::Finish(if saw_tool { FinishReason::ToolUse } else { FinishReason::Stop }));
    out.push(StreamEvent::Done);
    out
}

fn error(msg: String) -> Vec<StreamEvent> {
    vec![StreamEvent::Error(ProviderError::new(ErrorKind::BadRequest, msg)), StreamEvent::Done]
}

/// Words with their trailing space, so the stream looks typed.
fn words(t: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in t.chars() {
        cur.push(ch);
        if ch == ' ' || ch == '\n' {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn estimate(req: &ModelRequest, out: &[StreamEvent]) -> Usage {
    let input = nexo_core::message::estimate_tokens(&req.messages) + req.system.len() as u64 / 4;
    let output: u64 = out
        .iter()
        .map(|e| match e {
            StreamEvent::TextDelta(t) | StreamEvent::ReasoningDelta(t) => t.len() as u64 / 4 + 1,
            StreamEvent::ToolCallDelta { partial_json, .. } => partial_json.len() as u64 / 4 + 1,
            _ => 0,
        })
        .sum();
    Usage { input, output, ..Default::default() }
}

#[async_trait]
impl Protocol for Mock {
    fn id(&self) -> &'static str {
        ID
    }

    async fn stream(
        &self,
        cfg: &ProviderConfig,
        _key: Option<&str>,
        req: &ModelRequest,
    ) -> Result<EventStream, ProviderError> {
        let delay = cfg.extra.get("delay_ms").and_then(|v| v.as_integer()).unwrap_or(0).max(0) as u64;
        let events = self.events(cfg, req);
        let s = futures::stream::iter(events).then(move |ev| async move {
            if delay > 0 && matches!(ev, StreamEvent::TextDelta(_) | StreamEvent::ReasoningDelta(_)) {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            ev
        });
        Ok(Box::pin(s))
    }

    async fn list_models(&self, cfg: &ProviderConfig, _key: Option<&str>) -> Result<Vec<ModelInfo>, ProviderError> {
        let models = if cfg.models.is_empty() { vec!["mock".to_string()] } else { cfg.models.clone() };
        Ok(models.into_iter().map(|id| ModelInfo { id, tools: true, ..Default::default() }).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::{Collector, Message};

    fn cfg() -> ProviderConfig {
        ProviderConfig::new(ID, "", None)
    }

    #[tokio::test]
    async fn echoes_calls_tools_and_answers_with_the_result() {
        let m = Mock::new();
        let req = ModelRequest::new("mock", "", vec![Message::user("hello there")]);
        let mut c = Collector::new();
        let mut s = m.stream(&cfg(), None, &req).await.unwrap();
        while let Some(ev) = s.next().await {
            c.push(ev);
        }
        assert_eq!(c.message().text(), "echo: hello there");
        assert_eq!(c.finish_reason(), FinishReason::Stop);
        assert!(c.usage.input > 0);

        let req = ModelRequest::new("mock", "", vec![Message::user("tool:read {\"path\":\"a.rs\"}")]);
        let evs = m.events(&cfg(), &req);
        let calls: Vec<_> = evs.iter().filter(|e| matches!(e, StreamEvent::ToolCallEnd { .. })).collect();
        assert_eq!(calls.len(), 1);
        assert!(
            matches!(calls[0], StreamEvent::ToolCallEnd { name, args, .. } if name == "read" && args["path"] == "a.rs")
        );
        assert!(evs.contains(&StreamEvent::Finish(FinishReason::ToolUse)));

        let req = ModelRequest::new(
            "mock",
            "",
            vec![
                Message::user("x"),
                Message::new(Role::Assistant, vec![Part::tool_call("c", "read", serde_json::json!({}))]),
                Message::tool_results(vec![Part::tool_result("c", "line one\nline two", false)]),
            ],
        );
        let text: String = m
            .events(&cfg(), &req)
            .into_iter()
            .filter_map(|e| if let StreamEvent::TextDelta(t) = e { Some(t) } else { None })
            .collect();
        assert_eq!(text, "result: line one");
    }

    #[test]
    fn a_request_with_a_call_and_no_result_is_refused_as_the_network_would() {
        let call = Message::new(Role::Assistant, vec![Part::tool_call("t1", "bash", serde_json::json!({}))]);
        let req = ModelRequest::new("mock", "", vec![Message::user("go"), call, Message::user("next")]);
        let evs = Mock::new().events(&cfg(), &req);
        assert!(
            matches!(&evs[0], StreamEvent::Error(e) if e.kind == ErrorKind::BadRequest && e.message.contains("each tool call needs its result"))
        );
    }

    #[tokio::test]
    async fn plays_a_script_one_reply_per_request() {
        let dir = std::env::temp_dir().join(format!("nexo-mock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("s.json");
        std::fs::write(
            &script,
            r#"[[{"reasoning": "think"}, {"tool": {"id": "t1", "name": "bash", "args": {"command": "ls"}}}], [{"text": "all done"}, {"finish": "stop"}]]"#,
        )
        .unwrap();
        let mut cfg = cfg();
        cfg.extra.insert("script".into(), toml::Value::String(script.to_string_lossy().into_owned()));
        let m = Mock::new();
        let req = ModelRequest::new("mock", "", vec![Message::user("go")]);
        let first = m.events(&cfg, &req);
        assert!(first.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { name, .. } if name == "bash")));
        let second = m.events(&cfg, &req);
        let text: String =
            second.into_iter().filter_map(|e| if let StreamEvent::TextDelta(t) = e { Some(t) } else { None }).collect();
        assert_eq!(text, "all done");
        let third = m.events(&cfg, &req);
        assert!(
            third.iter().any(|e| matches!(e, StreamEvent::Finish(FinishReason::Stop))),
            "past the script: an empty reply"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
