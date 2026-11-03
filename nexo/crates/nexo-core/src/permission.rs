//! Asking before a tool acts.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::tool::{ToolDisplay, ToolKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    /// Ask before writes and commands.
    #[default]
    Ask,
    /// Writes go through; ask before commands.
    AcceptEdits,
    /// Never ask (rules in `deny` still hold).
    Auto,
    /// Only reads and searches.
    ReadOnly,
    /// Reads and searches while the model works out a plan; it ends when the
    /// person approves the plan (`exit_plan`).
    Plan,
}

impl PermissionMode {
    /// Whether only reads, searches and page fetches run in this mode.
    pub fn reads_only(&self) -> bool {
        matches!(self, PermissionMode::ReadOnly | PermissionMode::Plan)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            PermissionMode::Ask => "ask",
            PermissionMode::AcceptEdits => "accept-edits",
            PermissionMode::Auto => "auto",
            PermissionMode::ReadOnly => "read-only",
            PermissionMode::Plan => "plan",
        }
    }

    pub fn parse(s: &str) -> Option<PermissionMode> {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "ask" | "default" => Some(PermissionMode::Ask),
            "accept-edits" | "edits" => Some(PermissionMode::AcceptEdits),
            "auto" | "yolo" | "bypass" => Some(PermissionMode::Auto),
            "read-only" | "readonly" => Some(PermissionMode::ReadOnly),
            "plan" | "planning" => Some(PermissionMode::Plan),
            _ => None,
        }
    }
}

/// What the agent asks the person before a call runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: String,
    pub tool: String,
    pub kind: ToolKind,
    /// One line: what the call does.
    pub summary: String,
    /// The diff, command or arguments, for the dialog.
    pub detail: ToolDisplay,
    /// Rules the person may adopt to not be asked again (`bash(git *)`, `write(src/**)`).
    pub suggested_rules: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    /// Allow and remember `rule` for the session (or the project when `persist`).
    AllowAlways {
        rule: String,
        persist: bool,
    },
    Deny {
        reason: Option<String>,
    },
}

/// Who answers: the interface (a dialog) or a policy (headless).
#[async_trait]
pub trait PermissionGate: Send + Sync {
    async fn ask(&self, req: PermissionRequest) -> PermissionDecision;
}

/// A question the agent puts to the person: pick one (or several) of the
/// options, or write an answer of their own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    /// A word or two for the title (`Approach`, `Plan`).
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    /// Several options may be picked.
    #[serde(default)]
    pub multi: bool,
    /// The plan to approve, as markdown (the `exit_plan` question).
    #[serde(default)]
    pub plan: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}
