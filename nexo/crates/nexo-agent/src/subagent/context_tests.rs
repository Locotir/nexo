//! A sub-agent sends what the session's context policy keeps of its own messages: under the model's window
//! however long it runs, everything while that fits and pays, and `recall` answered from its own history.

use super::*;
use std::sync::Mutex;

use nexo_core::config::ContextConfig;
use nexo_core::message::estimate_tokens;
use nexo_core::provider::EventStream;

/// Replies with a call of `read` (its `n` the request's place) until `calls` of them, then a call of `recall` when
/// asked to, then text; keeps every request.
struct Scripted {
    calls: usize,
    recall: Option<&'static str>,
    seen: Mutex<Vec<ModelRequest>>,
}

impl Scripted {
    fn new(calls: usize, recall: Option<&'static str>) -> Arc<Scripted> {
        Arc::new(Scripted { calls, recall, seen: Mutex::new(Vec::new()) })
    }
}

fn call(name: &str, args: Value) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolCallStart { id: "c".into(), name: name.into() },
        StreamEvent::ToolCallEnd { id: "c".into(), name: name.into(), args },
        StreamEvent::Finish(FinishReason::ToolUse),
        StreamEvent::Done,
    ]
}

#[async_trait]
impl ModelClient for Scripted {
    async fn stream(&self, _: &ProviderConfig, req: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        let n = {
            let mut seen = self.seen.lock().unwrap();
            seen.push(req.clone());
            seen.len() - 1
        };
        let events = if n < self.calls {
            call("read", json!({ "n": n }))
        } else if let (true, Some(m)) = (n == self.calls, self.recall) {
            call("recall", json!({ "message": m }))
        } else {
            vec![StreamEvent::TextDelta("done".into()), StreamEvent::Finish(FinishReason::Stop), StreamEvent::Done]
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// A read whose output is `words` distinct words naming the call.
struct Read(usize);

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str {
        "read"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "read".into(), description: "reads".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn summary(&self, _: &Value) -> String {
        "read".into()
    }
    async fn run(&self, args: Value, _: &ToolContext) -> ToolOutput {
        let n = args["n"].as_u64().unwrap_or(0);
        ToolOutput::ok((0..self.0).map(|w| format!("out{n}w{w}")).collect::<Vec<_>>().join(" "))
    }
}

const BRIEF_TASK: &str = "Find every caller of parse_header and say which of them ignore its error.";

/// Runs an `explore` sub-agent (17 requests at most) over `client` with the context `policy`, on a model
/// with a window of `window` tokens (0: none), and gives back its answer.
async fn run(client: Arc<Scripted>, policy: &str, window: u32, words: usize) -> ToolOutput {
    let mut cfg = ProviderConfig { default_model: Some("m".into()), ..Default::default() };
    if window > 0 {
        cfg.context_window.insert("m".into(), window);
    }
    let mut tools = ToolSet::new();
    tools.add(Arc::new(Read(words)));
    let sub = SubAgent::new(client, Target::new("p", cfg, "m"), &tools, Vec::new(), Default::default())
        .with_context(ContextConfig { policy: policy.into(), ..Default::default() });
    let dir = tempfile::tempdir().unwrap();
    sub.run(json!({ "prompt": BRIEF_TASK, "agent": "explore" }), &ToolContext::new(dir.path().into())).await
}

#[tokio::test]
async fn a_long_sub_agent_run_never_sends_more_than_the_window() {
    let window = 24_000u32;
    let client = Scripted::new(16, None);
    // 16 outputs of about 2500 tokens: 40 000 in all, far past the window
    let out = run(client.clone(), "curator", window, 1_000).await;
    assert!(!out.is_error, "{out:?}");
    let seen = client.seen.lock().unwrap();
    assert_eq!(seen.len(), 17, "every round ran");
    let room = u64::from(window - run::REPLY_TOKENS);
    let sent: Vec<u64> =
        seen.iter().map(|r| crate::turn::fixed_tokens(&r.system, &r.tools) + estimate_tokens(&r.messages)).collect();
    let plain = crate::turn::fixed_tokens(&seen[16].system, &seen[16].tools) + 16 * 2_500;
    assert!(plain > room, "the run must outgrow the window to test anything: {plain}");
    assert!(sent.iter().all(|&t| t <= room), "a request passed the window of {room}: {sent:?}");
    // the brief and the task stay whole in every request, cut or not
    for r in seen.iter() {
        assert!(r.messages[0].text().contains(BRIEF_TASK), "{:?}", r.messages[0]);
    }
}

#[tokio::test]
async fn a_short_sub_agent_run_sends_the_same_messages_as_plain() {
    let (cur, plain) = (Scripted::new(4, None), Scripted::new(4, None));
    run(cur.clone(), "curator", 128_000, 50).await;
    run(plain.clone(), "plain", 128_000, 50).await;
    let (cur, plain) = (cur.seen.lock().unwrap(), plain.seen.lock().unwrap());
    assert_eq!(cur.len(), plain.len());
    for (c, p) in cur.iter().zip(plain.iter()) {
        assert_eq!(serde_json::to_string(&c.messages).unwrap(), serde_json::to_string(&p.messages).unwrap());
        assert_eq!(c.system, p.system);
    }
    // the curator offers `recall`, plain does not
    assert!(cur[0].tools.iter().any(|t| t.name == "recall"));
    assert!(!plain[0].tools.iter().any(|t| t.name == "recall"));
}

#[tokio::test]
async fn recall_is_answered_from_the_sub_agents_own_messages() {
    // m00003 is the output of the first read
    let client = Scripted::new(2, Some("m00003"));
    run(client.clone(), "curator", 128_000, 30).await;
    let seen = client.seen.lock().unwrap();
    let last = seen.last().unwrap().messages.last().unwrap();
    let text = match last.content.as_slice() {
        [Part::ToolResult { content, is_error: false, .. }] => content
            .iter()
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>(),
        other => panic!("not one result: {other:?}"),
    };
    assert!(text.contains("out0w0") && text.contains("out0w29"), "{text}");
}
