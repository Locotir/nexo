//! A run past its wall-clock limit is interrupted, its commands killed and its record says `timeout`, with what it
//! had spent so far; the next run starts. The real agent loop, a scripted model, no network.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nexo_agent::ModelClient;
use nexo_bench::arm::Arm;
use nexo_bench::client::Stop;
use nexo_bench::run::{self, Bench, Record};
use nexo_core::{EventStream, FinishReason, ModelRequest, ProviderConfig, ProviderError, StreamEvent, Usage};
use serde_json::{json, Value};

/// One step of a script: a reply, or a reply that never ends, or a request that never answers.
enum Step {
    Reply(Vec<StreamEvent>),
    Hang(Vec<StreamEvent>),
    Stall,
}

struct Seq(Mutex<VecDeque<Step>>);

#[async_trait]
impl ModelClient for Seq {
    async fn stream(&self, _: &ProviderConfig, _: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        let step = self.0.lock().unwrap().pop_front().unwrap_or_else(|| Step::Reply(reply("All done here.", None)));
        match step {
            Step::Reply(evs) => Ok(Box::pin(futures::stream::iter(evs))),
            Step::Hang(evs) => {
                Ok(Box::pin(futures::StreamExt::chain(futures::stream::iter(evs), futures::stream::pending())))
            }
            Step::Stall => std::future::pending().await,
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
    evs.push(StreamEvent::Usage(Usage { input: 1_000_000, output: 5, ..Default::default() }));
    evs.push(StreamEvent::Finish(finish));
    evs.push(StreamEvent::Done);
    evs
}

fn bench(script: Vec<Step>, limit: Option<Duration>, out: &std::path::Path) -> Bench {
    static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
    HOME.get_or_init(|| {
        let d = test_tmpdir().unwrap();
        for var in ["NEXO_CONFIG_DIR", "NEXO_DATA_DIR", "NEXO_CACHE_DIR"] {
            std::env::set_var(var, d.path().join(var));
        }
        d
    });
    Bench {
        provider: ProviderConfig::new("anthropic", "http://127.0.0.1:9", None),
        model: "deepseek-chat".into(),
        price: nexo_bench::cost::DEEPSEEK,
        estimated: false,
        max_steps: 30,
        max_tokens: None,
        temperature: None,
        out: out.to_path_buf(),
        hide: vec![],
        client: Arc::new(Seq(Mutex::new(script.into()))),
        spent: Arc::default(),
        close_on_verified: false,
        run_timeout: limit,
    }
}

/// The run, and the money the bench counted for it, and how long it took.
async fn run_script(script: Vec<Step>, limit: Option<Duration>) -> (Record, f64, Duration) {
    let out = test_tmpdir().unwrap();
    let bench = bench(script, limit, out.path());
    let task = nexo_bench::tasks::all().into_iter().find(|t| t.id == "misleading-error").unwrap();
    let t = Instant::now();
    let r = run::run(&bench, &task, &Arm::parse("plain").unwrap(), 0, 5.0).await.unwrap();
    let spent = *bench.spent.lock().unwrap();
    (r, spent, t.elapsed())
}

#[cfg(unix)]
fn call(command: &str, background: bool) -> Vec<StreamEvent> {
    reply("Running it.", Some(("bash", json!({"command": command, "background": background}))))
}

/// Processes whose command line holds `marker`, once the killed ones are gone (given up to 5 s to be reaped).
#[cfg(unix)]
fn running(marker: &str) -> usize {
    let mut n = 0;
    for _ in 0..50 {
        let out = std::process::Command::new("pgrep").args(["-fc", marker]).output().unwrap();
        n = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0);
        if n == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    n
}

const LIMIT: Duration = Duration::from_secs(3);

/// The limit for a test in which a step has to finish before the limit: three times what a short whole run (a reply, a
/// tool call, a last reply) takes on this machine right now, plus 2 s, and never under `LIMIT`. A busy CI runner on
/// Windows needed more than 3 s for one reply, and the run was cut before the step the test counts.
async fn roomy_limit() -> Duration {
    let first = Step::Reply(reply("Looking.", Some(("ls", json!({"path": "."})))));
    let (r, _, took) = run_script(vec![first, Step::Reply(reply("The parser is fixed.", None))], None).await;
    assert_eq!(r.stop, Some(Stop::Done), "{r:?}");
    LIMIT.max(took * 3 + Duration::from_secs(2))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provider_that_stalls_forever_ends_as_timeout_with_the_partial_stats() {
    let first = Step::Reply(reply("Looking.", Some(("ls", json!({"path": "."})))));
    let limit = roomy_limit().await;
    let (r, spent, took) = run_script(vec![first, Step::Stall], Some(limit)).await;
    assert_eq!(r.stop, Some(Stop::Timeout), "{r:?}");
    assert!(!r.finished, "{r:?}");
    assert!(took >= limit && took < limit + Duration::from_secs(20), "{took:?} of {limit:?}");
    assert_eq!((r.steps, r.requests), (2, 2), "{r:?}");
    assert!(r.input >= 1_000_000 && r.cost > 0.0, "the reply before the stall is kept: {r:?}");
    assert_eq!(spent, r.cost, "what the run spent counts toward the bench's budget");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_that_never_ends_ends_as_timeout() {
    let (r, _, took) = run_script(vec![Step::Hang(reply("Thinking", None))], Some(LIMIT)).await;
    assert_eq!(r.stop, Some(Stop::Timeout), "{r:?}");
    assert!(took >= LIMIT && took < LIMIT + Duration::from_secs(20), "{took:?}");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_command_that_hangs_ends_as_timeout_and_leaves_no_process() {
    let marker = "sleep 4217";
    let hang = Step::Reply(call(&format!("{marker}; echo never"), false));
    let limit = roomy_limit().await;
    let (r, spent, took) = run_script(vec![hang], Some(limit)).await;
    assert_eq!(r.stop, Some(Stop::Timeout), "{r:?}");
    assert!(took >= limit && took < limit + Duration::from_secs(20), "{took:?} of {limit:?}");
    assert_eq!((r.steps, r.tool_calls), (1, 1), "{r:?}");
    assert!(r.cost > 0.0 && spent == r.cost, "{r:?}");
    assert_eq!(running(marker), 0, "the command outlived the run");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_background_job_does_not_outlive_a_run_that_timed_out() {
    let marker = "sleep 4218";
    let start = Step::Reply(call(marker, true));
    let (r, _, _) = run_script(vec![start, Step::Stall], Some(LIMIT)).await;
    assert_eq!(r.stop, Some(Stop::Timeout), "{r:?}");
    assert_eq!(running(marker), 0, "the background job outlived the run");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_inside_its_time_is_unaffected_and_zero_means_no_limit() {
    for limit in [Some(Duration::from_secs(60)), None] {
        let (r, _, took) = run_script(vec![Step::Reply(reply("The parser is fixed.", None))], limit).await;
        assert_eq!((r.stop.clone(), r.finished), (Some(Stop::Done), true), "{limit:?} {r:?}");
        assert!(took < Duration::from_secs(30), "{took:?}");
    }
    assert_eq!(nexo_bench::run_limit(0), None);
    assert_eq!(nexo_bench::run_limit(900), Some(Duration::from_secs(900)));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_run_after_a_timeout_starts_and_finishes() {
    let limit = roomy_limit().await;
    let out = test_tmpdir().unwrap();
    let bench = bench(vec![Step::Stall, Step::Reply(reply("The parser is fixed.", None))], Some(limit), out.path());
    let task = nexo_bench::tasks::all().into_iter().find(|t| t.id == "misleading-error").unwrap();
    let arm = Arm::parse("plain").unwrap();
    let a = run::run(&bench, &task, &arm, 0, 5.0).await.unwrap();
    let b = run::run(&bench, &task, &arm, 1, 5.0).await.unwrap();
    assert_eq!((a.stop, b.stop), (Some(Stop::Timeout), Some(Stop::Done)));
}

/// Scratch dir on disk under the Cargo target dir, never in the RAM-backed system temp dir.
fn test_tmpdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("nexo-test-")
        .tempdir_in(nexo_core::compat::scratch_base(std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))))
}
