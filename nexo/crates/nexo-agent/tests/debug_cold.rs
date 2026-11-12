#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! The `debug` tool through the agent loop with a scripted model (no provider, 0 $): a model finds where a
//! program crashes, reads a value, keeps what it learned, and the whole round is five tool calls.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nexo_agent::{AgentSetup, ModelClient};
use nexo_core::agent::{channels, AgentCommand, AgentEvent, TurnEnd};
use nexo_core::config::{Config, ProviderConfig};
use nexo_core::error::ProviderError;
use nexo_core::message::{result_text, Part};
use nexo_core::permission::PermissionMode;
use nexo_core::provider::{EventStream, ModelRequest};
use nexo_core::stream::{FinishReason, StreamEvent, Usage};
use nexo_core::tool::ToolSet;
use serde_json::json;

struct Scripted {
    replies: Mutex<VecDeque<Vec<StreamEvent>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

#[async_trait]
impl ModelClient for Scripted {
    async fn stream(&self, _c: &ProviderConfig, req: &ModelRequest, _r: u32) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(req.clone());
        let evs = self.replies.lock().unwrap().pop_front().expect("a scripted reply");
        Ok(Box::pin(futures::stream::iter(evs)))
    }

    fn backoff(&self, _n: u32, _e: &ProviderError) -> Duration {
        Duration::ZERO
    }
}

fn call(id: &str, command: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolCallStart { id: id.into(), name: "debug".into() },
        StreamEvent::ToolCallEnd { id: id.into(), name: "debug".into(), args: json!({"command": command}) },
        StreamEvent::Usage(Usage { input: 10, output: 5, ..Default::default() }),
        StreamEvent::Finish(FinishReason::ToolUse),
        StreamEvent::Done,
    ]
}

fn say(s: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::Start { response_id: None, model: None },
        StreamEvent::TextDelta(s.into()),
        StreamEvent::Finish(FinishReason::Stop),
        StreamEvent::Done,
    ]
}

/// The program under test: build it where the test binary's own target folder is, when it is not there yet.
fn target() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let debug = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let t = debug.join("examples").join("dbg_target");
    if !t.is_file() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let st = std::process::Command::new(cargo)
            .args(["build", "-q", "-p", "nexo-dbg", "--example", "dbg_target", "--target-dir"])
            .arg(debug.parent().unwrap())
            .status()
            .expect("cargo runs");
        assert!(st.success() && t.is_file(), "dbg_target did not build");
    }
    t
}

static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Run a scripted model that makes these `debug` calls and then answers; gives back what each call returned.
async fn drive(dir: &std::path::Path, steps: Vec<String>, ask: &str) -> Vec<String> {
    let _one = ONE_AT_A_TIME.lock().await;
    std::env::set_var("NEXO_TMP", dir.join("tmp"));
    std::env::set_var("NEXO_DATA_DIR", dir.join("data"));
    std::env::set_var("NEXO_CONFIG_DIR", dir);
    std::env::set_var("NEXO_CACHE_DIR", dir.join("cache"));

    let mut replies: VecDeque<Vec<StreamEvent>> =
        steps.iter().enumerate().map(|(i, c)| call(&format!("c{i}"), c)).collect();
    replies.push_back(say("done"));
    let client = Arc::new(Scripted { replies: Mutex::new(replies), requests: Mutex::new(Vec::new()) });

    let mut cfg = Config::default();
    let mut p = ProviderConfig::new("mock", "http://mock", None);
    p.models = vec!["m1".into()];
    cfg.providers.insert("mock".into(), p);
    cfg.permissions.mode = PermissionMode::Auto;
    cfg.context.policy = "plain".into();
    cfg.agent.attach_budget = 0;
    cfg.agent.check_at_start = false;

    let (handle, ends) = channels();
    let setup = AgentSetup {
        config: cfg,
        cwd: dir.to_path_buf(),
        provider: "mock".into(),
        model: "m1".into(),
        tools: ToolSet::new().with(Arc::new(nexo_tools::debug::Debug::new())),
        client: client.clone(),
        resume: None,
        gate: None,
        mail: None,
        mcp: None,
        model_pinned: false,
    };
    let task = tokio::spawn(nexo_agent::run(ends, setup));
    let mut handle = handle;
    handle.commands.send(AgentCommand::Submit { text: ask.into(), attachments: vec![] }).unwrap();
    let mut calls = 0;
    let end = loop {
        let ev =
            tokio::time::timeout(Duration::from_secs(60), handle.events.recv()).await.expect("event").expect("alive");
        match ev {
            AgentEvent::ToolCall { name, .. } => {
                assert_eq!(name, "debug");
                calls += 1;
            }
            AgentEvent::TurnEnd { reason } => break reason,
            _ => {}
        }
    };
    assert_eq!(end, TurnEnd::Done);
    assert_eq!(calls, steps.len());

    // what the model was shown after each call
    let reqs = client.requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), steps.len() + 1);
    let results: Vec<String> = reqs[1..]
        .iter()
        .map(|r| match &r.messages.last().unwrap().content[0] {
            Part::ToolResult { content, .. } => result_text(content),
            other => panic!("{other:?}"),
        })
        .collect();
    let _ = handle.commands.send(AgentCommand::Shutdown);
    let _ = task.await;
    results
}

#[tokio::test]
async fn a_model_finds_a_crash_reads_a_value_and_keeps_it_in_five_calls() {
    let dir = tempfile::tempdir().unwrap();
    let prog = target();
    let steps = vec![
        format!("run {} crash", prog.display()),
        "cont".to_string(),
        "print rdi".to_string(),
        "note crash: dbg_crash reads through a null pointer (rdi is 0)".to_string(),
        "end".to_string(),
    ];
    let results = drive(dir.path(), steps, "why does it crash?").await;
    for (i, r) in results.iter().enumerate() {
        let first = r.lines().next().unwrap();
        let h = nexo_dbg::parse_header(first).unwrap_or_else(|| panic!("call {i} has no header line: {r}"));
        assert!(!h.session.is_empty());
    }
    let h1 = nexo_dbg::parse_header(results[1].lines().next().unwrap()).unwrap();
    assert_eq!(h1.stop.as_ref().map(|s| s.0.as_str()), Some("signal"), "{}", results[1]);
    assert!(results[1].contains("SIGSEGV") && results[1].contains("backtrace:") && results[1].contains("dbg_crash"));
    assert!(results[2].contains("0x0"), "{}", results[2]);
    let saved: Vec<&str> = results.iter().flat_map(|r| r.lines()).filter(|l| l.starts_with("saved:")).collect();
    assert_eq!(saved.len(), 1, "{saved:?}");
    // and it stayed: the file for this binary holds the note
    let kept = std::fs::read_dir(dir.path().join("data").join("bin")).unwrap().flatten().any(|e| {
        e.path().extension().is_some_and(|x| x == "json")
            && std::fs::read_to_string(e.path()).unwrap().contains("null pointer")
    });
    assert!(kept);
}

/// A tiny C program with an array read, built with -g: the model pokes an index out of range and runs on.
#[tokio::test]
async fn a_model_pokes_an_index_out_of_range_and_reads_the_crash_in_five_calls() {
    let Some(cc) = ["cc", "gcc", "clang"]
        .into_iter()
        .find(|c| std::process::Command::new(c).arg("--version").output().is_ok_and(|o| o.status.success()))
    else {
        eprintln!("no C compiler: skipped");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("paint.c");
    std::fs::write(
        &src,
        "#include <stdio.h>\nstatic int pixels[16];\n__attribute__((noinline)) int put(int *buf, long i, int v) {\n    buf[i] = v;\n    return buf[i];\n}\nint main(void) {\n    int s = 0;\n    for (long i = 0; i < 16; i++) s += put(pixels, i, 7);\n    printf(\"%d\\n\", s);\n    return 0;\n}\n",
    )
    .unwrap();
    let exe = dir.path().join("paint");
    let st = std::process::Command::new(cc).args(["-g", "-O0", "-no-pie", "-o"]).arg(&exe).arg(&src).status().unwrap();
    assert!(st.success());
    let steps = vec![
        format!("run {}", exe.display()),
        // `*put` is the entry itself: past the prologue the index is already on the stack, not in rsi
        "break *put".to_string(),
        "cont".to_string(),
        "setreg rsi 0x10000000000".to_string(),
        "cont".to_string(),
    ];
    let results = drive(dir.path(), steps, "what happens with a pixel far out of range?").await;
    let last = &results[4];
    assert!(last.contains("SIGSEGV"), "{last}");
    assert!(last.contains("put"), "the faulting function is named: {last}");
    assert!(last.contains("paint.c:4"), "and the source line: {last}");
    assert!(last.contains("backtrace:") && last.contains("main"), "{last}");
    assert!(last.contains("fault address 0x"), "{last}");
    assert!(last.lines().count() < 30, "{last}");
}
