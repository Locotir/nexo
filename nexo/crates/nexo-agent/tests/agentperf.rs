//! The agent's own cost per request in long sessions, with a scripted model that answers at once
//! (0 $). `cargo test -p nexo-agent --test agentperf --release -- --ignored --nocapture`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nexo_agent::{AgentSetup, ModelClient};
use nexo_core::agent::{channels, AgentCommand, AgentEvent};
use nexo_core::config::{Config, ProviderConfig};
use nexo_core::error::ProviderError;
use nexo_core::message::{Message, Part, Role};
use nexo_core::provider::{EventStream, ModelRequest};
use nexo_core::stream::{FinishReason, StreamEvent, Usage};
use nexo_core::tool::ToolSet;
use serde_json::json;

struct Quick {
    n: Mutex<VecDeque<()>>,
    seen: Mutex<Vec<usize>>,
}

#[async_trait]
impl ModelClient for Quick {
    async fn stream(&self, _: &ProviderConfig, req: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        self.seen.lock().unwrap().push(req.messages.len());
        let _ = self.n.lock().unwrap().pop_front();
        let evs = vec![
            StreamEvent::Start { response_id: None, model: None },
            StreamEvent::TextDelta("ok".into()),
            StreamEvent::Usage(Usage { input: 1000, output: 10, ..Default::default() }),
            StreamEvent::Finish(FinishReason::Stop),
            StreamEvent::Done,
        ];
        Ok(Box::pin(futures::stream::iter(evs)))
    }
}

/// `n` messages: requests, a call and its output, a reply, in turn.
fn history(n: usize) -> Vec<Message> {
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while out.len() < n {
        out.push(Message::user(format!("please look at module {i} and fix the failing check, then say what changed")));
        out.push(Message::new(
            Role::Assistant,
            vec![Part::tool_call(format!("c{i}"), "read", json!({"path": format!("src/m{i}.rs")}))],
        ));
        out.push(Message::tool_results(vec![Part::tool_result(
            format!("c{i}"),
            format!("fn f{i}() {{}}\n").repeat(12),
            false,
        )]));
        out.push(Message::assistant(format!("module {i} is fixed: the check passes now")));
        i += 1;
    }
    out.truncate(n);
    out
}

async fn per_request(n: usize, policy: &str, requests: usize) -> Vec<f64> {
    let d = tempfile::tempdir().unwrap();
    std::env::set_var("NEXO_DATA_DIR", d.path());
    std::env::set_var("NEXO_CONFIG_DIR", d.path());
    std::env::set_var("NEXO_CACHE_DIR", d.path().join("cache"));
    let mut s = nexo_agent::Session::create(d.path(), "mock", "m1").unwrap();
    for m in history(n) {
        s.append(&m).unwrap();
    }
    let id = s.id().to_string();
    drop(s);
    let mut cfg = Config::default();
    let mut p = ProviderConfig::new("mock", "http://mock", None);
    p.models = vec!["m1".into()];
    cfg.providers.insert("mock".into(), p);
    cfg.context.policy = policy.into();
    let (handle, ends) = channels();
    let client = Arc::new(Quick { n: Mutex::new(VecDeque::new()), seen: Mutex::new(Vec::new()) });
    let setup = AgentSetup {
        config: cfg,
        cwd: d.path().to_path_buf(),
        provider: "mock".into(),
        model: "m1".into(),
        tools: ToolSet::new(),
        client,
        resume: Some(id),
        gate: None,
        mail: None,
    };
    let task = tokio::spawn(nexo_agent::run(ends, setup));
    let mut handle = handle;
    let mut times = Vec::new();
    for k in 0..requests {
        let t = Instant::now();
        handle.commands.send(AgentCommand::Submit { text: format!("next {k}"), attachments: vec![] }).unwrap();
        loop {
            let ev = tokio::time::timeout(Duration::from_secs(120), handle.events.recv()).await.unwrap().unwrap();
            if matches!(ev, AgentEvent::TurnEnd { .. }) {
                break;
            }
        }
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    handle.commands.send(AgentCommand::Shutdown).unwrap();
    while let Some(ev) = handle.events.recv().await {
        if matches!(ev, AgentEvent::Shutdown) {
            break;
        }
    }
    let _ = task.await;
    times
}

/// The built-in curator told the history version gives the same views as the one that compares
/// the whole history, through 1,000 random appends, middle edits and cuts.
#[tokio::test]
async fn the_version_hint_never_changes_a_view() {
    use nexo_agent::context::BuiltIn;
    use nexo_agent::ContextPolicy;
    let (mut a, mut b) = (BuiltIn::new("").unwrap(), BuiltIn::new("").unwrap());
    let mut h = history(40);
    let (mut version, mut s) = (1u64, 11u64);
    let mut next = move || {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (s >> 33) as usize
    };
    for round in 0..1000 {
        match next() % 3 {
            0 => h.extend(history(next() % 4 + 1)),
            1 => {
                let at = next() % h.len();
                h[at] = Message::user(format!("edited {round}"));
                version += 1;
            }
            _ => {
                let to = next() % h.len();
                h.truncate(to + 1);
                version += 1;
            }
        }
        a.history_version(version);
        let (va, vb) = (a.view(0, &h).await.unwrap(), b.view(0, &h).await.unwrap());
        assert_eq!(
            serde_json::to_string(&va.messages).unwrap(),
            serde_json::to_string(&vb.messages).unwrap(),
            "round {round}"
        );
        assert_eq!(va.tokens, vb.tokens, "round {round}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn per_request_cost_by_history_size() {
    for policy in ["plain", "curator"] {
        for n in [1_000usize, 5_000, 20_000] {
            let mut t = per_request(n, policy, 6).await;
            t.remove(0);
            t.sort_by(|a, b| a.partial_cmp(b).unwrap());
            println!("PERF policy={policy} history={n} median_ms={:.2} max_ms={:.2}", t[t.len() / 2], t[t.len() - 1]);
        }
    }
}
