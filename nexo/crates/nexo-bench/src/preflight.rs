//! The preflight: one tiny request, before any money goes to the tasks, that checks the key, the
//! model and the address work and that the model answers with a structured tool call (a model that
//! writes its calls as text, or a server that drops the tools, ends every task after one step).

use futures::StreamExt;
use nexo_agent::ModelClient;
use nexo_core::{Collector, Message, ModelRequest, Part, ProviderConfig};

const PROMPT: &str = "List the files in the working directory. You must use the ls tool to do it; do not answer \
                      from memory.";

/// Ok with the call the model made, or why the benchmark should not start.
pub async fn check(
    client: &dyn ModelClient,
    cfg: &ProviderConfig,
    model: &str,
    max_tokens: u32,
) -> Result<String, String> {
    let ls = nexo_tools::local().get("ls").map(|t| t.spec()).ok_or("nexo has no ls tool")?;
    let mut req = ModelRequest::new(model, "You are a coding agent. Use the tools.", vec![Message::user(PROMPT)]);
    req.tools = vec![ls];
    req.max_tokens = max_tokens;
    let mut stream = client.stream(cfg, &req, 2).await.map_err(|e| format!("the request failed: {e}"))?;
    let mut col = Collector::new();
    while let Some(ev) = stream.next().await {
        col.push(ev);
    }
    if let Some(e) = &col.error {
        return Err(format!("the reply failed: {e}"));
    }
    verdict(&col.message(), col.finish.map(|f| format!("{f:?}")))
}

/// What the reply to the preflight says about the model.
pub fn verdict(reply: &Message, finish: Option<String>) -> Result<String, String> {
    let mut text = String::new();
    for p in &reply.content {
        match p {
            Part::ToolCall { name, args, .. } => return Ok(format!("structured tool call: {name}({args})")),
            Part::Text { text: t } => text.push_str(t),
            _ => {}
        }
    }
    let shown: String = text.chars().take(800).collect();
    if looks_like_a_call(&text) {
        return Err(format!(
            "the model wrote the tool call as text instead of a structured call, so no agent can run it; the \
             server's tool-call handling does not suit this model. It wrote:\n{shown}"
        ));
    }
    Err(format!(
        "the model answered without a tool call although it was told to use one: the server dropped the tool \
         definitions or the model does not call tools (finish {}). It wrote:\n{shown}",
        finish.unwrap_or_else(|| "unknown".into())
    ))
}

/// The usual shapes of a tool call written out as text.
fn looks_like_a_call(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    ["<tool_call>", "<function=", "\"name\": \"ls\"", "\"name\":\"ls\"", "ls("].iter().any(|m| t.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_structured_call_passes_and_a_written_one_does_not() {
        let call = Message {
            content: vec![Part::ToolCall { id: "1".into(), name: "ls".into(), args: json!({"path": "."}) }],
            ..Message::assistant("")
        };
        assert!(verdict(&call, None).unwrap().contains("ls"));
        let written = Message::assistant("<tool_call>{\"name\": \"ls\"}</tool_call>");
        assert!(verdict(&written, None).unwrap_err().contains("as text"));
        let plain = Message::assistant("There are three files.");
        assert!(verdict(&plain, Some("Stop".into())).unwrap_err().contains("without a tool call"));
    }
}
