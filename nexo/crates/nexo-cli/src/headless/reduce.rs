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
                if self.fmt == Fmt::Text && self.printed {
                    println!();
                    self.printed = false;
                }
                if !matches!(reason, TurnEnd::Done) && self.r.end.is_none() {
                    self.r.end = Some(*reason);
                }
            }
            AgentEvent::Idle { .. } | AgentEvent::Shutdown => return true,
            _ => {}
        }
        false
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
        if let Some(e) = error {
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
}
