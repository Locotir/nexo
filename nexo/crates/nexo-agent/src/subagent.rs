//! The `task` tool: a sub-agent the model hands a self-contained piece of work.
//! It runs its own loop with its own tools and answers with a report; only the
//! report enters the main conversation, so a wide search costs the main context
//! one message.
//!
//! Kinds of sub-agent:
//! - `explore` (the default): reads and searches inside the project, changes nothing;
//! - `general`: every tool but `task`, each call asked for like the main agent's;
//! - custom ones from `.nexo/agents/*.md` (the project) and `~/.nexo/agents/*.md`
//!   (the person), a front matter with `name`, `description`, `tools`, `model` and
//!   `rounds`, and a body that is the sub-agent's instructions.
//!
//! With `background: true` a sub-agent that changes nothing runs as a job: the main agent goes on,
//! and the report is the job's output. One that can change files runs in the foreground.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use futures::StreamExt;
use nexo_core::jobs::JobKind;
use nexo_core::message::{Message, Part, Role};
use nexo_core::provider::{ModelRequest, ToolSpec};
use nexo_core::stream::{StreamEvent, Usage};
use nexo_core::tool::{Tool, ToolContext, ToolKind, ToolOutput, ToolSet};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::brief::{self, Brief, BriefConfig};
use crate::client::{ModelClient, Target};

pub const DEFAULT: &str = "explore";

const BASE: &str = "You are a sub-agent of a coding assistant. You get one task; do it with the tools you have and \
answer with a report for the assistant that sent you: what you found or did, with file paths and line numbers, \
in at most 300 words. Do not ask questions back: decide and report. A briefing may come before the task: \
passages of the conversation that sent you, picked for it. Trust it, do not read again what it holds when it \
says current, and do not quote it back in the report: cite path:line instead.
Search narrow first: the working directory and the paths the task names, with the search tools; widen one step \
at a time only when that finds nothing. Never sweep the whole home folder, / or /tmp in one command: it is slow \
and the person waits for it. Ask for independent reads and searches together in one round.";

/// Said to a sub-agent whose rounds are over, so its work comes back as a report instead of being lost.
const LAST_ROUND: &str = "[nexo] No rounds left for tools. Answer now with your report: what you found, with \
path:line, and what you could not settle.";

const EXPLORE: &str = "You can only read: read files, search, list, search the web. You cannot change anything.";

const GENERAL: &str = "You can read, edit and run commands. Keep to the task: change only what it needs, check \
your work (build, tests) when you can, and say in the report exactly which files you changed.";

/// One kind of sub-agent.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub name: String,
    pub description: String,
    /// The tools it may use; `None` is every tool but `task`.
    pub tools: Option<Vec<String>>,
    /// Only reading and searching, inside the project, with nothing to ask.
    pub read_only: bool,
    /// Another model of the same provider.
    pub model: Option<String>,
    /// Its instructions, after the common ones.
    pub prompt: String,
    /// Rounds of tool calls before it must answer.
    pub rounds: usize,
    /// The file it came from (none for the built-in ones).
    pub source: Option<PathBuf>,
}

pub fn builtin() -> Vec<Profile> {
    vec![
        Profile {
            name: "explore".into(),
            description: "read-only: wide searches (where is X handled, what calls Y, how does Z work)".into(),
            tools: None,
            read_only: true,
            model: None,
            prompt: EXPLORE.into(),
            rounds: 16,
            source: None,
        },
        Profile {
            name: "general".into(),
            description: "can edit files and run commands: a self-contained change or check (fix this test, \
                          port that module); its calls are asked for like yours"
                .into(),
            tools: None,
            read_only: false,
            model: None,
            prompt: GENERAL.into(),
            rounds: 40,
            source: None,
        },
    ]
}

/// The built-in kinds, then the person's, then the project's; a later one
/// with the same name replaces the earlier. A project nobody trusted yet (`nexo config trust`) has
/// its agents left out: their instructions come from the repository.
pub fn load(cwd: &Path) -> Vec<Profile> {
    load_from(cwd, &nexo_core::paths::config_dir(), &nexo_core::paths::trusted_projects_file())
}

fn load_from(cwd: &Path, config_dir: &Path, trust_file: &Path) -> Vec<Profile> {
    let mut out = builtin();
    let mut dirs = vec![config_dir.join("agents")];
    if let Some(p) = nexo_core::paths::project_dir(cwd) {
        if p.parent().is_some_and(|root| nexo_core::config::is_trusted(root, trust_file)) {
            dirs.push(p.join("agents"));
        }
    }
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut files: Vec<PathBuf> =
            rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "md")).collect();
        files.sort();
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("agent").to_string();
            if let Some(mut p) = parse(&text, &stem) {
                p.source = Some(f.clone());
                out.retain(|o| o.name != p.name);
                out.push(p);
            }
        }
    }
    out
}

/// A custom sub-agent: an optional front matter between `---` lines
/// (`name`, `description`, `tools` as `a, b` or `[a, b]`, `model`, `rounds`),
/// then its instructions.
pub fn parse(text: &str, fallback_name: &str) -> Option<Profile> {
    let text = text.trim_start_matches('\u{feff}');
    let (head, body) = match text.strip_prefix("---") {
        Some(rest) => match rest.find("\n---") {
            Some(i) => (&rest[..i], rest[i + 4..].trim_start_matches(|c| c != '\n').trim()),
            None => ("", text.trim()),
        },
        None => ("", text.trim()),
    };
    let mut p = Profile {
        name: fallback_name.to_string(),
        description: String::new(),
        tools: None,
        read_only: false,
        model: None,
        prompt: body.to_string(),
        rounds: 30,
        source: None,
    };
    for line in head.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
        match k.trim() {
            "name" if !v.is_empty() => p.name = v.to_string(),
            "description" => p.description = v.to_string(),
            "model" if !v.is_empty() => p.model = Some(v.to_string()),
            "rounds" => p.rounds = v.parse().unwrap_or(p.rounds).clamp(1, 200),
            "tools" => {
                let list: Vec<String> = v
                    .trim_matches(|c| c == '[' || c == ']')
                    .split(',')
                    .map(|t| t.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
                    .filter(|t| !t.is_empty() && t != "task")
                    .collect();
                if !list.is_empty() {
                    p.tools = Some(list);
                }
            }
            _ => {}
        }
    }
    let ok = !p.name.is_empty() && p.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then_some(p)
}

/// What a sub-agent needs from the agent that runs it: leave to run a call
/// (the main agent's permissions and asks), and a place to put what it spent.
#[async_trait]
pub trait Host: Send + Sync {
    /// `Err` holds why the call may not run.
    async fn permit(
        &self,
        call_id: &str,
        tool: &dyn Tool,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<(), String>;
    async fn spent(&self, model: &str, usage: Usage);
    /// The conversation so far, to brief the sub-agent from.
    async fn history(&self) -> Vec<Message> {
        Vec::new()
    }
    /// The sub-agent is about to write `path`: what it holds now, for a rewind.
    async fn before_write(&self, _path: PathBuf) {}
}

/// Set once the agent's state exists (the tool is made before it).
pub type HostCell = Arc<OnceLock<Arc<dyn Host>>>;

pub struct SubAgent {
    client: Arc<dyn ModelClient>,
    target: Target,
    /// Every tool but `task`.
    all: ToolSet,
    profiles: Vec<Profile>,
    host: HostCell,
    brief: BriefConfig,
}

impl SubAgent {
    pub fn new(
        client: Arc<dyn ModelClient>,
        target: Target,
        all: &ToolSet,
        profiles: Vec<Profile>,
        host: HostCell,
    ) -> SubAgent {
        let mut tools = ToolSet::new();
        for spec in all.specs() {
            if let Some(t) = all.get(&spec.name) {
                if spec.name != "task" {
                    tools.add(t.clone());
                }
            }
        }
        let profiles = if profiles.is_empty() { builtin() } else { profiles };
        SubAgent { client, target, all: tools, profiles, host, brief: BriefConfig { tokens: 0, ..Default::default() } }
    }

    /// Hands every sub-agent a briefing picked from the conversation (see [`crate::brief`]).
    pub fn with_brief(mut self, cfg: BriefConfig) -> SubAgent {
        self.brief = cfg;
        self
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// Whether a sub-agent of `p` has a tool that may change something (anything but reads and searches).
    fn can_write(&self, p: &Profile) -> bool {
        let set = self.tools_for(p);
        let writes = set
            .names()
            .filter_map(|n| set.get(n))
            .any(|t| !matches!(t.kind(), ToolKind::Read | ToolKind::Search | ToolKind::Network));
        writes
    }

    /// The tools a sub-agent of `p` gets.
    fn tools_for(&self, p: &Profile) -> ToolSet {
        let mut set = ToolSet::new();
        for spec in self.all.specs() {
            let Some(t) = self.all.get(&spec.name) else { continue };
            let listed = match &p.tools {
                None => true,
                Some(l) => l.iter().any(|n| n == &spec.name),
            };
            // `diagnostics` starts language servers, which may run the project's build scripts
            let readable = matches!(t.kind(), ToolKind::Read | ToolKind::Search)
                && !matches!(spec.name.as_str(), "jobs" | "diagnostics");
            if listed && (!p.read_only || readable) {
                set.add(t.clone());
            }
        }
        set
    }
}

/// A path argument that leaves the project: resolved as the tools resolve it (`~` expanded), with
/// links followed, so a link inside the project that points out of it counts as outside.
fn outside(args: &Value, cwd: &Path) -> Option<String> {
    let p = args.get("path").and_then(Value::as_str)?;
    // `~` is the home directory for the file tools, so it is here
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    let p = p.trim();
    let path = match (p, home) {
        ("~", Some(h)) => h,
        (_, Some(h)) if p.starts_with("~/") => h.join(&p[2..]),
        _ => cwd.join(p),
    };
    let mut norm = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                norm.pop();
            }
            std::path::Component::CurDir => {}
            other => norm.push(other),
        }
    }
    let path = norm;
    let (real, root) = (real_path(&path), real_path(cwd));
    (!real.starts_with(root)).then(|| p.to_string())
}

/// The path with links resolved as far as it exists, the rest as written.
fn real_path(p: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut base = p.to_path_buf();
    loop {
        if let Ok(c) = base.canonicalize() {
            return rest.iter().rev().fold(c, |acc, x| acc.join(x));
        }
        match (base.file_name().map(|n| n.to_os_string()), base.parent().map(Path::to_path_buf)) {
            (Some(n), Some(parent)) => {
                rest.push(n);
                base = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

/// One sub-agent at work.
struct Run {
    client: Arc<dyn ModelClient>,
    target: Target,
    tools: ToolSet,
    profile: Profile,
    host: Option<Arc<dyn Host>>,
}

impl Run {
    /// The report, or why there is none. `say` gets a line per step.
    async fn go(
        &self,
        prompt: &str,
        brief: Option<&Brief>,
        ctx: &ToolContext,
        say: &(dyn Fn(String) + Send + Sync),
    ) -> Result<String, String> {
        let (_, cfg, main_model) = self.target.get();
        let model = self.profile.model.clone().unwrap_or(main_model);
        let rounds = self.profile.rounds;
        let system = format!("{BASE}\nYou have {rounds} rounds of tool calls.\n\n{}", self.profile.prompt);
        let first = match brief {
            Some(b) => Message::new(Role::User, vec![Part::text(b.text.clone()), Part::text(prompt)]),
            None => Message::user(prompt),
        };
        let mut messages = vec![first];
        let specs = self.tools.specs();
        for round in 0..=rounds {
            // the rounds are over: one more request, told to report what it has
            let last = round == rounds;
            if last {
                if let Some(m) = messages.last_mut() {
                    m.content.push(Part::text(LAST_ROUND));
                }
            }
            let mut req = ModelRequest::new(model.clone(), system.clone(), messages.clone());
            req.tools = specs.clone();
            req.max_tokens = 4096;
            let mut stream = self
                .client
                .stream(&cfg, &req, 2)
                .await
                .map_err(|e| format!("the sub-agent's model call failed: {}", e.message))?;
            let mut text = String::new();
            let mut calls: Vec<(String, String, Value)> = Vec::new();
            let mut usage = Usage::default();
            let failed = loop {
                let ev = tokio::select! {
                    ev = stream.next() => ev,
                    _ = ctx.cancel.cancelled() => break Some("stopped".to_string()),
                };
                match ev {
                    Some(StreamEvent::TextDelta(d)) => text.push_str(&d),
                    Some(StreamEvent::ToolCallEnd { id, name, args }) => calls.push((id, name, args)),
                    Some(StreamEvent::Usage(u)) => usage += u,
                    Some(StreamEvent::Error(e)) => break Some(format!("the sub-agent's model failed: {}", e.message)),
                    Some(StreamEvent::Done) | None => break None,
                    _ => {}
                }
            };
            if let (Some(h), false) = (&self.host, usage.is_empty()) {
                h.spent(&model, usage).await;
            }
            if let Some(why) = failed {
                return Err(why);
            }
            if calls.is_empty() || last {
                let report = text.trim();
                return if report.is_empty() {
                    Err(if last {
                        format!("the sub-agent took {rounds} rounds without an answer; ask it something narrower")
                    } else {
                        "the sub-agent answered nothing".into()
                    })
                } else {
                    Ok(brief::compact_report(report, brief.map(|b| b.text.as_str()), brief::REPORT_TOKENS))
                };
            }
            let mut asked = vec![];
            if !text.is_empty() {
                asked.push(Part::text(text));
            }
            let mut results = vec![];
            for (id, name, a) in calls {
                asked.push(Part::tool_call(&id, &name, a.clone()));
                let out = self.call(&id, &name, &a, ctx, say).await;
                results.push(Part::ToolResult {
                    call_id: id,
                    name: Some(name),
                    content: vec![Part::text(if out.content.is_empty() { "(no output)".into() } else { out.content })],
                    is_error: out.is_error,
                });
            }
            messages.push(Message::new(Role::Assistant, asked));
            messages.push(Message::tool_results(results));
        }
        unreachable!("the last round always returns")
    }

    async fn call(
        &self,
        id: &str,
        name: &str,
        args: &Value,
        ctx: &ToolContext,
        say: &(dyn Fn(String) + Send + Sync),
    ) -> ToolOutput {
        let Some(t) = self.tools.get(name) else {
            let can = if self.profile.read_only { " (it can only read)" } else { "" };
            return ToolOutput::error(format!("the sub-agent has no tool `{name}`{can}"));
        };
        if self.profile.read_only {
            if let Some(p) = outside(args, &ctx.cwd) {
                return ToolOutput::error(format!("`{p}` is outside the project"));
            }
        }
        // the main agent's rules hold for every sub-agent: its deny rules, and its asks (keys, writes)
        let call_id = format!("{}.{id}", ctx.call_id);
        let verdict = match &self.host {
            Some(h) => h.permit(&call_id, t.as_ref(), args, &ctx.cancel).await,
            None if self.profile.read_only => Ok(()),
            None => Err("no one to ask for leave".into()),
        };
        if let Err(why) = verdict {
            return ToolOutput::error(format!("Permission denied: {why}"));
        }
        if let (Some(h), ToolKind::Write) = (&self.host, t.kind()) {
            if let Some(p) = args.get("path").and_then(Value::as_str) {
                let path = if Path::new(p).is_absolute() { PathBuf::from(p) } else { ctx.cwd.join(p) };
                h.before_write(path).await;
            }
        }
        say(format!("→ {}", t.summary(args)));
        t.run(args.clone(), ctx).await
    }
}

#[async_trait]
impl Tool for SubAgent {
    fn name(&self) -> &str {
        "task"
    }

    fn spec(&self) -> ToolSpec {
        let kinds: Vec<String> =
            self.profiles.iter().map(|p| format!("`{}`: {}", p.name, p.description.trim_end_matches('.'))).collect();
        let names: Vec<&str> = self.profiles.iter().map(|p| p.name.as_str()).collect();
        ToolSpec {
            name: "task".into(),
            description: format!(
                "Hand a self-contained piece of work to a sub-agent that works on its own and answers with a \
                 short report; only the report enters the conversation. Several task calls in one message run at \
                 the same time. With `background: true` a sub-agent that changes nothing runs as a job while you go on (one \
                 that can change files runs in the foreground): you are told when it \
                 ends and read its report with the `jobs` tool. Kinds ({DEFAULT} when not given): {}.",
                kinds.join("; ")
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "description": {"type": "string", "description": "Three to six words saying what the task is."},
                    "prompt": {"type": "string", "description": "The task, with what the sub-agent needs to know. It does not see this conversation; nexo hands it the passages of it that bear on the task (files read, failing commands, the person's constraints), so name the files and symbols involved rather than pasting them."},
                    "agent": {"type": "string", "enum": names, "description": "The kind of sub-agent."},
                    "background": {"type": "boolean", "description": "Run it as a background job and go on at once."}
                },
                "required": ["prompt"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Search
    }

    /// A sub-agent that can change files runs on its own, as a write: two at once could edit the same
    /// file and lose a change. Read-only ones still run side by side.
    fn call_kind(&self, args: &Value) -> ToolKind {
        let name = args.get("agent").and_then(Value::as_str).filter(|a| !a.is_empty()).unwrap_or(DEFAULT);
        match self.profiles.iter().find(|p| p.name == name) {
            Some(p) if !self.can_write(p) => ToolKind::Search,
            // an unknown kind is refused at once: nothing to keep apart
            None => ToolKind::Search,
            Some(_) => ToolKind::Write,
        }
    }

    fn summary(&self, args: &Value) -> String {
        let d = args.get("description").or_else(|| args.get("prompt")).and_then(Value::as_str).unwrap_or("");
        let d: String = d.chars().take(60).collect();
        let agent = args.get("agent").and_then(Value::as_str).filter(|a| *a != DEFAULT);
        let bg = args.get("background").and_then(Value::as_bool).unwrap_or(false);
        let mut tag = String::new();
        if let Some(a) = agent {
            tag.push_str(&format!(" [{a}]"));
        }
        if bg {
            tag.push_str(" (background)");
        }
        format!("task{tag}: {d}")
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(prompt) = args.get("prompt").and_then(Value::as_str).filter(|p| !p.trim().is_empty()) else {
            return ToolOutput::error("`prompt` is required: the task for the sub-agent");
        };
        let name = args.get("agent").and_then(Value::as_str).filter(|a| !a.is_empty()).unwrap_or(DEFAULT);
        let Some(profile) = self.profiles.iter().find(|p| p.name == name).cloned() else {
            let names: Vec<&str> = self.profiles.iter().map(|p| p.name.as_str()).collect();
            return ToolOutput::error(format!("no sub-agent `{name}`; there are: {}", names.join(", ")));
        };
        let host = self.host.get().cloned();
        let briefing = match &host {
            Some(h) if self.brief.tokens > 0 => brief::build(prompt, &h.history().await, &ctx.cwd, &self.brief).await,
            _ => None,
        };
        let run = Run {
            client: self.client.clone(),
            target: self.target.clone(),
            tools: self.tools_for(&profile),
            profile,
            host,
        };
        let asked = matches!(args.get("background"), Some(Value::Bool(true)))
            || args.get("background").and_then(Value::as_str).is_some_and(|s| s.eq_ignore_ascii_case("true"));
        // one that can change files keeps its write slot to the end: in the background it would edit
        // beside the main agent and other sub-agents
        let writes = self.can_write(&run.profile);
        if !asked || writes {
            if let Some(b) = &briefing {
                ctx.report(b.line());
            }
            let note = if asked { "(ran in the foreground: it can change files)\n\n" } else { "" };
            return match run.go(prompt, briefing.as_ref(), ctx, &|l| ctx.report(l)).await {
                Ok(report) => ToolOutput::ok(format!("{note}{report}")),
                Err(e) if ctx.cancel.is_cancelled() => ToolOutput::error(format!("interrupted by the person ({e})")),
                Err(e) => ToolOutput::error(e),
            };
        }
        let label = args.get("description").and_then(Value::as_str).unwrap_or(prompt);
        let label: String = label.lines().next().unwrap_or("").chars().take(80).collect();
        let job = ctx.jobs.start(JobKind::Agent, label.clone());
        let id = job.id();
        let mut jctx = ctx.clone();
        jctx.cancel = job.cancel();
        jctx.progress = None;
        let prompt = prompt.to_string();
        tokio::spawn(async move {
            let job = Arc::new(job);
            let say = {
                let job = job.clone();
                move |l: String| job.line(&l)
            };
            if let Some(b) = &briefing {
                say(b.line());
            }
            let res = run.go(&prompt, briefing.as_ref(), &jctx, &say).await;
            drop(say);
            let Ok(job) = Arc::try_unwrap(job) else { return };
            match res {
                Ok(report) => {
                    job.line("--- report ---");
                    job.text(&report);
                    job.finish(true, "reported");
                }
                // killed: the registry already says so
                Err(_) if jctx.cancel.is_cancelled() => drop(job),
                Err(e) => {
                    job.line(&e);
                    job.finish(false, "failed");
                }
            }
        });
        ToolOutput::ok(format!(
            "Started sub-agent job {id} ({name}): \"{label}\". It works while you go on; you will be told when it \
             ends. Its report is the job's output: jobs {{\"action\": \"output\", \"id\": {id}}} (add \"wait\" to \
             wait for it), stop it with {{\"action\": \"kill\", \"id\": {id}}}."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_read_only_sandbox_sees_home_paths_and_links_leaving_the_project() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let args = |p: &str| serde_json::json!({ "path": p });
        assert!(outside(&args("src/a.rs"), &cwd).is_none());
        assert!(outside(&args("../x"), &cwd).is_some());
        assert!(outside(&args("~/.ssh/id_rsa"), &cwd).is_some());
        assert!(outside(&args("~"), &cwd).is_some());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.path(), cwd.join("link")).unwrap();
            assert!(outside(&args("link/secret"), &cwd).is_some());
        }
    }

    #[test]
    fn a_custom_agent_reads_its_front_matter() {
        let p = parse(
            "---\nname: reviewer\ndescription: reviews a diff\ntools: [read, grep, \"bash\"]\nmodel: small-model\nrounds: 8\n---\nLook for bugs.\n",
            "file",
        )
        .unwrap();
        assert_eq!(p.name, "reviewer");
        assert_eq!(p.description, "reviews a diff");
        assert_eq!(p.tools, Some(vec!["read".into(), "grep".into(), "bash".into()]));
        assert_eq!(p.model.as_deref(), Some("small-model"));
        assert_eq!(p.rounds, 8);
        assert_eq!(p.prompt, "Look for bugs.");
        let bare = parse("Just instructions.", "helper").unwrap();
        assert_eq!(
            (bare.name.as_str(), bare.prompt.as_str(), bare.tools.clone()),
            ("helper", "Just instructions.", None)
        );
        assert!(parse("---\nname: bad name\n---\nx", "f").is_none());
    }

    #[test]
    fn custom_agents_load_from_the_project_and_replace_by_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".nexo/agents")).unwrap();
        std::fs::write(dir.path().join(".nexo/agents/docs.md"), "---\ndescription: writes docs\n---\nWrite docs.")
            .unwrap();
        std::fs::write(dir.path().join(".nexo/agents/explore.md"), "---\nname: explore\ndescription: mine\n---\nX")
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let trust = home.path().join("trusted-projects");
        // not trusted yet: only the built-in kinds
        let untrusted = load_from(dir.path(), home.path(), &trust);
        assert!(!untrusted.iter().any(|p| p.name == "docs"));
        assert!(untrusted.iter().any(|p| p.name == "explore" && p.source.is_none()));
        nexo_core::config::trust_project(dir.path(), &trust).unwrap();
        let all = load_from(dir.path(), home.path(), &trust);
        assert!(all.iter().any(|p| p.name == "docs" && p.description == "writes docs"));
        let ex: Vec<&Profile> = all.iter().filter(|p| p.name == "explore").collect();
        assert_eq!(ex.len(), 1);
        assert_eq!(ex[0].description, "mine");
        assert!(all.iter().any(|p| p.name == "general"));
    }
}
