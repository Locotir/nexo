//! The `task` tool: a sub-agent the model hands a self-contained piece of work.
//! It runs its own loop with its own tools and answers with a report; only the
//! report enters the main conversation, so a wide search costs the main context
//! one message.
//!
//! Kinds of sub-agent:
//! - `explore`: reads and searches where the main agent may, changes nothing;
//! - `general` (the default): every tool but `task`, each call asked for like the main agent's;
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

#[cfg(test)]
mod context_tests;
mod guard;
mod ledger;
#[cfg(test)]
mod ledger_tests;
mod profile;
mod provider;
#[cfg(test)]
mod provider_tests;
mod run;
#[cfg(test)]
mod tests;
mod tool;
mod types;

use guard::*;
pub use profile::*;
use provider::*;
pub use run::*;
pub(crate) use types::*;

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
    /// A sub-agent is about to ask the model: `Err` says a spend limit is reached (counting the calls in flight).
    /// `Ok` holds the output cap to send, lowered when the budget left could not pay a full reply, and a hold to
    /// keep until the call is billed. `pricing`: the sub-agent's prices for `model`, none: the main agent's.
    async fn reserve(
        &self,
        _model: &str,
        _pricing: Option<Pricing>,
        _input_tokens: u64,
        max_tokens: u32,
    ) -> Result<(u32, crate::budget::Flight), String> {
        Ok((max_tokens, crate::budget::Flight::default()))
    }
    /// Bills a sub-agent's model call to the session and says what it cost. `pricing`: the prices the call was
    /// reserved at; none: priced as the main agent prices `model`. `other`: it ran on another provider than the
    /// main agent's (the audit log says so).
    async fn spent(&self, model: &str, usage: Usage, pricing: Option<Pricing>, other: bool) -> f64;
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

/// The fact line for messages the main model sent that the sub-agent never read before it ended.
fn unread_note(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => "\n\n1 message you sent it arrived after its last step and was not read.".into(),
        n => format!("\n\n{n} messages you sent it arrived after its last step and were not read."),
    }
}

/// What the parent is told when an isolated sub-agent ends: its branch and what it changed, or that its
/// worktree had no changes and is gone.
fn worktree_note(wt: &nexo_core::worktree::Worktree) -> String {
    match nexo_core::worktree::finish(wt) {
        nexo_core::worktree::Finished::Removed => {
            "\n\n[worktree] the sub-agent changed nothing; its worktree was removed".into()
        }
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
    /// The session's context policy (`[context]`), which each sub-agent runs over its own messages (none: plain).
    context: Option<nexo_core::config::ContextConfig>,
    /// The reply size a sub-agent asks for, as the main agent's (`[agent] max_tokens`), under its model's limits.
    max_tokens: u32,
    launch_gate: Arc<tokio::sync::RwLock<()>>,
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
            context: None,
            max_tokens: nexo_core::config::DEFAULT_MAX_TOKENS,
            launch_gate: Arc::new(tokio::sync::RwLock::new(())),
        }
    }

    /// The session's context policy: each sub-agent sends what it keeps of its own messages, as the main turn
    /// does (without it, everything).
    pub fn with_context(mut self, cfg: nexo_core::config::ContextConfig) -> SubAgent {
        self.context = Some(cfg);
        self
    }

    /// The reply size each sub-agent asks for: the main agent's `[agent] max_tokens`.
    pub fn with_max_tokens(mut self, n: u32) -> SubAgent {
        self.max_tokens = n;
        self
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
                // note, todo and recall are the parent's: a kind that does not list them does not get them.
                // So are the mail tools: they are bound to the parent session's mailbox and a sub-agent has no
                // session of its own, so what it sent would go out as the parent; its way back is its report
                None => !nexo_core::tool::catalog::PARENT_ONLY.contains(&spec.name.as_str()),
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

    /// A clean start does not offer the shared-memory tools, even to custom profiles.
    fn tools_for_launch(&self, profile: &Profile, start_clean: bool) -> ToolSet {
        let all = self.tools_for(profile);
        if !start_clean {
            return all;
        }
        let mut clean = ToolSet::new();
        for spec in all.specs() {
            if !matches!(spec.name.as_str(), "shared_memory_read" | "shared_memory_write") {
                if let Some(tool) = all.get(&spec.name) {
                    clean.add(tool.clone());
                }
            }
        }
        clean
    }

    /// Starts one sub-agent as a job and a task of its own and returns at once with the way to its end. The
    /// `task` tool and the workflow runner both start theirs here, so a sub-agent has the same hooks,
    /// briefing, claims, registry entry and permission questions whoever started it.
    pub(crate) fn exclusive_launches(&self) -> Option<tokio::sync::OwnedRwLockWriteGuard<()>> {
        self.launch_gate.clone().try_write_owned().ok()
    }

    pub(crate) async fn launch(&self, l: Launch<'_>, ctx: &ToolContext) -> Result<Started, String> {
        let guard = self.launch_gate.clone().try_read_owned().map_err(|_| "ultracode plan in progress".to_string())?;
        self.launch_inner(l, ctx, None, Some(guard), None).await
    }

    // Only the executor calls this while holding exclusive_launches for its entire turn.
    pub(crate) async fn launch_approved(
        &self,
        guard: std::sync::Arc<tokio::sync::OwnedRwLockWriteGuard<()>>,
        l: Launch<'_>,
        ctx: &ToolContext,
        permissions: &crate::ask::PlanCell,
        approved_mode: nexo_core::PermissionMode,
        permit: crate::turn::approval::InternalTaskPermit,
    ) -> Result<Started, String> {
        let args = json!({"agent": l.agent, "prompt": l.prompt, "provider": l.provider, "model": l.model, "start_clean": l.start_clean});
        self.launch_inner(l, ctx, Some((permissions, approved_mode, permit, &args)), None, Some(guard)).await
    }

    async fn launch_inner(
        &self,
        l: Launch<'_>,
        ctx: &ToolContext,
        approved: Option<(
            &crate::ask::PlanCell,
            nexo_core::PermissionMode,
            crate::turn::approval::InternalTaskPermit,
            &Value,
        )>,
        active_guard: Option<tokio::sync::OwnedRwLockReadGuard<()>>,
        exclusive_guard: Option<std::sync::Arc<tokio::sync::OwnedRwLockWriteGuard<()>>>,
    ) -> Result<Started, String> {
        let snapshot_check = approved.as_ref().map(|(_, _, permit, _)| permit.snapshot_check());
        let approved_check = approved.as_ref().map(|(p, mode, _, _)| (*p, *mode));
        let check = || -> Result<(), String> {
            if ctx.cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            if let Some((permissions, mode)) = approved_check {
                let current = permissions.lock().unwrap_or_else(|e| e.into_inner()).mode;
                if current.reads_only() || (mode == nexo_core::PermissionMode::Auto && current != mode) {
                    return Err("permission mode changed".into());
                }
            }
            Ok(())
        };
        check()?;
        let Launch {
            agent: name,
            prompt,
            provider,
            model,
            description,
            title,
            background: asked,
            detachable,
            start_clean,
        } = l;
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
            Some(p) => match self.providers.get_key_value(p).or_else(|| {
                let mut matches = self.providers.iter().filter(|(n, _)| n.eq_ignore_ascii_case(p));
                let first = matches.next();
                if matches.next().is_some() {
                    None
                } else {
                    first
                }
            }) {
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
        // Register before SubagentStart hooks, briefing, or worktree setup. If any
        // preparation fails, dropping the handle ends the registered job.
        let label = description.unwrap_or(prompt);
        let label: String = label.lines().next().unwrap_or("").chars().take(80).collect();
        // Serialize mode changes with the job registration itself. CancellationToken
        // has no shared lock with its cancel() operation: cancellation can still race
        // this registration; the executor must kill a newly registered job afterward.
        let job = if let Some((_, _, permit, args)) = approved {
            permit.register(args, ctx, label.clone()).await?
        } else {
            if ctx.cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            ctx.jobs.start(JobKind::Agent, label.clone())
        };
        check()?;
        if let Some(snapshot) = &snapshot_check {
            snapshot.check_snapshot(ctx).await?;
        }
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
            if start_clean || o.context.is_empty() {
                prompt.to_string()
            } else {
                format!("{prompt}\n\n{}", o.context.join("\n\n"))
            }
        };
        check()?;
        if let Some(snapshot) = &snapshot_check {
            snapshot.check_snapshot(ctx).await?;
        }
        // what names the run in the claims is the person's or model's own words, before the hooks add to them
        let own_words = description.unwrap_or(prompt);
        let prompt = prompt_owned.as_str();
        let briefing = match &host {
            Some(h) if !start_clean && self.brief.tokens > 0 => {
                brief::build(prompt, &h.history().await, &ctx.cwd, &self.brief).await
            }
            _ => None,
        };
        check()?;
        if let Some(snapshot) = &snapshot_check {
            snapshot.check_snapshot(ctx).await?;
        }
        let mut run = Run {
            client: self.client.clone(),
            target: self.target.clone(),
            provider,
            model,
            tools: self.tools_for_launch(&profile, start_clean),
            profile,
            host,
            claims: ctx.claims.clone(),
            id: String::new(),
            sub: None,
            context: self.context.clone(),
            max_tokens: self.max_tokens,
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
        let phase_check = match check() {
            Ok(()) => match &snapshot_check {
                Some(snapshot) => snapshot.check_snapshot(ctx).await,
                None => Ok(()),
            },
            Err(e) => Err(e),
        };
        if let Err(e) = phase_check {
            if let Some(wt) = isolated {
                let _ = tokio::task::spawn_blocking(move || worktree_note(&wt)).await;
            }
            return Err(e);
        }
        let mut ictx = ctx.clone();
        if let Some(wt) = &isolated {
            ictx.cwd = wt.path.clone();
        }
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
            // Keep the launch gate until the runner and its cleanup have finished.
            let _active_guard = active_guard;
            let _exclusive_guard = exclusive_guard;
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
            let unread = run.sub.as_ref().map_or(0, |s| {
                s.end(match &res {
                    Ok(_) => SubState::Ok,
                    Err(_) if jctx.cancel.is_cancelled() => SubState::Killed,
                    Err(_) => SubState::Failed,
                })
            });
            // its files are free before anyone is told it ended (the note starts a turn that may write them)
            drop(release);
            // an isolated one is finished here, also when it was stopped: no worktree is left behind
            let tail = match isolated {
                Some(wt) => tokio::task::spawn_blocking(move || worktree_note(&wt)).await.unwrap_or_default(),
                None => String::new(),
            };
            let tail = format!("{tail}{}", unread_note(unread));
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
