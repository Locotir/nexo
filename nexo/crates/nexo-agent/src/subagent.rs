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
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use nexo_core::compat::map_tool;
use nexo_core::config::ProviderConfig;
use nexo_core::error::{ErrorKind, ProviderError};
use nexo_core::jobs::JobKind;
use nexo_core::message::{Message, Part, Role};
use nexo_core::pricing::Pricing;
use nexo_core::provider::{ModelRequest, ToolSpec};
use nexo_core::stream::{FinishReason, StreamEvent, Usage};
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
    /// The model comes from another tool's file: kept only when the provider offers it.
    pub soft_model: bool,
    /// Runs in a fresh git worktree of its own (`isolation: worktree` in the front matter).
    pub isolation: bool,
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
            soft_model: false,
            isolation: false,
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
            soft_model: false,
            isolation: false,
        },
    ]
}

/// The built-in kinds, then the person's, then the project's; a later one
/// with the same name replaces the earlier. A project nobody trusted yet (`nexo config trust`) has
/// its agents left out: their instructions come from the repository.
pub fn load(cwd: &Path) -> Vec<Profile> {
    let home = nexo_core::compat::foreign_home();
    load_from(cwd, &nexo_core::paths::config_dir(), &nexo_core::paths::trusted_projects_file(), home.as_deref())
}

fn load_from(cwd: &Path, config_dir: &Path, trust_file: &Path, foreign_home: Option<&Path>) -> Vec<Profile> {
    let mut out = builtin();
    // lowest priority first: the other tools' `.claude/agents`, then nexo's own folders
    let sources = nexo_core::compat::sources(cwd, config_dir, trust_file, foreign_home, "agents", Some("agents"), None);
    for s in sources {
        let Ok(rd) = std::fs::read_dir(&s.dir) else { continue };
        let mut files: Vec<PathBuf> =
            rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "md")).collect();
        files.sort();
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("agent").to_string();
            if let Some(mut p) = parse(&text, &stem) {
                if s.foreign {
                    adopt_foreign(&mut p);
                }
                p.source = Some(f.clone());
                if let Some(plugin) = &s.plugin {
                    p.name = format!("{plugin}:{}", p.name);
                }
                out.retain(|o| o.name != p.name);
                out.push(p);
            }
        }
    }
    out
}

/// Maps what a `.claude/agents` file says onto nexo: its tool names (a list that names none nexo
/// has stays read-only, never all tools) and its model (checked when the sub-agent runs).
fn adopt_foreign(p: &mut Profile) {
    if let Some(list) = p.tools.take() {
        let mut mapped: Vec<String> = Vec::new();
        for t in list.iter().filter_map(|t| map_tool(t)) {
            if !mapped.contains(&t) {
                mapped.push(t);
            }
        }
        if mapped.is_empty() {
            mapped = ["read", "grep", "glob", "ls"].map(String::from).to_vec();
        }
        p.tools = Some(mapped);
    }
    if p.model.as_deref().is_some_and(|m| m.eq_ignore_ascii_case("inherit")) {
        p.model = None;
    }
    p.soft_model = p.model.is_some();
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
        soft_model: false,
        isolation: false,
    };
    for line in head.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
        match k.trim() {
            "name" if !v.is_empty() => p.name = v.to_string(),
            "description" => p.description = v.to_string(),
            "model" if !v.is_empty() => p.model = Some(v.to_string()),
            "rounds" => p.rounds = v.parse().unwrap_or(p.rounds).clamp(1, 200),
            "isolation" => p.isolation = v.eq_ignore_ascii_case("worktree"),
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
    /// `pricing`: the prices of another provider the sub-agent ran on; none: priced as the main agent prices `model`.
    async fn spent(&self, model: &str, usage: Usage, pricing: Option<Pricing>);
    /// The conversation so far, to brief the sub-agent from.
    async fn history(&self) -> Vec<Message> {
        Vec::new()
    }
    /// The sub-agent is about to write `path`: what it holds now, for a rewind.
    async fn before_write(&self, _path: PathBuf) {}
}

/// Set once the agent's state exists (the tool is made before it).
pub type HostCell = Arc<OnceLock<Arc<dyn Host>>>;

/// The files sub-agents running at the same time are changing: path, then (the call that holds it, its
/// label). A file one of them changes is closed to the others until that one ends.
type Claims = Arc<std::sync::Mutex<std::collections::HashMap<PathBuf, (String, String)>>>;

/// Gives back the files a sub-agent held when it ends, also when it is stopped half way.
struct Release {
    claims: Claims,
    owner: String,
}

impl Drop for Release {
    fn drop(&mut self) {
        self.claims.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, (o, _)| *o != self.owner);
    }
}

/// The path as written, made absolute and without `.` and `..`, so two spellings of one file meet.
fn plain_path(p: &str, cwd: &Path) -> PathBuf {
    let path = if Path::new(p).is_absolute() { PathBuf::from(p) } else { cwd.join(p) };
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// What the parent is told when an isolated sub-agent ends: its branch and what it changed, or that its
/// worktree had no changes and is gone.
fn worktree_note(wt: &nexo_core::worktree::Worktree) -> String {
    match nexo_core::worktree::finish(wt) {
        nexo_core::worktree::Finished::Removed => "\n\n[worktree] the sub-agent changed nothing; its worktree was removed".into(),
        nexo_core::worktree::Finished::Kept(c) => format!(
            "\n\n[worktree] the sub-agent worked in its own worktree.\nbranch: {}\npath: {}\n{} commit(s), {} uncommitted path(s). Changes:\n{}\nTo take them: git merge {}  (remove: nexo worktree remove {})",
            wt.branch,
            wt.path.display(),
            c.commits,
            c.dirty.len(),
            if c.stat.is_empty() { "(only commits; see git diff)".to_string() } else { c.stat },
            wt.branch,
            wt.name
        ),
    }
}

/// Runs the SubagentStop hooks with the report; returns the lines for hooks that failed.
async fn stop_hooks(
    hooks: &[nexo_core::config::HookConfig],
    cwd: &Path,
    session_id: &str,
    agent: &str,
    report: &str,
) -> Vec<String> {
    crate::hooks::run(
        hooks,
        "subagent_stop",
        Some(agent),
        &json!({"agent_type": agent, "report": report}),
        cwd,
        session_id,
    )
    .await
    .failures
}

pub struct SubAgent {
    client: Arc<dyn ModelClient>,
    target: Target,
    /// Every tool but `task`.
    all: ToolSet,
    profiles: Vec<Profile>,
    host: HostCell,
    brief: BriefConfig,
    /// The providers set up, by name: a sub-agent may be sent to another one.
    providers: std::collections::BTreeMap<String, ProviderConfig>,
    /// Sub-agents that can change files run beside each other too (see [`Claims`]).
    parallel: bool,
    claims: Claims,
    /// SubagentStart and SubagentStop hooks.
    hooks: Vec<nexo_core::config::HookConfig>,
    /// Commands run in a new worktree (`[agent] worktree_setup`).
    worktree_setup: Vec<String>,
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
        SubAgent {
            client,
            target,
            all: tools,
            profiles,
            host,
            brief: BriefConfig { tokens: 0, ..Default::default() },
            providers: Default::default(),
            parallel: false,
            claims: Default::default(),
            hooks: Vec::new(),
            worktree_setup: Vec::new(),
        }
    }

    /// The commands to run in the worktree of a sub-agent with `isolation: worktree`.
    pub fn with_worktree_setup(mut self, cmds: Vec<String>) -> SubAgent {
        self.worktree_setup = cmds;
        self
    }

    /// The hooks that hear when a sub-agent starts and stops.
    pub fn with_hooks(mut self, hooks: Vec<nexo_core::config::HookConfig>) -> SubAgent {
        self.hooks = hooks;
        self
    }

    /// Hands every sub-agent a briefing picked from the conversation (see [`crate::brief`]).
    pub fn with_brief(mut self, cfg: BriefConfig) -> SubAgent {
        self.brief = cfg;
        self
    }

    /// The providers a sub-agent may be sent to, besides the main agent's.
    pub fn with_providers(mut self, providers: std::collections::BTreeMap<String, ProviderConfig>) -> SubAgent {
        self.providers = providers;
        self
    }

    /// Lets the sub-agents of one message that can change files run at the same time.
    pub fn with_parallel(mut self, on: bool) -> SubAgent {
        self.parallel = on;
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

/// A provider as the `task` tool offers it: its name and the models set up for it, a few at most.
fn offer(name: &str, cfg: &ProviderConfig) -> String {
    let mut models: Vec<&str> = cfg.default_model.iter().map(String::as_str).collect();
    for m in &cfg.models {
        if !models.contains(&m.as_str()) {
            models.push(m);
        }
    }
    let more = models.len() > 6;
    models.truncate(6);
    match (models.is_empty(), more) {
        (true, _) => name.to_string(),
        (false, false) => format!("{name} ({})", models.join(", ")),
        (false, true) => format!("{name} ({}, ...)", models.join(", ")),
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

/// Times a round whose model call broke off is asked again before the sub-agent gives up.
const ROUND_RETRIES: u32 = 3;
/// The longest wait before asking again, whatever the server said.
const MAX_WAIT: Duration = Duration::from_secs(60);

/// Why a request of a round gave no reply.
enum Failure {
    /// The person stopped the sub-agent.
    Stopped,
    Model(ProviderError),
}

fn stopped(messages: &[Message]) -> String {
    format!("stopped{}", progress(messages, usize::MAX, 0))
}

/// What a sub-agent that ends without a report had done: its calls (their effects stay on disk) and
/// its last words, so whoever started it goes on from there instead of doing it all again. Empty when
/// it had done nothing.
fn progress(messages: &[Message], round: usize, rounds: usize) -> String {
    let mut did: Vec<String> = Vec::new();
    let mut said = String::new();
    for m in messages.iter().filter(|m| m.role == Role::Assistant) {
        for p in &m.content {
            match p {
                Part::ToolCall { name, args, .. } => {
                    let what = ["path", "command", "pattern", "url"]
                        .iter()
                        .find_map(|k| args.get(*k).and_then(Value::as_str))
                        .map(|w| w.lines().next().unwrap_or("").chars().take(120).collect::<String>())
                        .unwrap_or_default();
                    did.push(if what.is_empty() { name.clone() } else { format!("{name} {what}") });
                }
                Part::Text { .. } => said = m.text(),
                _ => {}
            }
        }
    }
    if did.is_empty() {
        return String::new();
    }
    let at = if round == usize::MAX { String::new() } else { format!(" in round {}/{rounds}", round + 1) };
    let mut out = format!(
        "{at}. Before that it made {} calls; what they changed stays as they left it, so go on from there rather \
         than start over:\n",
        did.len()
    );
    const SHOWN: usize = 40;
    let skip = did.len().saturating_sub(SHOWN);
    if skip > 0 {
        out.push_str(&format!("- ({skip} earlier calls)\n"));
    }
    for d in &did[skip..] {
        out.push_str(&format!("- {d}\n"));
    }
    let said = said.trim();
    if !said.is_empty() {
        let tail: String = said.chars().rev().take(600).collect::<Vec<_>>().into_iter().rev().collect();
        out.push_str(&format!("Its last words: {tail}\n"));
    }
    out
}

/// One sub-agent at work.
struct Run {
    client: Arc<dyn ModelClient>,
    target: Target,
    /// Asked for this sub-agent: another provider (name and configuration) and another model.
    provider: Option<(String, ProviderConfig)>,
    model: Option<String>,
    tools: ToolSet,
    profile: Profile,
    host: Option<Arc<dyn Host>>,
    /// The files of the sub-agents at work, and this one's label in them.
    claims: Claims,
    label: String,
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
        let (main_provider, main_cfg, main_model) = self.target.get();
        let model = self
            .model
            .clone()
            .or_else(|| {
                let m = self.profile.model.clone()?;
                (!self.profile.soft_model || m == main_model || main_cfg.models.contains(&m)).then_some(m)
            })
            .unwrap_or(main_model);
        // on another provider, its prices; the same model name there is the same model
        let (cfg, pricing) = match &self.provider {
            Some((name, cfg)) if *name != main_provider => {
                (cfg.clone(), crate::limits::resolve(name, cfg, &model, None).pricing)
            }
            _ => (main_cfg, None),
        };
        if self.provider.is_some() || self.model.is_some() {
            let name = self.provider.as_ref().map_or(main_provider.as_str(), |(n, _)| n.as_str());
            say(format!("on {name}/{model}"));
        }
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
            // a round whose reply broke off is asked again whole: no call of it ran yet, so nothing is
            // done twice, and the rounds before it (files written, commands run) are not thrown away
            let mut tries = 0u32;
            let (text, calls) = loop {
                let (got, usage) = self.ask(&cfg, &req, ctx).await;
                if let (Some(h), false) = (&self.host, usage.is_empty()) {
                    h.spent(&model, usage, pricing).await;
                }
                match got {
                    Ok(reply) => break reply,
                    Err(Failure::Stopped) => return Err(stopped(&messages)),
                    Err(Failure::Model(e)) if e.is_retryable() && tries < ROUND_RETRIES => {
                        tries += 1;
                        let wait = e.retry_after.unwrap_or(Duration::from_secs(2u64 << tries)).min(MAX_WAIT);
                        say(format!("the model call broke off ({}); asking again in {}s", e.message, wait.as_secs()));
                        tokio::select! {
                            _ = tokio::time::sleep(wait) => {}
                            _ = ctx.cancel.cancelled() => return Err(stopped(&messages)),
                        }
                    }
                    Err(Failure::Model(e)) => {
                        return Err(format!(
                            "the sub-agent's model call failed: {}{}",
                            e.message,
                            progress(&messages, round, rounds)
                        ))
                    }
                }
            };
            if calls.is_empty() || last {
                let report = text.trim();
                return if report.is_empty() {
                    Err(if last {
                        format!(
                            "the sub-agent took {rounds} rounds without an answer; ask it something narrower{}",
                            progress(&messages, usize::MAX, 0)
                        )
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

    /// One request of a round: its text and calls, and what it spent (also when it failed).
    async fn ask(
        &self,
        cfg: &ProviderConfig,
        req: &ModelRequest,
        ctx: &ToolContext,
    ) -> (Result<(String, Vec<(String, String, Value)>), Failure>, Usage) {
        let mut usage = Usage::default();
        let mut stream = match self.client.stream(cfg, req, 2).await {
            Ok(s) => s,
            Err(e) => return (Err(Failure::Model(e)), usage),
        };
        let mut text = String::new();
        let mut calls: Vec<(String, String, Value)> = Vec::new();
        // the reply hit the output limit, and how many calls it opened without closing
        let (mut cut, mut open) = (false, 0usize);
        loop {
            let ev = tokio::select! {
                ev = stream.next() => ev,
                _ = ctx.cancel.cancelled() => return (Err(Failure::Stopped), usage),
            };
            match ev {
                Some(StreamEvent::TextDelta(d)) => text.push_str(&d),
                Some(StreamEvent::ToolCallStart { .. }) => open += 1,
                Some(StreamEvent::Finish(FinishReason::Length)) => cut = true,
                Some(StreamEvent::ToolCallEnd { id, name, args }) => {
                    open = open.saturating_sub(1);
                    calls.push((id, name, args));
                }
                Some(StreamEvent::Usage(u) | StreamEvent::RetriedUsage(u)) => usage += u,
                Some(StreamEvent::Error(e)) => return (Err(Failure::Model(e)), usage),
                Some(StreamEvent::Done) | None => {
                    // a call cut off is dropped by the parser: what is left would pass as the report
                    if cut && (calls.is_empty() || open > 0) {
                        let e = ProviderError::new(ErrorKind::Parse, "the reply was cut off at the output limit");
                        return (Err(Failure::Model(e)), usage);
                    }
                    return (Ok((text, calls)), usage);
                }
                _ => {}
            }
        }
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
        // a file another sub-agent at work is changing stays its own: two at once would lose a change
        if t.kind() == ToolKind::Write {
            if let Some(p) = args.get("path").and_then(Value::as_str) {
                let key = plain_path(p, &ctx.cwd);
                let mut held = self.claims.lock().unwrap_or_else(|e| e.into_inner());
                match held.get(&key) {
                    Some((owner, label)) if *owner != ctx.call_id => {
                        return ToolOutput::error(format!(
                            "`{p}` is being changed by another sub-agent running at the same time ({label}); \
                             leave it to that one and say so in your report"
                        ));
                    }
                    Some(_) => {}
                    None => {
                        held.insert(key, (ctx.call_id.clone(), self.label.clone()));
                    }
                }
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
        let together = if self.parallel {
            "Several task calls in one message run at the same time, also ones that change files: give \
             each its own files, as a file one of them is changing is closed to the others until it ends."
        } else {
            "Several task calls in one message run at the same time when none of them can change files; \
             ones that can run one after another."
        };
        let (main_provider, _, main_model) = self.target.get();
        let mut params = json!({
            "type": "object",
            "properties": {
                "description": {"type": "string", "description": "Three to six words saying what the task is."},
                "prompt": {"type": "string", "description": "The task, with what the sub-agent needs to know. It does not see this conversation; nexo hands it the passages of it that bear on the task (files read, failing commands, the person's constraints), so name the files and symbols involved rather than pasting them."},
                "agent": {"type": "string", "enum": names, "description": "The kind of sub-agent."},
                "background": {"type": "boolean", "description": "Run it as a background job and go on at once."},
                "model": {"type": "string", "description": format!("Another model for this sub-agent, only when the person asks for one; the conversation's ({main_model}) otherwise. Write it as its provider names it.")}
            },
            "required": ["prompt"]
        });
        // another provider only when there is one to go to
        if self.providers.keys().any(|n| *n != main_provider) {
            let names: Vec<&str> = self.providers.keys().map(String::as_str).collect();
            let offered: Vec<String> = self.providers.iter().map(|(n, c)| offer(n, c)).collect();
            params["properties"]["provider"] = json!({"type": "string", "enum": names, "description": format!("Another provider for this sub-agent, only when the person asks for one; the conversation's ({main_provider}) otherwise. Without `model` it runs the same model there. Set up: {}.", offered.join("; "))});
        }
        ToolSpec {
            name: "task".into(),
            description: format!(
                "Hand a self-contained piece of work to a sub-agent that works on its own and answers with a \
                 short report; only the report enters the conversation. {} With `background: true` a sub-agent that changes nothing runs as a job while you go on (one \
                 that can change files runs in the foreground): you are told when it \
                 ends and read its report with the `jobs` tool. Kinds ({DEFAULT} when not given): {}.",
                together,
                kinds.join("; ")
            ),
            parameters: params,
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Search
    }

    /// A sub-agent that can change files runs on its own, as a write: two at once could edit the same
    /// file and lose a change. Read-only ones still run side by side, and with `parallel` every kind
    /// does, each file held by the first one that changes it.
    fn call_kind(&self, args: &Value) -> ToolKind {
        let name = args.get("agent").and_then(Value::as_str).filter(|a| !a.is_empty()).unwrap_or(DEFAULT);
        match self.profiles.iter().find(|p| p.name == name) {
            // with `parallel` they run beside each other too: the claims keep their files apart
            Some(p) if !self.can_write(p) || self.parallel => ToolKind::Search,
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
        let on = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
        match (on("provider"), on("model")) {
            (Some(p), Some(m)) => tag.push_str(&format!(" [{p}/{m}]")),
            (Some(p), None) => tag.push_str(&format!(" [{p}]")),
            (None, Some(m)) => tag.push_str(&format!(" [{m}]")),
            (None, None) => {}
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
        let asked_for = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
        let provider = match asked_for("provider") {
            None => None,
            Some(p) => match self.providers.iter().find(|(n, _)| n.eq_ignore_ascii_case(p)) {
                Some((n, c)) => Some((n.clone(), c.clone())),
                None => {
                    let names: Vec<&str> = self.providers.keys().map(String::as_str).collect();
                    let set_up = if names.is_empty() { "none".to_string() } else { names.join(", ") };
                    return ToolOutput::error(format!("no provider `{p}` is set up; there are: {set_up}"));
                }
            },
        };
        let model = asked_for("model").map(str::to_string);
        let host = self.host.get().cloned();
        // SubagentStart hooks: what they print is added to the task
        let prompt_owned = {
            let o = crate::hooks::run(
                &self.hooks,
                "subagent_start",
                Some(&profile.name),
                &json!({"agent_type": profile.name, "prompt": prompt}),
                &ctx.cwd,
                &ctx.session_id,
            )
            .await;
            for f in &o.failures {
                ctx.report(f.clone());
            }
            if o.context.is_empty() {
                prompt.to_string()
            } else {
                format!("{prompt}\n\n{}", o.context.join("\n\n"))
            }
        };
        let prompt = prompt_owned.as_str();
        let briefing = match &host {
            Some(h) if self.brief.tokens > 0 => brief::build(prompt, &h.history().await, &ctx.cwd, &self.brief).await,
            _ => None,
        };
        let run = Run {
            client: self.client.clone(),
            target: self.target.clone(),
            provider,
            model,
            tools: self.tools_for(&profile),
            profile,
            host,
            claims: self.claims.clone(),
            label: args
                .get("description")
                .or_else(|| args.get("prompt"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(60)
                .collect(),
        };
        let asked = matches!(args.get("background"), Some(Value::Bool(true)))
            || args.get("background").and_then(Value::as_str).is_some_and(|s| s.eq_ignore_ascii_case("true"));
        // one that can change files keeps its write slot to the end: in the background it would edit
        // beside the main agent and other sub-agents
        let writes = self.can_write(&run.profile);
        // an isolated one works in a worktree of its own, on its own branch
        let isolated = if writes && run.profile.isolation {
            let (cwd, setup) = (ctx.cwd.clone(), self.worktree_setup.clone());
            let name = format!("agent-{}-{}", run.profile.name, nexo_core::worktree::random_name());
            match tokio::task::spawn_blocking(move || nexo_core::worktree::create(&cwd, Some(&name), &setup)).await {
                Ok(Ok(wt)) => Some(wt),
                Ok(Err(e)) => return ToolOutput::error(format!("could not make the sub-agent a worktree: {e}")),
                Err(e) => return ToolOutput::error(format!("could not make the sub-agent a worktree: {e}")),
            }
        } else {
            None
        };
        let mut ictx = ctx.clone();
        if let Some(wt) = &isolated {
            ictx.cwd = wt.path.clone();
        }
        if !asked || writes {
            if let Some(b) = &briefing {
                ctx.report(b.line());
            }
            let note = if asked { "(ran in the foreground: it can change files)\n\n" } else { "" };
            // whatever it held is free again when it ends, also when it is stopped
            let _release = Release { claims: self.claims.clone(), owner: ctx.call_id.clone() };
            let res = run.go(prompt, briefing.as_ref(), &ictx, &|l| ctx.report(l)).await;
            let tail = match isolated {
                Some(wt) => tokio::task::spawn_blocking(move || worktree_note(&wt)).await.unwrap_or_default(),
                None => String::new(),
            };
            if let Ok(report) = &res {
                let fails = stop_hooks(&self.hooks, &ctx.cwd, &ctx.session_id, &run.profile.name, report).await;
                fails.into_iter().for_each(|f| ctx.report(f));
            }
            return match res {
                Ok(report) => ToolOutput::ok(format!("{note}{report}{tail}")),
                Err(e) if ctx.cancel.is_cancelled() => {
                    ToolOutput::error(format!("interrupted by the person ({e}){tail}"))
                }
                Err(e) => ToolOutput::error(format!("{e}{tail}")),
            };
        }
        let label = args.get("description").and_then(Value::as_str).unwrap_or(prompt);
        let label: String = label.lines().next().unwrap_or("").chars().take(80).collect();
        let job = ctx.jobs.start(JobKind::Agent, label.clone());
        let (hooks, agent_name) = (self.hooks.clone(), run.profile.name.clone());
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
            if let Ok(report) = &res {
                for f in stop_hooks(&hooks, &jctx.cwd, &jctx.session_id, &agent_name, report).await {
                    say(f);
                }
            }
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
        let untrusted = load_from(dir.path(), home.path(), &trust, None);
        assert!(!untrusted.iter().any(|p| p.name == "docs"));
        assert!(untrusted.iter().any(|p| p.name == "explore" && p.source.is_none()));
        nexo_core::config::trust_project(dir.path(), &trust).unwrap();
        let all = load_from(dir.path(), home.path(), &trust, None);
        assert!(all.iter().any(|p| p.name == "docs" && p.description == "writes docs"));
        let ex: Vec<&Profile> = all.iter().filter(|p| p.name == "explore").collect();
        assert_eq!(ex.len(), 1);
        assert_eq!(ex[0].description, "mine");
        assert!(all.iter().any(|p| p.name == "general"));
    }

    #[test]
    fn claude_agents_map_onto_nexo_and_nexo_files_win() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(proj.path().join(".git")).unwrap();
        let (cfg, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = proj.path().join(".claude/agents");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(
            a.join("rev.md"),
            "---\nname: rev\ndescription: reviews\ntools: Read, Grep, Bash, Task\nmodel: sonnet\n---\nReview.",
        )
        .unwrap();
        std::fs::write(a.join("odd.md"), "---\nname: odd\ntools: Foo\nmodel: inherit\n---\nX").unwrap();
        std::fs::write(a.join("docs.md"), "---\nname: docs\n---\nclaude docs").unwrap();
        std::fs::create_dir_all(cfg.path().join("agents")).unwrap();
        std::fs::write(cfg.path().join("agents/docs.md"), "---\nname: docs\n---\nnexo docs").unwrap();
        let trust = cfg.path().join("trusted-projects");
        nexo_core::config::trust_project(proj.path(), &trust).unwrap();
        let all = load_from(proj.path(), cfg.path(), &trust, Some(home.path()));
        let get = |n: &str| all.iter().find(|p| p.name == n).unwrap();
        assert_eq!(get("rev").tools, Some(vec!["read".into(), "grep".into(), "bash".into()]));
        assert!(get("rev").soft_model && get("rev").model.as_deref() == Some("sonnet"));
        assert_eq!(get("odd").tools, Some(vec!["read".into(), "grep".into(), "glob".into(), "ls".into()]));
        assert_eq!(get("odd").model, None);
        assert_eq!(get("docs").prompt, "nexo docs");
    }
}
