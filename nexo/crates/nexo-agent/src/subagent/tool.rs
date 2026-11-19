use super::*;

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
                "prompt": {"type": "string", "description": "The task, with what the sub-agent needs to know. It does not see this conversation; nexo hands it the passages of it that bear on the task (files read, failing commands, the person's constraints), so name the files and symbols involved rather than pasting them. A non-empty prompt is required."},
                "agent": {"type": "string", "enum": names, "description": "The kind of sub-agent."},
                "background": {"type": "boolean", "description": "Run it as a background job and go on at once."},
                "start_clean": {"type": "boolean", "description": "No briefing, hook context or shared-memory tools; profile and project tools remain."},
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
                ("prompt", "Task; required and non-empty"),
                ("background", "Run as a job"),
                ("start_clean", "No inherited context"),
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
        let start_clean = match args.get("start_clean") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => return ToolOutput::error("`start_clean` must be a boolean"),
        };
        let launch = Launch {
            agent: name,
            prompt,
            provider: asked_for("provider"),
            model: asked_for("model"),
            description: args.get("description").and_then(Value::as_str),
            title: None,
            background: asked,
            detachable: true,
            start_clean,
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
