//! The conversation: messages made of typed parts.
//!
//! Serialised, a [`Message`] is exactly the messages-API JSON the curator
//! understands: `{"role": "user", "content": [{"type": "text", "text": ...},
//! {"type": "tool_use", "id": ..., "name": ..., "input": {...}},
//! {"type": "tool_result", "tool_use_id": ..., "content": ...}, ...]}`.
//! A plain string `content` is accepted on input and read as one text part.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// One block of a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
    },
    /// The model's reasoning. `text` is what can be shown; `signature` and
    /// `encrypted` are opaque data some protocols require back verbatim on
    /// the next request (the adapter that produced them knows what to do).
    #[serde(rename = "thinking")]
    Reasoning {
        #[serde(rename = "thinking")]
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted: Option<String>,
        /// The model that produced it; the opaque data is only valid for that model.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// Reasoning the provider withheld; only its opaque data is kept.
    #[serde(rename = "redacted_thinking")]
    RedactedReasoning {
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// A call the model asked for. `id` ties the result to it.
    #[serde(rename = "tool_use")]
    ToolCall {
        id: String,
        name: String,
        #[serde(rename = "input", default = "empty_object")]
        args: Value,
    },
    /// What a tool answered, sent back in the next user message.
    ToolResult {
        #[serde(rename = "tool_use_id")]
        call_id: String,
        /// The tool's name (some protocols want it with the result).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, with = "string_or_parts")]
        content: Vec<Part>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    Image {
        source: ImageSource,
    },
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

impl Part {
    pub fn text(s: impl Into<String>) -> Part {
        Part::Text { text: s.into() }
    }

    pub fn tool_call(id: impl Into<String>, name: impl Into<String>, args: Value) -> Part {
        Part::ToolCall { id: id.into(), name: name.into(), args }
    }

    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>, is_error: bool) -> Part {
        Part::ToolResult { call_id: call_id.into(), name: None, content: vec![Part::text(content)], is_error }
    }

    /// The readable text of this part (reasoning included; tool calls and results give nothing).
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Part::Text { text } | Part::Reasoning { text, .. } => Some(text),
            _ => None,
        }
    }

    /// True for signed or opaque reasoning that another model than `model` produced.
    /// Parts without a recorded model (older sessions) are never foreign.
    pub fn is_foreign_reasoning(&self, model: &str) -> bool {
        match self {
            Part::Reasoning { model: Some(m), .. } | Part::RedactedReasoning { model: Some(m), .. } => m != model,
            _ => false,
        }
    }

    pub fn is_tool_call(&self) -> bool {
        matches!(self, Part::ToolCall { .. })
    }

    pub fn is_tool_result(&self) -> bool {
        matches!(self, Part::ToolResult { .. })
    }
}

/// One message of the conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(with = "string_or_parts")]
    pub content: Vec<Part>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Message {
        Message { role: Role::User, content: vec![Part::text(text)] }
    }

    pub fn assistant(text: impl Into<String>) -> Message {
        Message { role: Role::Assistant, content: vec![Part::text(text)] }
    }

    pub fn new(role: Role, content: Vec<Part>) -> Message {
        Message { role, content }
    }

    /// The message that carries the results of the calls in `calls`, in their order.
    pub fn tool_results(results: Vec<Part>) -> Message {
        debug_assert!(results.iter().all(Part::is_tool_result));
        Message { role: Role::User, content: results }
    }

    /// The visible text (text parts only, joined by blank lines).
    pub fn text(&self) -> String {
        let mut out = String::new();
        for p in &self.content {
            if let Part::Text { text } = p {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(text);
            }
        }
        out
    }

    /// The reasoning text, if any.
    pub fn reasoning(&self) -> String {
        self.content
            .iter()
            .filter_map(|p| match p {
                Part::Reasoning { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = (&str, &str, &Value)> {
        self.content.iter().filter_map(|p| match p {
            Part::ToolCall { id, name, args } => Some((id.as_str(), name.as_str(), args)),
            _ => None,
        })
    }

    pub fn has_tool_calls(&self) -> bool {
        self.content.iter().any(Part::is_tool_call)
    }

    pub fn is_tool_results(&self) -> bool {
        !self.content.is_empty() && self.content.iter().all(Part::is_tool_result)
    }
}

/// The text of a tool result (its text parts joined).
pub fn result_text(content: &[Part]) -> String {
    content.iter().filter_map(Part::as_text).collect::<Vec<_>>().join("\n")
}

/// The text of a result for a call that has none recorded.
pub const NO_RESULT: &str = "The call did not run: no result was recorded.";

/// Whether every protocol takes `messages` as they are: the calls of each assistant message answered
/// in the user message right after it, once each, and no result without its call there.
pub fn is_well_formed(messages: &[Message]) -> bool {
    let mut open: Vec<&str> = Vec::new();
    for m in messages {
        match m.role {
            Role::Assistant => {
                if !open.is_empty() {
                    return false;
                }
                open = m.tool_calls().map(|(id, _, _)| id).collect();
            }
            Role::User => {
                let mut calls = std::mem::take(&mut open);
                for p in &m.content {
                    if let Part::ToolResult { call_id, .. } = p {
                        match calls.iter().position(|c| c == call_id) {
                            Some(i) => {
                                calls.swap_remove(i);
                            }
                            None => return false,
                        }
                    }
                }
                if !calls.is_empty() {
                    return false;
                }
            }
        }
    }
    open.is_empty()
}

/// `messages` as every protocol takes them, and one line per mend. A history cut short (a session that
/// stopped between a call and its result, a message lost or left out) is mended without losing what it
/// holds: a call with no result gets an error result saying so, a result found in a later user message
/// moves to the one right after its call, and a result whose call is not right before becomes text with
/// the tool's name. A well formed history comes back as it is, with no lines.
pub fn well_formed(messages: Vec<Message>) -> (Vec<Message>, Vec<String>) {
    if is_well_formed(&messages) {
        return (messages, Vec::new());
    }
    let mut fixes = Vec::new();
    let mut src: std::collections::VecDeque<Message> = messages.into();
    let mut out: Vec<Message> = Vec::with_capacity(src.len() + 1);
    while let Some(m) = src.pop_front() {
        if m.role == Role::User {
            out.push(orphans_to_text(m, &mut fixes));
            continue;
        }
        let calls: Vec<(String, String)> = m.tool_calls().map(|(i, n, _)| (i.to_string(), n.to_string())).collect();
        out.push(m);
        if calls.is_empty() {
            continue;
        }
        // the results of these calls, wherever they are in the user messages that follow
        let run = src.iter().take_while(|m| m.role == Role::User).count();
        let mut found = vec![false; calls.len()];
        let mut taken = Vec::new();
        for (j, um) in src.iter_mut().take(run).enumerate() {
            let mut keep = Vec::with_capacity(um.content.len());
            for p in std::mem::take(&mut um.content) {
                let at = match &p {
                    Part::ToolResult { call_id, .. } => calls.iter().position(|(id, _)| id == call_id),
                    _ => None,
                };
                match at {
                    Some(c) if !found[c] => {
                        found[c] = true;
                        if j > 0 {
                            fixes.push(format!("the result of {} was moved next to its call", calls[c].1));
                        }
                        taken.push(p);
                    }
                    _ => keep.push(p),
                }
            }
            um.content = keep;
        }
        for (id, name) in calls.iter().zip(&found).filter(|(_, f)| !**f).map(|(c, _)| c) {
            fixes.push(format!("the call {id} ({name}) had no result; it is answered as not run"));
            taken.push(Part::ToolResult {
                call_id: id.clone(),
                name: Some(name.clone()),
                content: vec![Part::text(NO_RESULT)],
                is_error: true,
            });
        }
        let first = if run > 0 { src.pop_front().map(|m| orphans_to_text(m, &mut fixes)) } else { None };
        let mut content = taken;
        content.extend(first.map(|m| m.content).unwrap_or_default());
        out.push(Message { role: Role::User, content });
    }
    // a user message left empty by a move says nothing: it goes
    out.retain(|m| m.role == Role::Assistant || !m.content.is_empty());
    (out, fixes)
}

/// `m` with each tool result in it turned into text (its call is not right before it).
fn orphans_to_text(mut m: Message, fixes: &mut Vec<String>) -> Message {
    if !m.content.iter().any(Part::is_tool_result) {
        return m;
    }
    let mut parts = Vec::with_capacity(m.content.len());
    for p in std::mem::take(&mut m.content) {
        match p {
            Part::ToolResult { name, content, is_error, .. } => {
                let name = name.unwrap_or_else(|| "a call".into());
                fixes.push(format!("a result of {name} had no call before it; it is kept as text"));
                let what = if is_error { "error" } else { "output" };
                parts.push(Part::text(format!("[{what} of {name}]\n{}", result_text(&content))));
                parts.extend(content.into_iter().filter(|p| matches!(p, Part::Image { .. })));
            }
            other => parts.push(other),
        }
    }
    m.content = parts;
    m
}

/// `content` as a list of parts, or as a plain string meaning one text part.
mod string_or_parts {
    use super::*;

    pub fn serialize<S: Serializer>(parts: &[Part], s: S) -> Result<S::Ok, S::Error> {
        parts.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Part>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either {
            Text(String),
            Parts(Vec<Part>),
        }
        Ok(match Either::deserialize(d)? {
            Either::Text(t) => vec![Part::text(t)],
            Either::Parts(p) => p,
        })
    }
}

/// The tokens an image counts for in [`estimate_tokens`], whatever its bytes (the curator's figure).
pub const MEDIA_TOKENS: u64 = 1500;

/// A cheap token estimate (about four characters per token), the curator's rule with one
/// difference: the visible text of reasoning counts here, while the curator counts a reasoning
/// block as its type alone. Sizes agree for a history without reasoning text.
pub fn estimate_tokens(messages: &[Message]) -> u64 {
    messages.iter().map(message_tokens).sum()
}

/// [`estimate_tokens`] of one message: the characters of its JSON over 4, with each image at
/// [`MEDIA_TOKENS`] and the opaque data of reasoning (signatures, encrypted or withheld blocks) left out.
pub fn message_tokens(m: &Message) -> u64 {
    let mut media = 0;
    let light;
    let m = if m.content.iter().any(heavy) {
        light = Message { role: m.role, content: m.content.iter().map(|p| lighten(p, &mut media)).collect() };
        &light
    } else {
        m
    };
    serde_json::to_string(m).map(|s| s.chars().count() as u64 / 4).unwrap_or(0) + MEDIA_TOKENS * media
}

fn heavy(p: &Part) -> bool {
    match p {
        Part::Image { .. } | Part::RedactedReasoning { .. } => true,
        Part::Reasoning { signature, encrypted, .. } => signature.is_some() || encrypted.is_some(),
        Part::ToolResult { content, .. } => content.iter().any(heavy),
        _ => false,
    }
}

/// `p` without its opaque data; images are counted in `media` and left out.
fn lighten(p: &Part, media: &mut u64) -> Part {
    match p {
        Part::Image { .. } => {
            *media += 1;
            Part::text("")
        }
        Part::RedactedReasoning { model, .. } => Part::RedactedReasoning { data: String::new(), model: model.clone() },
        Part::Reasoning { text, model, .. } => {
            Part::Reasoning { text: text.clone(), signature: None, encrypted: None, model: model.clone() }
        }
        Part::ToolResult { call_id, name, content, is_error } => Part::ToolResult {
            call_id: call_id.clone(),
            name: name.clone(),
            content: content.iter().map(|c| lighten(c, media)).collect(),
            is_error: *is_error,
        },
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_estimate_counts_characters_and_gives_media_a_nominal_size() {
        let text = |s: &str| estimate_tokens(&[Message::user(s.to_string())]);
        assert_eq!(text(&"é".repeat(400)), text(&"e".repeat(400)), "characters, not bytes");
        let image = |n: usize| {
            let source = ImageSource::Base64 { media_type: "image/png".into(), data: "A".repeat(n) };
            estimate_tokens(&[Message::new(Role::User, vec![Part::Image { source }])])
        };
        assert_eq!(image(10), image(4_000_000), "the picture's bytes are not counted");
        assert!(image(10) >= MEDIA_TOKENS);
        let reasoning = |sig: usize| {
            let p =
                Part::Reasoning { text: "hm".into(), signature: Some("s".repeat(sig)), encrypted: None, model: None };
            estimate_tokens(&[Message::new(Role::Assistant, vec![p])])
        };
        assert_eq!(reasoning(1), reasoning(100_000), "opaque signatures are not counted");
    }

    #[test]
    fn round_trips_the_messages_api_shape() {
        let m = Message::new(
            Role::Assistant,
            vec![
                Part::Reasoning { text: "hm".into(), signature: Some("sig".into()), encrypted: None, model: None },
                Part::text("I will read it"),
                Part::tool_call("c1", "read", serde_json::json!({"path": "a.rs"})),
            ],
        );
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["role"], "assistant");
        assert_eq!(j["content"][0]["type"], "thinking");
        assert_eq!(j["content"][0]["thinking"], "hm");
        assert_eq!(j["content"][2]["type"], "tool_use");
        assert_eq!(j["content"][2]["input"]["path"], "a.rs");
        let back: Message = serde_json::from_value(j).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn reads_string_content_and_string_tool_results() {
        let m: Message = serde_json::from_str(r#"{"role":"user","content":"hi"}"#).unwrap();
        assert_eq!(m.text(), "hi");
        let m: Message = serde_json::from_str(
            r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"ok"}]}"#,
        )
        .unwrap();
        assert!(m.is_tool_results());
        match &m.content[0] {
            Part::ToolResult { content, is_error, .. } => {
                assert_eq!(result_text(content), "ok");
                assert!(!is_error);
            }
            _ => panic!(),
        }
        let j = serde_json::to_value(&m).unwrap();
        assert!(j["content"][0].get("is_error").is_none(), "false is left out");
    }

    fn calls(ids: &[&str]) -> Message {
        Message::new(Role::Assistant, ids.iter().map(|i| Part::tool_call(*i, "read", serde_json::json!({}))).collect())
    }

    fn results(ids: &[&str]) -> Message {
        Message::tool_results(ids.iter().map(|i| Part::tool_result(*i, format!("out {i}"), false)).collect())
    }

    #[test]
    fn a_well_formed_history_comes_back_as_it_is() {
        let ms = vec![
            Message::user("go"),
            calls(&["a", "b"]),
            // results in another order than the calls, and the person's words after them
            Message::new(Role::User, {
                let mut c = results(&["b", "a"]).content;
                c.push(Part::text("and then?"));
                c
            }),
            Message::assistant("done"),
        ];
        assert!(is_well_formed(&ms));
        let (out, fixes) = well_formed(ms.clone());
        assert_eq!(out, ms);
        assert!(fixes.is_empty());
        assert!(is_well_formed(&[]) && is_well_formed(&[Message::user("hi")]));
    }

    #[test]
    fn a_call_with_no_result_is_answered_as_not_run() {
        // the session stopped between the calls and their results
        let ms = vec![Message::user("go"), calls(&["a", "b"])];
        let (out, fixes) = well_formed(ms);
        assert!(is_well_formed(&out));
        assert_eq!(out.len(), 3);
        assert!(out[2].is_tool_results());
        assert!(out[2].content.iter().all(|p| matches!(p, Part::ToolResult { is_error: true, content, .. }
            if result_text(content) == NO_RESULT)));
        assert_eq!(fixes.len(), 2, "{fixes:?}");
        // one of two answered, and the next turn already begun
        let ms = vec![calls(&["a", "b"]), results(&["a"]), Message::assistant("so"), Message::user("more")];
        let (out, fixes) = well_formed(ms);
        assert!(is_well_formed(&out));
        assert_eq!(out[1].content.len(), 2);
        assert!(fixes[0].contains("the call b (read) had no result"), "{fixes:?}");
        // a call followed by another assistant message
        let (out, _) = well_formed(vec![calls(&["a"]), Message::assistant("hm")]);
        assert!(is_well_formed(&out) && out.len() == 3 && out[1].is_tool_results());
    }

    #[test]
    fn a_result_away_from_its_call_moves_or_becomes_text() {
        // the person's words came between the call and its result
        let ms = vec![calls(&["a"]), Message::user("wait"), results(&["a"])];
        let (out, fixes) = well_formed(ms);
        assert!(is_well_formed(&out), "{out:?}");
        assert_eq!(out.len(), 2, "the emptied message goes");
        assert!(matches!(&out[1].content[0], Part::ToolResult { call_id, .. } if call_id == "a"));
        assert_eq!(out[1].text(), "wait", "nothing is lost");
        assert!(fixes[0].contains("moved next to its call"), "{fixes:?}");
        // a result with no call before it (its call was left out): text, with the tool's name
        let orphan = Part::ToolResult {
            call_id: "x".into(),
            name: Some("grep".into()),
            content: vec![Part::text("3 matches")],
            is_error: false,
        };
        let ms = vec![Message::user("go"), Message::new(Role::User, vec![orphan]), Message::assistant("ok")];
        let (out, fixes) = well_formed(ms);
        assert!(is_well_formed(&out));
        assert_eq!(out[1].text(), "[output of grep]\n3 matches");
        assert!(fixes[0].contains("a result of grep had no call"), "{fixes:?}");
        // the same call answered twice: the second answer is text
        let ms = vec![calls(&["a"]), results(&["a", "a"])];
        let (out, fixes) = well_formed(ms);
        assert!(is_well_formed(&out) && out[1].content.len() == 2 && fixes.len() == 1);
    }

    #[test]
    fn reasoning_model_is_optional_and_not_written_when_absent() {
        let old = r#"{"type":"thinking","thinking":"hm","signature":"s"}"#;
        let p: Part = serde_json::from_str(old).unwrap();
        assert_eq!(p, Part::Reasoning { text: "hm".into(), signature: Some("s".into()), encrypted: None, model: None });
        assert_eq!(serde_json::to_string(&p).unwrap(), old);
        let r: Part = serde_json::from_str(r#"{"type":"redacted_thinking","data":"d"}"#).unwrap();
        assert_eq!(serde_json::to_string(&r).unwrap(), r#"{"type":"redacted_thinking","data":"d"}"#);

        let tagged = Part::Reasoning { text: "hm".into(), signature: None, encrypted: None, model: Some("m1".into()) };
        let back: Part = serde_json::from_str(&serde_json::to_string(&tagged).unwrap()).unwrap();
        assert_eq!(back, tagged);
        let tagged = Part::RedactedReasoning { data: "d".into(), model: Some("m1".into()) };
        let back: Part = serde_json::from_str(&serde_json::to_string(&tagged).unwrap()).unwrap();
        assert_eq!(back, tagged);
        assert!(tagged.is_foreign_reasoning("m2") && !tagged.is_foreign_reasoning("m1"));
        assert!(!r.is_foreign_reasoning("m2"));
    }
}
