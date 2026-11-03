//! How the loop reaches a model, and which model that is right now.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nexo_core::config::ProviderConfig;
use nexo_core::error::{ErrorKind, ProviderError};
use nexo_core::provider::{EventStream, ModelInfo, ModelRequest};

/// One call to a provider, with the client's own retries.
#[async_trait]
pub trait ModelClient: Send + Sync {
    async fn stream(
        &self,
        cfg: &ProviderConfig,
        req: &ModelRequest,
        retries: u32,
    ) -> Result<EventStream, ProviderError>;

    /// The vendor's model list, where there is one.
    async fn list_models(&self, _cfg: &ProviderConfig) -> Result<Vec<ModelInfo>, ProviderError> {
        Err(ProviderError::new(ErrorKind::Other, "no model list"))
    }
}

/// The provider and model in use, shared by the loop and whatever else makes
/// model calls (the summariser), changed by `SetModel`.
#[derive(Clone)]
pub struct Target {
    inner: Arc<Mutex<(String, ProviderConfig, String)>>,
}

impl Target {
    pub fn new(provider: impl Into<String>, config: ProviderConfig, model: impl Into<String>) -> Target {
        Target { inner: Arc::new(Mutex::new((provider.into(), config, model.into()))) }
    }

    /// (provider name, its configuration, model).
    pub fn get(&self) -> (String, ProviderConfig, String) {
        self.inner.lock().map(|g| g.clone()).unwrap_or_else(|e| e.into_inner().clone())
    }

    pub fn set(&self, provider: impl Into<String>, config: ProviderConfig, model: impl Into<String>) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        *g = (provider.into(), config, model.into());
    }

    pub fn model(&self) -> String {
        self.get().2
    }

    pub fn provider(&self) -> String {
        self.get().0
    }
}
