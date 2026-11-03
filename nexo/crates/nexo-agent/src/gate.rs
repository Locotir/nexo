//! The permission gate of an interface: the ask goes out as an event, the
//! answer comes back as a command.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nexo_core::agent::{AgentEvent, EventTx};
use nexo_core::permission::{PermissionDecision, PermissionGate, PermissionRequest};
use tokio::sync::oneshot;

pub struct ChannelGate {
    events: EventTx,
    pending: Mutex<HashMap<String, oneshot::Sender<PermissionDecision>>>,
}

impl ChannelGate {
    pub fn new(events: EventTx) -> Arc<ChannelGate> {
        Arc::new(ChannelGate { events, pending: Mutex::new(HashMap::new()) })
    }

    /// Answers the ask with this id; false when no ask waits for it.
    pub fn resolve(&self, id: &str, decision: PermissionDecision) -> bool {
        let tx = self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
        match tx {
            Some(tx) => tx.send(decision).is_ok(),
            None => false,
        }
    }

    /// Denies every ask still waiting (on interrupt and shutdown).
    pub fn deny_all(&self, reason: &str) {
        let all: Vec<_> = self.pending.lock().unwrap_or_else(|e| e.into_inner()).drain().collect();
        for (_, tx) in all {
            let _ = tx.send(PermissionDecision::Deny { reason: Some(reason.to_string()) });
        }
    }

    pub fn waiting(&self) -> usize {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[async_trait]
impl PermissionGate for ChannelGate {
    async fn ask(&self, req: PermissionRequest) -> PermissionDecision {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(req.id.clone(), tx);
        if self.events.send(AgentEvent::PermissionAsk { request: req.clone() }).is_err() {
            self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&req.id);
            return PermissionDecision::Deny { reason: Some("no interface to ask".into()) };
        }
        rx.await.unwrap_or(PermissionDecision::Deny { reason: Some("the ask was dropped".into()) })
    }
}
