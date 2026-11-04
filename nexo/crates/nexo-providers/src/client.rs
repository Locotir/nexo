//! The registry of protocols and the client that retries.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use nexo_core::{
    ErrorKind, EventStream, ModelInfo, ModelRequest, Protocol, ProviderConfig, ProviderError, StreamEvent,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::{
    anthropic::Anthropic, gemini::Gemini, mock::Mock, openai_chat::OpenAiChat, openai_responses::OpenAiResponses,
};

/// The protocols by id.
#[derive(Clone, Default)]
pub struct Registry {
    map: BTreeMap<&'static str, Arc<dyn Protocol>>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    pub fn register(&mut self, p: Arc<dyn Protocol>) {
        self.map.insert(p.id(), p);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Protocol>> {
        self.map.get(id).cloned()
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.map.keys().copied().collect()
    }
}

/// Every protocol this crate implements.
pub fn registry() -> Registry {
    let mut r = Registry::new();
    r.register(Arc::new(OpenAiChat));
    r.register(Arc::new(OpenAiResponses));
    r.register(Arc::new(Anthropic));
    r.register(Arc::new(Gemini));
    r.register(Arc::new(crate::copilot::Copilot));
    r.register(Arc::new(Mock::new()));
    r
}

/// One request and what re-issuing it takes.
struct Call {
    protocol: Arc<dyn Protocol>,
    cfg: ProviderConfig,
    key: Option<String>,
    req: ModelRequest,
    retries: u32,
}

/// Calls a provider and retries what can be retried: a failed request, or
/// a stream that broke before it said anything (after the first word the
/// partial reply is the caller's to keep).
#[derive(Clone)]
pub struct Client {
    registry: Registry,
    /// Longest wait between two attempts.
    pub max_backoff: Duration,
}

impl Client {
    pub fn new(registry: Registry) -> Client {
        Client { registry, max_backoff: Duration::from_secs(30) }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    fn protocol(&self, cfg: &ProviderConfig) -> Result<Arc<dyn Protocol>, ProviderError> {
        self.registry.get(&cfg.protocol).ok_or_else(|| {
            ProviderError::new(
                ErrorKind::Other,
                format!("unknown protocol {:?} (known: {})", cfg.protocol, self.registry.ids().join(", ")),
            )
        })
    }

    /// The wait before attempt `n` (1-based), honouring what the server asked.
    pub fn backoff(&self, n: u32, e: &ProviderError) -> Duration {
        let wanted = e.retry_after.unwrap_or_else(|| Duration::from_secs(1u64 << (n.saturating_sub(1)).min(5)));
        wanted.min(self.max_backoff)
    }

    pub async fn stream(
        &self,
        cfg: &ProviderConfig,
        req: &ModelRequest,
        retries: u32,
    ) -> Result<EventStream, ProviderError> {
        let protocol = self.protocol(cfg)?;
        let (shaped, key) = crate::login::prepare(cfg).await.map_err(|e| self.explain(cfg, true, e))?;
        let cfg = &shaped;
        let mut attempt = 0u32;
        let inner = loop {
            match protocol.stream(cfg, key.as_deref(), req).await {
                Ok(s) => break s,
                Err(e) if e.is_retryable() && attempt < retries => {
                    attempt += 1;
                    let wait = self.backoff(attempt, &e);
                    tracing::warn!(attempt, ?wait, "{e}; retrying");
                    tokio::time::sleep(wait).await;
                }
                Err(e) => return Err(self.explain(cfg, key.is_some(), e)),
            }
        };
        let call = Call { protocol, cfg: cfg.clone(), key, req: req.clone(), retries };
        Ok(self.forward(inner, call, attempt))
    }

    /// Forwards the events; a retryable error before any content re-issues the request.
    fn forward(&self, mut inner: EventStream, call: Call, mut attempt: u32) -> EventStream {
        let (tx, rx) = mpsc::unbounded_channel::<StreamEvent>();
        let this = self.clone();
        let Call { protocol, cfg, key, req, retries } = call;
        tokio::spawn(async move {
            'attempts: loop {
                let mut content = false;
                // what comes before the first content (start, opaque reasoning, usage) is held
                // back, so a retry never hands the caller the same preamble twice
                let mut held: Vec<StreamEvent> = Vec::new();
                while let Some(ev) = inner.next().await {
                    let preamble = matches!(
                        ev,
                        StreamEvent::Start { .. }
                            | StreamEvent::RedactedReasoning { .. }
                            | StreamEvent::ReasoningMeta { .. }
                            | StreamEvent::Usage(_)
                    );
                    if preamble && !content {
                        held.push(ev);
                        continue;
                    }
                    match &ev {
                        StreamEvent::TextDelta(_)
                        | StreamEvent::ReasoningDelta(_)
                        | StreamEvent::ToolCallStart { .. } => content = true,
                        StreamEvent::Error(e) if !content && e.is_retryable() && attempt < retries => {
                            attempt += 1;
                            let wait = this.backoff(attempt, e);
                            tracing::warn!(attempt, ?wait, "{e}; retrying");
                            tokio::time::sleep(wait).await;
                            match protocol.stream(&cfg, key.as_deref(), &req).await {
                                Ok(s) => {
                                    inner = s;
                                    continue 'attempts;
                                }
                                Err(e) => {
                                    for h in held.drain(..) {
                                        let _ = tx.send(h);
                                    }
                                    let _ = tx.send(StreamEvent::Error(this.explain(&cfg, key.is_some(), e)));
                                    let _ = tx.send(StreamEvent::Done);
                                    return;
                                }
                            }
                        }
                        _ => {}
                    }
                    for h in held.drain(..) {
                        if tx.send(h).is_err() {
                            return;
                        }
                    }
                    let done = matches!(ev, StreamEvent::Done);
                    if tx.send(ev).is_err() {
                        return; // the caller went away
                    }
                    if done {
                        return;
                    }
                }
                for h in held.drain(..) {
                    let _ = tx.send(h);
                }
                let _ = tx.send(StreamEvent::Done);
                return;
            }
        });
        Box::pin(UnboundedReceiverStream::new(rx))
    }

    /// A missing key is the usual cause of an auth error: say where one goes.
    fn explain(&self, cfg: &ProviderConfig, had_key: bool, mut e: ProviderError) -> ProviderError {
        if e.kind != ErrorKind::Auth {
            return e;
        }
        let name = cfg.auth.as_deref().unwrap_or("<provider>");
        if !had_key {
            let fix = match (&cfg.api_key_env, &cfg.login) {
                (Some(var), Some(_)) => format!("set {var} or run `nexo auth login {name}`"),
                (Some(var), None) => format!("set {var} or run `nexo auth login {name} --key`"),
                (None, Some(_)) => format!("run `nexo auth login {name}`"),
                (None, None) => format!("run `nexo auth login {name} --key`"),
            };
            e.message = format!("{} (no key found: {fix})", e.message);
        } else if cfg.login.is_some() && !e.message.contains("nexo auth login") {
            e.message = format!("{} (if the login expired: `nexo auth login {name}`)", e.message);
        }
        e
    }

    pub async fn list_models(&self, cfg: &ProviderConfig) -> Result<Vec<ModelInfo>, ProviderError> {
        let protocol = self.protocol(cfg)?;
        let (shaped, key) = crate::login::prepare(cfg).await.map_err(|e| self.explain(cfg, true, e))?;
        protocol.list_models(&shaped, key.as_deref()).await.map_err(|e| self.explain(cfg, key.is_some(), e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use nexo_core::{Message, Usage};
    use std::sync::Mutex;

    /// A protocol that answers each re-issued request with the next scripted stream.
    struct Scripted(Mutex<Vec<Vec<StreamEvent>>>);

    #[async_trait]
    impl Protocol for Scripted {
        fn id(&self) -> &'static str {
            "scripted"
        }

        async fn stream(
            &self,
            _cfg: &ProviderConfig,
            _key: Option<&str>,
            _req: &ModelRequest,
        ) -> Result<EventStream, ProviderError> {
            let evs = self.0.lock().unwrap().remove(0);
            Ok(Box::pin(futures::stream::iter(evs)))
        }
    }

    #[tokio::test]
    async fn a_retry_never_repeats_the_preamble_of_the_failed_attempt() {
        let start = StreamEvent::Start { response_id: Some("r".into()), model: Some("m".into()) };
        let mut overloaded = ProviderError::new(ErrorKind::Overloaded, "Overloaded").with_status(529);
        overloaded.retry_after = Some(Duration::ZERO);
        let failed = vec![
            start.clone(),
            StreamEvent::RedactedReasoning { data: "R".into() },
            StreamEvent::ReasoningMeta { signature: Some("S".into()), encrypted: None },
            StreamEvent::Error(overloaded),
            StreamEvent::Done,
        ];
        let good = vec![
            start.clone(),
            StreamEvent::TextDelta("hi".into()),
            StreamEvent::Usage(Usage { input: 1, output: 1, ..Default::default() }),
            StreamEvent::Done,
        ];
        let protocol: Arc<dyn Protocol> = Arc::new(Scripted(Mutex::new(vec![good.clone()])));
        let client = Client::new(Registry::new());
        let call = Call {
            protocol,
            cfg: ProviderConfig::new("scripted", "http://127.0.0.1:9", None),
            key: None,
            req: ModelRequest::new("m", "", vec![Message::user("hi")]),
            retries: 2,
        };
        let evs: Vec<StreamEvent> = client.forward(Box::pin(futures::stream::iter(failed)), call, 0).collect().await;
        assert_eq!(evs, good, "only the second attempt reaches the caller");
    }
}
