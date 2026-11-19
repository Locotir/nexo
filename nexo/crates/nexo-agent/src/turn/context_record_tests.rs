//! A call whose view failed says so on its usage line, and only that one (0 $, no network).

use super::start::view_now;
use super::*;
use crate::context::Plain;
use nexo_core::agent::channels;
use nexo_core::config::{Config, ProviderConfig};
use nexo_core::error::{ErrorKind, ProviderError};
use nexo_core::message::Message;
use nexo_core::provider::{EventStream, ModelRequest};

/// A model nothing is asked of.
struct Idle;

#[async_trait::async_trait]
impl ModelClient for Idle {
    async fn stream(&self, _: &ProviderConfig, _: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        Err(ProviderError::new(ErrorKind::Other, "not called"))
    }
}

/// A policy whose view always fails.
struct Failing;

#[async_trait::async_trait]
impl ContextPolicy for Failing {
    fn name(&self) -> &str {
        "failing"
    }
    async fn view(&mut self, _system_tokens: u64, _history: &[Message]) -> anyhow::Result<View> {
        anyhow::bail!("no view today")
    }
    fn reset(&mut self) {}
}

fn usage_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| l.starts_with(r#"{"type":"usage""#))
        .map(String::from)
        .collect()
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_failed_view_marks_the_usage_line_of_that_call_only() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    std::env::set_var("NEXO_DATA_DIR", d.path());
    std::env::set_var("NEXO_MACHINE_MAP", "0");
    let mut config = Config::default();
    let mut p = ProviderConfig::new("mock", "http://mock", None);
    p.models = vec!["m1".into()];
    config.providers.insert("mock".into(), p);
    config.context.policy = "plain".into();
    config.agent.attach_budget = 0;
    config.agent.check_at_start = false;
    let cwd = d.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let (_handle, ends) = channels();
    let setup = crate::AgentSetup {
        config,
        cwd,
        provider: "mock".into(),
        model: "m1".into(),
        tools: ToolSet::new(),
        client: Arc::new(Idle),
        resume: None,
        gate: None,
        mail: None,
        mcp: None,
        model_pinned: false,
    };
    let rt = Runtime::build(ends.events, setup).await.unwrap();
    let mut st = rt.state.lock().await;
    let call = Usage { input: 10, output: 1, ..Default::default() };
    st.policy = Box::new(Failing);
    view_now(&mut st, 0).await;
    host::add_usage(&mut st, &rt.events, "m1", call, call, UsageKind::Main);
    st.policy = Box::new(Plain::default());
    view_now(&mut st, 0).await;
    host::add_usage(&mut st, &rt.events, "m1", call, call, UsageKind::Main);
    let lines = usage_lines(st.session.path());
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].contains(r#""fallback":true"#), "{}", lines[0]);
    assert!(!lines[1].contains("fallback"), "{}", lines[1]);
    // the policy that ran is on the record too, once
    let text = std::fs::read_to_string(st.session.path()).unwrap();
    assert_eq!(text.matches(r#"{"type":"context""#).count(), 1);
    std::env::remove_var("NEXO_DATA_DIR");
}
