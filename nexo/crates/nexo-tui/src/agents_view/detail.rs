//! The detail line under the sessions: what the selected one is, what it coordinates and where it runs.

use nexo_agent::supervise::{AgentRow, Secs};
use ratatui::text::Span;

use super::draw::legible;
use super::{age, AgentsView};
use crate::render::wrap::{truncate, width};
use crate::theme::Theme;

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

    fn coordination(&self, row: &AgentRow) -> Vec<String> {
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

    pub(super) fn detail(&self, row: &AgentRow, theme: &Theme, w: usize) -> Vec<Span<'static>> {
        // Coordination goes first so it remains visible when paths or session keys are long.
        let mut parts = self.coordination(row);
        if row.parent.is_some() {
            if let Some(model) = &row.model {
                parts.insert(0, model.clone());
            }
        }
        if row.paused {
            parts.insert(0, "supervisor paused".into());
        }
        let priority_width = width(&parts.iter().take(2).cloned().collect::<Vec<_>>().join(" · "));
        if let Some(c) = &row.cwd {
            parts.push(c.clone());
        }
        if let Some(m) = &row.permission_mode {
            parts.push(m.clone());
        }
        if row.parent.is_none() {
            if let Some(m) = &row.model {
                parts.push(m.clone());
            }
        }
        if self.open {
            parts.push(row.key.clone());
            parts.push(format!("{} since {}", row.state.label(), self.since_action(row.since)));
        } else {
            parts.push(row.key.chars().take(12).collect());
        }
        // Anchor the selected session's detail line; on narrow terminals reserve the
        // space for information rather than the label.
        let label = if w >= 40 && priority_width + width("selected  ") <= w { "selected  " } else { "" };
        let text = parts.join(" · ");
        vec![
            Span::styled(label, theme.bold(legible(theme, theme.accent))),
            Span::styled(truncate(&text, w.saturating_sub(width(label))), theme.dim()),
        ]
    }
}
