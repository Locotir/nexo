//! The model's own earlier replies reach each request of a long goal, through the curator: a model that greets at
//! every round does not do it for want of seeing that it already did.

use super::*;

struct Big;
#[async_trait]
impl Tool for Big {
    fn name(&self) -> &str {
        "read"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "read".into(), description: "fake".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn summary(&self, args: &Value) -> String {
        format!("read {}", args.get("path").and_then(Value::as_str).unwrap_or(""))
    }
    async fn run(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
        let name = args.get("path").and_then(Value::as_str).unwrap_or("");
        ToolOutput::ok((0..300).map(|k| format!("{name} line {k} holds its own value {}\n", k * 7)).collect::<String>())
    }
}

fn said_and_call(said: &str, id: &str, path: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::Start { response_id: None, model: None },
        StreamEvent::TextDelta(said.into()),
        StreamEvent::ToolCallStart { id: id.into(), name: "read".into() },
        StreamEvent::ToolCallEnd { id: id.into(), name: "read".into(), args: json!({"path": path}) },
        StreamEvent::Usage(Usage { input: 10, output: 5, ..Default::default() }),
        StreamEvent::Finish(FinishReason::ToolUse),
        StreamEvent::Done,
    ]
}

/// "hola" answered "Hola.", then a goal of six rounds; each step of a round says a short line with its call (as
/// "Hola. Sigo con …" did in the session this is about) and each round closes with a short reply. Gives the requests
/// the mock got.
async fn long_goal(spec: &str) -> Vec<ModelRequest> {
    let d = test_tmpdir().unwrap();
    let mut replies = vec![Reply::Events(text("Hola."))];
    let mut n = 0;
    for round in 0..6 {
        for k in 0..4 {
            n += 1;
            let said = format!("Hola. Sigo con r{round}s{k}");
            replies.push(Reply::Events(said_and_call(&said, &format!("c{n}"), &format!("f{n}"))));
        }
        replies.push(Reply::Events(text(&format!("Hola. Sigo con r{round}: listo"))));
    }
    let client = Scripted::new(replies);
    let mut cfg = config(PermissionMode::Auto);
    cfg.context.policy = "curator".into();
    cfg.context.curator_spec = spec.into();
    let mut r = start_with(client.clone(), cfg, None, d.path(), ToolSet::new().with(Arc::new(Big))).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "hola".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::SetGoal { text: Some("go through the files".into()) });
    for _ in 0..6 {
        r.turn().await;
    }
    r.stop().await;
    client.requests()
}

/// Every text the model said before request `k`, closing replies and lines said with a call alike, in the order
/// it said them.
fn said_before(reqs: &[ModelRequest], k: usize) -> Vec<String> {
    let mut all = vec!["Hola.".to_string()];
    for round in 0..6 {
        for s in 0..4 {
            all.push(format!("Hola. Sigo con r{round}s{s}"));
        }
        all.push(format!("Hola. Sigo con r{round}: listo"));
    }
    all.truncate(k.min(reqs.len()));
    all
}

/// Past the size where the curator starts to curate, the model's own earlier replies are all still in the request:
/// the closing replies in the conversation or in the list of requests that moved out, the lines said with a call on
/// the line that names the closed calls (`shut_lines`; before, request 12 lacked "Hola. Sigo con r1s0") or among the
/// facts kept of what moved out. Nothing tells the model that what came before the goal is answered.
#[tokio::test]
async fn a_long_goal_through_the_curator_keeps_the_models_own_replies_in_view() {
    let reqs = long_goal("eoq=0;floor=0.6;target=2500;cap=5000;hard=60000;still=0").await;
    assert_eq!(reqs.len(), 31);
    let last = reqs.last().unwrap();
    let sent = last.messages.iter().map(Message::text).collect::<Vec<_>>().join("\n");
    assert!(sent.contains("[curator] Messages 2 to "), "the curator curated: {sent}");
    for k in [12, 22, 30] {
        let sent = reqs[k].messages.iter().map(Message::text).collect::<Vec<_>>().join("\n");
        for said in said_before(&reqs, k) {
            assert!(sent.contains(&said), "request {k} lacks the model's own \"{said}\":\n{sent}");
        }
    }
    assert!(reqs.iter().all(|q| !q.system.contains("answered already")));
}

/// With the default curator the same: every earlier closing reply is in each later request.
#[tokio::test]
async fn a_long_goal_with_the_default_curator_keeps_every_closing_reply() {
    let reqs = long_goal("").await;
    assert_eq!(reqs.len(), 31);
    for (k, q) in reqs.iter().enumerate() {
        let sent = q.messages.iter().map(Message::text).collect::<Vec<_>>().join("\n");
        for said in said_before(&reqs, k).iter().filter(|s| s.as_str() == "Hola." || s.ends_with(": listo")) {
            assert!(sent.contains(said.as_str()), "request {k} lacks \"{said}\"");
        }
    }
}
