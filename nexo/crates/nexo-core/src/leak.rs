//! Tool calls a model wrote as text.
//!
//! Some models, when the conversation looks unusual to them, write a call in their own markup as plain text
//! and end the reply there instead of making the call through the protocol: DeepSeek's
//! `<｜DSML｜invoke name="bash">` blocks, or the `<tool_call>{"name": ..}</tool_call>` lines of models trained
//! on that format. The reply is then not an answer, and taking it as one ends the turn with the markup as its
//! text. [`recover`] reads such a reply back into the calls it meant, so they run like any other. It only acts
//! when the reply has no real call, every written call names (or, by its arguments, can only be) one of the
//! session's tools, and every call has arguments.

use serde_json::{Map, Value};

use crate::message::{Message, Part};
use crate::provider::ToolSpec;

/// One call written as text: the tool, when the text names it, and its arguments.
#[derive(Debug, PartialEq)]
struct Written {
    name: Option<String>,
    args: Map<String, Value>,
}

/// The reply `msg` with the calls it wrote as text made real calls (ids `{id}_0`, `{id}_1`, ...), or None
/// when it holds no such calls or one of them cannot be read for certain. The text before the first call
/// stays; the markup goes.
pub fn recover(msg: &Message, specs: &[ToolSpec], id: &str) -> Option<Message> {
    if msg.has_tool_calls() {
        return None;
    }
    let (at, (before, written)) = msg.content.iter().enumerate().rev().find_map(|(i, p)| match p {
        Part::Text { text } => parse(text).map(|r| (i, r)),
        _ => None,
    })?;
    let mut calls = Vec::new();
    for (k, w) in written.into_iter().enumerate() {
        let name = match w.name {
            Some(n) => specs.iter().any(|s| s.name == n).then_some(n)?,
            None => only_tool_for(&w.args, specs)?,
        };
        calls.push(Part::tool_call(format!("{id}_{k}"), name, Value::Object(w.args)));
    }
    let mut out = msg.clone();
    let mut put = Vec::new();
    if !before.trim().is_empty() {
        put.push(Part::text(before.trim_end()));
    }
    put.extend(calls);
    out.content.splice(at..=at, put);
    Some(out)
}

/// The one tool whose parameters take every argument in `args`.
fn only_tool_for(args: &Map<String, Value>, specs: &[ToolSpec]) -> Option<String> {
    let mut fit = specs.iter().filter(|s| {
        let props = s.parameters.get("properties").and_then(Value::as_object);
        props.is_some_and(|p| args.keys().all(|k| p.contains_key(k)))
    });
    let one = fit.next()?;
    fit.next().is_none().then(|| one.name.clone())
}

/// The text before the first call and the calls written in `text`, or None when it holds no call.
fn parse(text: &str) -> Option<(String, Vec<Written>)> {
    let (at, calls) = tagged_json(text).or_else(|| dsml(text))?;
    (!calls.is_empty() && calls.iter().all(|c| !c.args.is_empty())).then(|| (text[..at].to_string(), calls))
}

/// `<tool_call>{"name": "bash", "arguments": {...}}</tool_call>`, once or more; the arguments may come as a
/// JSON string, and under `parameters`.
fn tagged_json(text: &str) -> Option<(usize, Vec<Written>)> {
    const OPEN: &str = "<tool_call>";
    const CLOSE: &str = "</tool_call>";
    let first = text.find(OPEN)?;
    let mut calls = Vec::new();
    let mut rest = &text[first..];
    while let Some(s) = rest.find(OPEN) {
        let body = &rest[s + OPEN.len()..];
        let (inner, next) = match body.find(CLOSE) {
            Some(e) => (&body[..e], &body[e + CLOSE.len()..]),
            None => (body, ""),
        };
        let v: Value = serde_json::from_str(inner.trim()).ok()?;
        let name = v.get("name").and_then(Value::as_str)?.to_string();
        let args = match v.get("arguments").or_else(|| v.get("parameters")) {
            Some(Value::Object(m)) => m.clone(),
            Some(Value::String(s)) => match serde_json::from_str(s).ok()? {
                Value::Object(m) => m,
                _ => return None,
            },
            _ => return None,
        };
        calls.push(Written { name: Some(name), args });
        rest = next;
    }
    Some((first, calls))
}

/// One `<｜DSML｜kind attr="v">` or `</｜DSML｜kind>` tag: where it starts and ends in the text, whether it
/// closes, its kind and its attributes.
struct Tag {
    start: usize,
    end: usize,
    close: bool,
    kind: String,
    attrs: Vec<(String, String)>,
}

impl Tag {
    fn attr(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }
}

/// DeepSeek's markup: `<｜DSML｜invoke name="bash">` holding `<｜DSML｜parameter name="command"
/// string="true">ls</｜DSML｜parameter>` lines (`string="false"` marks a JSON value), with one or two bars
/// on each side. A parameter outside any invoke opens a call with no name.
fn dsml(text: &str) -> Option<(usize, Vec<Written>)> {
    let tags = dsml_tags(text);
    let first = tags.first()?.start;
    let mut calls: Vec<Written> = Vec::new();
    let mut open = false;
    let mut i = 0;
    while i < tags.len() {
        let t = &tags[i];
        match (t.kind.as_str(), t.close) {
            ("invoke", false) => {
                calls.push(Written { name: t.attr("name").map(str::to_string), args: Map::new() });
                open = true;
            }
            ("invoke", true) => open = false,
            ("parameter", false) => {
                let key = t.attr("name")?.to_string();
                let end = tags[i + 1..].iter().position(|c| c.kind == "parameter" && c.close)? + i + 1;
                let raw = &text[t.end..tags[end].start];
                let value = match t.attr("string") {
                    Some("false") => serde_json::from_str(raw.trim()).unwrap_or_else(|_| Value::String(raw.into())),
                    _ => Value::String(raw.to_string()),
                };
                if !open {
                    calls.push(Written { name: None, args: Map::new() });
                    open = true;
                }
                calls.last_mut()?.args.insert(key, value);
                i = end;
            }
            _ => {}
        }
        i += 1;
    }
    Some((first, calls))
}

fn dsml_tags(text: &str) -> Vec<Tag> {
    let bars = |s: &str| s.len() - s.trim_start_matches(['｜', '|']).len();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(off) = text[from..].find('<') {
        let start = from + off;
        from = start + 1;
        let mut s = &text[start + 1..];
        let close = s.starts_with('/');
        if close {
            s = &s[1..];
        }
        let b = bars(s);
        if b == 0 || !s[b..].starts_with("DSML") {
            continue;
        }
        s = &s[b + 4..];
        let b = bars(s);
        if b == 0 {
            continue;
        }
        let Some(gt) = s.find('>') else { break };
        let head = s[b..gt].trim();
        let end = start + (text[start..].len() - s.len()) + gt + 1;
        let mut words = head.splitn(2, char::is_whitespace);
        let kind = words.next().unwrap_or("").to_string();
        let attrs = attrs(words.next().unwrap_or(""));
        out.push(Tag { start, end, close, kind, attrs });
        from = end;
    }
    out
}

/// `name="bash" string="true"` as pairs.
fn attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(eq) = rest.find("=\"") {
        let key = rest[..eq].trim().to_string();
        let after = &rest[eq + 2..];
        let Some(q) = after.find('"') else { break };
        out.push((key, after[..q].to_string()));
        rest = &after[q + 1..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;
    use serde_json::json;

    fn specs() -> Vec<ToolSpec> {
        let spec = |name: &str, props: Value| ToolSpec {
            name: name.into(),
            description: String::new(),
            parameters: json!({"type": "object", "properties": props}),
        };
        vec![
            spec("bash", json!({"command": {"type": "string"}, "timeout": {"type": "integer"}})),
            spec("read", json!({"path": {"type": "string"}, "offset": {"type": "integer"}})),
            spec("write", json!({"path": {"type": "string"}, "content": {"type": "string"}})),
        ]
    }

    fn reply(text: &str) -> Message {
        Message { role: Role::Assistant, content: vec![Part::text(text)] }
    }

    fn calls(m: &Message) -> Vec<(String, String, Value)> {
        m.tool_calls().map(|(i, n, a)| (i.to_string(), n.to_string(), a.clone())).collect()
    }

    #[test]
    fn reads_a_dsml_invoke_back_into_a_call() {
        let text = "\n\n<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ invoke name=\"bash\">\n<｜｜DSML｜｜ parameter name=\"command\" string=\"true\">cd material; sed -n 330,400p a.txt</｜｜DSML｜｜ parameter>\n<｜｜DSML｜｜ parameter name=\"timeout\" string=\"false\">30</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>";
        let m = recover(&reply(text), &specs(), "t").unwrap();
        assert_eq!(
            calls(&m),
            vec![(
                "t_0".into(),
                "bash".into(),
                json!({"command": "cd material; sed -n 330,400p a.txt", "timeout": 30})
            )]
        );
        assert_eq!(m.content.len(), 1, "the blank text before the markup goes");
    }

    #[test]
    fn a_dsml_call_without_its_invoke_line_goes_to_the_one_tool_that_takes_its_arguments() {
        let text = "<｜DSML｜calls>\n<｜DSML｜parameter name=\"command\" string=\"true\">ls</｜DSML｜parameter>\n</｜DSML｜invoke>\n</｜DSML｜calls>";
        let m = recover(&reply(text), &specs(), "t").unwrap();
        assert_eq!(calls(&m), vec![("t_0".into(), "bash".into(), json!({"command": "ls"}))]);
        // `path` alone fits two tools: no guess
        let text =
            "<｜DSML｜calls><｜DSML｜parameter name=\"path\" string=\"true\">a.rs</｜DSML｜parameter></｜DSML｜calls>";
        assert!(recover(&reply(text), &specs(), "t").is_none());
    }

    #[test]
    fn several_tool_call_lines_keep_the_text_before_them() {
        let text = "Reading both.\n<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"a.rs\"}}</tool_call>\n<tool_call>{\"name\": \"bash\", \"arguments\": \"{\\\"command\\\": \\\"ls\\\"}\"}</tool_call>";
        let m = recover(&reply(text), &specs(), "r").unwrap();
        assert_eq!(m.text(), "Reading both.");
        assert_eq!(
            calls(&m),
            vec![
                ("r_0".into(), "read".into(), json!({"path": "a.rs"})),
                ("r_1".into(), "bash".into(), json!({"command": "ls"})),
            ]
        );
    }

    #[test]
    fn answers_and_unknown_tools_stay_as_they_are() {
        // an answer, with stray closing tags after it (seen once): no call opened, nothing to run
        let text =
            "{\"return_value\": [\"a\"]}</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>";
        assert!(recover(&reply(text), &specs(), "t").is_none());
        assert!(recover(&reply("The answer is 4."), &specs(), "t").is_none());
        let unknown = "<tool_call>{\"name\": \"browse\", \"arguments\": {\"url\": \"x\"}}</tool_call>";
        assert!(recover(&reply(unknown), &specs(), "t").is_none());
        let broken = "<tool_call>{\"name\": \"bash\", \"argu</tool_call>";
        assert!(recover(&reply(broken), &specs(), "t").is_none());
        // a real call already there: the reply is left alone
        let mut m = reply("<tool_call>{\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}}</tool_call>");
        m.content.push(Part::tool_call("c1", "bash", json!({"command": "pwd"})));
        assert!(recover(&m, &specs(), "t").is_none());
    }
}
