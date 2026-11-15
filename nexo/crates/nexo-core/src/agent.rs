//! The line between the agent and whoever drives it (the TUI, the headless
//! runner, a test): commands in, events out, over channels. The interface
//! never touches providers or tools; it draws what these events say.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::config::ProviderConfig;
use crate::message::Message;
use crate::permission::{PermissionDecision, PermissionMode, PermissionRequest, Question};
use crate::pricing::Pricing;
use crate::problem::Problem;
use crate::provider::{ModelCheck, ServerProbe};
use crate::stream::Usage;
use crate::tool::ToolOutput;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

pub type CommandTx = mpsc::UnboundedSender<AgentCommand>;
pub type CommandRx = mpsc::UnboundedReceiver<AgentCommand>;
/// Sender that also records allowed events in an ephemeral observation registry.
#[derive(Clone)]
pub struct EventTx {
    sender: mpsc::UnboundedSender<AgentEvent>,
    observation: Observation,
    send_lock: std::sync::Arc<std::sync::Mutex<()>>,
}
impl EventTx {
    /// On failure, returns the unsent event inside a boxed channel error.
    pub fn send(&self, event: AgentEvent) -> Result<(), Box<mpsc::error::SendError<AgentEvent>>> {
        let _guard = self.send_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.observation.publish(&event);
        self.sender.send(event).map_err(Box::new)
    }
    pub fn observation(&self) -> Observation {
        self.observation.clone()
    }
}
pub type EventRx = mpsc::UnboundedReceiver<AgentEvent>;

/// The two channels of an agent, from the driver's side.
pub struct AgentHandle {
    pub commands: CommandTx,
    pub events: EventRx,
    /// Ephemeral observation of this agent's own channel.
    pub observation: Observation,
}

/// The two channels of an agent, from the agent's side.
pub struct AgentEnds {
    pub commands: CommandRx,
    pub events: EventTx,
}

/// Makes the pair.
pub fn channels() -> (AgentHandle, AgentEnds) {
    let (ctx, crx) = mpsc::unbounded_channel();
    let (etx, erx) = mpsc::unbounded_channel();
    let observation = Observation::default();
    let events = EventTx { sender: etx, observation: observation.clone(), send_lock: Default::default() };
    (AgentHandle { commands: ctx, events: erx, observation }, AgentEnds { commands: crx, events })
}

#[cfg(test)]
mod tests;

mod commands;
pub use commands::*;
mod mcp_info;
pub use mcp_info::*;
mod chrome_info;
pub use chrome_info::*;
mod goal_marks;
pub use goal_marks::*;
mod context_cells;
pub use context_cells::*;
mod events;
pub use events::*;
mod observe;
pub use observe::{Observation, ObservationSnapshot, ObservedEvent};
