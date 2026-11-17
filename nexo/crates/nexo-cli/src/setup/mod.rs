//! Putting the pieces together: presets under the configuration, the
//! provider and model to use, tools (built-in and MCP), the agent's setup,
//! and the informational subcommands.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use nexo_core::config::{Config, ProviderConfig};
use nexo_core::permission::{PermissionDecision, PermissionGate, PermissionRequest};
use nexo_core::ToolSet;

mod agent;
mod config_cmd;
mod listing;
mod pick;
mod pricing;
mod sessions_clean;
mod sessions_cmd;

pub use agent::*;
pub use config_cmd::*;
pub use listing::*;
pub use pick::*;
pub use pricing::*;
pub use sessions_clean::{prune_sessions, size_sessions};
pub use sessions_cmd::*;
