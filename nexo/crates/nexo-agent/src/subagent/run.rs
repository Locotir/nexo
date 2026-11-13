use super::*;

/// Times a round whose model call broke off is asked again before the sub-agent gives up.
pub(super) const ROUND_RETRIES: u32 = 3;
/// The longest wait before asking again, whatever the server said.
pub(super) const MAX_WAIT: Duration = Duration::from_secs(60);

/// Why a request of a round gave no reply.
pub(super) enum Failure {
    /// The person stopped the sub-agent.
    Stopped,
    Model(ProviderError),
}

/// A message a person sent a sub-agent joins its conversation as a user message: after the tool results it
/// answers, else as a message of its own.
pub(super) fn join_person(messages: &mut Vec<Message>, text: String) {
    match messages.last_mut() {
        Some(m) if m.role == Role::User => m.content.push(Part::text(text)),
        _ => messages.push(Message::user(text)),
    }
}

pub(super) fn stopped(messages: &[Message]) -> String {
    format!("stopped{}", progress(messages, usize::MAX, 0))
}

/// What a sub-agent that ends without a report had done: its calls (their effects stay on disk) and
/// its last words, so whoever started it goes on from there instead of doing it all again. Empty when
/// it had done nothing.
pub(super) fn progress(messages: &[Message], round: usize, rounds: usize) -> String {
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

/// A tool call of a sub-agent for the audit log: what it was, who let it run, how it ended.
pub struct AuditCall<'a> {
    pub tool: &'a str,
    pub args: &'a Value,
    /// What `Tool::summary` says of the call.
    pub summary: &'a str,
    pub why: Why,
    /// `ok`, `error` or `denied`.
    pub status: &'a str,
    pub duration_ms: u64,
}

/// How one call ended.
pub(super) struct Done {
    pub(super) out: ToolOutput,
    /// The input it ran with (a hook may have replaced it).
    pub(super) args: Value,
    pub(super) summary: String,
    pub(super) why: Why,
    /// Refused before it ran.
    pub(super) denied: bool,
}

impl Done {
    pub(super) fn denied(out: ToolOutput, args: Value, name: &str, why: Why) -> Done {
        Done { out, args, summary: name.to_string(), why, denied: true }
    }
}

/// One sub-agent at work.
pub(super) struct Run {
    pub(super) client: Arc<dyn ModelClient>,
    pub(super) target: Target,
    /// Asked for this sub-agent: another provider (name and configuration) and another model.
    pub(super) provider: Option<(String, ProviderConfig)>,
    pub(super) model: Option<String>,
    pub(super) tools: ToolSet,
    pub(super) profile: Profile,
    pub(super) host: Option<Arc<dyn Host>>,
    /// The files of the sub-agents at work, and this one's label in them.
    pub(super) claims: Claims,
    pub(super) label: String,
    /// This run's own id (the task call's id and the job's): the owner of its claims and the start of the
    /// ids of its questions. A provider's call id alone can repeat, so it never names a run.
    pub(super) id: String,
    /// Its conversation, kept for whoever watches it, and its inbox (see [`nexo_core::subs`]).
    pub(super) sub: Option<SubRun>,
}

impl Run {
    /// The report, or why there is none. `say` gets a line per step.
    pub(super) async fn go(
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
        // a model the managed policy forbids does not run, as a sub-agent either
        {
            let on = self.provider.as_ref().map_or(main_provider.as_str(), |(n, _)| n.as_str());
            if let Some(p) = nexo_core::managed::current() {
                p.check_model(on, &model)?;
            }
        }
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
        let specs = self.tools.specs_for(false);
        // one id for every round of this run: its own prefix, apart from the parent's and the other runs'
        let cache_id = format!("{}-sub-{}", ctx.session_id, &uuid::Uuid::new_v4().simple().to_string()[..8]);
        for round in 0..=rounds {
            // what a person wrote to this sub-agent joins it here, between its rounds
            if let Some(s) = &self.sub {
                for t in s.take_inbox() {
                    s.item(SubItem::Person { text: t.clone() });
                    join_person(&mut messages, t);
                }
            }
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
            req.session_id = Some(cache_id.clone());
            // a round whose reply broke off is asked again whole: no call of it ran yet, so nothing is
            // done twice, and the rounds before it (files written, commands run) are not thrown away
            let mut tries = 0u32;
            let (text, calls) = loop {
                let (got, usage) = self.ask(&cfg, &req, ctx).await;
                let mut cost = 0.0;
                if let (Some(h), false) = (&self.host, usage.is_empty()) {
                    cost = h.spent(&model, usage, pricing).await;
                }
                if let Some(s) = &self.sub {
                    s.spent(&usage, cost);
                }
                match got {
                    Ok(reply) => break reply,
                    Err(Failure::Stopped) => return Err(stopped(&messages)),
                    Err(Failure::Model(e)) if e.is_retryable() && tries < ROUND_RETRIES => {
                        tries += 1;
                        let wait = e.retry_after.unwrap_or(Duration::from_secs(2u64 << tries)).min(MAX_WAIT);
                        say(format!("the model call broke off ({}); asking again in {}s", e.human(), wait.as_secs()));
                        tokio::select! {
                            _ = tokio::time::sleep(wait) => {}
                            _ = ctx.cancel.cancelled() => return Err(stopped(&messages)),
                        }
                    }
                    Err(Failure::Model(e)) => {
                        return Err(format!(
                            "the sub-agent's model call failed: {}{}",
                            e.human(),
                            progress(&messages, round, rounds)
                        ))
                    }
                }
            };
            if !text.is_empty() {
                if let Some(s) = &self.sub {
                    s.item(SubItem::Text { text: text.clone() });
                }
            }
            // a person wrote while it was answering: it reads that before it ends
            if calls.is_empty() && !last && self.sub.as_ref().is_some_and(|s| s.has_mail()) {
                if !text.trim().is_empty() {
                    messages.push(Message::new(Role::Assistant, vec![Part::text(text)]));
                }
                continue;
            }
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
                if let Some(s) = &self.sub {
                    let summary = self.tools.get(&name).map_or_else(|| name.clone(), |t| t.summary(&a));
                    s.item(SubItem::Call { id: id.clone(), name: name.clone(), summary });
                }
                let out = self.call(&id, &name, &a, ctx, say).await;
                if let Some(s) = &self.sub {
                    s.item(SubItem::Result { id: id.clone(), ok: !out.is_error, bytes: out.content.len() });
                }
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
    pub(super) async fn ask(
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
            // biased: a stop that is already asked for wins over a reply that is ready
            let ev = tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => return (Err(Failure::Stopped), usage),
                ev = stream.next() => ev,
            };
            match ev {
                Some(StreamEvent::TextDelta(d)) => {
                    if let Some(s) = &self.sub {
                        s.delta(&d);
                    }
                    text.push_str(&d);
                }
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

    /// One call of the sub-agent: its hooks, its permission, its run, and its line in the audit log.
    pub(super) async fn call(
        &self,
        id: &str,
        name: &str,
        args: &Value,
        ctx: &ToolContext,
        say: &(dyn Fn(String) + Send + Sync),
    ) -> ToolOutput {
        let t0 = std::time::Instant::now();
        let done = self.call_one(id, name, args, ctx, say).await;
        if let Some(h) = &self.host {
            let status = if done.denied {
                "denied"
            } else if done.out.is_error {
                "error"
            } else {
                "ok"
            };
            let call = AuditCall {
                tool: name,
                args: &done.args,
                summary: &done.summary,
                why: done.why,
                status,
                duration_ms: t0.elapsed().as_millis() as u64,
            };
            h.audit(call).await;
        }
        done.out
    }

    pub(super) async fn call_one(
        &self,
        id: &str,
        name: &str,
        args: &Value,
        ctx: &ToolContext,
        say: &(dyn Fn(String) + Send + Sync),
    ) -> Done {
        // the hooks of the main turn hold here too: PreToolUse may block a call, decide its permission or
        // replace its input, and PostToolUse adds a note after the output (they are told which sub-agent)
        let mut args = args.clone();
        let mut decision = None;
        if let Some(h) = &self.host {
            let o = h.hook("pre_tool", name, self.fields(name, &args)).await;
            if let Some(why) = o.blocked {
                let out = ToolOutput::error(format!("Blocked by a hook: {why}"));
                return Done::denied(out, args, name, Why::denied("hook"));
            }
            if let Some(u) = o.updated_input {
                args = u;
            }
            decision = o.decision;
        }
        let Some(t) = self.tools.get(name) else {
            let can = if self.profile.read_only { " (it can only read)" } else { "" };
            let out = ToolOutput::error(format!("the sub-agent has no tool `{name}`{can}"));
            return Done::denied(out, args, name, Why::denied("rule"));
        };
        let summary = t.summary(&args);
        let args = &args;
        let refused = |out: ToolOutput| Done {
            out,
            args: args.clone(),
            summary: summary.clone(),
            why: Why::denied("rule"),
            denied: true,
        };
        if self.profile.read_only {
            if let Some(p) = outside(args, &ctx.cwd) {
                return refused(ToolOutput::error(format!("`{p}` is outside the project")));
            }
        }
        // a file another sub-agent at work is changing stays its own: two at once would lose a change
        // (every file the call names: an `edit` of several files or a `paths` list claims each of them)
        let paths = if t.kind() == ToolKind::Write { nexo_core::tool::call_paths(args) } else { Vec::new() };
        if !paths.is_empty() {
            let keys: Vec<PathBuf> = paths.iter().map(|p| plain_path(p, &ctx.cwd)).collect();
            if let Err((i, label)) = self.claims.hold_all(&keys, &self.id, &self.label) {
                let p = &paths[i];
                return refused(ToolOutput::error(format!(
                    "`{p}` is being changed by another sub-agent running at the same time ({label}); \
                     leave it to that one and say so in your report"
                )));
            }
        }
        // the main agent's rules hold for every sub-agent: its deny rules, and its asks (keys, writes)
        let call_id = format!("{}.{id}", self.id);
        let verdict = match &self.host {
            Some(h) => h.permit(&call_id, &self.profile.name, t.as_ref(), args, &ctx.cancel, decision).await,
            None if self.profile.read_only => Ok(Why::RULE),
            None => Err(("no one to ask for leave".into(), Why::denied("rule"))),
        };
        let why = match verdict {
            Ok(why) => why,
            Err((reason, why)) => {
                return Done {
                    out: ToolOutput::error(format!("Permission denied: {reason}")),
                    args: args.clone(),
                    summary,
                    why,
                    denied: true,
                };
            }
        };
        if let Some(h) = &self.host {
            for p in &paths {
                let path = if Path::new(p).is_absolute() { PathBuf::from(p) } else { ctx.cwd.join(p) };
                h.before_write(path).await;
            }
        }
        say(format!("→ {summary}"));
        let started = std::time::Instant::now();
        let mut out = t.run(args.clone(), ctx).await;
        if let Some(h) = &self.host {
            let mut fields = self.fields(name, args);
            fields["output"] = json!(out.content);
            fields["tool_response"] = json!(out.content);
            fields["is_error"] = json!(out.is_error);
            fields["elapsed_ms"] = json!(started.elapsed().as_millis() as u64);
            let o = h.hook("post_tool", name, fields).await;
            let notes: Vec<String> = o.context.into_iter().chain(o.blocked).collect();
            if !notes.is_empty() {
                out.content = format!("{}\n\n[hook] {}", out.content, notes.join("\n[hook] "));
            }
        }
        Done { out, args: args.clone(), summary, why, denied: false }
    }

    /// The payload of a tool hook: the call, and which sub-agent makes it.
    pub(super) fn fields(&self, name: &str, args: &Value) -> Value {
        let mut f = crate::hooks::tool_fields(name, args);
        f["agent"] = json!(self.profile.name);
        f
    }
}
