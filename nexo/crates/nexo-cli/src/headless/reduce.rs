//! Reducing the agent events of one run to the totals and the result object.

use super::*;

#[derive(Default)]
pub(super) struct Totals {
    pub(super) session_id: String,
    pub(super) model: String,
    pub(super) turns: u32,
    pub(super) usage: nexo_core::Usage,
    pub(super) cost: f64,
}

/// What one request has done so far.
#[derive(Default)]
pub(super) struct Req {
    /// The last assistant message.
    pub(super) text: String,
    /// The first turn end that was not a normal one.
    pub(super) end: Option<TurnEnd>,
    /// The first refusal by the permission policy, as the model saw it: it names the call and what would allow it.
    pub(super) denied: Option<String>,
    pub(super) error: Option<String>,
}

impl Req {
    pub(super) fn code(&self) -> i32 {
        match self.end {
            Some(TurnEnd::Error) => EXIT_FAILED,
            Some(TurnEnd::MaxTurns) => EXIT_MAX_TURNS,
            Some(TurnEnd::Interrupted) => EXIT_INTERRUPTED,
            _ if self.denied.is_some() => EXIT_DENIED,
            _ => 0,
        }
    }
}

/// How a job's kind is worded in the output.
fn kind_word(kind: nexo_core::jobs::JobKind) -> &'static str {
    match kind {
        nexo_core::jobs::JobKind::Shell => "shell",
        nexo_core::jobs::JobKind::Agent => "agent",
    }
}

/// A job's label as one short line for a message: a bash job's label is its whole command, which can be a script.
fn job_label(label: &str) -> String {
    nexo_tools::util::one_line(label, 80)
}

pub(super) struct Run {
    pub(super) fmt: Fmt,
    pub(super) tools: Vec<String>,
    /// Whether the curator offers `recall`.
    pub(super) recall: bool,
    /// The init line waits for the model's window, which decides what the model is offered.
    pub(super) init: Option<Value>,
    pub(super) started: Instant,
    pub(super) t: Totals,
    pub(super) r: Req,
    /// Text printed so far ends in the middle of a line.
    pub(super) printed: bool,
    /// Text mode with a schema prints the validated answer at the end, not the stream.
    pub(super) quiet_text: bool,
    /// What to add when the provider cannot be reached (see `unreachable_note`).
    pub(super) reach: Option<String>,
    /// A turn of this run has started. A resumed session replays its recorded calls before that, for the
    /// interface: they are history, not work this run did, so they are not reported as new calls.
    pub(super) live: bool,
    /// Events held until the startup model limits let stream JSON announce the correct tools.
    pub(super) startup: Vec<AgentEvent>,
    /// The background jobs as the last `Jobs` event listed them; what is still running when the request ends is
    /// what the end kills.
    pub(super) jobs: Vec<nexo_core::jobs::JobInfo>,
    /// The agent is idle with jobs running and the request waits for them (their end starts a follow-up turn).
    pub(super) waiting: bool,
    /// A turn is open: it has been prepared or started and has not ended.
    pub(super) turn_open: bool,
    /// Another request follows the one that just ended, so the jobs still running are not killed by this end.
    pub(super) keeps_jobs: bool,
    /// The last result was built with `keeps_jobs` set: the run can still end with jobs running that it did not name.
    pub(super) last_kept: bool,
    /// The jobs a request still waited for when the next message ended it: their follow-up is not the answer of a
    /// later request, so no later request waits for them. Ids of jobs still running.
    pub(super) carried: Vec<u32>,
}

/// The tools the model is offered (as `turn::offered` hands them out) among `all`, for a window of `window`.
pub(super) fn offered_names(all: &[String], window: Option<u64>, plan: bool, recall: bool) -> Vec<String> {
    all.iter()
        .filter(|n| {
            !nexo_agent::prompt::left_out(window, n)
                && (plan || n.as_str() != "exit_plan")
                && (recall || n.as_str() != "recall")
                && n.as_str() != "goal_brief"
        })
        .cloned()
        .collect()
}

impl Run {
    /// Prints the init line, once, with the tools offered for `window`.
    pub(super) fn send_init(&mut self, window: Option<u64>) {
        let Some(mut v) = self.init.take() else { return };
        let plan = v["permission_mode"] == json!(nexo_core::permission::PermissionMode::Plan);
        v["tools"] = json!(offered_names(&self.tools, window, plan, self.recall));
        line(&v);
    }

    pub(super) fn new(fmt: Fmt, tools: Vec<String>, recall: bool, reach: Option<String>) -> Run {
        Run {
            fmt,
            tools,
            recall,
            init: None,
            started: Instant::now(),
            t: Totals::default(),
            r: Req::default(),
            printed: false,
            quiet_text: false,
            reach,
            live: false,
            startup: Vec::new(),
            jobs: Vec::new(),
            waiting: false,
            turn_open: false,
            keeps_jobs: false,
            last_kept: false,
            carried: Vec::new(),
        }
    }

    /// Handles one agent event; true when the request is over.
    pub(super) fn on_event(&mut self, ev: &AgentEvent) -> bool {
        if self.init.is_some() {
            match ev {
                AgentEvent::ModelLimits { context_window, .. } => {
                    self.send_init(context_window.map(u64::from));
                    for event in std::mem::take(&mut self.startup) {
                        self.on_event(&event);
                    }
                }
                AgentEvent::Idle { .. } | AgentEvent::Shutdown => {
                    self.send_init(None);
                    for event in std::mem::take(&mut self.startup) {
                        self.on_event(&event);
                    }
                }
                _ => {
                    self.startup.push(ev.clone());
                    return false;
                }
            }
        }
        if self.fmt == Fmt::Legacy {
            if let Some(s) = legacy_line(ev) {
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{s}");
            }
        }
        let stream = self.fmt == Fmt::StreamJson;
        match ev {
            AgentEvent::Ready { session_id, provider, model, cwd, permission_mode } => {
                self.t.session_id = session_id.clone();
                self.t.model = model.clone();
                if stream {
                    self.init = Some(json!({
                        "type": "init", "session_id": session_id, "provider": provider, "model": model,
                        "cwd": cwd, "permission_mode": permission_mode,
                    }));
                }
            }
            AgentEvent::ModelLimits { context_window, .. } => self.send_init(context_window.map(u64::from)),
            AgentEvent::SessionChanged { session_id, .. } => self.t.session_id = session_id.clone(),
            AgentEvent::TurnStart { .. } => {
                self.t.turns += 1;
                self.live = true;
                self.turn_open = true;
            }
            AgentEvent::Preparing { .. } => self.turn_open = true,
            AgentEvent::Jobs { jobs } => {
                self.jobs = jobs.clone();
                self.carried.retain(|id| jobs.iter().any(|j| j.id == *id && j.status.running()));
                // a job that ended with no turn to follow it (the model was told of it already, or the session is
                // held) is all the wait is for; with a turn open, that turn's own end decides. Jobs an earlier
                // request left running are not waited for (as at `Idle`); a wait for them alone starts again
                let own = jobs.iter().any(|j| j.status.running() && !self.carried.contains(&j.id));
                if self.waiting && !self.turn_open && !own {
                    self.waiting = false;
                    return true;
                }
            }
            AgentEvent::ToolCall { .. } | AgentEvent::ToolDone { .. } if !self.live => {}
            AgentEvent::AssistantStart => {
                self.r.text.clear();
                if self.fmt == Fmt::Text && self.printed {
                    println!();
                    self.printed = false;
                }
            }
            AgentEvent::TextDelta { text } => {
                self.r.text.push_str(text);
                if stream {
                    line(&json!({"type": "assistant_delta", "text": text}));
                } else if self.fmt == Fmt::Text && !self.quiet_text {
                    let mut out = std::io::stdout().lock();
                    let _ = out.write_all(text.as_bytes());
                    let _ = out.flush();
                    self.printed = !text.ends_with('\n');
                }
            }
            AgentEvent::AssistantDone { message } if stream => {
                line(&json!({"type": "assistant", "text": message.text()}));
            }
            AgentEvent::ToolCall { id, name, args, summary } => {
                if stream {
                    line(&json!({"type": "tool_use", "id": id, "name": name, "input": args, "summary": summary}));
                } else if self.fmt != Fmt::Legacy {
                    eprintln!("· {summary}");
                }
            }
            AgentEvent::ToolDone { id, output, elapsed_ms } => {
                if output.is_error && output.content.starts_with("Permission denied") {
                    self.r.denied.get_or_insert_with(|| output.content.clone());
                }
                if stream {
                    line(&json!({
                        "type": "tool_result", "id": id, "is_error": output.is_error,
                        "content": output.content, "elapsed_ms": elapsed_ms,
                    }));
                }
            }
            AgentEvent::Usage { session, cost_usd, .. } => {
                self.t.usage = *session;
                self.t.cost = *cost_usd;
            }
            AgentEvent::Notice { level, text, problem } => {
                let hinted = with_reach_note(self.reach.as_deref(), *level, text, problem.is_some());
                let text = hinted.as_str();
                // a failure card has several lines; the result's `error` and the stream's `text` stay the one
                // line a script reads (the card itself is the `problem` of the stream, and stderr's text)
                let said = problem.as_ref().map_or_else(|| text.to_string(), |p| p.one_line());
                if *level == Level::Error {
                    self.r.error = Some(said.clone());
                }
                if stream {
                    let mut v = json!({"type": "notice", "level": format!("{level:?}").to_lowercase(), "text": said});
                    if let Some(p) = problem {
                        v["problem"] = json!(p);
                    }
                    line(&v);
                } else if self.fmt != Fmt::Legacy {
                    eprintln!("{}", notice_line(*level, text, problem.is_some(), color_ok()));
                }
            }
            AgentEvent::TurnEnd { reason } => {
                self.turn_open = false;
                if self.fmt == Fmt::Text && self.printed {
                    println!();
                    self.printed = false;
                }
                if !matches!(reason, TurnEnd::Done) && self.r.end.is_none() {
                    self.r.end = Some(*reason);
                }
            }
            AgentEvent::Idle { running_jobs } => {
                self.turn_open = false;
                // jobs still run and the request has not ended badly: the end of a job starts a turn with its
                // result, and that reply is the answer, so the request stays open for it
                // the jobs an earlier request left running are not this request's to wait for
                let own = running_jobs.saturating_sub(self.carried_running());
                if own > 0 && self.live && self.r.end.is_none() {
                    self.waiting = true;
                    // stderr in every format but the old event stream: stream JSON keeps stdout for its own lines
                    if self.fmt != Fmt::Legacy {
                        eprintln!("{}", self.waiting_line(*running_jobs));
                    }
                    return false;
                }
                self.waiting = false;
                if *running_jobs == 0 {
                    self.jobs.clear();
                }
                return true;
            }
            AgentEvent::Shutdown => {
                self.waiting = false;
                return true;
            }
            _ => {}
        }
        false
    }

    /// The jobs the last `Jobs` event listed as running.
    fn running_jobs(&self) -> impl Iterator<Item = &nexo_core::jobs::JobInfo> {
        self.jobs.iter().filter(|j| j.status.running())
    }

    /// How many of the running jobs an earlier request left running.
    fn carried_running(&self) -> usize {
        self.running_jobs().filter(|j| self.carried.contains(&j.id)).count()
    }

    /// The request ends while it waits for jobs because the next message is here: the jobs it waited for are carried.
    pub(super) fn carry_running(&mut self) {
        let ids: Vec<u32> = self.running_jobs().map(|j| j.id).filter(|id| !self.carried.contains(id)).collect();
        self.carried.extend(ids);
    }

    /// True when a job an earlier request left running is still running.
    pub(super) fn has_carried(&self) -> bool {
        self.carried_running() > 0
    }

    /// The input ended and only jobs an earlier request left running remain: a new request waits for them, and the
    /// reply to their end is its answer. False when none runs.
    pub(super) fn wait_for_carried(&mut self) -> bool {
        let n = self.carried_running();
        if n == 0 {
            return false;
        }
        self.r = Req::default();
        self.waiting = true;
        if self.fmt != Fmt::Legacy {
            eprintln!("{}", self.waiting_line(n));
        }
        true
    }

    /// What the script reads on stderr while the request waits for background jobs. Every running job is named, and
    /// there is no time limit: the wait ends when the jobs end or the run is stopped.
    fn waiting_line(&self, running: usize) -> String {
        let named: Vec<String> = self
            .running_jobs()
            .map(|j| format!("job {} {}: {}", j.id, kind_word(j.kind), job_label(&j.label)))
            .collect();
        let who = if named.is_empty() { String::new() } else { format!(" ({})", named.join(", ")) };
        format!(
            "nexo: waiting for {running} background job(s) to end{who} · no time limit · Ctrl+C ends the run and kills them"
        )
    }

    /// The jobs the end of this result kills, as they are in the result (the full label). None when another request
    /// follows (`keeps_jobs`): they go on running then.
    fn killed(&self) -> Vec<Value> {
        if self.keeps_jobs {
            return Vec::new();
        }
        self.running_jobs().map(|j| json!({"id": j.id, "kind": kind_word(j.kind), "label": j.label})).collect()
    }

    /// The line that says which jobs the end of this run killed (labels cut to one short line); None when none was.
    pub(super) fn killed_line(&self) -> Option<String> {
        if self.keeps_jobs {
            return None;
        }
        let each: Vec<String> = self
            .running_jobs()
            .map(|j| format!("job {} ({}) {}", j.id, kind_word(j.kind), job_label(&j.label)))
            .collect();
        (!each.is_empty()).then(|| format!("killed: nexo -p ended · {}", each.join(" · ")))
    }

    /// The last result of a run that ends with jobs still running, which it was built to say nothing of because
    /// another request was to follow: names the jobs the end kills. Call after `keeps_jobs` is cleared.
    pub(super) fn name_killed(&self, last: &mut Value) {
        let killed = self.killed();
        if killed.is_empty() {
            return;
        }
        last["killed_jobs"] = Value::Array(killed);
        if last.get("error").is_none() {
            if let Some(e) = self.killed_line() {
                last["error"] = json!(e);
            }
        }
    }

    /// The result object, for a request that ended with `code`.
    pub(super) fn result(&self, code: i32, text: &str, structured: Option<Value>, error: Option<String>) -> Value {
        let (subtype, stop) = match code {
            0 => ("success", "end_turn"),
            EXIT_SCHEMA => ("error_schema", "end_turn"),
            EXIT_MAX_TURNS => ("error_max_turns", "max_turns"),
            EXIT_DENIED => ("error_permission_denied", "end_turn"),
            EXIT_INTERRUPTED => ("interrupted", "interrupted"),
            _ => ("error", "error"),
        };
        let mut v = json!({
            "type": "result", "subtype": subtype, "is_error": code != 0, "exit_code": code,
            "result": text, "session_id": self.t.session_id, "model": self.t.model,
            "num_turns": self.t.turns,
            "usage": {
                "input_tokens": self.t.usage.input, "output_tokens": self.t.usage.output,
                "cache_read_tokens": self.t.usage.cache_read, "cache_write_tokens": self.t.usage.cache_write,
            },
            "cost_usd": self.t.cost, "duration_ms": self.started.elapsed().as_millis() as u64,
            "stop_reason": stop,
        });
        // the subscription plan the request ran on, when its backend said how much of it is left
        let now = nexo_core::auth::now() as i64;
        let plans: Vec<Value> = nexo_providers::usage::all().iter().map(|u| u.to_json(now)).collect();
        if !plans.is_empty() {
            v["plan_usage"] = Value::Array(plans);
        }
        if let Some(s) = structured {
            v["structured_output"] = s;
        }
        // what this end kills: the jobs still running, named, so a script is never left with no word of them
        let killed = self.killed();
        if !killed.is_empty() {
            v["killed_jobs"] = Value::Array(killed);
        }
        if let Some(e) = error.or_else(|| self.killed_line()) {
            v["error"] = json!(e);
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    pub(super) fn the_calls_a_resumed_session_replays_are_not_reported_as_this_run_s_work() {
        let mut run = super::Run::new(super::Fmt::Text, vec![], false, None);
        let denied = || AgentEvent::ToolDone {
            id: "c1".into(),
            output: nexo_core::ToolOutput::error("Permission denied: rm"),
            elapsed_ms: 0,
        };
        // the replay comes before the first turn of the run
        run.on_event(&denied());
        assert!(run.r.denied.is_none(), "a recorded call counted as one this run made");
        run.on_event(&AgentEvent::TurnStart { turn: 1 });
        run.on_event(&denied());
        assert!(run.r.denied.is_some(), "a call of this run went unreported");
    }
    #[test]
    pub(super) fn the_first_refusal_is_kept_for_the_person_not_a_later_one() {
        let mut run = super::Run::new(super::Fmt::Text, vec![], false, None);
        run.on_event(&AgentEvent::TurnStart { turn: 1 });
        for text in ["Permission denied: bash(rm x) not allowed in a non-interactive run", "Permission denied: write"] {
            run.on_event(&AgentEvent::ToolDone {
                id: "c".into(),
                output: nexo_core::ToolOutput::error(text),
                elapsed_ms: 0,
            });
        }
        assert_eq!(run.r.denied.as_deref(), Some("Permission denied: bash(rm x) not allowed in a non-interactive run"));
        assert_eq!(run.r.code(), EXIT_DENIED);
    }

    fn job(id: u32, kind: nexo_core::jobs::JobKind, label: &str, running: bool) -> nexo_core::jobs::JobInfo {
        let status = if running {
            nexo_core::jobs::JobStatus::Running
        } else {
            nexo_core::jobs::JobStatus::Done { ok: true, how: "exit 0".into() }
        };
        nexo_core::jobs::JobInfo { id, kind, label: label.into(), status, elapsed_ms: 0, lines: 0 }
    }
    fn jobs(list: Vec<nexo_core::jobs::JobInfo>) -> AgentEvent {
        AgentEvent::Jobs { jobs: list }
    }
    fn say(run: &mut super::Run, text: &str) {
        run.on_event(&AgentEvent::AssistantStart);
        run.on_event(&AgentEvent::TextDelta { text: text.into() });
    }

    #[test]
    pub(super) fn idle_with_jobs_running_keeps_the_request_open_until_they_end() {
        use nexo_core::jobs::JobKind::Shell;
        let mut run = super::Run::new(super::Fmt::Json, vec![], false, None);
        run.on_event(&AgentEvent::TurnStart { turn: 1 });
        say(&mut run, "it is running");
        assert!(!run.on_event(&jobs(vec![job(3, Shell, "sleep 1", true)])));
        run.on_event(&AgentEvent::TurnEnd { reason: TurnEnd::Done });
        assert!(!run.on_event(&AgentEvent::Idle { running_jobs: 1 }), "the request ended with a job running");
        // the job ends: a turn for it is prepared before the list shows the end
        assert!(!run.on_event(&AgentEvent::Preparing { what: "preparing the request".into() }));
        assert!(!run.on_event(&jobs(vec![job(3, Shell, "sleep 1", false)])), "the wait ended before the follow-up");
        run.on_event(&AgentEvent::TurnStart { turn: 2 });
        say(&mut run, "the job finished");
        run.on_event(&AgentEvent::TurnEnd { reason: TurnEnd::Done });
        assert!(run.on_event(&AgentEvent::Idle { running_jobs: 0 }));
        let v = run.result(0, &run.r.text.clone(), None, None);
        assert_eq!(v["result"], "the job finished");
        assert_eq!(v["num_turns"], 2);
        assert!(v.get("killed_jobs").is_none() && v.get("error").is_none(), "{v}");
    }

    #[test]
    pub(super) fn a_job_left_running_by_an_earlier_request_does_not_hold_the_wait_of_a_later_one() {
        use nexo_core::jobs::JobKind::Shell;
        let mut run = super::Run::new(super::Fmt::Json, vec![], false, None);
        run.on_event(&jobs(vec![job(1, Shell, "serve", true)]));
        // the next message ended the wait for job 1
        run.carry_running();
        run.on_event(&AgentEvent::TurnStart { turn: 1 });
        say(&mut run, "started another");
        run.on_event(&AgentEvent::TurnEnd { reason: TurnEnd::Done });
        run.on_event(&jobs(vec![job(1, Shell, "serve", true), job(2, Shell, "sleep 1", true)]));
        assert!(!run.on_event(&AgentEvent::Idle { running_jobs: 2 }), "the request waits for job 2");
        // job 2 ends with no turn to follow it: the wait is over although job 1 still runs
        assert!(run.on_event(&jobs(vec![job(1, Shell, "serve", true), job(2, Shell, "sleep 1", false)])));
        assert!(run.has_carried());
    }

    #[test]
    pub(super) fn a_job_that_ends_with_no_follow_up_ends_the_wait() {
        use nexo_core::jobs::JobKind::Agent;
        let mut run = super::Run::new(super::Fmt::Json, vec![], false, None);
        run.on_event(&AgentEvent::TurnStart { turn: 1 });
        say(&mut run, "started it");
        run.on_event(&AgentEvent::TurnEnd { reason: TurnEnd::Done });
        run.on_event(&jobs(vec![job(4, Agent, "review", true)]));
        assert!(!run.on_event(&AgentEvent::Idle { running_jobs: 1 }), "the request ended with a job running");
        // the model was told of this job already: its end sends a list and no turn
        assert!(run.on_event(&jobs(vec![job(4, Agent, "review", false)])));
        assert_eq!(run.r.text, "started it");
    }

    #[test]
    pub(super) fn a_request_that_ends_with_jobs_running_names_each_killed_job() {
        use nexo_core::jobs::JobKind::Shell;
        let mut run = super::Run::new(super::Fmt::Json, vec![], false, None);
        run.on_event(&AgentEvent::TurnStart { turn: 1 });
        run.on_event(&AgentEvent::TurnEnd { reason: TurnEnd::Interrupted });
        run.on_event(&jobs(vec![job(3, Shell, "sleep 60", true)]));
        assert!(run.on_event(&AgentEvent::Idle { running_jobs: 1 }));
        let v = run.result(EXIT_INTERRUPTED, "", None, None);
        assert_eq!(v["killed_jobs"], json!([{"id": 3, "kind": "shell", "label": "sleep 60"}]), "{v}");
        assert_eq!(v["error"], "killed: nexo -p ended · job 3 (shell) sleep 60", "{v}");
        // an error the run already has stays the error
        let v = run.result(EXIT_INTERRUPTED, "", None, Some("boom".into()));
        assert_eq!(v["error"], "boom");
        assert!(v["killed_jobs"].is_array());
    }

    #[test]
    pub(super) fn the_waiting_line_names_every_running_job_even_when_the_last_result_kept_them() {
        use nexo_core::jobs::JobKind::Shell;
        let mut run = super::Run::new(super::Fmt::Json, vec![], false, None);
        run.on_event(&jobs(vec![job(3, Shell, "sleep 60", true), job(4, Shell, "tail -f log", true)]));
        run.keeps_jobs = true;
        let line = run.waiting_line(2);
        assert!(line.contains("job 3 shell: sleep 60") && line.contains("job 4 shell: tail -f log"), "{line}");
        // the same flag hides them from what a result says was killed
        assert!(run.killed().is_empty() && run.killed_line().is_none());
    }

    #[test]
    pub(super) fn a_script_as_a_job_label_is_one_short_line_in_the_messages_and_whole_in_the_result() {
        use nexo_core::jobs::JobKind::Shell;
        let script = format!("cat <<EOF > run.sh\n{}\nEOF\nbash run.sh", "echo hello world; ".repeat(40));
        let mut run = super::Run::new(super::Fmt::Json, vec![], false, None);
        run.on_event(&jobs(vec![job(3, Shell, &script, true)]));
        let line = run.waiting_line(1);
        assert_eq!(line.lines().count(), 1, "{line}");
        assert!(line.len() < 300, "{line}");
        let k = run.killed_line().expect("a job is running");
        assert_eq!(k.lines().count(), 1, "{k}");
        assert!(k.len() < 200, "{k}");
        let v = run.result(EXIT_INTERRUPTED, "", None, None);
        assert_eq!(v["killed_jobs"][0]["label"], script.as_str(), "{v}");
        assert_eq!(v["error"].as_str().unwrap().lines().count(), 1, "{v}");
    }

    #[test]
    pub(super) fn a_last_result_built_with_the_jobs_kept_is_told_of_them_when_the_run_ends() {
        use nexo_core::jobs::JobKind::Shell;
        let mut run = super::Run::new(super::Fmt::Json, vec![], false, None);
        run.on_event(&jobs(vec![job(3, Shell, "sleep 60", true)]));
        // the last request ended (error, turn limit) while more input could still come
        run.keeps_jobs = true;
        let mut last = run.result(EXIT_MAX_TURNS, "", None, None);
        assert!(last.get("killed_jobs").is_none() && last.get("error").is_none(), "{last}");
        // the input ended: the jobs are killed and the result says so
        run.keeps_jobs = false;
        run.name_killed(&mut last);
        assert_eq!(last["killed_jobs"][0]["id"], 3, "{last}");
        assert_eq!(last["error"], "killed: nexo -p ended · job 3 (shell) sleep 60", "{last}");
        // an error the result has stays the error
        let mut with_error = run.result(EXIT_MAX_TURNS, "", None, Some("boom".into()));
        run.name_killed(&mut with_error);
        assert_eq!(with_error["error"], "boom");
    }
}
