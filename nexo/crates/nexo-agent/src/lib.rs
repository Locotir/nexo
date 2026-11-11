//! The agent: what turns a person's request into model calls and tool runs,
//! and what the interface sees of it. Driven through the channels of
//! [`nexo_core::agent`]: [`run`] takes the agent's ends and a setup and runs
//! until `Shutdown`.
//!
//! The model is reached through [`ModelClient`], a one-method trait the
//! provider client implements, so the loop can be tested with a scripted
//! client; the tools are whatever [`nexo_core::ToolSet`] the setup carries.

pub mod ask;
pub mod audit;
mod bin_hint;
pub mod brief;
pub mod budget;
pub mod checkpoint;
pub mod client;
pub mod compact;
pub mod config_edit;
pub mod context;
mod context_map;
pub mod export;
pub mod gate;
pub mod gitcmds;
pub mod goal_brief;
mod history;
pub mod hooks;
pub mod limits;
mod missing;
pub mod permission;
pub mod prompt;
pub mod providers;
mod reader;
mod reads;
pub mod recall;
mod repeat;
mod review;
mod scrub;
pub mod sent;
pub mod session;
pub mod session_scan;
pub mod setup;
pub mod subagent;
pub mod supervise;
mod turn;
pub mod workflow;

use std::path::PathBuf;
use std::sync::Arc;

use nexo_core::agent::AgentEnds;
use nexo_core::config::Config;
use nexo_core::permission::PermissionGate;
use nexo_core::tool::ToolSet;

pub use client::{ModelClient, Target};
pub use context::{ContextPolicy, View};
pub use limits::Limits;
pub use permission::{Decision, Policy};
pub use session::{Session, SessionMeta};

/// Everything the loop needs to start.
pub struct AgentSetup {
    pub config: Config,
    pub cwd: PathBuf,
    /// A key of `config.providers`.
    pub provider: String,
    pub model: String,
    pub tools: ToolSet,
    pub client: Arc<dyn ModelClient>,
    /// A session id to continue, with its history replayed as `History` events.
    pub resume: Option<String>,
    /// Who answers permission asks; None: the interface, through
    /// `PermissionAsk` events and `Permission` commands.
    pub gate: Option<Arc<dyn PermissionGate>>,
    /// The mailbox other sessions write to (the interface and its `/msg`); None: no mail.
    pub mail: Option<Arc<nexo_core::mailbox::Mailbox>>,
    /// The MCP servers of the session, for resources, prompts, /mcp and list changes; None: no servers.
    pub mcp: Option<Arc<nexo_tools::mcp_hub::McpHub>>,
    /// The provider or model was named for this launch (`--model`, `--provider`): a resumed session does
    /// not replace them with the ones it last used.
    pub model_pinned: bool,
}

/// Runs the agent until `Shutdown` (or the command channel closes).
pub async fn run(ends: AgentEnds, setup: AgentSetup) -> anyhow::Result<()> {
    turn::run(ends, setup).await
}
