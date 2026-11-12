//! The chat task through the real agent loop with a scripted model: no network, no tools, graded on the answers.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use nexo_agent::ModelClient;
use nexo_bench::arm::Arm;
use nexo_bench::client::Stop;
use nexo_bench::run::Bench;
use nexo_bench::tasks::padded;
use nexo_core::{EventStream, FinishReason, ModelRequest, ProviderConfig, ProviderError, StreamEvent, Usage};

/// Replies in order; when they run out, a closing text.
struct Seq(Mutex<VecDeque<String>>);

#[async_trait]
impl ModelClient for Seq {
    async fn stream(&self, _: &ProviderConfig, _: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        let text = self.0.lock().unwrap().pop_front().unwrap_or_else(|| "All done here.".into());
        let evs = vec![
            StreamEvent::TextDelta(text),
            StreamEvent::Usage(Usage { input: 10, output: 5, ..Default::default() }),
            StreamEvent::Finish(FinishReason::Stop),
            StreamEvent::Done,
        ];
        Ok(Box::pin(futures::stream::iter(evs)))
    }
}

fn bench(script: Vec<String>, max_steps: u32, out: &std::path::Path) -> Bench {
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
        max_steps,
        max_tokens: None,
        temperature: None,
        out: out.to_path_buf(),
        hide: vec![],
        client: Arc::new(Seq(Mutex::new(script.into()))),
        spent: Arc::default(),
        close_on_verified: false,
        run_timeout: None,
    }
}

async fn run_chat(script: Vec<String>) -> nexo_bench::run::Record {
    let out = test_tmpdir().unwrap();
    let bench = bench(script, 40, out.path());
    padded::run_chat(&bench, &padded::chat_task(), &Arm::parse("plain").unwrap(), 0, 5.0).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mock_run_of_the_chat_completes_and_the_right_answers_score_1() {
    let r = run_chat((padded::chat_task().reference)()).await;
    assert_eq!(r.task, "padded-revert-chat");
    assert_eq!(r.stop, Some(Stop::Done), "{r:?}");
    assert!(r.finished && r.ok && r.score == 1.0, "{r:?}");
    assert_eq!(r.steps, 0, "no tool steps: the chat offers no tools");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mock_run_that_gives_another_topic_last_scores_low() {
    let mut script = (padded::chat_task().reference)();
    script[4] = script[2].clone();
    let r = run_chat(script).await;
    assert_eq!(r.stop, Some(Stop::Done), "{r:?}");
    assert!(!r.ok && r.score < 1.0, "{r:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_run_reports_the_size_of_each_request_it_sent() {
    let r = run_chat((padded::chat_task().reference)()).await;
    assert_eq!(r.view_tokens.len(), 5, "one view per question: {r:?}");
    assert!(r.max_view_tokens > 0 && r.final_view_tokens > 0, "{r:?}");
}

/// Scratch dir on disk under the Cargo target dir, never in the RAM-backed system temp dir.
fn test_tmpdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new().prefix("nexo-test-").tempdir_in(env!("CARGO_TARGET_TMPDIR"))
}
