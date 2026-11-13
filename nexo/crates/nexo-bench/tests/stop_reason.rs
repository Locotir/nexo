//! Every way a run can end leaves a named `stop` in its record: the real agent loop, a scripted model, no network.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use nexo_agent::ModelClient;
use nexo_bench::arm::Arm;
use nexo_bench::client::Stop;
use nexo_bench::run::{self, Bench, Record};
use nexo_bench::stats::LoopStats;
use nexo_core::agent::{channels, AgentCommand, AgentEvent};
use nexo_core::{
    ErrorKind, EventStream, FinishReason, ModelRequest, ProviderConfig, ProviderError, StreamEvent, Usage,
};
use serde_json::{json, Value};

/// One step of a script: a reply, an error, a panic, or a reply that never ends.
enum Step {
    Reply(Vec<StreamEvent>),
    Fail(ProviderError),
    Panic,
    Hang(Vec<StreamEvent>),
}

/// The steps in order; when the script runs out, a closing text.
struct Seq(Mutex<VecDeque<Step>>);

#[async_trait]
impl ModelClient for Seq {
    async fn stream(&self, _: &ProviderConfig, _: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        let step = self.0.lock().unwrap().pop_front().unwrap_or_else(|| Step::Reply(reply("All done here.", None)));
        match step {
            Step::Reply(evs) => Ok(Box::pin(futures::stream::iter(evs))),
            Step::Fail(e) => Err(e),
            Step::Panic => panic!("the scripted model broke"),
            Step::Hang(evs) => {
                Ok(Box::pin(futures::StreamExt::chain(futures::stream::iter(evs), futures::stream::pending())))
            }
        }
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

#[path = "../../nexo-agent/tests/scratch/mod.rs"]
mod scratch;

fn bench(script: Vec<Step>, max_steps: u32, on: bool, out: &std::path::Path) -> Bench {
    static HOME: OnceLock<std::path::PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let d = scratch::until_exit(test_tmpdir().unwrap());
        for var in ["NEXO_CONFIG_DIR", "NEXO_DATA_DIR", "NEXO_CACHE_DIR"] {
            std::env::set_var(var, d.join(var));
        }
        d
    });
    Bench {
        provider: ProviderConfig::new("anthropic", "http://127.0.0.1:9", None),
        model: "deepseek-chat".into(),
        price: nexo_bench::cost::DEEPSEEK,
        estimated: false,
        max_steps,
        max_tokens: None,
        temperature: None,
        out: out.to_path_buf(),
        hide: vec![],
        client: Arc::new(Seq(Mutex::new(script.into()))),
        spent: Arc::default(),
        close_on_verified: on,
        run_timeout: None,
    }
}

async fn run_script(script: Vec<Step>, max_steps: u32, on: bool) -> Record {
    let out = test_tmpdir().unwrap();
    let bench = bench(script, max_steps, on, out.path());
    let task = nexo_bench::tasks::all().into_iter().find(|t| t.id == "misleading-error").unwrap();
    run::run(&bench, &task, &Arm::parse("plain").unwrap(), 0, 5.0).await.unwrap()
}

fn written(r: &Record) -> Value {
    serde_json::to_value(r).unwrap()["stop"].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_ends_as_done() {
    let r = run_script(vec![Step::Reply(reply("The parser is fixed.", None))], 30, false).await;
    assert_eq!(r.stop, Some(Stop::Done), "{r:?}");
    assert_eq!(written(&r), json!("done"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_step_limit_ends_as_max_steps() {
    let call = || Step::Reply(reply("Looking.", Some(("ls", json!({"path": "."})))));
    let r = run_script(vec![call(), call(), call()], 2, false).await;
    assert_eq!((r.stop.clone(), r.finished), (Some(Stop::MaxSteps), false), "{r:?}");
    assert_eq!(written(&r), json!("max_steps"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provider_error_ends_as_error() {
    let e = ProviderError::new(ErrorKind::BadRequest, "no such model");
    let r = run_script(vec![Step::Fail(e)], 30, false).await;
    assert_eq!(r.stop, Some(Stop::Error), "{r:?}");
    assert_eq!(written(&r), json!("error"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provider_timeout_ends_as_timeout() {
    let e = ProviderError::new(ErrorKind::Other, "operation timed out");
    let r = run_script(vec![Step::Fail(e)], 30, false).await;
    assert_eq!(r.stop, Some(Stop::Timeout), "{r:?}");
    assert_eq!(written(&r), json!("timeout"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_check_after_a_change_ends_as_verified() {
    let write = reply(
        "Fixed the parser; tests pass.",
        Some(("write", json!({"path": "note.txt", "content": "x\n", "check": "true"}))),
    );
    let on = run_script(vec![Step::Reply(write.clone())], 30, true).await;
    assert_eq!((on.stop.clone(), on.steps), (Some(Stop::Verified), 1), "{on:?}");
    assert_eq!(written(&on), json!("verified"));
    // the same script with the option off goes on to a closing answer
    let off = run_script(vec![Step::Reply(write)], 30, false).await;
    assert_eq!((off.stop.clone(), off.steps), (Some(Stop::Done), 2), "{off:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_panic_in_the_turn_ends_as_other_panic() {
    let r = run_script(vec![Step::Panic], 30, false).await;
    assert_eq!(r.stop, Some(Stop::Other("panic".into())), "{r:?}");
    assert_eq!(written(&r), json!("other:panic"));
}

/// The agent driven by hand: nothing in a bench run interrupts, so the interrupt is sent here.
#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_turn_ends_as_cancelled() {
    let out = test_tmpdir().unwrap();
    let wd = test_tmpdir().unwrap();
    let b = bench(vec![Step::Hang(reply("Thinking", None))], 30, false, out.path());
    let setup = nexo_agent::AgentSetup {
        config: run::config(&b, &Arm::parse("plain").unwrap()),
        cwd: wd.path().to_path_buf(),
        provider: run::PROVIDER.into(),
        model: b.model.clone(),
        tools: nexo_bench::tools::toolset_with(wd.path(), None, None, false),
        client: b.client.clone(),
        resume: None,
        gate: Some(Arc::new(nexo_bench::tools::AllowAll)),
        mail: None,
        mcp: None,
        model_pinned: false,
    };
    let (mut handle, ends) = channels();
    let agent = tokio::spawn(nexo_agent::run(ends, setup));
    handle.commands.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] }).unwrap();
    let mut stats = LoopStats::default();
    let mut sent = false;
    let seen = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while let Some(ev) = handle.events.recv().await {
            stats.observe(&ev);
            if matches!(ev, AgentEvent::TextDelta { .. }) && !sent {
                sent = true;
                handle.commands.send(AgentCommand::Interrupt).unwrap();
            }
            if matches!(ev, AgentEvent::Idle { .. }) {
                break;
            }
        }
    })
    .await;
    let _ = handle.commands.send(AgentCommand::Shutdown);
    let _ = agent.await;
    assert!(seen.is_ok() && sent, "the turn never started or never went idle");
    assert_eq!(stats.stop(None, false), Stop::Cancelled);
}

#[test]
fn a_record_of_an_older_benchmark_still_reads_and_unknown_exits_are_named() {
    let old: Record = serde_json::from_str(r#"{"task":"t","stop":null}"#).unwrap();
    assert_eq!(old.stop, None);
    for (s, want) in [("budget", Stop::Budget), ("max_steps", Stop::MaxSteps), ("other:odd", Stop::Other("odd".into()))]
    {
        let r: Record = serde_json::from_str(&format!(r#"{{"task":"t","stop":"{s}"}}"#)).unwrap();
        assert_eq!(r.stop, Some(want));
    }
    let mut s = LoopStats::default();
    assert_eq!(s.stop(None, false), Stop::Other("no turn ended".into()));
    s.observe(&AgentEvent::Shutdown);
    assert_eq!(s.stop(None, false), Stop::Other("agent shut down".into()));
}

/// Scratch dir on disk under the Cargo target dir, never in the RAM-backed system temp dir.
fn test_tmpdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("nexo-test-")
        .tempdir_in(nexo_core::compat::scratch_base(std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))))
}
