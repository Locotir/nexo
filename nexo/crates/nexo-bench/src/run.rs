//! One run: a task done once by the agent with one arm's context policy, in a fresh working
//! directory, through nexo's own agent loop, then graded.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use nexo_agent::{AgentSetup, ModelClient};
use nexo_core::agent::{channels, AgentCommand, AgentEvent};
use nexo_core::{Config, PermissionDecision, PermissionMode, Pricing, ProviderConfig};
use serde::{Deserialize, Serialize};

use crate::arm::Arm;
use crate::client::{Limits, Recorder, Stop, Tally};
use crate::seal::Seal;
use crate::stats::LoopStats;
use crate::tasks::Task;

/// The provider name runs use in their config.
pub const PROVIDER: &str = "bench";

/// The environment variable the key is handed to the provider through (set in this process only).
pub const KEY_ENV: &str = "NEXO_BENCH_KEY";

/// The nexo commit this bench was built from.
pub const COMMIT: &str = env!("NEXO_COMMIT");
/// When [`COMMIT`] was made, UTC (`2026-11-04T18:05:00Z`), or `unknown`.
pub const COMMIT_TIME: &str = env!("NEXO_COMMIT_TIME");
/// The version shown to people: the package version and the short commit (`0.1.0 (aa7820a)`).
pub const VERSION: &str = env!("NEXO_VERSION");

/// What every run of a benchmark shares.
pub struct Bench {
    pub provider: ProviderConfig,
    pub model: String,
    pub price: Pricing,
    /// No prices are known for the model: every cost is 0 and the reports say "unknown".
    pub estimated: bool,
    pub max_steps: u32,
    pub max_tokens: Option<u32>,
    /// Sampling temperature of every request; None: the provider's own.
    pub temperature: Option<f32>,
    pub out: PathBuf,
    /// Folders a sealed task's commands must not see.
    pub hide: Vec<PathBuf>,
    pub client: Arc<dyn ModelClient>,
    /// The money every run has spent so far, a run that ends in an error included.
    pub spent: Arc<Mutex<f64>>,
}

/// One line of `results.jsonl`. Fields missing from older files read as their defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    pub task: String,
    pub arm: String,
    pub policy: String,
    pub spec: String,
    pub rep: u32,
    pub model: String,
    /// The nexo commit measured.
    pub commit: String,
    /// The settings of the run: `max_steps` (steps a run may take), `max_tokens` (reply cap, none:
    /// the agent's), `temperature` (none: the provider's), `compact_at` (the `compact` threshold in
    /// input tokens, `compact` arms only).
    pub max_steps: u32,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub compact_at: Option<u64>,
    /// 0..1.
    pub score: f64,
    /// Every check passed.
    pub ok: bool,
    pub detail: String,
    /// The agent stopped by itself, not at the step limit or the budget.
    pub finished: bool,
    pub stop: Option<Stop>,
    /// Requests that offered tools (one per step of the loop).
    pub steps: u32,
    pub requests: u32,
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    pub cost: f64,
    pub estimated: bool,
    pub tool_calls: u32,
    pub tool_errors: u32,
    pub repeated_calls: u32,
    /// Requests that summarised the history (in `requests`, not in `steps`).
    pub compactions: u32,
    /// Steps whose messages were not the previous step's plus new ones (0 for `plain`).
    pub rewrites: u32,
    /// The size of every step's request in input tokens (provider count, else chars / 4).
    pub view_tokens: Vec<u64>,
    /// The last step's and the largest step's request.
    pub final_view_tokens: u64,
    pub max_view_tokens: u64,
    pub wall_s: f64,
    /// Errors the agent reported (provider failures, refused requests), the first few.
    pub errors: Vec<String>,
    /// The transcript's file name, next to `results.jsonl`.
    pub transcript: String,
    /// The graded parts of a task of several objectives, in the task's order (empty otherwise).
    pub parts: Vec<Part>,
    /// Recall calls, re-reads and duplicate lines read from the transcript.
    pub counters: crate::counters::Counters,
    /// The agent's tool calls reached absolute paths outside the run's folders: the run could have
    /// read the answer elsewhere, so it is left out of every mean and delta.
    pub contaminated: bool,
    /// Those paths, the first few.
    pub outside: Vec<String>,
    /// Its commands ran sealed: no network, nothing outside its folders.
    pub sealed: bool,
}

/// One graded part of a run: right, wrong, or never answered (`ok` None).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Part {
    pub id: String,
    pub ok: Option<bool>,
}

/// The config a run's agent gets: the bench provider, the arm's policy, every permission given,
/// no sub-agents, hooks or MCP servers.
pub fn config(bench: &Bench, arm: &Arm) -> Config {
    let mut c = Config {
        provider: Some(PROVIDER.into()),
        model: Some(bench.model.clone()),
        providers: BTreeMap::from([(PROVIDER.to_string(), bench.provider.clone())]),
        ..Default::default()
    };
    c.permissions.mode = PermissionMode::Auto;
    c.agent.subagents = false;
    // the same prompt on every machine: no AGENTS.md from a parent folder, no ~/.nexo/prompt.md
    c.agent.hermetic = true;
    c.agent.max_turns = bench.max_steps;
    // a benchmark run stops hard at max_steps: a checkpoint would let it go past
    c.agent.on_limit = nexo_core::config::OnLimit::Ask;
    if let Some(n) = bench.max_tokens {
        c.agent.max_tokens = n;
    }
    if bench.temperature.is_some() {
        c.agent.temperature = bench.temperature;
    }
    c.context.policy = arm.policy.clone();
    c.context.curator_spec = arm.curator_spec();
    c.context.reader_at = arm.reader_at();
    c.context.compact_at = arm.compact_at();
    c
}

/// What one run does: the turns the agent gets in a prepared working directory, and how.
pub struct Plan {
    /// The record's task name (`repoqa/<id>` and the like).
    pub name: String,
    pub wd: PathBuf,
    pub seal: Option<Seal>,
    /// The tools offered, by name; None: every local tool.
    pub tools: Option<Vec<&'static str>>,
    /// Tool paths kept inside `wd` even without a seal.
    pub confine: bool,
    /// Replaces the agent's base system prompt.
    pub system_prompt: Option<PathBuf>,
    /// (session, text): each session number starts the agent afresh, resuming the previous history.
    pub turns: Vec<(u32, String)>,
    /// The steps the whole run may take (the agent's own limit stays `max_steps` per turn).
    pub max_steps: u32,
    /// Asked after each reply of turn `i` (its index in `turns`) with the replies so far: a
    /// follow-up question to put in the same session, or None to go on to the next turn.
    pub followup: Option<Followup>,
}

/// A follow-up policy: (turn index, the turn's replies so far) to the next question, if any.
pub type Followup = Arc<dyn Fn(usize, &[String]) -> Option<String> + Send + Sync>;

/// What a run did, before it is graded.
pub struct Driven {
    pub tally: Tally,
    pub stats: LoopStats,
    pub wall_s: f64,
    pub transcript: String,
    /// The last non-empty assistant text of each turn submitted, in order.
    pub answers: Vec<String>,
    /// Every reply of each turn: the turn's own, then one per follow-up question.
    pub replies: Vec<Vec<String>>,
    /// The working directory of the run.
    pub wd: PathBuf,
    /// When the run started: temporary files made after it are the run's own.
    pub since: SystemTime,
    /// Its commands ran sealed: nothing outside its folders could be reached.
    pub sealed: bool,
}

/// The transcript file name of a run.
pub fn transcript_name(task: &str, arm: &Arm, rep: u32) -> String {
    format!("{}-{}-{}.jsonl", task.replace(['/', '\\'], "_"), arm.name, rep)
}

/// Runs `plan` with `arm`; `budget_left` is the money it may still spend.
pub async fn drive(bench: &Bench, plan: &Plan, arm: &Arm, rep: u32, budget_left: f64) -> Result<Driven, String> {
    let name = transcript_name(&plan.name, arm, rep);
    let file = File::create(bench.out.join(&name)).map_err(|e| format!("{name}: {e}"))?;
    let tally = Arc::new(Mutex::new(Tally::default()));
    let client: Arc<dyn ModelClient> = Arc::new(Recorder {
        inner: bench.client.clone(),
        price: bench.price,
        limits: Limits { max_steps: plan.max_steps, max_cost: budget_left },
        tally: tally.clone(),
        spent: bench.spent.clone(),
        transcript: Arc::new(Mutex::new(file)),
        secrets: std::env::var(KEY_ENV).into_iter().collect(),
    });
    let mut config = config(bench, arm);
    config.agent.system_prompt_file = plan.system_prompt.clone();
    let since = SystemTime::now();
    let started = Instant::now();
    let mut stats = LoopStats::default();
    let mut answers = Vec::new();
    let mut replies = Vec::new();
    let mut session: Option<String> = None;
    let mut sessions: Vec<u32> = plan.turns.iter().map(|(n, _)| *n).collect();
    sessions.dedup();
    for n in sessions {
        if tally.lock().unwrap().stop.is_some() {
            break;
        }
        let setup = AgentSetup {
            config: config.clone(),
            cwd: plan.wd.clone(),
            provider: PROVIDER.into(),
            model: bench.model.clone(),
            tools: crate::tools::toolset_with(&plan.wd, plan.seal.clone(), plan.tools.as_deref(), plan.confine),
            client: client.clone(),
            resume: session.clone(),
            gate: Some(Arc::new(crate::tools::AllowAll)),
            mail: None,
        };
        let id =
            converse(setup, &plan.turns, n, plan.followup.as_ref(), &mut stats, &tally, &mut answers, &mut replies)
                .await?;
        session = (!id.is_empty()).then_some(id).or(session);
    }
    let wall_s = started.elapsed().as_secs_f64();
    let tally = tally.lock().unwrap().clone();
    Ok(Driven {
        tally,
        stats,
        wall_s,
        transcript: name,
        answers,
        replies,
        wd: plan.wd.clone(),
        since,
        sealed: plan.seal.is_some(),
    })
}

/// The record of a driven run graded `score` (0..1).
#[allow(clippy::too_many_arguments)]
pub fn record(
    bench: &Bench,
    arm: &Arm,
    rep: u32,
    task: &str,
    d: &Driven,
    score: f64,
    ok: bool,
    detail: String,
) -> Record {
    let t = &d.tally;
    // a sealed run saw only its own folders: the paths it names are inside the box
    let outside = if d.sealed {
        Vec::new()
    } else {
        crate::contam::Scope::new(&d.wd).made_since(d.since).file(&bench.out.join(&d.transcript))
    };
    Record {
        task: task.to_string(),
        arm: arm.name.clone(),
        policy: arm.policy.clone(),
        spec: arm.spec.clone(),
        rep,
        model: bench.model.clone(),
        commit: COMMIT.into(),
        max_steps: bench.max_steps,
        max_tokens: bench.max_tokens,
        temperature: bench.temperature,
        compact_at: (arm.policy == "compact").then(|| arm.compact_at()),
        score,
        ok,
        detail,
        finished: t.stop.is_none() && !d.stats.hit_max_steps,
        stop: t.stop.or(d.stats.hit_max_steps.then_some(Stop::MaxSteps)),
        steps: t.steps,
        requests: t.requests,
        input: t.usage.input,
        cache_read: t.usage.cache_read,
        cache_write: t.usage.cache_write,
        output: t.usage.output,
        cost: t.cost,
        estimated: bench.estimated,
        tool_calls: d.stats.tool_calls,
        tool_errors: d.stats.tool_errors,
        repeated_calls: d.stats.repeated_calls,
        compactions: t.compactions,
        rewrites: t.rewrites,
        final_view_tokens: t.views.last().copied().unwrap_or(0),
        max_view_tokens: t.views.iter().copied().max().unwrap_or(0),
        view_tokens: t.views.clone(),
        wall_s: d.wall_s,
        errors: d.stats.errors.iter().take(5).cloned().collect(),
        transcript: d.transcript.clone(),
        parts: Vec::new(),
        counters: crate::counters::from_file(&bench.out.join(&d.transcript)),
        contaminated: !outside.is_empty(),
        outside,
        sealed: d.sealed,
    }
}

/// Runs `task` once with `arm`; `budget_left` is the money the run may still spend. Err when this
/// machine cannot run or grade the task (what it spent is in `bench.spent` all the same).
pub async fn run(bench: &Bench, task: &Task, arm: &Arm, rep: u32, budget_left: f64) -> Result<Record, String> {
    (task.ready)()?;
    let dir = tempfile::Builder::new().prefix("nexo-bench-").tempdir().map_err(|e| e.to_string())?;
    let wd = dir.path().canonicalize().map_err(|e| e.to_string())?;
    task.materialize(&wd)?;
    // every task is sealed where this machine can seal commands, with a /tmp of its own; a sealed
    // task refuses to run elsewhere, the rest run open and their transcripts are checked afterwards
    let tmp = tempfile::Builder::new().prefix("nexo-bench-tmp-").tempdir().map_err(|e| e.to_string())?;
    let seal = match crate::seal::available() {
        Ok(()) => {
            let hide = bench.hide.iter().filter(|h| !wd.starts_with(h)).cloned().collect();
            let mask = task.mask.iter().map(|s| s.to_string()).collect();
            Some(Seal::new(&wd, hide, mask)?.with_tmp(tmp.path()))
        }
        Err(e) if task.sealed => return Err(e),
        Err(_) => None,
    };
    let plan = Plan {
        name: task.id.into(),
        wd: wd.clone(),
        seal,
        tools: None,
        confine: true,
        system_prompt: None,
        turns: task.turns.clone(),
        max_steps: bench.max_steps,
        followup: None,
    };
    let driven = drive(bench, &plan, arm, rep, budget_left).await?;
    let check = task.check;
    let graded = wd.clone();
    let outcome = tokio::task::spawn_blocking(move || check(&graded)).await.map_err(|e| e.to_string())?;
    drop(tmp);
    drop(dir);
    Ok(record(bench, arm, rep, task.id, &driven, outcome.score, outcome.ok, outcome.detail))
}

/// Runs one session of the agent: every message of session `n` in `turns`, each submitted once
/// the previous one is answered, none once the run is stopped. After each turn's reply, asks
/// `followup` for a question to put in the same session, as long as it has one. Pushes each
/// turn's last assistant text to `answers` and all its replies (the turn's, then each follow-up's)
/// to `replies`; returns the session id, for the next session to resume.
#[allow(clippy::too_many_arguments)]
async fn converse(
    setup: AgentSetup,
    turns: &[(u32, String)],
    n: u32,
    followup: Option<&Followup>,
    stats: &mut LoopStats,
    tally: &Mutex<Tally>,
    answers: &mut Vec<String>,
    replies: &mut Vec<Vec<String>>,
) -> Result<String, String> {
    let (mut handle, ends) = channels();
    let agent = tokio::spawn(nexo_agent::run(ends, setup));
    let mut session = String::new();
    // a resumed session first replays its history as events; only what happens from the first turn on counts
    let mut live = false;
    'turns: for (i, (_, text)) in turns.iter().enumerate().filter(|(_, (s, _))| *s == n) {
        if tally.lock().unwrap().stop.is_some() {
            break;
        }
        let (last, idle) = ask(&mut handle, text, stats, &mut live, &mut session).await?;
        answers.push(last.clone());
        let mut asked = vec![last];
        if !idle {
            replies.push(asked);
            break;
        }
        while let Some(q) = followup.and_then(|f| f(i, &asked)) {
            if tally.lock().unwrap().stop.is_some() {
                break;
            }
            let (reply, idle) = ask(&mut handle, &q, stats, &mut live, &mut session).await?;
            asked.push(reply);
            if !idle {
                replies.push(asked);
                break 'turns;
            }
        }
        replies.push(asked);
    }
    let _ = handle.commands.send(AgentCommand::Shutdown);
    let _ = tokio::time::timeout(Duration::from_secs(10), agent).await;
    Ok(session)
}

/// Submits `text` and waits for the agent to go idle: the last non-empty assistant text, and
/// whether the agent went idle (false: it shut down).
async fn ask(
    handle: &mut nexo_core::agent::AgentHandle,
    text: &str,
    stats: &mut LoopStats,
    live: &mut bool,
    session: &mut String,
) -> Result<(String, bool), String> {
    handle
        .commands
        .send(AgentCommand::Submit { text: text.to_string(), attachments: vec![] })
        .map_err(|_| "the agent stopped before the task was sent".to_string())?;
    let mut idle = false;
    let mut last = String::new();
    while let Some(ev) = handle.events.recv().await {
        *live |= matches!(ev, AgentEvent::TurnStart { .. });
        if *live {
            stats.observe(&ev);
        }
        match ev {
            AgentEvent::Ready { session_id, .. } | AgentEvent::SessionChanged { session_id, .. } => {
                *session = session_id
            }
            AgentEvent::AssistantDone { message } if *live => {
                let t = message.text();
                if !t.trim().is_empty() {
                    last = t;
                }
            }
            AgentEvent::PermissionAsk { request } => {
                let _ = handle
                    .commands
                    .send(AgentCommand::Permission { id: request.id, decision: PermissionDecision::Allow });
            }
            AgentEvent::Question { question } => {
                let answer = Some("Nobody is here to answer: decide for yourself and carry on.".to_string());
                let _ = handle.commands.send(AgentCommand::Answer { id: question.id, answer });
            }
            AgentEvent::Idle { .. } => {
                idle = true;
                break;
            }
            AgentEvent::Shutdown => break,
            _ => {}
        }
    }
    Ok((last, idle))
}

/// Appends `rec` to `results.jsonl` in `out`, flushed so a killed benchmark keeps it.
pub fn append(out: &Path, rec: &Record) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(out.join("results.jsonl"))?;
    writeln!(f, "{}", serde_json::to_string(rec).map_err(std::io::Error::other)?)?;
    f.flush()
}

/// A record with every count set, for the tests of what reads records.
#[cfg(test)]
pub(crate) fn sample(task: &str, arm: &str, score: f64, cost: f64) -> Record {
    Record {
        task: task.into(),
        arm: arm.into(),
        policy: arm.into(),
        spec: String::new(),
        rep: 0,
        model: "deepseek-chat".into(),
        commit: "abc1234".into(),
        max_steps: 100,
        max_tokens: None,
        temperature: None,
        compact_at: None,
        score,
        ok: score == 1.0,
        detail: String::new(),
        finished: true,
        stop: None,
        steps: 10,
        requests: 11,
        input: 1000,
        cache_read: 5000,
        cache_write: 0,
        output: 300,
        cost,
        estimated: false,
        tool_calls: 9,
        tool_errors: 2,
        repeated_calls: 1,
        compactions: 0,
        rewrites: 0,
        view_tokens: vec![4000, 6000],
        final_view_tokens: 6000,
        max_view_tokens: 6000,
        wall_s: 12.5,
        errors: vec![],
        transcript: String::new(),
        parts: Vec::new(),
        counters: Default::default(),
        contaminated: false,
        outside: Vec::new(),
        sealed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_config_uses_the_arms_policy_and_never_asks() {
        let bench = Bench {
            provider: ProviderConfig::new("anthropic", "https://example.invalid", Some(KEY_ENV)),
            model: "deepseek-chat".into(),
            price: crate::cost::DEEPSEEK,
            estimated: false,
            max_steps: 100,
            max_tokens: Some(2048),
            temperature: Some(0.0),
            out: PathBuf::new(),
            hide: vec![],
            client: Arc::new(nexo_providers::Client::new(nexo_providers::registry())),
            spent: Arc::default(),
        };
        let c = config(&bench, &Arm::parse("tight@curator:eoq=4000").unwrap());
        assert_eq!(c.provider.as_deref(), Some(PROVIDER));
        assert_eq!((c.context.policy.as_str(), c.context.curator_spec.as_str()), ("curator", "eoq=4000"));
        assert_eq!((c.agent.max_turns, c.agent.max_tokens, c.agent.subagents), (100, 2048, false));
        assert_eq!(c.agent.on_limit, nexo_core::config::OnLimit::Ask, "the step limit is a hard stop");
        assert!(c.agent.hermetic, "no AGENTS.md or ~/.nexo/prompt.md from this machine");
        assert_eq!(c.agent.temperature, Some(0.0));
        assert_eq!(c.permissions.mode, PermissionMode::Auto);
        assert!(c.mcp.is_empty() && c.hooks.is_empty());
        let c = config(&bench, &Arm::parse("compact:at=50000").unwrap());
        assert_eq!(c.agent.on_limit, nexo_core::config::OnLimit::Ask, "every arm stops hard at max_steps");
        assert_eq!((c.context.policy.as_str(), c.context.compact_at), ("compact", 50_000));
        assert_eq!(c.context.curator_spec, "");
        assert_eq!(config(&bench, &Arm::parse("compact").unwrap()).context.compact_at, crate::arm::COMPACT_AT);
        assert!(COMMIT.len() >= 7, "{COMMIT}");
    }
}
