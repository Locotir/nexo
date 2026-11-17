//! The calls of one round of a sub-agent: reads overlap as they do in the main turn, and a stop ends the round.

use super::*;
use nexo_core::message::Part;

/// A reader that takes a while and notes how many of its calls were in flight at the same time.
#[derive(Default)]
struct Nap {
    now: AtomicUsize,
    most: AtomicUsize,
}

#[async_trait]
impl Tool for Nap {
    fn name(&self) -> &str {
        "nap"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "nap".into(), description: "fake".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn summary(&self, _: &Value) -> String {
        "nap".into()
    }
    async fn run(&self, args: Value, _: &ToolContext) -> ToolOutput {
        let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(n, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.now.fetch_sub(1, Ordering::SeqCst);
        ToolOutput::ok(format!("napped {}", args["n"]))
    }
}

/// A reader that holds until it is stopped when asked to (`hold`), and is a call of another kind (it ends the
/// overlap, as a command would) when asked to be `serial`. It notes every call that ran in full.
struct Step {
    probe: Arc<Probe>,
    ran: AtomicUsize,
}

#[async_trait]
impl Tool for Step {
    fn name(&self) -> &str {
        "step"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "step".into(), description: "fake".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn call_kind(&self, args: &Value) -> ToolKind {
        if args["serial"].as_bool() == Some(true) {
            ToolKind::Exec
        } else {
            ToolKind::Read
        }
    }
    fn summary(&self, _: &Value) -> String {
        "step".into()
    }
    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        if args["hold"].as_bool() == Some(true) {
            self.probe.bump(&self.probe.tools);
            ctx.cancel.cancelled().await;
            return ToolOutput::error("cancelled");
        }
        self.ran.fetch_add(1, Ordering::SeqCst);
        ToolOutput::ok("stepped")
    }
}

/// One reply that asks for all of `calls` (id, tool, input) at once.
fn calls_of(calls: &[(&str, &str, Value)]) -> Vec<StreamEvent> {
    let mut evs = Vec::new();
    for (id, name, args) in calls {
        evs.push(StreamEvent::ToolCallStart { id: (*id).into(), name: (*name).into() });
        evs.push(StreamEvent::ToolCallEnd { id: (*id).into(), name: (*name).into(), args: args.clone() });
    }
    evs.push(StreamEvent::Finish(FinishReason::ToolUse));
    evs.push(StreamEvent::Done);
    evs
}

/// The `task` tool over a scripted model with these tools (the fixed set of the other tests is part of the golden
/// requests, so these tools are not added to it).
fn task_tool_with(client: &Arc<Scripted>, set: &ToolSet) -> SubAgent {
    let mut cfg = ProviderConfig::new("mock", "http://mock", None);
    cfg.models = vec!["m1".into()];
    let target = Target::new("mock", cfg, "m1");
    SubAgent::new(client.clone(), target, set, vec![], Arc::new(OnceLock::new()))
}

#[tokio::test]
async fn three_reads_asked_in_one_round_run_at_once_and_are_answered_in_the_order_asked() {
    let d = test_tmpdir().unwrap();
    let nap = Arc::new(Nap::default());
    let set = ToolSet::new().with(nap.clone());
    let client = Scripted::new(
        vec![],
        vec![
            Reply::Events(calls_of(&[
                ("c1", "nap", json!({"n": 1})),
                ("c2", "nap", json!({"n": 2})),
                ("c3", "nap", json!({"n": 3})),
            ])),
            Reply::Events(text("napped")),
        ],
    );
    let (agent, ctx) = (task_tool_with(&client, &set), call_ctx(d.path(), "n1"));
    let out = agent.run(task("explore", "nap three times"), &ctx).await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(nap.most.load(Ordering::SeqCst), 3, "the three calls did not overlap");
    // the lines of the run: every call is shown before the results of the run, which come in the order asked
    let subs = ctx.jobs.subs();
    let id = subs.list()[0].id.clone();
    let lines: Vec<String> = subs
        .transcript(&id)
        .unwrap()
        .items
        .iter()
        .filter_map(|i| match i {
            SubItem::Call { id, .. } => Some(format!("call {id}")),
            SubItem::Result { id, .. } => Some(format!("result {id}")),
            _ => None,
        })
        .collect();
    assert_eq!(lines, ["call c1", "call c2", "call c3", "result c1", "result c2", "result c3"]);
    // and the model reads them in that order too
    let last = client.sub_requests().into_iter().last().unwrap();
    let answered: Vec<String> = last
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .filter_map(|p| match p {
            Part::ToolResult { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(answered, ["c1", "c2", "c3"]);
}

#[tokio::test]
async fn a_stop_during_a_call_leaves_the_calls_after_it_and_the_next_round_unrun() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![],
        vec![
            Reply::Events(calls_of(&[("c1", "step", json!({"hold": true})), ("c2", "step", json!({"serial": true}))])),
            Reply::Events(text("never asked")),
        ],
    );
    let step = Arc::new(Step { probe: client.probe.clone(), ran: AtomicUsize::new(0) });
    let set = ToolSet::new().with(step.clone());
    let agent = Arc::new(task_tool_with(&client, &set));
    let ctx = call_ctx(d.path(), "k2");
    let cancel = ctx.cancel.clone();
    let run = {
        let (agent, ctx) = (agent.clone(), ctx.clone());
        tokio::spawn(async move { agent.run(task("explore", "hold then step"), &ctx).await })
    };
    until_tool(&client, 1).await;
    cancel.cancel();
    let out = run.await.unwrap();
    assert!(out.is_error && out.content.contains("interrupted"), "{}", out.content);
    assert_eq!(step.ran.load(Ordering::SeqCst), 0, "a call ran after the stop");
    assert_eq!(client.sub_count(), 1, "the next round was asked for after the stop");
}
