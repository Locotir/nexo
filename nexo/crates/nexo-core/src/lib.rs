//! The contracts of nexo. Every other crate is written against what is in
//! here and nothing else: the providers produce [`stream::StreamEvent`]s from
//! a [`provider::ModelRequest`], the tools implement [`tool::Tool`], the
//! agent turns [`agent::AgentCommand`]s into [`agent::AgentEvent`]s, and the
//! user interface only ever sees those events. A crate that needs another
//! crate's *implementation* is wired together in the binary, never here.
//!
//! The one representation of a conversation is [`message::Message`]: the
//! messages-API shape (`role` + `content` blocks) that the context curator
//! in this repository reads and writes, so a curated view travels unchanged
//! to any provider.

pub mod agent;
pub mod auth;
pub mod claims;
pub mod compat;
pub mod config;
pub mod error;
pub mod fast;
pub mod jobs;
pub mod leak;
pub mod mail;
pub mod mailbox;
pub mod managed;
pub mod mcp_json;
pub mod message;
pub mod paths;
pub mod permission;
pub mod plugins;
pub mod pricing;
pub mod problem;
pub mod provider;
pub mod reach;
pub mod redact;
pub mod sandbox;
pub mod seen;
pub mod shell;
pub mod skills;
pub mod stream;
pub mod subs;
pub mod tool;
pub mod trust;
pub mod version;
pub mod worktree;

/// `n` and the word, plural unless `n` is one: `count(1, "line")` is "1 line".
pub fn count(n: usize, word: &str) -> String {
    match (n, word.strip_suffix('y')) {
        (1, _) => format!("1 {word}"),
        (_, Some(stem)) => format!("{n} {stem}ies"),
        _ if word.ends_with("ch") || word.ends_with('s') => format!("{n} {word}es"),
        _ => format!("{n} {word}s"),
    }
}

pub use agent::{
    AgentCommand, AgentEvent, CallMark, CellKind, CellState, CommandRx, CommandTx, ContextCell, ContextZone,
    CuratedItem, CuratedKind, CuratedState, EventRx, EventTx, GoalState, Level, TurnEnd,
};
pub use config::{Config, ProviderConfig, WebConfig};
pub use error::{is_context_error, ErrorKind, ProviderError};
pub use jobs::{JobInfo, JobKind, JobStatus, Jobs};
pub use message::{ImageSource, Message, Part, Role};
pub use permission::{PermissionDecision, PermissionGate, PermissionMode, PermissionRequest, Question, QuestionOption};
pub use pricing::Pricing;
pub use problem::{Problem, ProblemCtx};
pub use provider::{
    Effort, EventStream, ModelCheck, ModelInfo, ModelRequest, Protocol, ReasoningConfig, ServerProbe, ToolSpec,
};
pub use stream::{Collector, FinishReason, StreamEvent, Usage};
pub use tool::{
    Compact, TodoItem, TodoStatus, Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolProgress, ToolSet,
};

#[cfg(test)]
mod count_tests {
    #[test]
    fn plurals() {
        assert_eq!(super::count(1, "line"), "1 line");
        assert_eq!(super::count(0, "line"), "0 lines");
        assert_eq!(super::count(2, "match"), "2 matches");
        assert_eq!(super::count(3, "entry"), "3 entries");
    }
}
