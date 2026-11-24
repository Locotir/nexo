//! How the loop reaches a model, and which model that is right now.

use std::sync::{Arc, Mutex};

use nexo_core::config::ProviderConfig;
pub use nexo_core::model_client::ModelClient;
/// The provider and model in use, shared by the loop and whatever else makes
/// model calls (the summariser), changed by `SetModel`.
#[derive(Clone)]
pub struct Target {
    inner: Arc<Mutex<(String, ProviderConfig, String)>>,
}

impl Target {
    pub fn new(provider: impl Into<String>, mut config: ProviderConfig, model: impl Into<String>) -> Target {
        let (provider, model) = (provider.into(), model.into());
        crate::setup::fill_levels(&provider, &mut config, &model);
        Target { inner: Arc::new(Mutex::new((provider, config, model))) }
    }

    /// (provider name, its configuration, model).
    pub fn get(&self) -> (String, ProviderConfig, String) {
        self.inner.lock().map(|g| g.clone()).unwrap_or_else(|e| e.into_inner().clone())
    }

    pub fn set(&self, provider: impl Into<String>, mut config: ProviderConfig, model: impl Into<String>) {
        let (provider, model) = (provider.into(), model.into());
        crate::setup::fill_levels(&provider, &mut config, &model);
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        *g = (provider, config, model);
    }

    pub fn model(&self) -> String {
        self.get().2
    }

    pub fn provider(&self) -> String {
        self.get().0
    }
}
