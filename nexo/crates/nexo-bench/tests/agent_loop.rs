//! A run through nexo's real agent loop with a scripted model: no network.

use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use nexo_agent::ModelClient;
use nexo_bench::arm::Arm;
use nexo_bench::run::{self, Bench};
use nexo_core::{
    EventStream, FinishReason, ModelRequest, Part, ProviderConfig, ProviderError, Role, StreamEvent, Usage,
};
use serde_json::json;

/// Lists the files when given a request, then says it is done; remembers how long each
/// conversation it was sent was.
#[derive(Default)]
struct Script {
    seen: Mutex<Vec<usize>>,
}

#[async_trait]
impl ModelClient for Script {
    async fn stream(&self, _: &ProviderConfig, req: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        self.seen.lock().unwrap().push(req.messages.len());
        let last = req.messages.last().expect("a message");
        let asked = last.role == Role::User && last.content.iter().all(|p| matches!(p, Part::Text { .. }));
        let usage = StreamEvent::Usage(Usage { input: 1000, output: 100, cache_read: 4000, ..Default::default() });
        let evs = if asked {
            vec![
                StreamEvent::ToolCallStart { id: "c1".into(), name: "ls".into() },
                StreamEvent::ToolCallEnd { id: "c1".into(), name: "ls".into(), args: json!({"path": "."}) },
                usage,
                StreamEvent::Finish(FinishReason::ToolUse),
                StreamEvent::Done,
            ]
        } else {
            vec![
                StreamEvent::TextDelta("done".into()),
                usage,
                StreamEvent::Finish(FinishReason::Stop),
                StreamEvent::Done,
            ]
        };
        Ok(Box::pin(futures::stream::iter(evs)))
    }
}

/// A bench whose agents keep their sessions in a folder of the test, never the user's own.
fn bench(client: Arc<dyn ModelClient>, max_steps: u32, out: &std::path::Path) -> Bench {
    static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
    let home = HOME.get_or_init(|| {
        let d = tempfile::tempdir().unwrap();
        for var in ["NEXO_CONFIG_DIR", "NEXO_DATA_DIR", "NEXO_CACHE_DIR"] {
            std::env::set_var(var, d.path().join(var));
        }
        d
    });
    assert!(std::env::var_os("NEXO_DATA_DIR").is_some_and(|d| std::path::Path::new(&d).starts_with(home.path())));
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
        client,
        spent: Arc::default(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_ends_in_an_error_still_counts_what_it_spent() {
    let out = tempfile::tempdir().unwrap();
    let bench = bench(Arc::new(Script::default()), 100, out.path());
    let mut task = nexo_bench::tasks::all().into_iter().find(|t| t.id == "misleading-error").unwrap();
    task.check = |_| panic!("a checker that breaks");
    assert!(run::run(&bench, &task, &Arm::parse("plain").unwrap(), 0, 5.0).await.is_err());
    let one = (1000.0 * 0.28 + 4000.0 * 0.028 + 100.0 * 0.42) / 1e6;
    assert!((*bench.spent.lock().unwrap() - 2.0 * one).abs() < 1e-12, "{}", bench.spent.lock().unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_two_session_task_runs_through_the_agent_and_resumes_its_history() {
    let out = tempfile::tempdir().unwrap();
    let script = Arc::new(Script::default());
    let bench = bench(script.clone(), 100, out.path());
    let task = nexo_bench::tasks::all().into_iter().find(|t| t.id == "cross-session").unwrap();
    let rec = run::run(&bench, &task, &Arm::parse("plain").unwrap(), 0, 5.0).await.unwrap();

    // each session: a request that lists the files, then one that ends the turn
    assert_eq!((rec.requests, rec.steps, rec.tool_calls, rec.tool_errors), (4, 4, 2, 0));
    assert!(rec.finished && rec.stop.is_none(), "{rec:?}");
    assert_eq!((rec.compactions, rec.rewrites), (0, 0), "plain never summarises or rewrites its history");
    assert_eq!((rec.input, rec.cache_read, rec.output), (4000, 16000, 400));
    assert!((rec.cost - 4.0 * (1000.0 * 0.28 + 4000.0 * 0.028 + 100.0 * 0.42) / 1e6).abs() < 1e-12);
    assert!(!rec.ok, "nothing was written, so the task is not done");
    let seen = script.seen.lock().unwrap().clone();
    assert_eq!(seen[0], 1);
    assert!(seen[2] > seen[0], "the second session starts from the first one's history: {seen:?}");

    let transcript = std::fs::read_to_string(out.path().join(&rec.transcript)).unwrap();
    assert_eq!(transcript.lines().count(), 4);
    run::append(out.path(), &rec).unwrap();
    let line = std::fs::read_to_string(out.path().join("results.jsonl")).unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(
        (v["task"].as_str(), v["arm"].as_str(), v["requests"].as_u64()),
        (Some("cross-session"), Some("plain"), Some(4))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_stops_at_its_step_limit() {
    let out = tempfile::tempdir().unwrap();
    let bench = bench(Arc::new(Script::default()), 1, out.path());
    let task = nexo_bench::tasks::all().into_iter().find(|t| t.id == "misleading-error").unwrap();
    let rec = run::run(&bench, &task, &Arm::parse("compact").unwrap(), 0, 5.0).await.unwrap();
    assert_eq!(rec.steps, 1);
    assert!(!rec.finished, "{rec:?}");
    assert_eq!(rec.stop, Some(nexo_bench::client::Stop::MaxSteps));
}
