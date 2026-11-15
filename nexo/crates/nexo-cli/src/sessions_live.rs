//! `nexo sessions live`: the sessions whose heartbeat is fresh now, with the jobs they run. It reads what the
//! mailbox already keeps for its peers and the sessions folder for the provider and model; it opens no
//! conversation and prints no message, prompt or tool output.

use std::path::Path;

use serde::Serialize;

use nexo_core::mailbox::{live_sessions, LiveJob, LiveSession};

/// Characters of a title printed.
const TITLE_MAX: usize = 80;

/// One live session as `--json` prints it.
#[derive(Debug, Serialize)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub cwd: String,
    /// `busy` while a turn runs, `idle` between turns, `unknown` when the heartbeat does not say.
    pub state: &'static str,
    pub pid: u32,
    pub started_ms: i64,
    pub last_beat_ms: i64,
    pub beat_age_ms: i64,
    pub jobs: Vec<LiveJob>,
}

/// The live sessions under `mail` at `now` (ms), joined with their provider, model and title from `sessions`.
pub fn entries(mail: &Path, sessions: &Path, now: i64) -> Vec<Entry> {
    live_sessions(mail, now)
        .into_iter()
        .map(|s: LiveSession| {
            let meta = nexo_agent::session::meta_in(sessions, &s.id);
            // a title given with `nexo sessions rename` wins over the name in the heartbeat
            let title = meta.as_ref().map(|m| m.title.clone()).filter(|t| !t.is_empty()).unwrap_or(s.name);
            let title: String = title.chars().filter(|c| !c.is_control()).take(TITLE_MAX).collect();
            Entry {
                id: s.id,
                title,
                provider: meta.as_ref().map(|m| m.provider.clone()),
                model: meta.map(|m| m.model),
                cwd: s.cwd,
                state: match s.busy {
                    Some(true) => "busy",
                    Some(false) => "idle",
                    None => "unknown",
                },
                pid: s.pid,
                started_ms: s.started_ms,
                last_beat_ms: s.beat_ms,
                beat_age_ms: (now - s.beat_ms).max(0),
                jobs: s.jobs,
            }
        })
        .collect()
}

/// The text form: a line per session and an indented line per job.
pub fn text(list: &[Entry]) -> String {
    if list.is_empty() {
        return "no live sessions\n".to_string();
    }
    let mut out = String::new();
    for e in list {
        let model = match (&e.provider, &e.model) {
            (Some(p), Some(m)) => format!("{p}/{m}"),
            _ => "-".to_string(),
        };
        out.push_str(&format!(
            "{} {} {} {} beat {}s ago {}\n",
            e.id,
            e.state,
            model,
            e.cwd,
            e.beat_age_ms / 1000,
            e.title
        ));
        for j in &e.jobs {
            let kind = match j.kind {
                nexo_core::jobs::JobKind::Shell => "shell",
                nexo_core::jobs::JobKind::Agent => "agent",
            };
            out.push_str(&format!("  job {} {} {} {}\n", j.id, kind, j.state, j.title));
        }
    }
    out
}

pub fn run(json: bool) -> i32 {
    let list = entries(&nexo_core::mailbox::mail_root(), &nexo_agent::session::dir(), nexo_core::mailbox::now_ms());
    if json {
        println!("{}", serde_json::to_string(&list).unwrap_or_else(|_| "[]".into()));
    } else {
        print!("{}", text(&list));
    }
    0
}
