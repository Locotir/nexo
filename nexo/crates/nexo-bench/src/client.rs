//! The model client of a run: the real one, wrapped to count every request, price it, write it
//! to the run's transcript and refuse further requests once the run is out of steps or money.
//!
//! The transcript holds what the model was sent and what it answered, never the provider config:
//! no URL, headers or key. Any secret given is also blanked out of every line, error texts included.

use std::fs::File;
use std::io::Write;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures::Stream;
use nexo_agent::ModelClient;
use nexo_core::{
    Collector, ErrorKind, EventStream, ModelRequest, Pricing, ProviderConfig, ProviderError, StreamEvent, Usage,
};
use serde::Serialize;
use serde_json::{json, Value};

/// Why a run ended. The bench itself cuts a run for `Budget`, `MaxSteps` or `Timeout` (its time limit); the rest is how the agent's
/// last turn ended. Written to the record as one string (`other:<detail>` for the exits not named here);
/// records of older benchmarks that hold null or only `budget` / `max_steps` still read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// The whole benchmark reached its budget.
    Budget,
    /// The task used all its steps.
    MaxSteps,
    /// The agent answered and stopped by itself.
    Done,
    /// `agent.close_on_verified` ended the turn on a passing check.
    Verified,
    /// The provider failed, or the turn broke.
    Error,
    /// The provider timed out, or the run passed its time limit and the bench interrupted it.
    Timeout,
    /// The turn was interrupted.
    Cancelled,
    /// Any other exit, with a short detail.
    Other(String),
}

impl Stop {
    /// The value written to the record.
    pub fn as_str(&self) -> String {
        match self {
            Stop::Budget => "budget".into(),
            Stop::MaxSteps => "max_steps".into(),
            Stop::Done => "done".into(),
            Stop::Verified => "verified".into(),
            Stop::Error => "error".into(),
            Stop::Timeout => "timeout".into(),
            Stop::Cancelled => "cancelled".into(),
            Stop::Other(d) => format!("other:{d}"),
        }
    }

    /// The reverse of `as_str`; an unknown value is `Other` and keeps its text.
    pub fn parse(s: &str) -> Stop {
        match s {
            "budget" => Stop::Budget,
            "max_steps" => Stop::MaxSteps,
            "done" => Stop::Done,
            "verified" => Stop::Verified,
            "error" => Stop::Error,
            "timeout" => Stop::Timeout,
            "cancelled" => Stop::Cancelled,
            _ => Stop::Other(s.strip_prefix("other:").unwrap_or(s).to_string()),
        }
    }
}

impl Serialize for Stop {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for Stop {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Stop::parse(&String::deserialize(d)?))
    }
}

/// What a run has spent so far.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Tally {
    pub requests: u32,
    /// Requests that offered tools: the agent's steps (a compaction summary is not one).
    pub steps: u32,
    pub usage: Usage,
    pub cost: f64,
    /// Set when the bench cut the run: `Budget`, `MaxSteps` or `Timeout`. The record's own reason is `LoopStats::stop`.
    pub stop: Option<Stop>,
    /// Requests that summarised the history (the `compact` policy), counted in `requests`.
    pub compactions: u32,
    /// Steps whose messages do not extend the previous step's: the history was rewritten in between
    /// (a summary, the curator). Always 0 for `plain`. A chat (no tools) counts its requests like steps.
    pub rewrites: u32,
    /// The size of each step's request in input tokens: the provider's count, else chars / 4. A chat, which
    /// offers no tools, has one per request; a compaction summary has none.
    pub views: Vec<u64>,
    /// The messages of the last step, each as JSON, to tell a rewrite from an append.
    #[serde(skip)]
    last: Vec<String>,
    /// The last system prompt written to the transcript, so it is written only when it changes.
    #[serde(skip)]
    system: String,
}

/// The limits of one run.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_steps: u32,
    /// The money left in the benchmark's budget when the run started.
    pub max_cost: f64,
}

pub struct Recorder {
    pub inner: Arc<dyn ModelClient>,
    pub price: Pricing,
    pub limits: Limits,
    pub tally: Arc<Mutex<Tally>>,
    /// The money of every run of the benchmark, counted as each reply is priced.
    pub spent: Arc<Mutex<f64>>,
    /// One JSON line per request.
    pub transcript: Arc<Mutex<File>>,
    /// Text never written to the transcript (the API key).
    pub secrets: Vec<String>,
}

/// The size of a request in tokens when the provider gives none: its JSON chars / 4.
pub fn estimate(req: &ModelRequest) -> u64 {
    let tools = serde_json::to_string(&req.tools).map_or(0, |s| s.len());
    let messages = serde_json::to_string(&req.messages).map_or(0, |s| s.len());
    ((req.system.len() + tools + messages) / 4) as u64
}

impl Recorder {
    fn refuse(&self, req: &ModelRequest) -> Option<ProviderError> {
        let mut t = self.tally.lock().unwrap();
        let stop = if t.cost >= self.limits.max_cost {
            Some(Stop::Budget)
        } else if !req.tools.is_empty() && t.steps >= self.limits.max_steps {
            Some(Stop::MaxSteps)
        } else {
            None
        };
        let stop = stop.or(t.stop.clone())?;
        t.stop.get_or_insert(stop.clone());
        let why = match stop {
            Stop::MaxSteps => "the task has used all its steps",
            Stop::Timeout => "the run has used all its time",
            _ => "the benchmark's budget is spent",
        };
        Some(ProviderError::new(ErrorKind::Other, format!("bench: {why}")))
    }
}

#[async_trait]
impl ModelClient for Recorder {
    async fn stream(
        &self,
        cfg: &ProviderConfig,
        req: &ModelRequest,
        retries: u32,
    ) -> Result<EventStream, ProviderError> {
        if let Some(e) = self.refuse(req) {
            return Err(e);
        }
        let entry = {
            let mut t = self.tally.lock().unwrap();
            t.requests += 1;
            let step = !req.tools.is_empty();
            let compaction = req.system == nexo_agent::compact::SUMMARY_SYSTEM;
            if step {
                t.steps += 1;
            }
            // a chat offers no tools: its requests are the agent's too, only a compaction summary is not
            if !compaction {
                let now: Vec<String> =
                    req.messages.iter().map(|m| serde_json::to_string(m).unwrap_or_default()).collect();
                if !now.starts_with(&t.last) {
                    t.rewrites += 1;
                }
                t.last = now;
            }
            if compaction {
                t.compactions += 1;
            }
            let mut e = json!({
                "n": t.requests,
                "model": req.model,
                "tools": req.tools.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
                "step": step,
                "compaction": compaction,
                "view_est_tokens": estimate(req),
                "messages": req.messages,
            });
            if t.system != req.system {
                t.system = req.system.clone();
                e["system"] = json!(req.system);
            }
            e
        };
        let rec = Record {
            step: req.system != nexo_agent::compact::SUMMARY_SYSTEM,
            est: estimate(req),
            entry,
            tally: self.tally.clone(),
            spent: self.spent.clone(),
            price: self.price,
            transcript: self.transcript.clone(),
            secrets: self.secrets.clone(),
        };
        match self.inner.stream(cfg, req, retries).await {
            Ok(inner) => Ok(Box::pin(Recording { inner, col: Collector::new(), rec: Some(rec) })),
            Err(e) => {
                rec.write(json!({"error": e.to_string()}));
                Err(e)
            }
        }
    }
}

/// What is written for one request once its reply is complete.
struct Record {
    /// A request of the agent's own (not a compaction summary): its view is counted.
    step: bool,
    /// The request's size by [`estimate`].
    est: u64,
    entry: Value,
    tally: Arc<Mutex<Tally>>,
    spent: Arc<Mutex<f64>>,
    price: Pricing,
    transcript: Arc<Mutex<File>>,
    secrets: Vec<String>,
}

impl Record {
    fn write(mut self, extra: Value) {
        if let (Value::Object(e), Value::Object(x)) = (&mut self.entry, extra) {
            e.extend(x);
        }
        let mut line = self.entry.to_string();
        for s in self.secrets.iter().filter(|s| s.len() >= 8) {
            line = line.replace(s.as_str(), "[redacted]");
        }
        let mut f = self.transcript.lock().unwrap();
        let _ = writeln!(f, "{line}");
    }

    fn finish(self, col: &Collector) {
        let cost = self.price.cost(&col.usage);
        let prompt = col.call.total_input();
        let view = if prompt > 0 { prompt } else { self.est };
        {
            let mut t = self.tally.lock().unwrap();
            t.usage += col.usage;
            t.cost += cost;
            if self.step {
                t.views.push(view);
            }
        }
        *self.spent.lock().unwrap() += cost;
        self.write(json!({
            "reply": col.message(),
            "finish": col.finish,
            "usage": col.usage,
            "cost": cost,
            "view_tokens": view,
            "error": col.error.as_ref().map(|e| e.to_string()),
        }));
    }
}

/// The reply as it streams to the agent, collected on the side.
struct Recording {
    inner: EventStream,
    col: Collector,
    rec: Option<Record>,
}

impl Stream for Recording {
    type Item = StreamEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<StreamEvent>> {
        let this = &mut *self;
        let next = this.inner.as_mut().poll_next(cx);
        match &next {
            Poll::Ready(Some(StreamEvent::Done)) | Poll::Ready(None) => {
                if let Some(r) = this.rec.take() {
                    r.finish(&this.col);
                }
            }
            Poll::Ready(Some(ev)) => this.col.push(ev.clone()),
            Poll::Pending => {}
        }
        next
    }
}

impl Drop for Recording {
    /// A reply the agent stopped reading still cost what it used.
    fn drop(&mut self) {
        if let Some(r) = self.rec.take() {
            r.finish(&self.col);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    struct Canned;

    #[async_trait]
    impl ModelClient for Canned {
        async fn stream(&self, _: &ProviderConfig, _: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
            let evs = vec![
                StreamEvent::TextDelta("done".into()),
                StreamEvent::Usage(Usage { input: 1_000_000, ..Default::default() }),
                StreamEvent::Finish(nexo_core::FinishReason::Stop),
                StreamEvent::Done,
            ];
            Ok(Box::pin(futures::stream::iter(evs)))
        }
    }

    fn recorder(max_steps: u32, max_cost: f64) -> (Recorder, tempfile::NamedTempFile) {
        let f = tempfile::NamedTempFile::new().unwrap();
        let r = Recorder {
            inner: Arc::new(Canned),
            price: crate::cost::DEEPSEEK,
            limits: Limits { max_steps, max_cost },
            tally: Arc::default(),
            spent: Arc::default(),
            transcript: Arc::new(Mutex::new(f.reopen().unwrap())),
            secrets: vec![],
        };
        (r, f)
    }

    fn request() -> ModelRequest {
        let mut r = ModelRequest::new("m", "sys", vec![nexo_core::Message::user("hi")]);
        r.tools = vec![nexo_core::ToolSpec { name: "ls".into(), description: String::new(), parameters: json!({}) }];
        r
    }

    #[tokio::test]
    async fn every_request_is_counted_priced_and_written() {
        let (r, f) = recorder(10, 100.0);
        let cfg = ProviderConfig::default();
        for _ in 0..2 {
            let s = r.stream(&cfg, &request(), 0).await.unwrap();
            let _: Vec<_> = s.collect().await;
        }
        let t = r.tally.lock().unwrap().clone();
        assert_eq!((t.requests, t.steps, t.usage.input), (2, 2, 2_000_000));
        assert!((t.cost - 0.56).abs() < 1e-9);
        // the benchmark's total counts it too, whatever the run does next
        assert!((*r.spent.lock().unwrap() - 0.56).abs() < 1e-9);
        let text = std::fs::read_to_string(f.path()).unwrap();
        let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["system"], "sys");
        assert!(lines[1].get("system").is_none(), "an unchanged system prompt is written once");
        assert_eq!(lines[1]["messages"][0]["role"], "user");
    }

    /// Fails echoing the key, as a careless provider error might.
    struct Leaky;

    #[async_trait]
    impl ModelClient for Leaky {
        async fn stream(&self, cfg: &ProviderConfig, _: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
            Err(ProviderError::new(ErrorKind::Auth, format!("401 for {:?}", cfg.headers)))
        }
    }

    #[tokio::test]
    async fn the_transcript_holds_no_url_header_or_key() {
        let key = "sk-secret-0123456789";
        let mut cfg = ProviderConfig::new("openai-chat", "https://api.example.invalid", Some("NEXO_BENCH_KEY"));
        cfg.headers.insert("Authorization".into(), format!("Bearer {key}"));
        let (mut r, f) = recorder(10, 100.0);
        r.secrets = vec![key.to_string()];
        let _: Vec<_> = r.stream(&cfg, &request(), 0).await.unwrap().collect().await;
        r.inner = Arc::new(Leaky);
        assert!(r.stream(&cfg, &request(), 0).await.is_err());
        let text = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("[redacted]"), "the failed request is written, blanked: {text}");
        for bad in [key, "api.example.invalid", "NEXO_BENCH_KEY"] {
            assert!(!text.contains(bad), "{bad} in {text}");
        }
        let first: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        let keys: Vec<&str> = first.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert!(!keys.iter().any(|k| k.contains("header") || k.contains("auth") || k.contains("url")), "{keys:?}");
    }

    #[tokio::test]
    async fn steps_rewrites_compactions_and_view_sizes_are_counted() {
        let (r, f) = recorder(10, 100.0);
        let cfg = ProviderConfig::default();
        let mut req = request();
        let send = |req: ModelRequest| {
            let r = &r;
            let cfg = &cfg;
            async move {
                let _: Vec<_> = r.stream(cfg, &req, 0).await.unwrap().collect().await;
            }
        };
        send(req.clone()).await;
        req.messages.push(nexo_core::Message::assistant("ok"));
        req.messages.push(nexo_core::Message::user("more"));
        send(req.clone()).await; // appended: no rewrite
        let mut summary =
            ModelRequest::new("m", nexo_agent::compact::SUMMARY_SYSTEM, vec![nexo_core::Message::user("t")]);
        summary.tools.clear();
        send(summary).await;
        req.messages = vec![nexo_core::Message::user("[summary] ...")];
        send(req.clone()).await; // rewritten
        let t = r.tally.lock().unwrap().clone();
        assert_eq!((t.requests, t.steps, t.compactions, t.rewrites), (4, 3, 1, 1));
        assert_eq!(t.views, vec![1_000_000; 3], "the provider's count, steps only");
        let lines: Vec<Value> =
            std::fs::read_to_string(f.path()).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines[2]["compaction"], true);
        assert_eq!(lines[2]["step"], false);
        assert!(lines[0]["view_est_tokens"].as_u64().unwrap() > 0);
    }

    struct Retried;

    #[async_trait]
    impl ModelClient for Retried {
        async fn stream(&self, _: &ProviderConfig, _: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
            let u = Usage { input: 40_000, ..Default::default() };
            let evs = vec![
                StreamEvent::RetriedUsage(u),
                StreamEvent::TextDelta("done".into()),
                StreamEvent::Usage(u),
                StreamEvent::Finish(nexo_core::FinishReason::Stop),
                StreamEvent::Done,
            ];
            Ok(Box::pin(futures::stream::iter(evs)))
        }
    }

    #[tokio::test]
    async fn a_retried_attempt_is_spent_but_does_not_enlarge_the_view() {
        let (mut r, f) = recorder(10, 100.0);
        r.inner = Arc::new(Retried);
        let _: Vec<_> = r.stream(&ProviderConfig::default(), &request(), 0).await.unwrap().collect().await;
        let t = r.tally.lock().unwrap().clone();
        assert_eq!(t.usage.input, 80_000, "both attempts were billed");
        assert_eq!(t.views, vec![40_000], "the request is the answering attempt alone");
        let line: Value =
            serde_json::from_str(std::fs::read_to_string(f.path()).unwrap().lines().next().unwrap()).unwrap();
        assert_eq!(line["view_tokens"], 40_000);
    }

    #[test]
    fn a_request_is_estimated_from_all_it_sends() {
        let r = request();
        let bare = ModelRequest::new("m", "", vec![]);
        assert!(estimate(&r) > estimate(&bare));
        assert!(estimate(&bare) <= 1, "only the empty lists");
    }

    #[tokio::test]
    async fn requests_stop_at_the_step_limit_and_at_the_budget() {
        let cfg = ProviderConfig::default();
        let (r, _f) = recorder(1, 100.0);
        let _: Vec<_> = r.stream(&cfg, &request(), 0).await.unwrap().collect().await;
        assert!(r.stream(&cfg, &request(), 0).await.is_err());
        assert_eq!(r.tally.lock().unwrap().stop, Some(Stop::MaxSteps));
        let (r, _f) = recorder(10, 0.1);
        let _: Vec<_> = r.stream(&cfg, &request(), 0).await.unwrap().collect().await;
        assert!(r.stream(&cfg, &request(), 0).await.is_err());
        assert_eq!(r.tally.lock().unwrap().stop, Some(Stop::Budget));
    }
}
