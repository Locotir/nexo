//! The provider client of `nexo-providers` as the loop's [`ModelClient`].

use async_trait::async_trait;
use nexo_core::config::ProviderConfig;
use nexo_core::error::ProviderError;
use nexo_core::provider::{EventStream, ModelInfo, ModelRequest};

use crate::client::ModelClient;

#[async_trait]
impl ModelClient for nexo_providers::Client {
    async fn stream(
        &self,
        cfg: &ProviderConfig,
        req: &ModelRequest,
        retries: u32,
    ) -> Result<EventStream, ProviderError> {
        nexo_providers::Client::stream(self, cfg, req, retries).await
    }

    fn backoff(&self, n: u32, e: &ProviderError) -> std::time::Duration {
        nexo_providers::Client::backoff(self, n, e)
    }

    async fn list_models(&self, cfg: &ProviderConfig) -> Result<Vec<ModelInfo>, ProviderError> {
        nexo_providers::Client::list_models(self, cfg).await
    }
}
