//! Part of the agent loop; see the parent module.

use super::*;

/// Asks the vendor for its model list in the background and fills what the
/// limits still lack (context window, largest reply, prices).
/// Weighs every background shell command that runs past `job_review` seconds with one tool-less request apart
/// from the conversation, which never waits for it: STOP ends the job (the model learns why from its last
/// line), anything else lets it run and looks again at twice the time. Ends with the session.
pub(super) fn spawn_job_review(state: &Shared, jobs: Jobs) {
    let weak = Arc::downgrade(state);
    tokio::spawn(async move {
        // job -> (seconds of the next look, lines it had at the last one)
        let mut due: HashMap<u32, (u64, usize)> = HashMap::new();
        // the first look comes soon, to read the setting
        let mut tick = 1;
        loop {
            tokio::time::sleep(Duration::from_secs(tick)).await;
            let Some(state) = weak.upgrade() else { return };
            let (first, client, target, retries, request, events) = {
                let st = state.lock().await;
                let a = &st.config.agent;
                (
                    a.job_review as u64,
                    st.client.clone(),
                    st.target.clone(),
                    a.retries,
                    crate::review::request_of(&st.history),
                    st.events.clone(),
                )
            };
            drop(state);
            tick = crate::review::TICK_S.min(first.max(1));
            let running: Vec<_> = jobs
                .list()
                .into_iter()
                .filter(|j| j.kind == nexo_core::jobs::JobKind::Shell && j.status.running())
                .collect();
            due.retain(|id, _| running.iter().any(|j| j.id == *id));
            if first == 0 {
                continue;
            }
            let tails = jobs.tails();
            for j in running {
                let secs = j.elapsed_ms / 1000;
                let (next, seen) = *due.entry(j.id).or_insert((first, 0));
                if secs < next {
                    continue;
                }
                let tail = tails.iter().find(|t| t.id == j.id).map(|t| t.text.as_str()).unwrap_or("");
                let ask = crate::review::prompt(&j.label, secs, j.lines, j.lines.saturating_sub(seen), tail, &request);
                match crate::review::ask(&*client, &target, retries, ask).await {
                    Ok((stop, usage, model)) => {
                        let Some(state) = weak.upgrade() else { return };
                        add_usage(
                            &mut *state.lock().await,
                            &events,
                            &model,
                            usage,
                            Usage::default(),
                            UsageKind::Review,
                        );
                        if stop && jobs.stop(j.id, "a review judged it no longer worth waiting for") {
                            let took = nexo_core::jobs::duration(j.elapsed_ms);
                            notice(&events, Level::Info, format!("job {} stopped by the review after {took}", j.id));
                            continue;
                        }
                    }
                    Err(e) => notice(&events, Level::Warn, format!("the review of job {} failed: {e}", j.id)),
                }
                due.insert(j.id, (secs.max(1) * 2, j.lines));
            }
        }
    });
}

/// Sent after the last request of the conversation and its reply, to ask for a prompt suggestion.
pub(super) const SUGGEST_ASK: &str =
    "Suggest the next message the user is most likely to send, in their language and voice, \
     at most 12 words. Reply with only that message, or with nothing if unclear.";

/// The longest reply of a prompt suggestion's request.
pub(super) const SUGGEST_TOKENS: u32 = 200;

/// A side request that needs a few words: no thinking first (or the least the model takes, where off is not a
/// level it has), and a cap that keeps it cheap on any model.
pub(super) fn lean(req: &mut ModelRequest, levels: &[String], tokens: u32) {
    let has = |l: &str| levels.iter().any(|x| x == l);
    req.reasoning = Some(if has("minimal") && !has("off") && !has("none") {
        ReasoningConfig::Effort(Effort::Minimal)
    } else if has("low") && !has("off") && !has("none") {
        ReasoningConfig::Effort(Effort::Low)
    } else {
        ReasoningConfig::Off
    });
    req.max_tokens = tokens;
}

/// The request of a prompt suggestion: the last request of the conversation as it was sent (the same system
/// prompt, tools, tool choice and messages, so the provider's cache of it is hit), the reply it got, and one
/// more message from the person that asks for the suggestion.
pub(super) fn suggest_request(last: &ModelRequest, reply: Message, levels: &[String]) -> ModelRequest {
    let mut req = last.clone();
    req.messages.push(reply);
    req.messages.push(Message::user(SUGGEST_ASK));
    lean(&mut req, levels, SUGGEST_TOKENS);
    req
}

/// A suggestion as one line without the quotes a model puts round it; None when it holds nothing.
pub(super) fn tidy_suggestion(text: &str) -> Option<String> {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let line = line.trim_matches(|c| matches!(c, '"' | '\'' | '`' | '“' | '”')).trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// `AgentCommand::Suggest`: a guess at the person's next message, in a task of its own so the chat never waits
/// for it. It is let go when the reply calls a tool or holds nothing, or when a new request started (or the
/// conversation changed) while the call ran.
/// What a check-in asks the loop to do: resume the session, with `text` as hidden context. `turns` and `epoch`
/// tell the request it was made for, so an answer that arrives after anything else happened is let go.
pub(super) struct Resume {
    pub(super) turns: u32,
    pub(super) epoch: u64,
    pub(super) text: String,
}

pub(super) const CHECK_IN_TOKENS: u32 = 400;

/// Adds a note to the request about to be sent, never to the saved conversation: to the last message when it is
/// the person's or holds tool results (as the guards' notes do), else as a message of its own.
pub(super) fn attach_hidden(messages: &mut Vec<Message>, text: String) {
    match messages.last_mut() {
        Some(m) if m.role == Role::User => m.content.push(Part::text(text)),
        _ => messages.push(Message::user(text)),
    }
}

/// The supervisor's plain counting over the request in progress, after a tool round: what it sees for the first
/// time goes on the next request as a hidden note (`agent.watch`).
pub(super) async fn watch_round(state: &Shared) {
    let mut st = state.lock().await;
    if !st.config.agent.watch {
        return;
    }
    let from = st.turn_start.min(st.history.len());
    for f in crate::supervise::watch::scan_rounds(&st.history[from..]) {
        if st.noticed.insert(f.key) {
            st.hidden.push(f.text);
        }
    }
}

/// A stopped session whose work looks unfinished (or that the supervisor asks to look at itself): one background
/// request to its own model, with the evidence, no tools offered to act on and no trace in the conversation.
/// CONTINUE resumes the session through `tx`; DONE, or an answer that is not clear, changes nothing. At most one
/// per request of the person, and none when something else will wake the session.
pub(super) async fn check_in(state: &Shared, extra: Option<String>, tx: &tokio::sync::mpsc::UnboundedSender<Resume>) {
    let (req, cfg, client, retries, events, stamp) = {
        let mut st = state.lock().await;
        let a = &st.config.agent;
        if !a.check_in || st.checked_in || st.perm.mode == PermissionMode::Plan {
            return;
        }
        if (st.goal.is_some() && st.goal_state.is_active())
            || !st.inbox.is_empty()
            || !st.mail_wait.is_empty()
            || st.jobs.running() > 0
            || !st.jobs.untold().is_empty()
        {
            return;
        }
        let spent = budget::verdict(st.cost, budget::spent_today(), st.budget, a.max_cost_day);
        if matches!(spent, budget::Verdict::Stop(_)) {
            return;
        }
        let Some(last) = st.last_request.clone() else { return };
        let Some(reply) = st.history.last().filter(|m| m.role == Role::Assistant && m.tool_calls().next().is_none())
        else {
            return;
        };
        let (_, cfg, model) = st.target.get();
        if last.model != model {
            return;
        }
        let from = st.turn_start.min(st.history.len());
        let mut evidence: Vec<String> = extra.map(|e| e.chars().take(1000).collect()).into_iter().collect();
        evidence
            .extend(crate::supervise::watch::scan_end(&st.history[from..], &reply.text()).into_iter().map(|f| f.text));
        evidence.retain(|e| !e.trim().is_empty());
        if evidence.is_empty() {
            return;
        }
        let mut req = last;
        req.messages.push(reply.clone());
        req.messages.push(Message::user(crate::supervise::watch::check_in_ask(&evidence)));
        lean(&mut req, cfg.efforts.get(&model).map(Vec::as_slice).unwrap_or(&[]), CHECK_IN_TOKENS);
        st.checked_in = true;
        (req, cfg, st.client.clone(), st.config.agent.retries, st.events.clone(), (st.turns, st.epoch))
    };
    let state = state.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let Ok(mut stream) = client.stream(&cfg, &req, retries).await else { return };
        let mut c = Collector::for_model(req.model.clone());
        while let Some(ev) = stream.next().await {
            let failed = matches!(ev, StreamEvent::Error(_));
            c.push(ev);
            if failed {
                break;
            }
        }
        let msg = c.message();
        if !c.usage.is_empty() {
            let mut st = state.lock().await;
            add_usage(&mut st, &events, &req.model, c.usage, Usage::default(), UsageKind::CheckIn);
        }
        if c.error.is_some() {
            return;
        }
        if let crate::supervise::watch::Answer::Continue(next) = crate::supervise::watch::parse_answer(&msg.text()) {
            let said = if next.is_empty() { String::new() } else { format!(": {next}") };
            let text =
                format!("The automatic check-in asked whether the task is finished and the answer was CONTINUE{said}");
            let _ = tx.send(Resume { turns: stamp.0, epoch: stamp.1, text });
        }
    });
}

pub(super) fn spawn_suggest(state: &Shared) {
    let state = state.clone();
    tokio::spawn(async move {
        let (req, cfg, client, retries, turns, events) = {
            let st = state.lock().await;
            // off in the configuration (the merged one, the managed policy included): nothing is asked, whoever asks
            if !st.config.ui.suggest {
                return;
            }
            let Some(last) = st.last_request.as_ref() else { return };
            // the conversation ends on a reply that is words only, from the model that got that request
            let Some(reply) =
                st.history.last().filter(|m| m.role == Role::Assistant && m.tool_calls().next().is_none())
            else {
                return;
            };
            let (_, cfg, model) = st.target.get();
            // a model that always reasons first would spend the whole cap thinking and say nothing
            let thinks = model.split(['/', '-', ':']).any(|w| matches!(w, "reasoner" | "r1" | "o1" | "o3" | "o4"))
                || ["grok-4", "gpt-5", "codex"].iter().any(|m| model.contains(m));
            if last.model != model || !st.inbox.is_empty() || thinks {
                return;
            }
            let req = suggest_request(last, reply.clone(), cfg.efforts.get(&model).map(Vec::as_slice).unwrap_or(&[]));
            (req, cfg, st.client.clone(), st.config.agent.retries, (st.turns, st.epoch), st.events.clone())
        };
        let Ok(mut stream) = client.stream(&cfg, &req, retries).await else { return };
        let mut c = Collector::for_model(req.model.clone());
        while let Some(ev) = stream.next().await {
            let failed = matches!(ev, StreamEvent::Error(_));
            c.push(ev);
            if failed {
                break;
            }
        }
        let msg = c.message();
        let mut st = state.lock().await;
        if !c.usage.is_empty() {
            add_usage(&mut st, &events, &req.model, c.usage, Usage::default(), UsageKind::Suggest);
        }
        if c.error.is_some() || msg.tool_calls().next().is_some() || (st.turns, st.epoch) != turns {
            return;
        }
        if let Some(text) = tidy_suggestion(&msg.text()) {
            emit(&events, AgentEvent::Suggestion { text });
        }
    });
}

/// A cold or stale catalogue is fetched in the background; when it lands, what the start could not
/// resolve (no cache on the first run) is filled from it and reported again.
pub(super) fn spawn_catalogue_fill(state: &Shared) {
    let state = state.clone();
    let done: nexo_providers::catalogue::Done = Box::new(move |r| {
        if r.is_err() {
            return;
        }
        tokio::spawn(async move {
            let mut st = state.lock().await;
            if !st.limits.incomplete() {
                return;
            }
            let (p, c, m) = st.target.get();
            let new = limits::resolve(&p, &c, &m, None);
            if let Some(l) = limits::filled(&st.limits, new) {
                st.limits = l;
                emit(&st.events, st.limits.event(&p, &m));
            }
        });
    });
    catalogue::refresh_if_stale(Some(done));
}

pub(super) fn spawn_vendor_fill(state: &Shared) {
    let state = state.clone();
    tokio::spawn(async move {
        let (provider, cfg, model, client, incomplete) = {
            let st = state.lock().await;
            let (p, c, m) = st.target.get();
            (p, c, m, st.client.clone(), st.limits.incomplete())
        };
        if !incomplete {
            return;
        }
        let list = match tokio::time::timeout(Duration::from_secs(15), client.list_models(&cfg)).await {
            Ok(Ok(l)) => l,
            _ => return,
        };
        let mut st = state.lock().await;
        let (p, c, m) = st.target.get();
        if p != provider || m != model {
            return;
        }
        let l = limits::resolve(&p, &c, &m, Some(&list));
        if l != st.limits {
            st.limits = l;
            emit(&st.events, st.limits.event(&p, &m));
        }
    });
}

#[cfg(test)]
mod suggest_tests {
    use super::*;

    #[test]
    fn the_suggestion_asks_for_the_least_thinking_the_model_takes() {
        let last = ModelRequest::new("m", "", vec![]);
        let lv = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let r = |l: &[&str]| suggest_request(&last, Message::assistant("ok"), &lv(l)).reasoning;
        assert_eq!(r(&[]), Some(ReasoningConfig::Off));
        assert_eq!(r(&["off", "low", "high"]), Some(ReasoningConfig::Off));
        assert_eq!(r(&["minimal", "low", "medium", "high"]), Some(ReasoningConfig::Effort(Effort::Minimal)));
        assert_eq!(r(&["low", "high"]), Some(ReasoningConfig::Effort(Effort::Low)));
        assert_eq!(suggest_request(&last, Message::assistant("ok"), &[]).max_tokens, SUGGEST_TOKENS);
    }
}
