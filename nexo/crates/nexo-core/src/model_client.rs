//! How a caller reaches a model: one call to a provider, with the client's own retries.

use std::time::Duration;

use crate::config::ProviderConfig;
use crate::error::{ErrorKind, ProviderError};
use crate::provider::{EventStream, ModelInfo, ModelRequest};
use async_trait::async_trait;

/// One call to a provider, with the client's own retries.
#[async_trait]
pub trait ModelClient: Send + Sync {
    async fn stream(
        &self,
        cfg: &ProviderConfig,
        req: &ModelRequest,
        retries: u32,
    ) -> Result<EventStream, ProviderError>;

    /// The wait before the loop issues a request again itself, for the `n`th time (1-based), after `e` cut
    /// its reply. A client with no waits of its own (a scripted one) asks for none.
    fn backoff(&self, _n: u32, _e: &ProviderError) -> Duration {
        Duration::ZERO
    }

    /// The vendor's model list, where there is one.
    async fn list_models(&self, _cfg: &ProviderConfig) -> Result<Vec<ModelInfo>, ProviderError> {
        Err(ProviderError::new(ErrorKind::Other, "no model list"))
    }
}
