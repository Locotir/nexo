use super::*;

/// Times a round whose model call broke off is asked again before the sub-agent gives up.
pub(super) const ROUND_RETRIES: u32 = 3;
/// The longest wait before asking again, whatever the server said.
pub(super) const MAX_WAIT: Duration = Duration::from_secs(60);
/// The tokens a sub-agent asks for in a reply; its model's own limits (a quarter of the window, its largest
/// reply) can lower it, as they do the main agent's.
pub(super) const REPLY_TOKENS: u32 = 4096;

/// Why a request of a round gave no reply.
pub(super) enum Failure {
    /// The person stopped the sub-agent.
    Stopped,
    Model(ProviderError),
}

/// The least tokens of its task a sub-agent is given, however small its window.
const TASK_FLOOR_TOKENS: u64 = 256;

/// `prompt` as a sub-agent whose model has `window` tokens (`None`: unknown, nothing is cut) can be given it: at
/// most half of what its view leaves after the system prompt and tools (`fixed` tokens), the reply (`reply`) and
/// the briefing (`brief` tokens), so the rounds that follow have the other half. A task that does not fit keeps
/// its head, and the second value says how much was cut.
fn fit_task(prompt: &str, window: Option<u32>, reply: u32, fixed: u64, brief: u64) -> (String, Option<String>) {
    let Some(w) = window else { return (prompt.to_string(), None) };
    let view = crate::turn::view_window(w.saturating_sub(reply), None);
    let room = (view.saturating_sub(fixed) / 2).saturating_sub(brief).max(TASK_FLOOR_TOKENS);
    let cap = usize::try_from(room).unwrap_or(usize::MAX).saturating_mul(4);
    let whole = prompt.chars().count();
    if whole <= cap {
        return (prompt.to_string(), None);
    }
    let kept = crate::prompt::fit::head(prompt, cap);
    let note = format!("task cut to {} of {whole} characters for a {w}-token window", kept.chars().count());
    (kept.to_string(), Some(note))
}

/// A message the person or the parent sent a sub-agent joins its conversation as a user message: after the
/// tool results it answers, else as a message of its own.
pub(super) fn join_person(messages: &mut Vec<Message>, text: String) {
    match messages.last_mut() {
        Some(m) if m.role == Role::User => m.content.push(Part::text(text)),
        _ => messages.push(Message::user(text)),
    }
}

/// A message from the agent that started the sub-agent, as its model reads it: whose it is, then the text.
fn from_parent(text: &str) -> String {
    format!("[nexo: a message from the agent that started you, not from the person]\n{text}")
}

pub(super) fn stopped(messages: &[Message]) -> String {
    format!("stopped{}", progress(messages, usize::MAX, 0))
}

/// The answer to a call the person's stop came before: the text the main turn gives such a call, so the call
/// and its result still pair up.
fn interrupted() -> ToolOutput {
    ToolOutput::error("The call did not run: interrupted by the person.")
}

/// What a sub-agent that ends without a report had done: what its tool log shows of its calls (their effects
/// stay on disk) and its last words, so whoever started it goes on from there instead of doing it all again.
/// Empty when it had done nothing.
pub(super) fn progress(messages: &[Message], round: usize, rounds: usize) -> String {
    let fx = ledger::Effects::of(messages);
    let said = messages
        .iter()
        .rfind(|m| m.role == Role::Assistant && m.content.iter().any(|p| matches!(p, Part::Text { .. })))
        .map(Message::text)
        .unwrap_or_default();
    if fx.calls == 0 {
        return String::new();
    }
    let at = if round == usize::MAX { String::new() } else { format!(" in round {}/{rounds}", round + 1) };
    let mut out = format!(
        "{at}. Before that it made {} calls; what they changed stays as they left it, so go on from there rather \
         than start over:\n",
        fx.calls
    );
    for l in fx.lines() {
        out.push_str(&format!("- {l}\n"));
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
    /// The session's context policy, run over this sub-agent's own messages (none: everything is sent).
    pub(super) context: Option<nexo_core::config::ContextConfig>,
}

impl Run {
    /// The report, or why there is none. `say` gets a line per step. When the task did not fit the model's
    /// window and was cut, the report (or the failure) ends with that fact, as the sub-agent's first message has it.
    pub(super) async fn go(
        &self,
        prompt: &str,
        brief: Option<&Brief>,
        ctx: &ToolContext,
        say: &(dyn Fn(String) + Send + Sync),
    ) -> Result<String, String> {
        let mut cut = None;
        let res = self.rounds(prompt, brief, ctx, say, &mut cut).await;
        match (res, cut) {
            (Ok(report), Some(c)) => Ok(format!("{report}\n\n{c}")),
            (Err(e), Some(c)) => Err(format!("{e}\n\n{c}")),
            (res, None) => res,
        }
    }

    /// [`Run::go`]; `cut` gets the line that says the task was cut to fit the window, when it was.
    async fn rounds(
        &self,
        prompt: &str,
        brief: Option<&Brief>,
        ctx: &ToolContext,
        say: &(dyn Fn(String) + Send + Sync),
        cut: &mut Option<String>,
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
        let other = matches!(&self.provider, Some((name, _)) if *name != main_provider);
        let cfg = match &self.provider {
            Some((_, cfg)) if other => cfg.clone(),
            _ => main_cfg,
        };
        // the window and prices of the model it runs on: its view is kept under the one and priced at the other
        let name = self.provider.as_ref().map_or(main_provider.as_str(), |(n, _)| n.as_str());
        if let Some(sub) = &self.sub {
            sub.target(name, &model);
        }
        let limits = crate::limits::resolve(name, &cfg, &model, None);
        let pricing = if other { limits.pricing } else { None };
        let prices = limits.pricing.or_else(|| cfg.pricing_for(&model));
        if self.provider.is_some() || self.model.is_some() {
            say(format!("on {name}/{model}"));
        }
        let rounds = self.profile.rounds;
        let system = format!("{BASE}\nYou have {rounds} rounds of tool calls.\n\n{}", self.profile.prompt);
        // what it sends is the session's context policy over its own messages, as in the main turn: everything
        // with `plain`, and with the curator what it keeps, under the model's window and priced over the rounds
        // left (a cut the run ends before paying back is not made)
        let mut policy: Box<dyn crate::context::ContextPolicy> = match &self.context {
            Some(c) => crate::context::from_config(c),
            None => Box::new(crate::context::Plain::default()),
        };
        // the tool text of a small window, and without the tools it leaves out, as the main agent is offered them
        let window = limits.context_window.map(u64::from);
        let compact = crate::prompt::is_compact(window);
        let share = compact.then(|| nexo_core::tool::mcp_share(window));
        let mut specs: Vec<_> = self
            .tools
            .specs_with(compact, share)
            .into_iter()
            .filter(|s| !crate::prompt::left_out(window, &s.name))
            .collect();
        // the MCP tools it has by name and one line: a failed call to one answers with its whole schema
        let short = share.and_then(|b| self.tools.mcp_shortened(true, b)).map_or_else(Vec::new, |s| s.names());
        // a policy that leaves messages out offers `recall`, answered from this run's own messages
        if policy.recall() && !specs.iter().any(|s| s.name == "recall") {
            // in the text of a small window when the window is small, as the main agent has it; whole otherwise
            if crate::prompt::is_compact(window) {
                let recall = ToolSet::new().with(std::sync::Arc::new(nexo_tools::recall::Recall));
                specs.extend(recall.specs_for(true));
            } else {
                specs.push(nexo_tools::recall::Recall.spec());
            }
        }
        let names: Vec<String> = specs.iter().map(|s| s.name.clone()).collect();
        let system_tokens = crate::turn::fixed_tokens(&system, &specs);
        // the reply it asks for, under its model's own limits; the task is cut to what its window leaves
        let reply = limits.max_tokens(REPLY_TOKENS);
        let brief_tokens = brief.map_or(0, |b| b.tokens as u64);
        let (task, note) = fit_task(prompt, limits.context_window, reply, system_tokens, brief_tokens);
        if let Some(n) = &note {
            say(n.clone());
            *cut = Some(n.clone());
        }
        let mut parts = vec![];
        if let Some(b) = brief {
            parts.push(Part::text(b.text.clone()));
        }
        parts.push(Part::text(task));
        if let Some(n) = note {
            parts.push(Part::text(format!("[nexo: {n}]")));
        }
        // a task about a compiled program starts with the line a message of the person's about one carries, for the
        // tools this run is offered (see `ask_hint.rs`): the sub-agent sees no start note of the main agent's. The
        // `debug` of a read-only kind only looks at and steers a session another agent started (`tools_for`), so it
        // runs nothing; without `bin` the line names `nexo bin` in the shell, which a run without `bash` cannot call
        let has = |t: &str| names.iter().any(|n| n == t);
        let offer = crate::hint_cmd::Offer { bin: has("bin"), debug: has("debug") && !self.profile.read_only };
        if let Some(l) = (offer.bin || has("bash")).then(|| crate::ask_hint::line(prompt, &ctx.cwd, offer)).flatten() {
            parts.push(Part::text(l));
        }
        let mut messages = vec![Message::new(Role::User, parts)];
        let mut tok_ratio: Option<f64> = None;
        // one id for every round of this run: its own prefix, apart from the parent's and the other runs'
        let cache_id = format!("{}-sub-{}", ctx.session_id, &uuid::Uuid::new_v4().simple().to_string()[..8]);
        for round in 0..=rounds {
            // what a person wrote to this sub-agent joins it here, between its rounds
            if let Some(s) = &self.sub {
                for item in s.take_inbox() {
                    let text = match &item {
                        SubItem::Parent { text } => from_parent(text),
                        SubItem::Person { text } => text.clone(),
                        _ => continue,
                    };
                    s.item(item);
                    join_person(&mut messages, text);
                }
            }
            // the rounds are over: one more request, told to report what it has
            let last = round == rounds;
            if last {
                if let Some(m) = messages.last_mut() {
                    m.content.push(Part::text(LAST_ROUND));
                }
            }
            policy.tools(&names);
            if let Some(w) = limits.context_window.filter(|w| *w > reply) {
                policy.window(crate::turn::view_window(w - reply, tok_ratio));
            }
            if let Some(p) = prices {
                policy.prices(p.input, p.cache_read);
            }
            policy.calls_left(Some((rounds - round + 1) as u64));
            let view = match policy.view(system_tokens, &messages).await {
                Ok(v) => v,
                Err(e) => {
                    say(format!("context policy failed ({e}); sending everything"));
                    crate::context::View {
                        tokens: nexo_core::message::estimate_tokens(&messages),
                        messages: messages.clone(),
                        note: None,
                        memory: None,
                    }
                }
            };
            let sent_tokens = system_tokens + view.tokens;
            let (sent, _) = nexo_core::message::well_formed(view.messages);
            let mut req = ModelRequest::new(model.clone(), system.clone(), sent);
            req.tools = specs.clone();
            req.max_tokens = reply;
            req.session_id = Some(cache_id.clone());
            // the level set for this model holds for its sub-agents too (their own model's, when it differs)
            req.reasoning = crate::turn::reasoning_of(&crate::setup::effective_effort(&cfg, &model, &None));
            // a round whose reply broke off is asked again whole: no call of it ran yet, so nothing is
            // done twice, and the rounds before it (files written, commands run) are not thrown away
            let mut tries = 0u32;
            let (text, calls) = loop {
                // the spend limits hold for a sub-agent as for the main agent; the hold lasts until the call is billed
                let hold = match &self.host {
                    Some(h) => {
                        let (cap, hold) = h.reserve(&model, prices, sent_tokens, req.max_tokens).await?;
                        req.max_tokens = cap;
                        Some(hold)
                    }
                    None => None,
                };
                let (got, usage) = self.ask(&cfg, &req, ctx).await;
                let mut cost = 0.0;
                if let (Some(h), false) = (&self.host, usage.is_empty()) {
                    cost = h.spent(&model, usage, pricing).await;
                }
                drop(hold);
                if let Some(s) = &self.sub {
                    s.spent(&usage, cost);
                }
                if got.is_ok() && !usage.is_empty() {
                    policy.usage(usage.cache_read, usage.total_input());
                    tok_ratio = crate::turn::learn_ratio(tok_ratio, sent_tokens, usage.total_input());
                }
                if got.is_err() {
                    if let Some(s) = &self.sub {
                        s.reset_partial();
                    }
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
                        ));
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
                        format!("the sub-agent answered nothing{}", progress(&messages, usize::MAX, 0))
                    })
                } else {
                    let said = brief::compact_report(report, brief.map(|b| b.text.as_str()), brief::REPORT_TOKENS);
                    let did = ledger::Effects::of(&messages).block();
                    let said = if did.is_empty() { said } else { format!("{said}\n\n{did}") };
                    // A report forced by the last round looks like a finished one; the parent is told, with the
                    // live round count, so it can tell what was settled from what was cut off.
                    Ok(if last { format!("{said}\n\n{}", cut_note(rounds)) } else { said })
                };
            }
            let mut asked = vec![];
            if !text.is_empty() {
                asked.push(Part::text(text));
            }
            // every call is asked for and shown first: the calls of a run that overlaps start together, so
            // their lines come before any of their results
            for (id, name, a) in &calls {
                asked.push(Part::tool_call(id, name, a.clone()));
                if let Some(s) = &self.sub {
                    let summary = self.tools.get(name).map_or_else(|| name.clone(), |t| t.summary(a));
                    s.item(SubItem::Call { id: id.clone(), name: name.clone(), summary, args: a.to_string() });
                }
            }
            // as the main turn does: reads, searches, page fetches and pure checks overlap; any other call
            // (and `recall`, which reads this run's own messages) runs alone, in its place in the order
            let overlaps: Vec<bool> = calls
                .iter()
                .map(|(_, name, a)| {
                    name != "recall"
                        && self.tools.get(name).is_some_and(|t| {
                            matches!(t.call_kind(a), ToolKind::Read | ToolKind::Search | ToolKind::Network)
                                || crate::turn::is_pure_check(name, a)
                        })
                })
                .collect();
            let mut outs: Vec<Option<ToolOutput>> = calls.iter().map(|_| None).collect();
            let mut i = 0;
            while i < calls.len() {
                let end = if overlaps[i] { i + overlaps[i..].iter().take_while(|o| **o).count() } else { i + 1 };
                // once the person has stopped it, the calls left are not run (a write would land after the stop)
                if ctx.cancel.is_cancelled() {
                    for out in &mut outs[i..end] {
                        *out = Some(interrupted());
                    }
                } else {
                    let seen = &messages;
                    let runs = calls[i..end].iter().map(|(id, name, a)| async move {
                        if name == "recall" {
                            crate::recall::answer(seen, a)
                        } else {
                            self.call(id, name, a, ctx, say).await
                        }
                    });
                    for (k, out) in futures::future::join_all(runs).await.into_iter().enumerate() {
                        outs[i + k] = Some(out);
                    }
                }
                i = end;
            }
            let mut results = vec![];
            for ((id, name, _), out) in calls.into_iter().zip(outs) {
                let out = out.unwrap_or_else(interrupted);
                let out = match self.tools.get(&name) {
                    Some(t) if out.is_error => nexo_core::tool::with_schema(t.as_ref(), &short, out),
                    _ => out,
                };
                if let Some(s) = &self.sub {
                    s.item(SubItem::Result {
                        id: id.clone(),
                        ok: !out.is_error,
                        bytes: out.content.len(),
                        output: out.content.clone(),
                    });
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
            // stopped during the round: no request of the next one is opened (it would be billed)
            if ctx.cancel.is_cancelled() {
                return Err(stopped(&messages));
            }
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
        // with an agent above it, the main agent's rules decide where it reads, as for the main agent
        // (keys and credentials asked for); alone, nothing outside the project
        if self.profile.read_only && self.host.is_none() {
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
                return refused(ToolOutput::error(ledger::claim_refusal(p, &label)));
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
        // the stop can come while the hooks or the permission were waited for; the tools that watch the token
        // end on their own, the others (a write, an edit) would still land after it
        if ctx.cancel.is_cancelled() {
            return Done { out: interrupted(), args: args.clone(), summary, why, denied: false };
        }
        say(format!("→ {summary}"));
        let started = std::time::Instant::now();
        // as the main turn does: a tool that does not watch the token itself (the browser, a file read) is
        // left when the person stops, so one slow call does not hold the round and the calls started with it
        let mut out = if t.handles_cancel() {
            t.run(args.clone(), ctx).await
        } else {
            tokio::select! {
                o = t.run(args.clone(), ctx) => o,
                _ = ctx.cancel.cancelled() => {
                    return Done { out: interrupted(), args: args.clone(), summary, why, denied: false };
                }
            }
        };
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
