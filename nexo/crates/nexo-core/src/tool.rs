//! What a tool is: a spec the model sees, a kind the permission policy
//! classifies, and `run`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::provider::ToolSpec;

/// What a tool does to the world; the permission policy decides by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolKind {
    Read,
    Search,
    Write,
    Exec,
    Network,
    Other,
}

/// Progress a long tool reports while it runs (a line of output).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolProgress {
    pub call_id: String,
    pub line: String,
}

/// How the interface should show the result (the model gets `content`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolDisplay {
    Plain,
    /// A change to a file: the texts before and after. `start_line` is the line of the
    /// file where both texts begin: 1 for whole files, the line of the snippet for a part;
    /// 0, what a session saved before this field has, counts as 1.
    Diff {
        path: String,
        old: String,
        new: String,
        #[serde(default)]
        start_line: usize,
    },
    /// A file (or part of it) that was read.
    File {
        path: String,
        text: String,
        start_line: usize,
    },
    /// A command that ran.
    Shell {
        command: String,
        stdout: String,
        stderr: String,
        exit_code: Option<i32>,
    },
    /// Paths or lines found.
    List {
        items: Vec<String>,
    },
    /// The task list the model keeps (the `todo` tool); the interface shows it in its panel.
    Todo {
        items: Vec<TodoItem>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    pub text: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    /// What the model reads.
    pub content: String,
    pub is_error: bool,
    pub display: ToolDisplay,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> ToolOutput {
        ToolOutput { content: content.into(), is_error: false, display: ToolDisplay::Plain }
    }

    pub fn error(content: impl Into<String>) -> ToolOutput {
        ToolOutput { content: content.into(), is_error: true, display: ToolDisplay::Plain }
    }

    pub fn with_display(mut self, d: ToolDisplay) -> ToolOutput {
        self.display = d;
        self
    }
}

/// What a tool runs in.
#[derive(Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub session_id: String,
    pub call_id: String,
    /// Cancelled when the user interrupts the turn.
    pub cancel: CancellationToken,
    /// Where a long tool reports lines as they come.
    pub progress: Option<mpsc::UnboundedSender<ToolProgress>>,
    /// Environment the tool may read (never secrets).
    pub env: BTreeMap<String, String>,
    /// The session's background jobs.
    pub jobs: crate::jobs::Jobs,
    /// What the permission rules say of another call, without asking anyone: a tool led somewhere
    /// else on its own (a web page that redirects) checks the new place with it. None: no rules.
    pub check: Option<PolicyCheck>,
}

/// What the permission rules would do with a call, without asking anyone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    Allow,
    Ask,
    Deny,
}

/// The rules of a session as a function of a call (tool name, kind, arguments).
pub type PolicyCheck = Arc<dyn Fn(&str, ToolKind, &Value) -> Check + Send + Sync>;

impl ToolContext {
    pub fn new(cwd: PathBuf) -> ToolContext {
        ToolContext {
            cwd,
            session_id: String::new(),
            call_id: String::new(),
            cancel: CancellationToken::new(),
            progress: None,
            env: BTreeMap::new(),
            jobs: crate::jobs::Jobs::new(),
            check: None,
        }
    }

    pub fn report(&self, line: impl Into<String>) {
        if let Some(p) = &self.progress {
            let _ = p.send(ToolProgress { call_id: self.call_id.clone(), line: line.into() });
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn spec(&self) -> ToolSpec;
    fn kind(&self) -> ToolKind;
    /// The kind of this call, where `args` change it (a sub-agent that may write is a write);
    /// it decides which calls of one round may run at the same time.
    fn call_kind(&self, _args: &Value) -> ToolKind {
        self.kind()
    }
    /// One line that says what this call does (`read src/main.rs`, `bash: cargo test`).
    fn summary(&self, args: &Value) -> String;
    /// Never panics on bad `args`: it answers with an error output the model can read.
    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput;
}

/// The tools of a session, by name.
#[derive(Clone, Default)]
pub struct ToolSet {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolSet {
    pub fn new() -> ToolSet {
        ToolSet::default()
    }

    pub fn add(&mut self, t: Arc<dyn Tool>) {
        self.tools.insert(t.name().to_string(), t);
    }

    pub fn with(mut self, t: Arc<dyn Tool>) -> ToolSet {
        self.add(t);
        self
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_diff_saved_without_start_line_still_loads_and_one_with_it_keeps_it() {
        let legacy: ToolDisplay =
            serde_json::from_str(r#"{"kind":"diff","path":"a.rs","old":"x\n","new":"y\n"}"#).unwrap();
        assert_eq!(
            legacy,
            ToolDisplay::Diff { path: "a.rs".into(), old: "x\n".into(), new: "y\n".into(), start_line: 0 }
        );
        let d = ToolDisplay::Diff { path: "a.rs".into(), old: "x\n".into(), new: "y\n".into(), start_line: 40 };
        let back: ToolDisplay = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(back, d);
    }
}
