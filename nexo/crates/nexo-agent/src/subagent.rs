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
//! A request carries the short text of `task` ([`short_text`]): the kinds by name, the person's own with the
//! start of their description. The whole descriptions answer a call that names a kind there is not.
//!
//! Every sub-agent runs as its own task and job. With `background: true` the call returns at once and
//! the report comes in the note of the ended job. Without it the call waits for the report, unless the
//! person writes or presses the key: then the sub-agent goes to the background and the call returns.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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
use nexo_core::subs::{SubItem, SubRun, SubState};
use nexo_core::tool::{Compact, Tool, ToolContext, ToolKind, ToolOutput, ToolSet};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::audit::Why;
use crate::brief::{self, Brief, BriefConfig};
use crate::client::{ModelClient, Target};

mod profile;
#[cfg(test)]
mod provider_tests;
mod run;
#[cfg(test)]
mod tests;

pub use profile::*;
pub use run::*;

/// What a sub-agent needs from the agent that runs it: leave to run a call
/// (the main agent's permissions and asks), and a place to put what it spent.
#[async_trait]
pub trait Host: Send + Sync {
    /// `Err` holds why the call may not run. `agent` is the name of the sub-agent asking.
    /// `hook`: what a PreToolUse hook decided (`allow` runs a call that would be asked about, `ask` asks).
    /// Both answers say who decided, for the audit log.
    async fn permit(
        &self,
        call_id: &str,
        agent: &str,
        tool: &dyn Tool,
        args: &Value,
        cancel: &CancellationToken,
        hook: Option<crate::hooks::Decision>,
    ) -> Result<Why, (String, Why)>;
    /// Runs the hooks of `event` the way the main turn does (they are told which sub-agent in `fields`).
    async fn hook(&self, _event: &str, _subject: &str, _fields: Value) -> crate::hooks::Outcome {
        Default::default()
    }
    /// A tool call of a sub-agent is over: its line in the audit log (when that is on).
    async fn audit(&self, _call: AuditCall<'_>) {}
    /// Bills a sub-agent's model call to the session and says what it cost. `pricing`: the prices of another
    /// provider the sub-agent ran on; none: priced as the main agent prices `model`.
    async fn spent(&self, model: &str, usage: Usage, pricing: Option<Pricing>) -> f64;
    /// The conversation so far, to brief the sub-agent from.
    async fn history(&self) -> Vec<Message> {
        Vec::new()
    }
    /// The sub-agent is about to write `path`: what it holds now, for a rewind.
    async fn before_write(&self, _path: PathBuf) {}
}

/// Set once the agent's state exists (the tool is made before it).
pub type HostCell = Arc<OnceLock<Arc<dyn Host>>>;

use nexo_core::claims::{plain_path, Claims};

/// Gives back the files a sub-agent held when it ends, also when it is stopped half way or dropped.
struct Release {
    claims: Claims,
    owner: String,
}

impl Drop for Release {
    fn drop(&mut self) {
        self.claims.release(&self.owner);
    }
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
    /// The text of `task` in a request ([`short_text`] of the kinds).
    short: String,
    host: HostCell,
    brief: BriefConfig,
    /// The providers set up, by name: a sub-agent may be sent to another one.
    providers: std::collections::BTreeMap<String, ProviderConfig>,
    /// Sub-agents that can change files run beside each other too (see [`Claims`]; the session's are in the tool context).
    parallel: bool,
    /// SubagentStart and SubagentStop hooks.
    hooks: Vec<nexo_core::config::HookConfig>,
    /// Commands run in a new worktree (`[agent] worktree_setup`).
    worktree_setup: Vec<String>,
}

impl SubAgent {
    /// The names of the kinds of sub-agent it starts.
    pub(crate) fn kinds(&self) -> Vec<String> {
        self.profiles.iter().map(|p| p.name.clone()).collect()
    }

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
                if spec.name != "task" && spec.name != "workflow" {
                    tools.add(t.clone());
                }
            }
        }
        let profiles = if profiles.is_empty() { builtin() } else { profiles };
        SubAgent {
            client,
            target,
            all: tools,
            short: short_text(&profiles),
            profiles,
            host,
            brief: BriefConfig { tokens: 0, ..Default::default() },
            providers: Default::default(),
            parallel: false,
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

    /// How a call for the sub-agent kind `name` is scheduled: a kind that can change files is a write (it runs
    /// on its own) unless `parallel` is on. The `task` tool and the workflow runner both ask this.
    pub(crate) fn kind_of(&self, name: &str) -> ToolKind {
        match self.profiles.iter().find(|p| p.name == name) {
            // with `parallel` they run beside each other too: the claims keep their files apart
            Some(p) if !self.can_write(p) || self.parallel => ToolKind::Search,
            // an unknown kind is refused at once: nothing to keep apart
            None => ToolKind::Search,
            Some(_) => ToolKind::Write,
        }
    }

    /// The tools a sub-agent of `p` gets.
    fn tools_for(&self, p: &Profile) -> ToolSet {
        let mut set = ToolSet::new();
        for spec in self.all.specs() {
            let Some(t) = self.all.get(&spec.name) else { continue };
            let listed = match &p.tools {
                // note, todo and recall are the parent's: a kind that does not list them does not get them
                None => !matches!(spec.name.as_str(), "note" | "todo" | "recall"),
                Some(l) => l.iter().any(|n| n == &spec.name),
            };
            // `diagnostics` starts language servers, which may run the project's build scripts
            let readable = matches!(t.kind(), ToolKind::Read | ToolKind::Search)
                && !matches!(spec.name.as_str(), "jobs" | "diagnostics");
            if listed && (!p.read_only || readable) {
                set.add(t.clone());
            } else if listed && spec.name == "debug" {
                // a read-only kind may look at and steer a session another agent started, nothing more
                set.add(std::sync::Arc::new(nexo_tools::debug::Debug::steer_only()));
            }
        }
        set
    }

    /// Starts one sub-agent as a job and a task of its own and returns at once with the way to its end. The
    /// `task` tool and the workflow runner both start theirs here, so a sub-agent has the same hooks,
    /// briefing, claims, registry entry and permission questions whoever started it.
    pub(crate) async fn launch(&self, l: Launch<'_>, ctx: &ToolContext) -> Result<Started, String> {
        let Launch { agent: name, prompt, provider, model, description, title, background: asked, detachable } = l;
        let Some(profile) = self.profiles.iter().find(|p| p.name == name).cloned() else {
            // the text of `task` in a request holds the start of each description: here they are whole, so
            // the next call can name the right kind
            let kinds: Vec<String> = self
                .profiles
                .iter()
                .map(|p| match p.description.trim() {
                    "" => format!("- {}", p.name),
                    d => format!("- {}: {d}", p.name),
                })
                .collect();
            return Err(format!("no sub-agent `{name}`; there are:\n{}", kinds.join("\n")));
        };
        let provider = match provider {
            None => None,
            Some(p) => match self.providers.iter().find(|(n, _)| n.eq_ignore_ascii_case(p)) {
                Some((n, c)) => Some((n.clone(), c.clone())),
                None => {
                    let names: Vec<&str> = self.providers.keys().map(String::as_str).collect();
                    let set_up = if names.is_empty() { "none".to_string() } else { names.join(", ") };
                    return Err(format!("no provider `{p}` is set up; there are: {set_up}"));
                }
            },
        };
        let model = model.map(str::to_string);
        // a model only another provider lists runs there, when no provider was named
        let provider = match (provider, &model) {
            (None, Some(m)) => {
                let (main_provider, main_cfg, _) = self.target.get();
                match provider_of(m, (&main_provider, &main_cfg), &self.providers) {
                    Ok(None) => None,
                    Ok(Some(n)) => self.providers.get_key_value(n).map(|(n, c)| (n.clone(), c.clone())),
                    Err(on) => return Err(format!("model {m} is set up on: {}; pass provider", on.join(", "))),
                }
            }
            (p, _) => p,
        };
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
        // what names the run in the claims is the person's or model's own words, before the hooks add to them
        let own_words = description.unwrap_or(prompt);
        let prompt = prompt_owned.as_str();
        let briefing = match &host {
            Some(h) if self.brief.tokens > 0 => brief::build(prompt, &h.history().await, &ctx.cwd, &self.brief).await,
            _ => None,
        };
        let mut run = Run {
            client: self.client.clone(),
            target: self.target.clone(),
            provider,
            model,
            tools: self.tools_for(&profile),
            profile,
            host,
            claims: ctx.claims.clone(),
            id: String::new(),
            sub: None,
            label: own_words.lines().next().unwrap_or("").chars().take(60).collect(),
        };
        let writes = self.can_write(&run.profile);
        // an isolated one works in a worktree of its own, on its own branch
        let isolated = if writes && run.profile.isolation {
            let (cwd, setup) = (ctx.cwd.clone(), self.worktree_setup.clone());
            let name = format!("agent-{}-{}", run.profile.name, nexo_core::worktree::random_name());
            match tokio::task::spawn_blocking(move || nexo_core::worktree::create(&cwd, Some(&name), &setup)).await {
                Ok(Ok(wt)) => Some(wt),
                Ok(Err(e)) => return Err(format!("could not make the sub-agent a worktree: {e}")),
                Err(e) => return Err(format!("could not make the sub-agent a worktree: {e}")),
            }
        } else {
            None
        };
        let mut ictx = ctx.clone();
        if let Some(wt) = &isolated {
            ictx.cwd = wt.path.clone();
        }
        // every sub-agent is a job of its own that runs as its own task. `background: true` leaves it
        // there at once; otherwise this call waits for it (attached), and lets go of it when the person
        // writes or presses the key (see `Detach`)
        let label = description.unwrap_or(prompt);
        let label: String = label.lines().next().unwrap_or("").chars().take(80).collect();
        let job = ctx.jobs.start(JobKind::Agent, label.clone());
        let id = job.id();
        // this run's own id: the provider's call id can come again in a later turn
        run.id = format!("{}#{id}", ctx.call_id);
        let run_id = run.id.clone();
        let subs = ctx.jobs.subs();
        let sub = match title {
            Some(t) => subs.begin_titled(&run_id, &run.profile.name, t, prompt, asked),
            None => subs.begin(&run_id, &run.profile.name, prompt, asked),
        };
        let meter = sub.meter();
        run.sub = Some(sub);
        // a background reader keeps the note and the job output it always had; every other sub-agent that
        // ends away from its call brings its report in the note
        let inline = writes || !asked;
        let (hooks, agent_name) = (self.hooks.clone(), run.profile.name.clone());
        let (main_cwd, session_id, call_id) = (ctx.cwd.clone(), ctx.session_id.clone(), ctx.call_id.clone());
        let mut jctx = ictx;
        jctx.cancel = job.cancel();
        // lines go to the call's progress only while this call waits; the sender is held weakly so the
        // call's own end closes the channel
        let progress = if asked || !detachable { None } else { ctx.progress.as_ref().map(|p| p.downgrade()) };
        jctx.progress = None;
        let attached = Arc::new(AtomicBool::new(!asked));
        let release = Release { claims: ctx.claims.clone(), owner: run_id.clone() };
        let detach = ctx.detach.clone();
        // one asked to run in the background is one from the start: its questions stay open on Esc
        if let (Some(d), true) = (&detach, detachable) {
            d.begin(&run_id, asked);
        }
        let ended_id = run_id.clone();
        let prompt = prompt.to_string();
        let (tx, rx) = tokio::sync::oneshot::channel::<Ended>();
        let flag = attached.clone();
        tokio::spawn(async move {
            // whatever it held is free again when it ends, is stopped, or the task is dropped
            let release = release;
            let job = Arc::new(job);
            let say = {
                let job = job.clone();
                let call_id = call_id.clone();
                move |l: String| {
                    job.line(&l);
                    if flag.load(Ordering::Relaxed) {
                        if let Some(p) = progress.as_ref().and_then(|p| p.upgrade()) {
                            let _ = p.send(nexo_core::tool::ToolProgress { call_id: call_id.clone(), line: l });
                        }
                    }
                }
            };
            if let Some(b) = &briefing {
                say(b.line());
            }
            let res = run.go(&prompt, briefing.as_ref(), &jctx, &say).await;
            if let Some(s) = &run.sub {
                s.end(match &res {
                    Ok(_) => SubState::Ok,
                    Err(_) if jctx.cancel.is_cancelled() => SubState::Killed,
                    Err(_) => SubState::Failed,
                });
            }
            // its files are free before anyone is told it ended (the note starts a turn that may write them)
            drop(release);
            // an isolated one is finished here, also when it was stopped: no worktree is left behind
            let tail = match isolated {
                Some(wt) => tokio::task::spawn_blocking(move || worktree_note(&wt)).await.unwrap_or_default(),
                None => String::new(),
            };
            if let Ok(report) = &res {
                for f in stop_hooks(&hooks, &main_cwd, &session_id, &agent_name, report).await {
                    say(f);
                }
            }
            // from here on the call cannot let go of it: the report goes to the one that is still waiting
            if let Some(d) = &detach {
                d.end(&ended_id);
            }
            drop(say);
            let Ok(job) = Arc::try_unwrap(job) else { return };
            let ended = match res {
                Ok(report) => {
                    let full = format!("{report}{tail}");
                    job.line("--- report ---");
                    job.text(&full);
                    if inline {
                        job.set_report(&full);
                    }
                    job.finish(true, "reported");
                    Ended::Report(full)
                }
                // killed: the registry already says so
                Err(e) if jctx.cancel.is_cancelled() => {
                    drop(job);
                    Ended::Stopped(e, tail)
                }
                Err(e) => {
                    job.line(&e);
                    if inline {
                        job.set_report(&format!("failed: {e}{tail}"));
                    }
                    job.finish(false, "failed");
                    Ended::Failed(e, tail)
                }
            };
            let _ = tx.send(ended);
        });
        Ok(Started { id, label, run_id, rx, attached, inline, meter })
    }
}

/// The models a provider lists: its default and the others set up for it.
fn listed(cfg: &ProviderConfig) -> impl Iterator<Item = &str> {
    cfg.default_model.iter().chain(cfg.models.iter()).map(String::as_str)
}

/// Where a model asked for without a provider runs: `Ok(None)` on the conversation's provider (it lists
/// the model, or nobody does), `Ok(Some(name))` on the one other provider that lists it, `Err(names)`
/// when several others do. Names match exactly, ignoring ASCII case.
fn provider_of<'a>(
    model: &str,
    main: (&str, &ProviderConfig),
    providers: &'a std::collections::BTreeMap<String, ProviderConfig>,
) -> Result<Option<&'a str>, Vec<&'a str>> {
    if listed(main.1).any(|m| m.eq_ignore_ascii_case(model)) {
        return Ok(None);
    }
    let on: Vec<&str> = providers
        .iter()
        .filter(|(n, c)| n.as_str() != main.0 && listed(c).any(|m| m.eq_ignore_ascii_case(model)))
        .map(|(n, _)| n.as_str())
        .collect();
    match on.len() {
        0 => Ok(None),
        1 => Ok(Some(on[0])),
        _ => Err(on),
    }
}

/// A path argument that leaves the project: resolved as the tools resolve it (`~` expanded), with
/// links followed, so a link inside the project that points out of it counts as outside.
fn outside(args: &Value, cwd: &Path) -> Option<String> {
    nexo_core::tool::call_paths(args).iter().find_map(|p| outside_path(p, cwd))
}

fn outside_path(p: &str, cwd: &Path) -> Option<String> {
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
        let (main_provider, _, _) = self.target.get();
        let mut params = json!({
            "type": "object",
            "properties": {
                "description": {"type": "string", "description": "Three to six words saying what the task is."},
                "prompt": {"type": "string", "description": "The task, with what the sub-agent needs to know. It does not see this conversation; nexo hands it the passages of it that bear on the task (files read, failing commands, the person's constraints), so name the files and symbols involved rather than pasting them."},
                "agent": {"type": "string", "enum": names, "description": "The kind of sub-agent."},
                "background": {"type": "boolean", "description": "Run it as a background job and go on at once."},
                "model": {"type": "string", "description": "Another model for this sub-agent, only when the person asks for one."}
            },
            "required": ["prompt"]
        });
        // another provider only when there is one to go to; its name is checked when the call runs, and a
        // wrong one is answered with the names there are: the list is not paid for in every request
        if self.providers.keys().any(|n| *n != main_provider) {
            params["properties"]["provider"] = json!({"type": "string", "description": "Another provider for this sub-agent, by name, only when the person asks for one."});
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

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: &self.short,
            params: &[
                ("prompt", "The whole task, naming files and symbols"),
                ("background", "Run as a job"),
                ("model", "Only if the person asks"),
                ("provider", "Only if the person asks"),
            ],
        })
    }

    /// Seldom called, so the short text goes out in every window, whatever kinds the person has: the full
    /// text describes each kind whole, thousands of characters in every request once there are a dozen.
    fn rare(&self) -> bool {
        true
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Search
    }

    /// A sub-agent that can change files runs on its own, as a write: two at once could edit the same
    /// file and lose a change. Read-only ones still run side by side, and with `parallel` every kind
    /// does, each file held by the first one that changes it.
    fn call_kind(&self, args: &Value) -> ToolKind {
        let name = args.get("agent").and_then(Value::as_str).filter(|a| !a.is_empty()).unwrap_or(DEFAULT);
        self.kind_of(name)
    }

    fn handles_cancel(&self) -> bool {
        true
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
        let asked_for = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
        let asked = matches!(args.get("background"), Some(Value::Bool(true)))
            || args.get("background").and_then(Value::as_str).is_some_and(|s| s.eq_ignore_ascii_case("true"));
        let launch = Launch {
            agent: name,
            prompt,
            provider: asked_for("provider"),
            model: asked_for("model"),
            description: args.get("description").and_then(Value::as_str),
            title: None,
            background: asked,
            detachable: true,
        };
        let Started { id, label, run_id, rx, attached, inline, .. } = match self.launch(launch, ctx).await {
            Ok(s) => s,
            Err(why) => return ToolOutput::error(why),
        };
        let mut rx = rx;
        if asked && !inline {
            return ToolOutput::ok(format!(
                "Started sub-agent job {id} ({name}): \"{label}\". It works while you go on; you will be told when it \
                 ends. Its report is the job's output: jobs {{\"action\": \"output\", \"id\": {id}}} (add \"wait\" to \
                 wait for it), stop it with {{\"action\": \"kill\", \"id\": {id}}}."
            ));
        }
        if asked {
            return ToolOutput::ok(format!(
                "Started sub-agent job {id} ({name}): \"{label}\". It works while you go on; you will be told when it \
                 ends, with its report. Stop it with the jobs tool: {{\"action\": \"kill\", \"id\": {id}}}."
            ));
        }
        let gone = async {
            match &ctx.detach {
                Some(d) => d.wait().await,
                None => std::future::pending::<nexo_core::jobs::DetachWhy>().await,
            }
        };
        let ended = tokio::select! {
            biased;
            r = &mut rx => r,
            _ = ctx.cancel.cancelled() => {
                // Esc: stop it and give its cleanup (the worktree, the hooks) a few seconds
                ctx.jobs.kill(id);
                match tokio::time::timeout(CLEANUP, &mut rx).await {
                    Ok(r) => r,
                    Err(_) => return ToolOutput::error("interrupted by the person"),
                }
            }
            why = gone => {
                if ctx.detach.as_ref().is_some_and(|d| d.to_background(&run_id)) {
                    attached.store(false, Ordering::Relaxed);
                    ctx.jobs.subs().to_background(&run_id);
                    return ToolOutput::ok(why.text(id));
                }
                // it ended in the same moment: this call is the one that gives the model its report
                (&mut rx).await
            }
        };
        // the model has the outcome in this tool result: no ended note for it
        ctx.jobs.tell(id);
        match ended {
            Ok(Ended::Report(r)) => ToolOutput::ok(r),
            Ok(Ended::Failed(e, tail)) => ToolOutput::error(format!("{e}{tail}")),
            Ok(Ended::Stopped(e, tail)) if ctx.cancel.is_cancelled() => {
                ToolOutput::error(format!("interrupted by the person ({e}){tail}"))
            }
            Ok(Ended::Stopped(e, tail)) => ToolOutput::error(format!("the sub-agent was stopped ({e}){tail}")),
            Err(_) => ToolOutput::error("the sub-agent ended without a report"),
        }
    }
}

/// What starts a sub-agent: the arguments of a `task` call, or a step of a workflow.
pub(crate) struct Launch<'a> {
    /// The kind of sub-agent (a name of [`SubAgent::profiles`]).
    pub agent: &'a str,
    pub prompt: &'a str,
    /// Another provider or model, as the `task` tool takes them.
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
    /// What the job and the claims call it; the first line of the prompt when none.
    pub description: Option<&'a str>,
    /// What the registry calls it; the first line of the task when none.
    pub title: Option<&'a str>,
    /// Starts in the background: no call waits for it.
    pub background: bool,
    /// The call that waits for it may let go of it when the person writes or presses the key, and sees its
    /// progress lines. A workflow keeps its sub-agents until they end: false.
    pub detachable: bool,
}

/// A sub-agent at work, as whoever started it sees it.
pub(crate) struct Started {
    /// Its job.
    pub id: u32,
    pub label: String,
    /// Its own id in the registry.
    pub run_id: String,
    pub rx: tokio::sync::oneshot::Receiver<Ended>,
    pub attached: Arc<AtomicBool>,
    /// Its report is also the job's report (what the ended note carries).
    pub inline: bool,
    /// What it has spent so far; stays readable when the registry lets go of the run.
    pub meter: nexo_core::subs::SubMeter,
}

/// How a sub-agent's run ended, for the call that waits for it.
pub(crate) enum Ended {
    Report(String),
    /// It failed: the error and the worktree note.
    Failed(String, String),
    /// It was stopped: what it had said, and the worktree note.
    Stopped(String, String),
}

/// Seconds an interrupted call waits for the sub-agent to finish its cleanup.
const CLEANUP: Duration = Duration::from_secs(5);
