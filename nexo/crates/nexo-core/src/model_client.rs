//! How a caller reaches a model: one call to a provider, with the client's own retries.

use std::sync::Arc;
use std::time::Duration;

use crate::config::ProviderConfig;
use crate::error::{ErrorKind, ProviderError};
use crate::provider::{EventStream, ModelInfo, ModelRequest};
use async_trait::async_trait;

/// One retry the client is about to make: the request failed, and it will be sent again after `wait`.
#[derive(Debug, Clone, PartialEq)]
pub struct Retry {
    /// Which retry this is, from 1.
    pub attempt: u32,
    /// How many retries the call may make in all.
    pub retries: u32,
    /// How long the client waits before it sends the request again.
    pub wait: Duration,
    /// The provider asked for the wait because of its limits (a 429), not because it failed.
    pub rate_limit: bool,
    /// What went wrong, in the few words a status line has room for.
    pub reason: String,
}

impl Retry {
    /// The retry that follows `e`: the kind's plain phrase, with the HTTP status when the server gave one.
    pub fn after(e: &ProviderError, attempt: u32, retries: u32, wait: Duration) -> Retry {
        let reason = match e.status {
            Some(s) => format!("{}, HTTP {s}", e.kind.phrase()),
            None => e.kind.phrase().to_string(),
        };
        Retry { attempt, retries, wait, rate_limit: e.kind == ErrorKind::RateLimit, reason }
    }
}

/// Told of each retry as it is about to wait; called from the client's own task, so it must not block.
pub type RetryObserver = Arc<dyn Fn(Retry) + Send + Sync>;

/// One call to a provider, with the client's own retries.
#[async_trait]
pub trait ModelClient: Send + Sync {
    async fn stream(
        &self,
        cfg: &ProviderConfig,
        req: &ModelRequest,
        retries: u32,
    ) -> Result<EventStream, ProviderError>;

    /// `stream`, telling `on_retry` of each retry before its wait. A request that fails before any stream
    /// exists is retried too, which is why this is an observer of the call and not an event of the stream.
    /// A client that does not retry on its own has nothing to tell and just streams.
    async fn stream_observed(
        &self,
        cfg: &ProviderConfig,
        req: &ModelRequest,
        retries: u32,
        _on_retry: RetryObserver,
    ) -> Result<EventStream, ProviderError> {
        self.stream(cfg, req, retries).await
    }

    /// The wait before the loop issues a request again itself, for the `n`th time (1-based), after `e` cut
    /// its reply. A client with no waits of its own (a scripted one) asks for none.
    fn backoff(&self, _n: u32, _e: &ProviderError) -> Duration {
        Duration::ZERO
    }

    /// Whether the loop should send the request again after `e` cut its reply: it can be retried, and the
    /// wait the server asked for is one the client would sit through. A client with no waits of its own
    /// retries whatever can be retried.
    fn worth_retrying(&self, e: &ProviderError) -> bool {
        e.is_retryable()
    }

    /// The vendor's model list, where there is one.
    async fn list_models(&self, _cfg: &ProviderConfig) -> Result<Vec<ModelInfo>, ProviderError> {
        Err(ProviderError::new(ErrorKind::Other, "no model list"))
    }
}
