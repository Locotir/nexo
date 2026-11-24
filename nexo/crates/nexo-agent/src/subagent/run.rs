use super::*;

/// Times a round whose model call broke off is asked again before the sub-agent gives up.
pub(super) const ROUND_RETRIES: u32 = 3;

fn retry_wait(e: &ProviderError, attempt: u32) -> Duration {
    e.retry_after.unwrap_or(Duration::from_secs(2u64 << attempt))
}

/// Why a request of a round gave no reply.
pub(super) enum Failure {
    /// The person stopped the sub-agent.
    Stopped,
    Model(ProviderError),
    /// The reply broke off at the output limit with no call under way: what it wrote (maybe nothing, when it was
    /// still reasoning), which `rounds` asks again with more room or keeps as the report.
    Cut(String),
    /// A local resource watchdog stopped an uncapped backend. Never resend this round.
    Guarded(&'static str),
}

fn uncapped(cfg: &ProviderConfig) -> bool {
    // Credential shaping happens inside the client, after this decision. A stored
    // credential may add Codex's default even without a login hint. Guard all
    // unresolved store-backed configurations conservatively; explicit false is
    // preserved by login::apply. This is not a remote token/spend guarantee.
    cfg.extra.get("omit_max_tokens").and_then(|v| v.as_bool()).unwrap_or(cfg.auth.is_some())
}

/// A local byte/event/time bound, NOT a token limit or a guarantee of remote spend. Count even
/// ignored reasoning and argument fragments before retaining text, calls or UI deltas. Dropping the
/// stream also drops the parser's partial calls. No partial call may execute after this guard fires.
struct StreamGuard {
    bytes: usize,
    events: usize,
    deadline: tokio::time::Instant,
}

impl StreamGuard {
    fn new(req: &ModelRequest) -> Self {
        Self {
            bytes: (req.max_tokens as usize).saturating_mul(16).clamp(1024, 8 * 1024 * 1024),
            events: (req.max_tokens as usize).saturating_mul(4).clamp(1024, 262_144),
            deadline: tokio::time::Instant::now() + Duration::from_secs(300),
        }
    }

    fn observe(&mut self, ev: &StreamEvent) -> bool {
        use std::io::Write;
        // Serializing into a bounded counter avoids allocating another copy of a completed argument.
        struct Counter(usize);
        impl Write for Counter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if bytes.len() > self.0 {
                    return Err(std::io::Error::other("stream byte budget exhausted"));
                }
                self.0 -= bytes.len();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        if self.events == 0 {
            return false;
        }
        self.events -= 1;
        let size = match ev {
            StreamEvent::TextDelta(d) | StreamEvent::ReasoningDelta(d) | StreamEvent::RedactedReasoning { data: d } => {
                d.len()
            }
            StreamEvent::ToolCallStart { id, name } => id.len().saturating_add(name.len()),
            StreamEvent::ToolCallDelta { id, partial_json } => id.len().saturating_add(partial_json.len()),
            StreamEvent::ToolCallEnd { id, name, args } => {
                let mut counter = Counter(self.bytes);
                if serde_json::to_writer(&mut counter, args).is_err() {
                    return false;
                }
                self.bytes = counter.0;
                id.len().saturating_add(name.len())
            }
            StreamEvent::ReasoningMeta { signature, encrypted } => {
                signature.as_ref().map_or(0, String::len).saturating_add(encrypted.as_ref().map_or(0, String::len))
            }
            StreamEvent::Start { response_id, model } => {
                response_id.as_ref().map_or(0, String::len).saturating_add(model.as_ref().map_or(0, String::len))
            }
            _ => 0,
        };
        match self.bytes.checked_sub(size) {
            Some(left) => {
                self.bytes = left;
                true
            }
            None => false,
        }
    }
}

/// The line a report cut at the output limit ends with: `n` is the size asked for, `why` what stopped the ask
/// again with more room, when there was one.
fn cut_line(n: u32, why: Option<&str>) -> String {
    let again = why.map_or_else(String::new, |w| format!("; asking again with more room failed: {w}"));
    format!("[the report was cut at the output limit of {n} tokens; the rest was not written{again}]")
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

/// Reserve opaque blocks in every turn, including nested tool results. The generic estimate
/// excludes old signatures, although a worker's history may still send them back. Count all
/// protocols conservatively rather than guessing which serializer will omit a block.
fn admission_media(parts: &[Part]) -> (u64, bool) {
    parts.iter().fold((0u64, false), |(total, images), part| {
        let (reserve, image) = match part {
            Part::Reasoning { signature, encrypted, .. } if signature.is_some() || encrypted.is_some() => {
                (serde_json::to_vec(part).map_or(u64::MAX, |bytes| bytes.len() as u64), false)
            }
            Part::RedactedReasoning { .. } => {
                (serde_json::to_vec(part).map_or(u64::MAX, |bytes| bytes.len() as u64), false)
            }
            Part::ToolResult { content, .. } => admission_media(content),
            Part::Image { .. } => (0, true),
            _ => (0, false),
        };
        (total.saturating_add(reserve), images || image)
    })
}

/// Admit the final, protocol-repaired request, not the policy's token estimate. Plain and Compact do
/// not enforce a window, and a failed policy falls back to the full history. Keep every input word:
/// shorten only the reply, or refuse before reserving money or opening a provider stream.
/// Returns the raw input estimate (for usage learning) and the physical room for a retry's reply.
fn admit_request(
    req: &mut ModelRequest,
    limits: &crate::limits::Limits,
    learned: Option<f64>,
) -> Result<(u64, u32), String> {
    let (opaque, images) = req.messages.iter().fold((0u64, false), |(total, images), message| {
        let (reserve, has_image) = admission_media(&message.content);
        (total.saturating_add(reserve), images || has_image)
    });
    let estimated = crate::turn::fixed_tokens(&req.system, &req.tools)
        .saturating_add(nexo_core::message::estimate_tokens(&req.messages))
        .saturating_add(opaque);
    let mut room = limits.max_output.unwrap_or(u32::MAX);
    if let Some(window) = limits.context_window {
        // Match the main turn's conservative first estimate for small windows. A provider count
        // learned on a previous round replaces it; never let a policy's stale count admit a request.
        let ratio = learned.unwrap_or(if crate::prompt::is_compact(Some(u64::from(window))) { 4.0 / 3.0 } else { 1.0 });
        // Learned prose ratios cannot discount the nominal image or opaque reserve.
        let ratio = if images || opaque > 0 { ratio.max(1.0) } else { ratio };
        let input = (estimated as f64 * ratio).ceil() as u64;
        let floor = req.max_tokens.min(crate::limits::REPLY_FLOOR);
        if input.saturating_add(u64::from(floor)) > u64::from(window) {
            return Err(format!(
                "sub-agent request not sent: preserving its messages needs about {input} input tokens \
                 for a {window}-token context window, plus {floor} for the reply; ask for a narrower task or shorter message"
            ));
        }
        room = room.min((u64::from(window) - input) as u32);
    }
    req.max_tokens = req.max_tokens.min(room);
    // Also needed with an unknown window: adapters may otherwise increase a deliberately small
    // reply, and a known model output cap must still hold.
    req.output_ceiling = Some(req.max_tokens);
    Ok((estimated, room))
}

#[cfg(test)]
mod admission_tests {
    use super::*;

    fn request() -> ModelRequest {
        let mut req = ModelRequest::new("m", "system", vec![Message::user("words ".repeat(1000))]);
        req.max_tokens = 1024;
        req
    }

    #[test]
    fn only_the_reply_is_shortened_and_the_learned_count_is_respected() {
        let mut req = request();
        let original = serde_json::to_value(&req.messages).unwrap();
        let estimate =
            crate::turn::fixed_tokens(&req.system, &req.tools) + nexo_core::message::estimate_tokens(&req.messages);
        let input = estimate * 2;
        let limits = crate::limits::Limits { context_window: Some(input as u32 + 400), ..Default::default() };
        let (sent, room) = admit_request(&mut req, &limits, Some(2.0)).unwrap();
        assert_eq!(sent, estimate);
        assert_eq!(room, 400);
        assert_eq!(req.max_tokens, 400);
        assert_eq!(req.output_ceiling, Some(400));
        assert_eq!(serde_json::to_value(&req.messages).unwrap(), original);
        assert!(admit_request(&mut request(), &limits, Some(3.0)).unwrap_err().contains("request not sent"));
    }

    #[test]
    fn worker_admission_preserves_nested_media_and_old_opaque_reserves() {
        use nexo_core::message::{ImageSource, Role};
        let image = Part::Image { source: ImageSource::Url { url: "https://example.test/image".into() } };
        let nested = Part::ToolResult { call_id: "c".into(), name: None, content: vec![image], is_error: false };
        let mut req = ModelRequest::new("m", "system", vec![Message::new(Role::User, vec![nested])]);
        req.max_tokens = 300;
        let plain =
            crate::turn::fixed_tokens(&req.system, &req.tools) + nexo_core::message::estimate_tokens(&req.messages);
        let limits = crate::limits::Limits { context_window: Some(plain as u32 + 255), ..Default::default() };
        assert!(admit_request(&mut req, &limits, Some(0.1)).is_err());

        let old = Part::RedactedReasoning { data: "x".repeat(4000), model: None };
        let mut req =
            ModelRequest::new("m", "system", vec![Message::new(Role::Assistant, vec![old]), Message::user("next")]);
        req.max_tokens = 300;
        let plain =
            crate::turn::fixed_tokens(&req.system, &req.tools) + nexo_core::message::estimate_tokens(&req.messages);
        let limits = crate::limits::Limits { context_window: Some(plain as u32 + 1000), ..Default::default() };
        assert!(admit_request(&mut req, &limits, Some(0.1)).is_err());
    }

    #[test]
    fn an_unknown_window_still_has_a_hard_model_output_cap() {
        let mut req = request();
        let limits = crate::limits::Limits { max_output: Some(300), ..Default::default() };
        let (_, room) = admit_request(&mut req, &limits, None).unwrap();
        assert_eq!(room, 300);
        assert_eq!(req.max_tokens, 300);
        assert_eq!(req.output_ceiling, Some(300));
    }
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
    pub(super) effort: Option<String>,
    pub(super) checkout: Option<PathBuf>,
    pub(super) skill: Option<(String, String)>,
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
    /// The reply size it asks for (`[agent] max_tokens`), under its model's limits.
    pub(super) max_tokens: u32,
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
        let mut cfg = match &self.provider {
            Some((_, cfg)) if other => cfg.clone(),
            _ => main_cfg,
        };
        // the window and prices of the model it runs on: its view is kept under the one and priced at the other
        let name = self.provider.as_ref().map_or(main_provider.as_str(), |(n, _)| n.as_str());
        // The same effective level is published and used for every request of this run.
        if let Some(asked) = &self.effort {
            cfg.effort.insert(model.clone(), asked.clone());
        }
        let effort = crate::setup::effective_effort(&cfg, &model, &None);
        if let Some(note) = crate::setup::effort_clamp_note(&cfg, &model, &None) {
            say(note);
        }
        if let Some(sub) = &self.sub {
            sub.target(name, &model, effort.as_deref());
        }
        let limits = crate::limits::resolve(name, &cfg, &model, None);
        // one set of prices for the reserve and for the bill, so what is held back is what is spent
        let prices = limits.pricing.or_else(|| cfg.pricing_for(&model));
        say(format!(
            "on {name}/{model} · effort {} (request setting)",
            effort.as_deref().unwrap_or("provider default/unknown")
        ));
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
        // the reply it asks for: the main agent's size, under its model's own limits; the task is cut to what its
        // window leaves
        let reply = limits.reply_room(self.max_tokens, 1);
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
        if let Some((name, arguments)) = &self.skill {
            if ctx.cancel.is_cancelled() {
                return Err("skill loading cancelled".into());
            }
            let args = serde_json::json!({"name": name, "arguments": arguments});
            let gate = tokio::sync::Mutex::new(());
            let done = self.call_record("initial-skill", "skill", &args, ctx, say, &gate).await;
            if done.out.is_error {
                return Err(format!("cannot load skill `{name}`: {}", done.out.content));
            }
            let args = done.args;
            let out = done.out;
            // Record the actual loader call/result in the private worker history. In particular,
            // do not concatenate file instructions into a user message (or a system prompt).
            messages.push(Message::new(Role::Assistant, vec![Part::tool_call("initial-skill", "skill", args)]));
            messages.push(Message::tool_results(vec![Part::ToolResult {
                call_id: "initial-skill".into(),
                name: Some("skill".into()),
                content: vec![Part::text(out.content)],
                is_error: false,
            }]));
            if let Some(w) = limits.context_window {
                let used = system_tokens + nexo_core::message::estimate_tokens(&messages) + u64::from(reply);
                if used > u64::from(w) {
                    return Err(format!("skill `{name}` does not fit the worker context window"));
                }
            }
        }
        let opening = messages[0].clone();
        let mut tok_ratio: Option<f64> = None;
        // one id for every round of this run: its own prefix, apart from the parent's and the other runs'
        let cache_id = format!("{}-sub-{}", ctx.session_id, &uuid::Uuid::new_v4().simple().to_string()[..8]);
        for round in 0..=rounds {
            // what a person wrote to this sub-agent joins it here, between its rounds
            let mut inbox_words = Vec::new();
            if let Some(s) = &self.sub {
                for item in s.take_inbox() {
                    let text = match &item {
                        SubItem::Parent { text } => from_parent(text),
                        SubItem::Person { text } => text.clone(),
                        _ => continue,
                    };
                    s.item(item);
                    inbox_words.push(Message::user(text.clone()));
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
            if !inbox_words.is_empty() {
                // A policy may turn a large new user block into a recall pointer. Refuse an
                // impossible mailbox delivery before that can hide its size or original words.
                inbox_words.insert(0, opening.clone());
                let mut minimum = ModelRequest::new(model.clone(), system.clone(), inbox_words);
                minimum.tools = specs.clone();
                minimum.max_tokens = reply.min(crate::limits::REPLY_FLOOR);
                admit_request(&mut minimum, &limits, tok_ratio)
                    .map_err(|why| format!("{why}{}", progress(&messages, round, rounds)))?;
            }
            policy.tools(&names);
            if let Some(w) = limits.context_window.filter(|w| *w > reply) {
                let ratio = tok_ratio.or_else(|| compact.then_some(4.0 / 3.0));
                policy.window(crate::turn::view_window(w - reply, ratio));
            }
            if let Some(p) = prices {
                policy.prices(p.input, p.cache_read);
            }
            policy.calls_left(Some((rounds - round + 1) as u64));
            let view = match policy.view(system_tokens, &messages).await {
                Ok(v) => v,
                Err(e) => {
                    say(format!("context policy failed ({e}); checking the full history against the window"));
                    crate::context::View {
                        tokens: nexo_core::message::estimate_tokens(&messages),
                        messages: messages.clone(),
                        note: None,
                        memory: None,
                    }
                }
            };
            // A curator must not discard the explicitly requested skill before the first request.
            let view = if round == 0 && self.skill.is_some() {
                crate::context::View {
                    tokens: nexo_core::message::estimate_tokens(&messages),
                    messages: messages.clone(),
                    note: None,
                    memory: None,
                }
            } else {
                view
            };
            let (sent, _) = nexo_core::message::well_formed(view.messages);
            let mut req = ModelRequest::new(model.clone(), system.clone(), sent);
            req.tools = specs.clone();
            req.max_tokens = reply;
            req.session_id = Some(cache_id.clone());
            // the level set for this model holds for its sub-agents too (their own model's, when it differs)
            req.reasoning = crate::turn::reasoning_of(&effort);
            let (sent_tokens, output_room) = admit_request(&mut req, &limits, tok_ratio)
                .map_err(|why| format!("{why}{}", progress(&messages, round, rounds)))?;
            // a round whose reply broke off is asked again whole: no call of it ran yet, so nothing is
            // done twice, and the rounds before it (files written, commands run) are not thrown away
            let mut tries = 0u32;
            // a reply cut at the output limit with no call: its text and the size it was cut at, kept while the
            // same round is asked again with more room
            let mut first_cut: Option<(String, u32)> = None;
            // the report this round ends with was cut: the size asked for, and why it was not asked again
            let mut cut_at: Option<(u32, Option<String>)> = None;
            let (text, calls) = loop {
                // the spend limits hold for a sub-agent as for the main agent; the hold lasts until the call is billed
                let hold = match &self.host {
                    Some(h) => match h.reserve(&model, prices, sent_tokens, req.max_tokens).await {
                        Ok((cap, hold)) => {
                            req.max_tokens = req.max_tokens.min(cap);
                            Some(hold)
                        }
                        // what it did stays with the failure, as for any other one of a round; a cut reply that
                        // could not be asked again is the report instead
                        Err(why) => match first_cut.take() {
                            Some((kept, n)) => {
                                cut_at = Some((n, Some(why)));
                                break (kept, Vec::new());
                            }
                            None => return Err(format!("{why}{}", progress(&messages, round, rounds))),
                        },
                    },
                    None => None,
                };
                // For adapters accepting a limit, preserve admission and the spend reservation. Refresh on
                // every attempt, including the larger retry after a reply cut at the output limit.
                req.output_ceiling = Some(req.max_tokens);
                let (got, usage) = self.ask(&cfg, &req, ctx).await;
                let mut cost = 0.0;
                if let (Some(h), false) = (&self.host, usage.is_empty()) {
                    cost = h.spent(&model, usage, prices, other).await;
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
                    Err(Failure::Guarded(why)) => {
                        return Err(format!(
                            "sub-agent stopped by local stream guard ({why}); this backend does not accept a client \
                         output limit. The stream was dropped without executing this round's calls or retrying. \
                         Remote generation/cost may continue and final usage may be unavailable.{}",
                            progress(&messages, round, rounds)
                        ))
                    }
                    Err(Failure::Cut(t)) => {
                        let n = req.max_tokens;
                        match first_cut.take() {
                            // cut again: the longer try is the report, or the first one when it wrote nothing
                            Some((first, n1)) => {
                                let (kept, at) =
                                    if t.trim().is_empty() && !first.trim().is_empty() { (first, n1) } else { (t, n) };
                                cut_at = Some((at, None));
                                break (kept, Vec::new());
                            }
                            None => {
                                // twice the room, and no more than the window leaves after what is sent
                                let room = limits.reply_room(self.max_tokens, 2).min(output_room);
                                // (a reserve that lowered the first ask would lower this one too)
                                if uncapped(&cfg) || room <= n || n < reply {
                                    cut_at = Some((n, None));
                                    break (t, Vec::new());
                                }
                                first_cut = Some((t, n));
                                req.max_tokens = room;
                            }
                        }
                    }
                    Err(Failure::Model(e))
                        if !uncapped(&cfg) && self.client.worth_retrying(&e) && tries < ROUND_RETRIES =>
                    {
                        tries += 1;
                        // Never shorten a server's requested delay. The client decides whether it is worth waiting.
                        let wait = retry_wait(&e, tries);
                        say(format!("the model call broke off ({}); asking again in {}s", e.human(), wait.as_secs()));
                        tokio::select! {
                            _ = tokio::time::sleep(wait) => {}
                            _ = ctx.cancel.cancelled() => return Err(stopped(&messages)),
                        }
                    }
                    Err(Failure::Model(e)) => match first_cut.take() {
                        // the ask for more room failed: the text it had is the report, and the note says why
                        Some((kept, n)) => {
                            cut_at = Some((n, Some(e.human())));
                            break (kept, Vec::new());
                        }
                        None => {
                            return Err(format!(
                                "the sub-agent's model call failed: {}{}",
                                e.human(),
                                progress(&messages, round, rounds)
                            ));
                        }
                    },
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
                    Err(if let Some((n, why)) = &cut_at {
                        let again = why.as_ref().map(|w| format!(" (asking again with more room failed: {w})"));
                        format!(
                            "the sub-agent's reply was cut at the output limit of {n} tokens while it was still \
                             reasoning; it wrote no report{}{}",
                            again.unwrap_or_default(),
                            progress(&messages, usize::MAX, 0)
                        )
                    } else if last {
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
                    // a report that broke off says so, with the size it was cut at
                    let said = match &cut_at {
                        Some((n, why)) => format!("{said}\n\n{}", cut_line(*n, why.as_deref())),
                        None => said,
                    };
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
                    // the hooks and permission asks of the group's calls come one at a time, in order, as in the main
                    // turn; only their runs overlap
                    let gate = tokio::sync::Mutex::new(());
                    let gate = &gate;
                    let runs = calls[i..end].iter().map(|(id, name, a)| async move {
                        if name == "recall" {
                            crate::recall::answer(seen, a)
                        } else {
                            self.call(id, name, a, ctx, say, gate).await
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
                        output: if name == "skill" { "[worker skill]".into() } else { out.content.clone() },
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
        let mut guard = uncapped(cfg).then(|| StreamGuard::new(req));
        let deadline = guard.as_ref().map(|g| g.deadline);
        let timed_out = async {
            match deadline {
                Some(at) => tokio::time::sleep_until(at).await,
                None => futures::future::pending::<()>().await,
            }
        };
        tokio::pin!(timed_out);
        let opened = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => return (Err(Failure::Stopped), usage),
            _ = &mut timed_out => return (Err(Failure::Guarded("request deadline")), usage),
            // The round owns its retry budget, including failures after partial output. Disabling
            // client retries prevents multiplying attempts and resetting the server's delay policy.
            result = self.client.stream(cfg, req, 0) => result,
        };
        let mut stream = match opened {
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
                _ = &mut timed_out => return (Err(Failure::Guarded("request deadline")), usage),
                ev = stream.next() => ev,
            };
            // Account every received bill BEFORE a resource guard can reject its event.
            // Never invent usage for events not received or a missing final remote bill.
            if let Some(StreamEvent::Usage(u) | StreamEvent::RetriedUsage(u)) = &ev {
                usage += *u;
            }
            if let Some(g) = &mut guard {
                if let Some(StreamEvent::Usage(u)) = &ev {
                    if u.output.max(u.reasoning) > u64::from(req.max_tokens) {
                        return (Err(Failure::Guarded("reported output exceeded local allowance")), usage);
                    }
                }
                if ev.as_ref().is_some_and(|ev| !g.observe(ev)) {
                    return (Err(Failure::Guarded("byte/event allowance")), usage);
                }
            }
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
                Some(StreamEvent::Usage(_) | StreamEvent::RetriedUsage(_)) => {}
                Some(StreamEvent::Error(e)) => return (Err(Failure::Model(e)), usage),
                Some(StreamEvent::Done) | None => {
                    if cut {
                        // a call cut off is dropped by the parser: what is left would pass as the report
                        if open > 0 {
                            let e = ProviderError::new(ErrorKind::Parse, "the reply was cut off at the output limit");
                            return (Err(Failure::Model(e)), usage);
                        }
                        // text and no call: the round decides between asking again and keeping it as the report
                        if calls.is_empty() {
                            return (Err(Failure::Cut(text)), usage);
                        }
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
        gate: &tokio::sync::Mutex<()>,
    ) -> ToolOutput {
        self.call_record(id, name, args, ctx, say, gate).await.out
    }

    async fn call_record(
        &self,
        id: &str,
        name: &str,
        args: &Value,
        ctx: &ToolContext,
        say: &(dyn Fn(String) + Send + Sync),
        gate: &tokio::sync::Mutex<()>,
    ) -> Done {
        let t0 = std::time::Instant::now();
        let done = self.call_one(id, name, args, ctx, say, gate).await;
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
        done
    }

    pub(super) async fn call_one(
        &self,
        id: &str,
        name: &str,
        args: &Value,
        ctx: &ToolContext,
        say: &(dyn Fn(String) + Send + Sync),
        gate: &tokio::sync::Mutex<()>,
    ) -> Done {
        // hooks and the permission ask take turns: calls that overlap ask one at a time, in the order they came
        let turn = gate.lock().await;
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
            // Network access requires the parent's permission policy; without a host there is no policy to ask.
            None if self.profile.read_only && t.kind() != ToolKind::Network && name != "web_search" => Ok(Why::RULE),
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
        // A file-backed skill is a second, separately authorized read. Keep hooks and
        // permission questions serialized until it has been checked.
        let skill = if name == "skill" {
            match self.worker_skill(&call_id, t.as_ref(), args, ctx).await {
                Ok(skill) => Some(skill),
                Err((reason, why)) => return Done::denied(ToolOutput::error(reason), args.clone(), name, why),
            }
        } else {
            None
        };
        drop(turn);
        // the stop can come while the hooks or the permission were waited for; the tools that watch the token
        // end on their own, the others (a write, an edit) would still land after it
        if ctx.cancel.is_cancelled() {
            return Done { out: interrupted(), args: args.clone(), summary, why, denied: false };
        }
        say(format!("→ {summary}"));
        let started = std::time::Instant::now();
        // as the main turn does: a tool that does not watch the token itself (the browser, a file read) is
        // left when the person stops, so one slow call does not hold the round and the calls started with it
        let mut out = if let Some(skill) = skill {
            match skill.instructions(args.get("arguments").and_then(Value::as_str).unwrap_or("")) {
                Ok(text) => ToolOutput::ok(text),
                Err(_) => ToolOutput::error("cannot safely read the skill file"),
            }
        } else if t.handles_cancel() {
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
            let response = if name == "skill" { "[worker skill]" } else { &out.content };
            fields["output"] = json!(response);
            fields["tool_response"] = json!(response);
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

#[cfg(test)]
mod late_login_guard_tests {
    use super::*;
    use nexo_core::model_client::ModelClient;
    use nexo_core::provider::EventStream;

    struct LateLogin(bool);
    #[async_trait]
    impl ModelClient for LateLogin {
        async fn stream(
            &self,
            cfg: &ProviderConfig,
            req: &ModelRequest,
            retries: u32,
        ) -> Result<EventStream, ProviderError> {
            assert_eq!(retries, 0, "late Codex credentials must not enable hidden retries");
            assert!(!cfg.extra.contains_key("omit_max_tokens"));
            // The real credential shaper inserts the flag only inside the client.
            let cred = nexo_core::auth::Credential::Oauth {
                access: "test-only".into(),
                refresh: String::new(),
                expires: 0,
                metadata: [("flow".into(), "codex".into())].into(),
            };
            let effective = nexo_providers::login::apply(cfg, &cred);
            assert!(uncapped(&effective));
            let body = nexo_providers::openai_responses::OpenAiResponses::body(&effective, req);
            assert!(body.get("max_output_tokens").is_none());
            let evs = (0..1024).map(|_| StreamEvent::ReasoningDelta(String::new())).chain([
                if self.0 {
                    StreamEvent::RetriedUsage(Usage { input: 11, output: 7, ..Default::default() })
                } else {
                    StreamEvent::Usage(Usage { input: 11, output: 7, ..Default::default() })
                },
                StreamEvent::Done,
            ]);
            Ok(Box::pin(futures::stream::iter(evs)))
        }
    }

    #[tokio::test]
    async fn late_login_is_guarded_and_the_usage_crossing_event_limit_is_preserved() {
        let cfg = ProviderConfig { auth: Some("stored-login".into()), ..Default::default() };
        assert!(uncapped(&cfg));
        for retried in [false, true] {
            let sub = Run {
                client: Arc::new(LateLogin(retried)),
                target: Target::new("p", cfg.clone(), "m"),
                provider: None,
                model: None,
                effort: None,
                checkout: None,
                skill: None,
                tools: ToolSet::new(),
                profile: profile::builtin().remove(0),
                host: None,
                claims: Default::default(),
                label: String::new(),
                id: String::new(),
                sub: None,
                context: None,
                max_tokens: 128,
            };
            let mut req = ModelRequest::new("m", "", vec![]);
            req.max_tokens = 128;
            let (result, usage) = sub.ask(&cfg, &req, &ToolContext::new(std::env::temp_dir())).await;
            assert!(matches!(result, Err(Failure::Guarded("byte/event allowance"))));
            assert_eq!((usage.input, usage.output), (11, 7));
        }
        let mut capped = cfg;
        capped.extra.insert("omit_max_tokens".into(), toml::Value::Boolean(false));
        assert!(!uncapped(&capped), "explicit false is respected by credential shaping too");
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use nexo_core::model_client::ModelClient;
    use nexo_core::provider::EventStream;
    use std::sync::Mutex;

    struct Failing {
        delay: Duration,
        ceiling: Duration,
        in_stream: bool,
        attempts: Mutex<Vec<tokio::time::Instant>>,
    }

    #[async_trait]
    impl ModelClient for Failing {
        async fn stream(
            &self,
            _: &ProviderConfig,
            _: &ModelRequest,
            retries: u32,
        ) -> Result<EventStream, ProviderError> {
            assert_eq!(retries, 0, "only the round may retry");
            self.attempts.lock().unwrap().push(tokio::time::Instant::now());
            let mut e = ProviderError::new(ErrorKind::RateLimit, "busy");
            e.retry_after = Some(self.delay);
            if self.in_stream {
                Ok(Box::pin(futures::stream::iter(vec![
                    StreamEvent::TextDelta("partial".into()),
                    StreamEvent::Error(e),
                ])))
            } else {
                Err(e)
            }
        }
        fn worth_retrying(&self, e: &ProviderError) -> bool {
            e.is_retryable() && e.retry_after.is_none_or(|delay| delay <= self.ceiling)
        }
    }

    fn worker(client: Arc<Failing>) -> SubAgent {
        SubAgent::new(
            client,
            Target::new("p", ProviderConfig::default(), "m"),
            &ToolSet::new(),
            Vec::new(),
            Default::default(),
        )
    }

    #[tokio::test]
    async fn retry_budget_and_server_delay_apply_before_and_during_stream() {
        for in_stream in [false, true] {
            let delay = Duration::from_millis(30);
            let client =
                Arc::new(Failing { delay, ceiling: Duration::from_secs(120), in_stream, attempts: Mutex::new(vec![]) });
            let ctx = ToolContext::new(std::env::temp_dir());
            let out = worker(client.clone()).run(json!({"prompt":"test", "agent":"explore"}), &ctx).await;
            assert!(out.is_error);
            let attempts = client.attempts.lock().unwrap();
            assert_eq!(attempts.len(), 4);
            for pair in attempts.windows(2) {
                assert!(pair[1].duration_since(pair[0]) >= delay);
            }
        }
    }

    #[test]
    fn server_wait_is_never_shortened() {
        let mut e = ProviderError::new(ErrorKind::RateLimit, "busy");
        for seconds in [60, 120, 600] {
            e.retry_after = Some(Duration::from_secs(seconds));
            assert_eq!(retry_wait(&e, 1), Duration::from_secs(seconds));
        }
        e.retry_after = None;
        assert_eq!(retry_wait(&e, 1), Duration::from_secs(4));
    }

    #[tokio::test]
    async fn long_retry_after_obeys_ceiling_and_cancellation() {
        for seconds in [60, 120, 600] {
            let client = Arc::new(Failing {
                delay: Duration::from_secs(seconds),
                ceiling: Duration::from_secs(120),
                in_stream: false,
                attempts: Mutex::new(vec![]),
            });
            let ctx = ToolContext::new(std::env::temp_dir());
            let cancel = ctx.cancel.clone();
            let cancel_task = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(40)).await;
                cancel.cancel();
            });
            let out = worker(client.clone()).run(json!({"prompt":"test", "agent":"explore"}), &ctx).await;
            cancel_task.abort();
            assert!(out.is_error);
            assert_eq!(client.attempts.lock().unwrap().len(), 1, "retry after {seconds}s");
            if seconds <= 120 {
                assert!(ctx.cancel.is_cancelled());
            } else {
                assert!(!ctx.cancel.is_cancelled());
            }
        }
    }
}
