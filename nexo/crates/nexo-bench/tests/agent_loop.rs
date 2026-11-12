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
        let d = test_tmpdir().unwrap();
        for var in ["NEXO_CONFIG_DIR", "NEXO_DATA_DIR", "NEXO_CACHE_DIR"] {
            std::env::set_var(var, d.path().join(var));
        }
        // the start check runs a real cargo (each test project has its own package name, as the target dir is shared): keep it off the outer cargo's target dir and build lock
        std::env::set_var("CARGO_TARGET_DIR", d.path().join("target"));
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
        close_on_verified: false,
        run_timeout: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_ends_in_an_error_still_counts_what_it_spent() {
    let out = test_tmpdir().unwrap();
    let bench = bench(Arc::new(Script::default()), 100, out.path());
    let mut task = nexo_bench::tasks::all().into_iter().find(|t| t.id == "misleading-error").unwrap();
    task.check = |_| panic!("a checker that breaks");
    assert!(run::run(&bench, &task, &Arm::parse("plain").unwrap(), 0, 5.0).await.is_err());
    let one = (1000.0 * 0.28 + 4000.0 * 0.028 + 100.0 * 0.42) / 1e6;
    assert!((*bench.spent.lock().unwrap() - 2.0 * one).abs() < 1e-12, "{}", bench.spent.lock().unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_two_session_task_runs_through_the_agent_and_resumes_its_history() {
    let out = test_tmpdir().unwrap();
    let script = Arc::new(Script::default());
    let bench = bench(script.clone(), 100, out.path());
    let task = nexo_bench::tasks::all().into_iter().find(|t| t.id == "cross-session").unwrap();
    let rec = run::run(&bench, &task, &Arm::parse("plain").unwrap(), 0, 5.0).await.unwrap();

    // each session: a request that lists the files, then one that ends the turn
    assert_eq!((rec.requests, rec.steps, rec.tool_calls, rec.tool_errors), (4, 4, 2, 0));
    assert!(rec.finished && rec.stop == Some(nexo_bench::client::Stop::Done), "{rec:?}");
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
    let out = test_tmpdir().unwrap();
    let bench = bench(Arc::new(Script::default()), 1, out.path());
    let task = nexo_bench::tasks::all().into_iter().find(|t| t.id == "misleading-error").unwrap();
    let rec = run::run(&bench, &task, &Arm::parse("compact").unwrap(), 0, 5.0).await.unwrap();
    assert_eq!(rec.steps, 1);
    assert!(!rec.finished, "{rec:?}");
    assert_eq!(rec.stop, Some(nexo_bench::client::Stop::MaxSteps));
}

/// Edits two files in one call, then says it is done.
struct EditTwo;

#[async_trait]
impl ModelClient for EditTwo {
    async fn stream(&self, _: &ProviderConfig, req: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        let last = req.messages.last().expect("a message");
        let asked = last.role == Role::User && last.content.iter().all(|p| matches!(p, Part::Text { .. }));
        let evs = if asked {
            let args = json!({"files": [
                {"path": "src/lib.rs", "edits": [{"old_string": "a - b", "new_string": "a * b"}]},
                {"path": "src/other.rs", "old_string": "1", "new_string": "2"}
            ]});
            vec![
                StreamEvent::ToolCallStart { id: "c1".into(), name: "edit".into() },
                StreamEvent::ToolCallEnd { id: "c1".into(), name: "edit".into(), args },
                StreamEvent::Finish(FinishReason::ToolUse),
                StreamEvent::Done,
            ]
        } else {
            vec![StreamEvent::TextDelta("done".into()), StreamEvent::Finish(FinishReason::Stop), StreamEvent::Done]
        };
        Ok(Box::pin(futures::stream::iter(evs)))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn one_edit_call_of_two_files_comes_back_with_the_projects_test_result() {
    let out = test_tmpdir().unwrap();
    let bench = bench(Arc::new(EditTwo), 100, out.path());
    let mut task = nexo_bench::tasks::all().into_iter().find(|t| t.id == "misleading-error").unwrap();
    task.files = vec![
        ("Cargo.toml".into(), "[package]\nname = \"calc\"\nversion = \"0.1.0\"\nedition = \"2021\"\n".into()),
        ("src/lib.rs".into(), "pub mod other;\npub fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n\n#[cfg(test)]\nmod t {\n    #[test]\n    fn adds() {\n        assert_eq!(super::add(2, 3), 5);\n    }\n}\n".into()),
        ("src/other.rs".into(), "pub const ONE: i32 = 1;\n".into()),
    ];
    task.turns = vec![(0, "fix add".into())];
    task.setup = None;
    task.sealed = false;
    task.mask = &[];
    task.check = |_| nexo_bench::check::Outcome { ok: false, score: 0.0, detail: String::new() };
    let rec = run::run(&bench, &task, &Arm::parse("plain").unwrap(), 0, 5.0).await.unwrap();
    // one call changed two files and the run took two requests: no step went to asking for the test
    assert_eq!((rec.requests, rec.tool_calls, rec.tool_errors), (2, 1, 0), "{rec:?}");
    let transcript = std::fs::read_to_string(out.path().join(&rec.transcript)).unwrap();
    assert!(transcript.contains("edited 2 files: src/lib.rs, src/other.rs"), "{transcript}");
    assert!(transcript.contains("check: cargo test -q --offline --no-fail-fast | exit 101"), "{transcript}");
}

/// Records the text of every request it is sent; replies with an `edit` first when given one, then says it is done.
struct Recorder {
    requests: Mutex<Vec<String>>,
    first: Option<serde_json::Value>,
}

#[async_trait]
impl ModelClient for Recorder {
    async fn stream(&self, _: &ProviderConfig, req: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        let text: String = req.messages.iter().map(|m| m.text()).collect::<Vec<_>>().join("\n--\n");
        self.requests.lock().unwrap().push(text);
        let last = req.messages.last().expect("a message");
        let asked = last.role == Role::User && last.content.iter().all(|p| matches!(p, Part::Text { .. }));
        let evs = match (&self.first, asked) {
            (Some(args), true) => vec![
                StreamEvent::ToolCallStart { id: "c1".into(), name: "edit".into() },
                StreamEvent::ToolCallEnd { id: "c1".into(), name: "edit".into(), args: args.clone() },
                StreamEvent::Finish(FinishReason::ToolUse),
                StreamEvent::Done,
            ],
            _ => {
                vec![StreamEvent::TextDelta("done".into()), StreamEvent::Finish(FinishReason::Stop), StreamEvent::Done]
            }
        };
        Ok(Box::pin(futures::stream::iter(evs)))
    }
}

fn calc_manifest(name: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")
}
const CALC_LIB: &str =
    "pub fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n\n#[test]\nfn adds() {\n    assert_eq!(add(2, 3), 5);\n}\n";

fn calc_task(id: &'static str, files: Vec<(String, String)>) -> nexo_bench::tasks::Task {
    let mut task = nexo_bench::tasks::all().into_iter().find(|t| t.id == "misleading-error").unwrap();
    // its own id: the working directory of a run is named after it, and the tests run side by side
    task.id = id;
    task.files = files;
    task.turns = vec![(0, "fix add".into())];
    task.setup = None;
    task.sealed = false;
    task.mask = &[];
    task.check = |_| nexo_bench::check::Outcome { ok: false, score: 0.0, detail: String::new() };
    task
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fix_lands_in_the_first_call_when_the_tests_come_with_the_request() {
    let out = test_tmpdir().unwrap();
    let edit = json!({"path": "src/lib.rs", "old_string": "a - b", "new_string": "a + b"});
    let rec = Arc::new(Recorder { requests: Mutex::default(), first: Some(edit) });
    let bench = bench(rec.clone(), 100, out.path());
    let task = calc_task(
        "fix-lands-first",
        vec![("Cargo.toml".into(), calc_manifest("fixfirst")), ("src/lib.rs".into(), CALC_LIB.into())],
    );
    let r = run::run(&bench, &task, &Arm::parse("plain").unwrap(), 0, 5.0).await.unwrap();
    assert_eq!((r.requests, r.tool_calls, r.tool_errors), (2, 1, 0), "{r:?}");
    let first = rec.requests.lock().unwrap()[0].clone();
    assert!(first.contains("failed 1 of 1: adds"), "{first}");
    assert!(first.contains("a - b"), "the small source file comes whole: {first}");
}

#[tokio::test(flavor = "multi_thread")]
async fn check_at_start_puts_the_failing_tests_in_the_first_request_with_one_cargo_run() {
    let out = test_tmpdir().unwrap();
    // two failing tests in two test binaries
    let count = |name: &str| format!("#[test]\nfn {name}() {{\n    panic!(\"{name} fails\");\n}}\n");
    let rec = Arc::new(Recorder { requests: Mutex::default(), first: None });
    let bench = bench(rec.clone(), 100, out.path());
    let task = calc_task(
        "check-at-start",
        vec![
            ("Cargo.toml".into(), calc_manifest("startcheck")),
            ("src/lib.rs".into(), count("alpha_test").replace("#[test]", "#[cfg(test)]\n#[test]")),
            ("tests/it.rs".into(), count("beta_test")),
        ],
    );
    let r = run::run(&bench, &task, &Arm::parse("plain").unwrap(), 0, 5.0).await.unwrap();
    assert_eq!(r.tool_calls, 0, "{r:?}");
    let first = rec.requests.lock().unwrap()[0].clone();
    assert!(
        first.contains("failed 2 of 2: ") && first.contains("alpha_test") && first.contains("beta_test"),
        "{first}"
    );
    // the check came once, in the request
    assert_eq!(first.matches("\ncheck: cargo test").count(), 1, "{first}");
}

/// Edits a file, reads the same file back, then says it is done.
#[derive(Default)]
struct EditThenRead {
    calls: Mutex<u32>,
}

#[async_trait]
impl ModelClient for EditThenRead {
    async fn stream(&self, _: &ProviderConfig, _: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        let n = {
            let mut c = self.calls.lock().unwrap();
            *c += 1;
            *c
        };
        let call = |name: &str, args: serde_json::Value| {
            vec![
                StreamEvent::ToolCallStart { id: format!("c{n}"), name: name.into() },
                StreamEvent::ToolCallEnd { id: format!("c{n}"), name: name.into(), args },
                StreamEvent::Finish(FinishReason::ToolUse),
                StreamEvent::Done,
            ]
        };
        let evs = match n {
            1 => call("edit", json!({"path": "notes.txt", "old_string": "beta", "new_string": "BETA"})),
            2 => call("read", json!({"path": "notes.txt"})),
            _ => {
                vec![StreamEvent::TextDelta("done".into()), StreamEvent::Finish(FinishReason::Stop), StreamEvent::Done]
            }
        };
        Ok(Box::pin(futures::stream::iter(evs)))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_of_an_unchanged_file_already_shown_is_answered_in_one_line() {
    let out = test_tmpdir().unwrap();
    let bench = bench(Arc::new(EditThenRead::default()), 100, out.path());
    let mut task = nexo_bench::tasks::all().into_iter().find(|t| t.id == "misleading-error").unwrap();
    task.files = vec![("notes.txt".into(), "alpha\nbeta\ngamma\n".into())];
    task.turns = vec![(0, "capitalise beta".into())];
    task.setup = None;
    task.sealed = false;
    task.mask = &[];
    task.check = |_| nexo_bench::check::Outcome { ok: false, score: 0.0, detail: String::new() };
    let rec = run::run(&bench, &task, &Arm::parse("plain").unwrap(), 0, 5.0).await.unwrap();
    assert_eq!((rec.requests, rec.tool_calls, rec.tool_errors), (3, 2, 0), "{rec:?}");
    let transcript = std::fs::read_to_string(out.path().join(&rec.transcript)).unwrap();
    // the edit came back with the whole short file; the read is a one-liner, not the lines again
    assert!(transcript.contains("applied and verified on disk"), "{transcript}");
    assert!(transcript.contains("unchanged since shown above (3 lines)"), "{transcript}");
    // each step logs the whole conversation; the last one has the file's lines once
    let last = transcript.lines().last().unwrap();
    assert_eq!(last.matches("3│ gamma").count(), 1, "the file is in the conversation once: {last}");
}

/// Reads the next 500 lines of `big.txt` each time a result comes back, six times, then says it is done.
struct SixReads;

#[async_trait]
impl ModelClient for SixReads {
    async fn stream(&self, _: &ProviderConfig, req: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        let done =
            req.messages.iter().flat_map(|m| m.content.iter()).filter(|p| matches!(p, Part::ToolResult { .. })).count();
        let evs = if done < 6 {
            let id = format!("r{done}");
            let args = json!({"path": "big.txt", "offset": 1 + 500 * done, "limit": 500});
            vec![
                StreamEvent::ToolCallStart { id: id.clone(), name: "read".into() },
                StreamEvent::ToolCallEnd { id, name: "read".into(), args },
                StreamEvent::Finish(FinishReason::ToolUse),
                StreamEvent::Done,
            ]
        } else {
            vec![StreamEvent::TextDelta("done".into()), StreamEvent::Finish(FinishReason::Stop), StreamEvent::Done]
        };
        Ok(Box::pin(futures::stream::iter(evs)))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn six_reads_of_a_big_file_keep_the_last_request_under_the_window() {
    let body: String = (0..3000)
        .map(|i| format!("line {i:05} {:x} the quick brown fox jumps over the lazy dog again\n", i * 7919 + 13))
        .collect();
    let mut last = Vec::new();
    for arm in ["plain", "curator:cap=0;target=0;hard=0", "curator:cap=20000", "curator"] {
        let out = test_tmpdir().unwrap();
        let bench = bench(Arc::new(SixReads), 100, out.path());
        let mut task = nexo_bench::tasks::all().into_iter().find(|t| t.id == "misleading-error").unwrap();
        task.files = vec![("big.txt".into(), body.clone())];
        task.turns = vec![(0, "read it".into())];
        task.setup = None;
        task.sealed = false;
        task.mask = &[];
        task.check = |_| nexo_bench::check::Outcome { ok: false, score: 0.0, detail: String::new() };
        let rec = run::run(&bench, &task, &Arm::parse(arm).unwrap(), 0, 5.0).await.unwrap();
        assert_eq!((rec.tool_calls, rec.tool_errors), (6, 0), "{arm}: {rec:?}");
        last.push(rec.final_view_tokens.max(rec.max_view_tokens));
    }
    assert!(last[0] > 40_000 && last[1] > 40_000, "without a window the six reads pile up: {last:?}");
    // The default no longer cuts at a fixed 20k: six calls on a cheap cache do not pay for a rewrite (the cost rule
    // in `fire_point`), so it waits and the view stays what plain sends. An explicit `cap` keeps the fixed trigger.
    assert!(last[2] <= 40_000, "a fixed cap holds the request under 40k tokens: {last:?}");
    assert!(last[3] > 40_000, "the default waits while a rewrite does not pay: {last:?}");
}

/// Scratch dir on disk under the Cargo target dir, never in the RAM-backed system temp dir.
fn test_tmpdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new().prefix("nexo-test-").tempdir_in(env!("CARGO_TARGET_TMPDIR"))
}
