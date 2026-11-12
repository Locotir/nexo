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

use crate::message::ImageSource;
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

impl ToolKind {
    /// The heading the person reads for this kind of tool.
    pub fn label(self) -> &'static str {
        match self {
            ToolKind::Read => "Read files",
            ToolKind::Search => "Search",
            ToolKind::Write => "Edit",
            ToolKind::Exec => "Run commands",
            ToolKind::Network => "Web",
            ToolKind::Other => "Other",
        }
    }
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
    /// Instructions the agent saved for later sessions (the `note` tool): `scope` is `global` or
    /// `project`, `items` the lines that were new and `path` the file they went to.
    Remembered {
        scope: String,
        items: Vec<String>,
        path: String,
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
    /// Pictures the model looks at next to `content` (a `read` of a PNG).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageSource>,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> ToolOutput {
        ToolOutput { content: content.into(), is_error: false, display: ToolDisplay::Plain, images: Vec::new() }
    }

    pub fn error(content: impl Into<String>) -> ToolOutput {
        ToolOutput { content: content.into(), is_error: true, display: ToolDisplay::Plain, images: Vec::new() }
    }

    pub fn with_images(mut self, images: Vec<ImageSource>) -> ToolOutput {
        self.images = images;
        self
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
    /// The files the model has read or written, as they were then (`write` checks it).
    pub seen: crate::seen::Seen,
    /// What the permission rules say of another call, without asking anyone: a tool led somewhere
    /// else on its own (a web page that redirects) checks the new place with it. None: no rules.
    pub check: Option<PolicyCheck>,
    /// The OS sandbox the `bash` tool runs commands in (mode `off`: none).
    pub sandbox: crate::sandbox::SandboxSettings,
    /// Whether the model in use can look at pictures (false only where the model catalogue says it cannot).
    pub vision: bool,
    /// The agent runs the compiler and test check once per step after a change: `edit` and `write` leave it out.
    pub post_check: bool,
    /// Seconds a `check` command of `edit` or `write` may take.
    pub check_secs: u64,
    /// The files running sub-agents are changing: a write to one of them is refused.
    pub claims: crate::claims::Claims,
    /// Sends the sub-agents the turn waits for to the background (the interface or a message asks). None: never.
    pub detach: Option<crate::jobs::Detach>,
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
            seen: Default::default(),
            check: None,
            sandbox: Default::default(),
            vision: true,
            post_check: false,
            check_secs: 45,
            claims: Default::default(),
            detach: None,
        }
    }

    pub fn report(&self, line: impl Into<String>) {
        if let Some(p) = &self.progress {
            let _ = p.send(ToolProgress { call_id: self.call_id.clone(), line: line.into() });
        }
    }
}

/// The text of a tool for a model with a small window, and in any window for a tool seldom called
/// ([`Tool::rare`]): one or two sentences, and a short phrase for the parameters worth one (`(name, phrase)`;
/// a parameter left out keeps its name and type only). The description is lent by the tool, so one that
/// depends on the session (the kinds of sub-agent the person has) is written once and kept by it.
#[derive(Debug, Clone, Copy)]
pub struct Compact<'a> {
    pub description: &'a str,
    pub params: &'static [(&'static str, &'static str)],
}

impl Compact<'_> {
    /// `spec` with this description, and the parameter schema cut down to names, types, `required` and short
    /// enums: the phrases of `params` for descriptions, no bounds.
    pub fn apply(&self, mut spec: ToolSpec) -> ToolSpec {
        spec.description = self.description.to_string();
        self.shrink(&mut spec.parameters);
        spec
    }

    fn shrink(&self, node: &mut Value) {
        let Some(o) = node.as_object_mut() else { return };
        o.remove("minimum");
        o.remove("maximum");
        if o.get("enum").and_then(Value::as_array).is_some_and(|e| e.len() > 6) {
            o.remove("enum");
        }
        if let Some(props) = o.get_mut("properties").and_then(Value::as_object_mut) {
            for (name, prop) in props.iter_mut() {
                if let Some(p) = prop.as_object_mut() {
                    match self.params.iter().find(|(n, _)| n == name) {
                        Some((_, phrase)) => p.insert("description".into(), Value::from(*phrase)),
                        None => p.remove("description"),
                    };
                }
                self.shrink(prop);
            }
        }
        if let Some(items) = o.get_mut("items") {
            self.shrink(items);
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn spec(&self) -> ToolSpec;
    /// The short text for a window of 32k tokens or less; `None` keeps the full one.
    fn compact(&self) -> Option<Compact<'_>> {
        None
    }
    /// A tool the model seldom calls: it is offered with its short text ([`Tool::compact`]) in every window,
    /// as the full one would take its place in every request for a call in a few hundred.
    fn rare(&self) -> bool {
        false
    }
    /// A last cut of the compact spec (a parameter the tool takes but a short text leaves out, to be learnt from
    /// its own help when needed). The full spec is untouched.
    fn trim_compact(&self, _spec: &mut ToolSpec) {}
    fn kind(&self) -> ToolKind;
    /// The kind of this call, where `args` change it (a sub-agent that may write is a write);
    /// it decides which calls of one round may run at the same time.
    fn call_kind(&self, _args: &Value) -> ToolKind {
        self.kind()
    }
    /// Whether the OS sandbox of the session holds this call in, so it needs no asking (only `bash`).
    fn sandbox_holds(&self, _args: &Value, _sandbox: &crate::sandbox::SandboxSettings) -> bool {
        false
    }
    /// What the sandbox does for this tool, for `/status`: the line and whether it works.
    fn sandbox_status(&self, _sandbox: &crate::sandbox::SandboxSettings) -> Option<(String, bool)> {
        None
    }
    /// The tool ends a call that is cancelled by itself (it has cleanup to do, and answers in time): the
    /// agent does not drop its future when the turn is interrupted.
    fn handles_cancel(&self) -> bool {
        false
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

    /// The specs with each tool's [`Compact`] text where it has one.
    pub fn compact_specs(&self) -> Vec<ToolSpec> {
        self.specs_for(true)
    }

    /// The specs as a model is offered them: the [`Compact`] text of every tool that has one when `compact`
    /// (a small window), else only of the tools seldom called ([`Tool::rare`]).
    pub fn specs_for(&self, compact: bool) -> Vec<ToolSpec> {
        self.tools
            .values()
            .map(|t| match t.compact() {
                Some(c) if compact || t.rare() => {
                    let mut spec = c.apply(t.spec());
                    t.trim_compact(&mut spec);
                    spec
                }
                _ => t.spec(),
            })
            .collect()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }

    /// Takes a tool out of the set (it is no longer offered or run).
    pub fn remove(&mut self, name: &str) -> bool {
        self.tools.remove(name).is_some()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// The files a file tool call names: `path`, then each entry of `paths` (a `read` of several files) and
/// the `path` of each entry of `files` (an `edit` of several files), without repeats, empty names left out.
pub fn call_paths(args: &Value) -> Vec<String> {
    let one = args.get("path").and_then(Value::as_str);
    let many = args.get("paths").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str);
    let files = args
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| f.get("path").and_then(Value::as_str));
    let mut out: Vec<String> = Vec::new();
    for p in one.into_iter().chain(many).chain(files).map(str::trim).filter(|p| !p.is_empty()) {
        if !out.iter().any(|q| q == p) {
            out.push(p.to_string());
        }
    }
    out
}

/// Whether the call names its files in a `paths` list: then no single `path` says what it touched.
pub fn has_paths(args: &Value) -> bool {
    args.get("paths").is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty()))
}

/// `a.rs, b.rs` for two files, `a.rs, b.rs (+3)` for more: how a call of several files is named.
pub fn paths_label(paths: &[String]) -> String {
    match paths {
        [] => String::new(),
        [a, b] => format!("{a}, {b}"),
        [a] => a.clone(),
        [a, b, rest @ ..] => format!("{a}, {b} (+{})", rest.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tool with a long text and a short one, seldom called or not.
    struct Wordy(&'static str, bool);

    #[async_trait]
    impl Tool for Wordy {
        fn name(&self) -> &str {
            self.0
        }
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.0.into(),
                description: "The long text.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"path": {"type": "string", "description": "Long."}},
                    "required": ["path"]
                }),
            }
        }
        fn compact(&self) -> Option<Compact<'_>> {
            Some(Compact { description: "Short.", params: &[] })
        }
        fn rare(&self) -> bool {
            self.1
        }
        fn kind(&self) -> ToolKind {
            ToolKind::Read
        }
        fn summary(&self, _: &Value) -> String {
            self.0.into()
        }
        async fn run(&self, _: Value, _: &ToolContext) -> ToolOutput {
            ToolOutput::ok("")
        }
    }

    #[test]
    fn a_rare_tool_is_offered_with_its_short_text_in_every_window() {
        let set = ToolSet::new().with(Arc::new(Wordy("often", false))).with(Arc::new(Wordy("seldom", true)));
        let texts = |specs: Vec<ToolSpec>| specs.into_iter().map(|s| s.description).collect::<Vec<_>>();
        assert_eq!(texts(set.specs()), ["The long text.", "The long text."]);
        assert_eq!(texts(set.specs_for(false)), ["The long text.", "Short."]);
        assert_eq!(texts(set.specs_for(true)), ["Short.", "Short."]);
        assert_eq!(texts(set.compact_specs()), ["Short.", "Short."]);
        // the short text keeps the call as it is: same parameters, same required ones
        let seldom = &set.specs_for(false)[1];
        assert_eq!(
            seldom.parameters,
            serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]})
        );
    }

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

    #[test]
    fn a_call_names_its_files_once_each_and_in_order() {
        let a = serde_json::json!({"path": " a.rs ", "paths": ["b.rs", "a.rs", "", 3, "c.rs"]});
        assert_eq!(call_paths(&a), ["a.rs", "b.rs", "c.rs"]);
        assert!(has_paths(&a));
        assert!(!has_paths(&serde_json::json!({"path": "a.rs"})));
        assert!(!has_paths(&serde_json::json!({"paths": []})));
        assert!(call_paths(&serde_json::json!({})).is_empty());
        let l = |n: usize| paths_label(&(0..n).map(|i| format!("f{i}")).collect::<Vec<_>>());
        assert_eq!((l(0).as_str(), l(1).as_str(), l(2).as_str(), l(4).as_str()), ("", "f0", "f0, f1", "f0, f1 (+2)"));
    }
}
