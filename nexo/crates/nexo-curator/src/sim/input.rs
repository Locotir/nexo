//! Request loading: fixtures and `nexo bench` transcripts, as the replay reads them.

use serde_json::Value;

/// One request as the agent made it: the conversation so far and the system prompt.
pub struct Request {
    pub now: f64,
    pub system_tokens: u64,
    /// The system prompt's text, when the file holds it.
    pub system: String,
    pub messages: Vec<Value>,
    /// The tools the host offered (their names), when the file says.
    pub tools: Option<Vec<String>>,
    /// The tokens the model wrote at this step, when the file holds the usage (a transcript).
    pub out_tokens: Option<u64>,
}

/// The text of a message, flattened, for printing.
pub fn text_of(v: &Value) -> String {
    let s = match v.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => b["text"].as_str().unwrap_or("").to_string(),
                Some("tool_use") => {
                    let input = match &b["input"] {
                        Value::Object(m) => m
                            .values()
                            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
                            .collect::<Vec<_>>()
                            .join(", "),
                        other => other.to_string(),
                    };
                    format!("{}({input})", b["name"].as_str().unwrap_or("?"))
                }
                Some("tool_result") => match &b["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(inner) => {
                        inner.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n")
                    }
                    other => other.to_string(),
                },
                _ => b.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    s
}

/// The requests of a golden fixture: each line's `req`, a full view or a delta on the previous one.
pub fn fixture_requests(text: &str) -> Vec<Request> {
    let mut msgs: Vec<Value> = Vec::new();
    let mut system = String::new();
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let rec: Value = serde_json::from_str(line).expect("a JSON line");
        let req = &rec["req"];
        if req.get("op").and_then(Value::as_str) == Some("reset") {
            msgs.clear();
            continue;
        }
        if let Some(s) = req.get("system").and_then(Value::as_str) {
            system = s.to_string();
        }
        if let Some(new) = req.get("new") {
            let base = req["base"].as_u64().unwrap_or(0) as usize;
            msgs.truncate(base);
            msgs.extend(new.as_array().cloned().unwrap_or_default());
        } else if let Some(all) = req.get("messages").and_then(Value::as_array) {
            msgs = all.clone();
        } else {
            continue;
        }
        out.push(Request {
            now: rec["now"].as_f64().unwrap_or(0.0),
            system_tokens: req["system_tokens"].as_u64().unwrap_or(0),
            system: system.clone(),
            messages: msgs.clone(),
            tools: names(&req["tools"]),
            out_tokens: None,
        });
    }
    out
}

/// The names in a list of tools (names or `{"name": ...}`), if `v` is one.
pub fn names(v: &Value) -> Option<Vec<String>> {
    let list = v.as_array()?;
    Some(list.iter().filter_map(|t| t.as_str().or_else(|| t["name"].as_str())).map(str::to_string).collect())
}

/// The requests of a `nexo bench` transcript: the `messages` of each step, as the model was sent them.
pub fn transcript_requests(text: &str) -> Vec<Request> {
    let mut out = Vec::new();
    for (i, line) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let rec: Value = serde_json::from_str(line).expect("a JSON line");
        let Some(msgs) = rec["messages"].as_array() else { continue };
        let head = rec["system"].as_str().map_or(0, str::len) + rec["tools"].to_string().len();
        out.push(Request {
            now: 1_750_000_000.0 + 2.0 * i as f64,
            system_tokens: (head as u64).div_ceil(4),
            system: rec["system"].as_str().unwrap_or("").to_string(),
            messages: msgs.clone(),
            tools: names(&rec["tools"]),
            out_tokens: rec["usage"]["output"].as_u64(),
        });
    }
    out
}

/// The reply the agent gave to request `i`: the assistant message the next request adds after it, when
/// the next request continues the same conversation.
pub fn reply_to(requests: &[Request], i: usize) -> Option<&Value> {
    let (now, next) = (&requests[i].messages, &requests.get(i + 1)?.messages);
    let m = next.get(now.len())?;
    (next[..now.len()] == now[..] && m["role"] == "assistant").then_some(m)
}

/// The steps a run may take before it counts as failed, when the file does not say.
pub const MAX_STEPS: usize = 40;

/// A session to simulate: its requests and the step limit.
pub struct Session {
    pub path: String,
    pub requests: Vec<Request>,
    pub max_steps: usize,
}

/// Reads a golden fixture or a `nexo bench` transcript (told apart by the `req` key of the first line).
pub fn load(path: &str) -> Result<Session, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let first: Value =
        text.lines().find(|l| !l.trim().is_empty()).and_then(|l| serde_json::from_str(l).ok()).unwrap_or_default();
    let requests = if first.get("req").is_some() { fixture_requests(&text) } else { transcript_requests(&text) };
    if requests.is_empty() {
        return Err(format!("{path}: no requests found (expected a golden fixture or a nexo bench transcript)"));
    }
    Ok(Session { path: path.to_string(), requests, max_steps: MAX_STEPS })
}
