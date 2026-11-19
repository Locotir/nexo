//! The loop: commands in, events out; per request, model calls and tool
//! runs until the model answers without calling a tool.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime};

use futures::StreamExt;
use nexo_core::agent::{
    AddedKind, AddedPart, AgentCommand, AgentEnds, AgentEvent, Attachment, EventTx, GoalLog, GoalState, GoalStop,
    Level, TurnEnd, GOAL_BRIEF_HEAD, GOAL_CONTINUE_MARK, GOAL_SET_MARK, GOAL_STOPS, SUPERVISOR_RESUME_MARK,
};
use nexo_core::config::{Config, OnLimit, ProviderConfig};
use nexo_core::error::{ErrorKind, ProviderError};
use nexo_core::fast::{self, Twin};
use nexo_core::jobs::Jobs;
use nexo_core::leak;
use nexo_core::mail::Mail;
use nexo_core::mailbox::Mailbox;
use nexo_core::message::{ImageSource, Message, Part, Role};
use nexo_core::permission::{PermissionDecision, PermissionGate, PermissionMode, PermissionRequest};
use nexo_core::pricing::Pricing;
use nexo_core::problem::{Problem, ProblemCtx};
use nexo_core::provider::{Effort, ModelRequest, ReasoningConfig};
use nexo_core::stream::{Collector, FinishReason, StreamEvent, Usage};
use nexo_core::tool::{Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolProgress, ToolSet};
use nexo_providers::{catalogue, hints};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::audit::{Audit, RequestRecord, ToolRecord, Why};
use crate::budget;
use crate::checkpoint;
use crate::client::{ModelClient, Target};
use crate::compact::{self, Summariser};
use crate::config_edit;
use crate::context::{self, ContextPolicy, View};
use crate::gate::ChannelGate;
use crate::limits::{self, Limits};
use crate::permission::{self, Decision, Policy};
use crate::prompt;
use crate::reader;
use crate::reads::{attached_header, headed_file, included_header, Reads};
use crate::session::{Session, UsageKind, UsageMeta};
use crate::setup;
use crate::AgentSetup;

pub(crate) mod approval;
mod background;
mod cache_ttl;
mod chrome;
mod commands;
mod compaction;
mod connect;
mod dispatch;
mod flow;
mod goal;
mod guards;
mod hook_calls;
mod host;
mod lesson;
mod lifecycle;
mod named;
mod notices;
mod orient;
mod prefix;
mod queue;
mod room;
mod round;
mod runtime;
mod saved_state;
mod settings;
mod shutdown;
mod start;
mod window_fit;
// what a sub-agent's loop shares with the main turn's to build its view
pub(crate) use start::{fixed_tokens, learn_ratio, reasoning_of, view_window};
// and the rule for which calls of one round may overlap
pub(crate) use tools::is_pure_check;
mod state;
mod stop;
mod tools;
mod ultracode;
mod verify;

use hook_calls::*;
use notices::*;
use state::*;

use self::approval::*;
use self::background::*;
use self::commands::*;
use self::compaction::*;
use self::goal::*;
use self::guards::*;
use self::host::*;
use self::lesson::*;
use self::round::*;
use self::runtime::Runtime;
use self::settings::*;
use self::start::*;
use self::tools::*;

/// Lines a `read` without `limit` returns (as the read tool does).
const READ_LIMIT: u64 = 2000;
/// Bytes of a file attached with `@` that are sent whole.
const ATTACHMENT_BYTES: usize = 200_000;
/// Characters of a `!` command's output kept.
const SHELL_CHARS: usize = 30_000;
const SUMMARY_TOKENS: u32 = 4_096;
/// The `compact` policy compacts only when the part it would replace holds, besides an earlier
/// summary, at least this fraction (1/n) of its threshold.
const COMPACT_MARGIN: u64 = 10;
/// Continuation rounds a goal gets after a turn ends without `GOAL DONE` (`NEXO_GOAL_ROUNDS`).
const GOAL_ROUNDS: u32 = 5;
/// What the agent submits by itself to keep working towards the goal.
pub(crate) const GOAL_CONTINUE: &str = GOAL_CONTINUE_MARK;
/// Characters at the end of a reply where `GOAL DONE` or `GOAL WAIT` counts even when it is not the last line.
const GOAL_TAIL: usize = 200;

/// A Stop hook may ask the agent to go on this many times in a row; then the request ends anyway.
const STOP_HOOK_CAP: u32 = 3;

/// Runs the session: `runtime` builds what the loop holds, then each event source has its handler, `dispatch`
/// for a command and `lifecycle` for the rest, until `Shutdown` or the end of the command channel; `shutdown`
/// closes the session.
pub async fn run(ends: AgentEnds, setup: AgentSetup) -> anyhow::Result<()> {
    let AgentEnds { mut commands, events } = ends;
    let mut rt = Runtime::build(events, setup).await?;
    loop {
        tokio::select! {
            cmd = commands.recv() => {
                let Some(cmd) = cmd else { break };
                if dispatch::on_command(&mut rt, cmd).await.is_break() {
                    break;
                }
            }
            Some(r) = rt.resume_rx.recv() => lifecycle::on_resume(&mut rt, r).await,
            change = rt.sub_changes.recv(), if rt.subs_open => lifecycle::on_sub_change(&mut rt, change),
            change = rt.job_changes.recv() => lifecycle::on_job_change(&mut rt, change).await,
            res = lifecycle::turn_ended(&mut rt.turn), if rt.turn.is_some() => lifecycle::on_turn_end(&mut rt, res).await,
        }
    }
    shutdown::close(rt).await;
    Ok(())
}
