//! A reply with its answer and a successful check after a change ends the turn (`agent.close_on_verified`): the real loop,
//! a scripted model, no network.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use nexo_agent::ModelClient;
use nexo_bench::arm::Arm;
use nexo_bench::run::{self, Bench};
use nexo_core::{EventStream, FinishReason, ModelRequest, ProviderConfig, ProviderError, StreamEvent, Usage};
use serde_json::{json, Value};

/// Replies in order; when the script runs out, a closing text.
struct Seq(Mutex<VecDeque<Vec<StreamEvent>>>);

#[async_trait]
impl ModelClient for Seq {
    async fn stream(&self, _: &ProviderConfig, _: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        let evs =
            self.0.lock().unwrap().pop_front().unwrap_or_else(|| reply("All done, nothing left to do here.", None));
        Ok(Box::pin(futures::stream::iter(evs)))
    }
}

fn reply(text: &str, call: Option<(&str, Value)>) -> Vec<StreamEvent> {
    let mut evs = vec![StreamEvent::TextDelta(text.into())];
    let finish = if let Some((name, args)) = call {
        evs.push(StreamEvent::ToolCallStart { id: "c1".into(), name: name.into() });
        evs.push(StreamEvent::ToolCallEnd { id: "c1".into(), name: name.into(), args });
        FinishReason::ToolUse
    } else {
        FinishReason::Stop
    };
    evs.push(StreamEvent::Usage(Usage { input: 10, output: 5, ..Default::default() }));
    evs.push(StreamEvent::Finish(finish));
    evs.push(StreamEvent::Done);
    evs
}

async fn run_script(script: Vec<Vec<StreamEvent>>, on: bool) -> nexo_bench::run::Record {
    static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
    HOME.get_or_init(|| {
        let d = tempfile::tempdir().unwrap();
        for var in ["NEXO_CONFIG_DIR", "NEXO_DATA_DIR", "NEXO_CACHE_DIR"] {
            std::env::set_var(var, d.path().join(var));
        }
        d
    });
    let out = tempfile::tempdir().unwrap();
    let bench = Bench {
        provider: ProviderConfig::new("anthropic", "http://127.0.0.1:9", None),
        model: "deepseek-chat".into(),
        price: nexo_bench::cost::DEEPSEEK,
        estimated: false,
        max_steps: 30,
        max_tokens: None,
        temperature: None,
        out: out.path().to_path_buf(),
        hide: vec![],
        client: Arc::new(Seq(Mutex::new(script.into()))),
        spent: Arc::default(),
        close_on_verified: on,
        run_timeout: None,
    };
    let task = nexo_bench::tasks::all().into_iter().find(|t| t.id == "misleading-error").unwrap();
    run::run(&bench, &task, &Arm::parse("plain").unwrap(), 0, 5.0).await.unwrap()
}

const DONE: &str = "Fixed the parser; tests pass.";

fn bash(cmd: &str) -> Vec<StreamEvent> {
    reply(DONE, Some(("bash", json!({"command": cmd}))))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_check_with_nothing_changed_goes_on() {
    for cmd in ["true", "cargo --version"] {
        let r = run_script(vec![bash(cmd)], true).await;
        assert_eq!((r.steps, r.finished), (2, true), "{cmd}: {r:?}");
    }
}

/// The vuln-hunt shape: a short answer-like text with reads and listings, nothing changed this turn.
#[tokio::test(flavor = "multi_thread")]
async fn short_text_with_reads_goes_on() {
    let read = reply("Nothing found in the parser so far.", Some(("read", json!({"path": "Cargo.toml"}))));
    let r = run_script(vec![read, bash("ls")], true).await;
    assert_eq!((r.steps, r.finished), (3, true), "{r:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_and_then_reads_only_goes_on() {
    let edit = reply(DONE, Some(("write", json!({"path": "note.txt", "content": "x\n"}))));
    let r = run_script(vec![edit, bash("ls")], true).await;
    assert_eq!((r.steps, r.finished), (3, true), "{r:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_check_goes_on() {
    let r = run_script(vec![bash("false")], true).await;
    assert_eq!((r.steps, r.finished), (2, true), "{r:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_without_check_goes_on_and_with_a_passing_check_ends() {
    let w = |extra: Value| {
        let mut a = json!({"path": "note.txt", "content": "x\n"});
        a.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        reply(DONE, Some(("write", a)))
    };
    let plain = run_script(vec![w(json!({}))], true).await;
    assert_eq!((plain.steps, plain.finished), (2, true), "{plain:?}");
    let off = run_script(vec![w(json!({"check": "true"}))], false).await;
    assert_eq!((off.steps, off.finished), (2, true), "{off:?}");
    let checked = run_script(vec![w(json!({"check": "true"}))], true).await;
    assert_eq!((checked.steps, checked.finished), (1, true), "{checked:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lead_in_goes_on() {
    let r = run_script(vec![reply("Let me check:", Some(("bash", json!({"command": "true"}))))], true).await;
    assert_eq!((r.steps, r.finished), (2, true), "{r:?}");
}
