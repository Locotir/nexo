//! What the pane says about a row besides its fields: how long ago an action was, the workflow it ran, and what
//! it coordinates.

use nexo_agent::supervise::{AgentRow, Secs};

use super::{age, AgentsView};


impl AgentsView {
    pub(super) fn since_action(&self, at: Secs) -> String {
        let d = (self.clock - at).max(0) as u64;
        format!("{} ago", age(d))
    }

    /// Only label a workflow when the last observed tool step names it; a step is not a run status.
    pub(super) fn workflow_step(step: Option<&str>) -> Option<&str> {
        let step = step?.trim();
        let name = step.split_whitespace().next()?;
        name.eq_ignore_ascii_case("workflow").then_some(step)
    }

    pub(super) fn coordination(&self, row: &AgentRow) -> Vec<String> {
        let mut parts = Vec::new();
        if let Some(parent) = &row.parent {
            let name = self.rows.iter().find(|r| &r.key == parent).map_or(parent.as_str(), |r| r.name.as_str());
            parts.push(format!("subagent of {name}"));
        }
        // Keep the observed workflow visible even when a session has many children.
        // This is a tool step, not evidence that a workflow is still running.
        if let Some(step) = Self::workflow_step(row.last_step.as_deref()) {
            parts.push(format!("last observed workflow: {step}"));
        }
        if !row.subs.is_empty() {
            let running = Self::running_subs(row);
            parts.push(format!("subagents {running} active / {} total", row.subs.len()));
            let children = row
                .subs
                .iter()
                .map(|(key, state)| {
                    let name = self.rows.iter().find(|r| &r.key == key).map_or(key.as_str(), |r| r.name.as_str());
                    format!("{name}: {}", state.label())
                })
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(children);
        }
        parts
    }
}
