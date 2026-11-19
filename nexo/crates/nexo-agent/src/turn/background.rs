//! Part of the agent loop; see the parent module.

use super::*;

/// Whether the spending has reached `max_cost` or `max_cost_day`: no model call is made past it, background
/// requests included.
fn limit_reached(session: f64, day: f64, session_limit: Option<f64>, day_limit: Option<f64>) -> bool {
    matches!(budget::verdict(session, day, session_limit, day_limit), budget::Verdict::Stop(_))
}

/// `limit_reached` for this session's spending and today's.
fn over_budget(st: &State) -> bool {
    limit_reached(st.cost, budget::spent_today(), st.budget, st.config.agent.max_cost_day)
}

/// The shell jobs a review looks at: the ones running, less those a foreground command left running (a server is
/// silent by nature, and a STOP would kill the person's server).
fn reviewable(jobs: &Jobs) -> Vec<nexo_core::jobs::JobInfo> {
    jobs.list()
        .into_iter()
        .filter(|j| j.kind == nexo_core::jobs::JobKind::Shell && j.status.running() && !jobs.held(j.id))
        .collect()
}

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
            let (first, client, target, retries, request, events, over) = {
                let st = state.lock().await;
                let a = &st.config.agent;
                (
                    a.job_review as u64,
                    st.client.clone(),
                    st.target.clone(),
                    a.retries,
                    crate::review::request_of(&st.history),
                    st.events.clone(),
                    // a stop of the person holds the reviews too: a job that runs on is judged when they write
                    over_budget(&st) || st.held,
                )
            };
            drop(state);
            tick = crate::review::TICK_S.min(first.max(1));
            // a spend limit reached means no more model calls, the reviews included
            if over {
                continue;
            }
            let running = reviewable(&jobs);
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
    // a model that lists no `off` gets its lowest level, not off, which it may refuse
    req.reasoning = nexo_core::provider::lowest_level(levels, true).or(Some(ReasoningConfig::Off));
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

/// A suggestion as one line without the quotes a model puts round it; None when it holds nothing or is not a
/// next request: a model that cannot guess says so in a long line, a question or a first-person apology, and
/// grey text like that in the prompt reads as the person's own words.
pub(super) fn tidy_suggestion(text: &str) -> Option<String> {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let line = line.trim_matches(|c| matches!(c, '"' | '\'' | '`' | '“' | '”')).trim();
    if line.is_empty() || line.chars().count() > SUGGESTION_MAX || line.ends_with('?') {
        return None;
    }
    let first = line.split(|c: char| !c.is_alphanumeric() && c != '\'' && c != '’').next().unwrap_or("");
    let refusal = ["i", "i'm", "i’m", "i've", "sorry", "unfortunately", "apologies", "as"];
    if refusal.iter().any(|w| first.eq_ignore_ascii_case(w)) {
        return None;
    }
    Some(line.to_string())
}

/// The longest suggestion shown, in characters: a next request is a few words.
const SUGGESTION_MAX: usize = 70;

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
/// Gives back how many notes it added.
pub(super) async fn watch_round(state: &Shared) -> u32 {
    let mut st = state.lock().await;
    if !st.config.agent.watch {
        return 0;
    }
    let from = st.turn_start.min(st.history.len());
    let mut added = 0;
    // an active goal's facts line says what a run of reads or a call made again brought (see `GoalIdle::facts`): no
    // note to guess it
    let goal_facts = st.goal.is_some() && st.goal_state.is_active();
    for f in nexo_supervise::watch::scan_rounds(&st.history[from..]) {
        if goal_facts && (f.key.starts_with("reads:") || f.key.starts_with("repeat:")) {
            continue;
        }
        if st.noticed.insert(f.key) {
            st.hidden.push(f.text);
            added += 1;
        }
    }
    added
}

/// Whether the request in `turn` (the slice of the history from its start) is one of a goal's own rounds: the loop
/// sent it, so its message carries the goal's mark.
fn goal_round(turn: &[Message]) -> bool {
    turn.first().is_some_and(|m| m.role == Role::User && nexo_core::agent::goal_mark_at(&m.text()).is_some())
}

/// The model did no tool call since the person's latest words joined the request (or since it began): what it
/// said answers them, and there is no work of its own to check.
pub(super) fn answered_in_words(st: &State) -> bool {
    let from = st.turn_start.max(st.person_at).min(st.history.len());
    !st.history[from..].iter().any(|m| m.role == Role::Assistant && m.tool_calls().next().is_some())
}

/// A stopped session whose work looks unfinished (or that the supervisor asks to look at itself): one background
/// request to its own model, with the evidence, no tools offered to act on and no trace in the conversation.
/// CONTINUE resumes the session through `tx`; DONE, or an answer that is not clear, changes nothing. At most one
/// per request of the person, and none when something else will wake the session.
pub(super) async fn check_in(state: &Shared, extra: Option<String>, tx: &tokio::sync::mpsc::UnboundedSender<Resume>) {
    let asked = extra.is_some();
    let (req, cfg, client, retries, events, stamp, flight) = {
        let mut st = state.lock().await;
        let a = &st.config.agent;
        if !a.check_in || st.checked_in || st.held || st.perm.mode == PermissionMode::Plan {
            return;
        }
        // a request with no tool call of its own (the person said stop, asked a question) has nothing unfinished
        // to check, whatever the supervisor says about older work
        if answered_in_words(&st) {
            return;
        }
        let own = st.turn_start.min(st.history.len());
        // a goal owns the turn that just ended when it is active, paused or waiting, or when the turn was one of its
        // rounds: the goal's own loop decides what comes next. A met goal stays set until `/goal off`, so a later
        // request of the person in that session is still checked
        if st.goal.is_some() && (st.goal_state != GoalState::Done || goal_round(&st.history[own..])) {
            return;
        }
        if !st.inbox.is_empty() || !st.mail_wait.is_empty() || st.jobs.running() > 0 || !st.jobs.untold().is_empty() {
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
            .extend(nexo_supervise::watch::scan_end(&st.history[from..], &reply.text()).into_iter().map(|f| f.text));
        evidence.retain(|e| !e.trim().is_empty());
        if evidence.is_empty() {
            return;
        }
        let mut req = last;
        req.messages.push(reply.clone());
        req.messages.push(Message::user(nexo_supervise::watch::check_in_ask(&evidence)));
        lean(&mut req, cfg.efforts.get(&model).map(Vec::as_slice).unwrap_or(&[]), CHECK_IN_TOKENS);
        // the limits count the calls in flight, and the check-in's reply is capped to what the budget can pay
        let input = (st.est_sent as f64 * st.tok_ratio.unwrap_or(1.0)).round() as u64;
        let priced = main_pricing(&st, &req.model);
        let Ok((cap, flight)) = budget::admit(
            st.session.id(),
            st.cost,
            st.budget,
            st.config.agent.max_cost_day,
            priced,
            input,
            req.max_tokens,
        ) else {
            return;
        };
        req.max_tokens = cap;
        st.checked_in = true;
        if asked {
            notice(&st.events, Level::Info, "supervisor: checking in with this session");
        }
        (req, cfg, st.client.clone(), st.config.agent.retries, st.events.clone(), (st.turns, st.epoch), flight)
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
        drop(flight);
        if c.error.is_some() {
            return;
        }
        if let nexo_supervise::watch::Answer::Continue(next) = nexo_supervise::watch::parse_answer(&msg.text()) {
            let said = if next.is_empty() { String::new() } else { format!(": {next}") };
            let text =
                format!("The automatic check-in asked whether the task is finished and the answer was CONTINUE{said}");
            let _ = tx.send(Resume { turns: stamp.0, epoch: stamp.1, text });
        }
    });
}

/// Whether the model reasons before every reply and cannot be asked not to. The names of the known fixed-reasoning
/// models say it. So do the probed levels where the vendor's own table made them (Gemini Pro lists no `off`): a
/// list with no `off` or `none` there is a model that cannot be turned down. On any other host the levels are the
/// probe's default for every reasoning model, which says nothing about whether the model can think less, so only
/// the names decide there.
fn always_thinks(protocol: &str, levels: &[String], model: &str) -> bool {
    let table = nexo_providers::vendors::by_protocol(protocol).is_some_and(|v| v.efforts.is_some());
    if table && !levels.is_empty() && !levels.iter().any(|l| l == "off" || l == "none") {
        return true;
    }
    model.split(['/', '-', ':']).any(|w| matches!(w, "reasoner" | "r1" | "o1" | "o3" | "o4"))
        || ["grok-4", "gpt-5", "codex"].iter().any(|m| model.contains(m))
}

pub(super) fn spawn_suggest(state: &Shared) {
    let state = state.clone();
    tokio::spawn(async move {
        let (req, cfg, client, retries, turns, events) = {
            let st = state.lock().await;
            // off in the configuration (the merged one, the managed policy included): nothing is asked, whoever asks
            if !st.config.ui.suggest || over_budget(&st) || st.held {
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
            let thinks =
                always_thinks(&cfg.protocol, cfg.efforts.get(&model).map(Vec::as_slice).unwrap_or(&[]), &model);
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
/// resolve (no cache on the first run) is filled from it and reported again; a window that was only the
/// provider's cap gives way to a smaller catalogue one.
pub(super) fn spawn_catalogue_fill(state: &Shared) {
    let state = state.clone();
    let done: nexo_providers::catalogue::Done = Box::new(move |r| {
        if r.is_err() {
            return;
        }
        tokio::spawn(async move {
            let mut st = state.lock().await;
            // the catalogue is new: so is what `/model` offers
            emit(&st.events, AgentEvent::ModelsListed { models: nexo_providers::modellist::choices(&st.config) });
            if !st.limits.incomplete() && !st.limits.window_is_cap_only() {
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

/// What `/model` offers is read again from the vendors' own lists (each asked at most once a day, in the
/// background, with a short timeout) and the catalogue; the interface is told what it offers now. A
/// vendor that does not answer keeps its last list.
pub(super) fn spawn_model_lists(state: &Shared) {
    let state = state.clone();
    tokio::spawn(async move {
        let (config, client, events) = {
            let st = state.lock().await;
            (st.config.clone(), st.client.clone(), st.events.clone())
        };
        let file = nexo_providers::modellist::cache_file();
        let ask = |cfg: nexo_core::config::ProviderConfig| {
            let client = client.clone();
            async move {
                client.list_models(&cfg).await.map(|l| l.into_iter().map(|m| m.id).collect()).map_err(|e| e.message)
            }
        };
        let before = nexo_providers::modellist::choices(&config);
        nexo_providers::modellist::refresh(&config, &file, Duration::from_secs(8), ask).await;
        let after = nexo_providers::modellist::choices(&config);
        if after != before {
            emit(&events, AgentEvent::ModelsListed { models: after });
        }
    });
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

/// After a switch: asks the server for its models and, when it lists the new one nowhere, says so at once with
/// the closest names, instead of leaving the failure for the first message. A server with no list, or one
/// that does not answer in time, says nothing.
pub(super) fn spawn_model_check(state: &Shared) {
    let state = state.clone();
    tokio::spawn(async move {
        let (provider, cfg, model, client) = {
            let st = state.lock().await;
            let (p, c, m) = st.target.get();
            (p, c, m, st.client.clone())
        };
        if cfg.models.contains(&model) {
            return;
        }
        let list = match tokio::time::timeout(Duration::from_secs(8), client.list_models(&cfg)).await {
            Ok(Ok(l)) => l,
            _ => return,
        };
        let names: Vec<String> = list.into_iter().map(|m| m.id).collect();
        let st = state.lock().await;
        // the person may have moved on while the server answered
        if st.target.get().2 != model || names.is_empty() || names.contains(&model) {
            return;
        }
        let close = hints::closest(&model, names.iter().map(String::as_str), 5);
        let hint =
            if close.is_empty() { String::new() } else { format!("; closest: {} (`/model <name>`)", close.join(", ")) };
        notice(&st.events, Level::Warn, format!("{provider} does not list the model `{model}`{hint}"));
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
        // no `off` and no low rung in the list: its lowest level, not off
        assert_eq!(r(&["medium", "high"]), Some(ReasoningConfig::Effort(Effort::Medium)));
        assert_eq!(suggest_request(&last, Message::assistant("ok"), &[]).max_tokens, SUGGEST_TOKENS);
    }

    /// The session's level does not ride on a side request: it asks for the least thinking the model lists, or none.
    #[test]
    fn a_suggestion_is_not_raised_to_the_level_of_the_session() {
        let mut last = ModelRequest::new("m", "", vec![]);
        last.reasoning = Some(ReasoningConfig::Effort(Effort::Xhigh));
        let lv = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let r = suggest_request(&last, Message::assistant("ok"), &lv(&["low", "high", "xhigh"])).reasoning;
        assert_eq!(r, Some(ReasoningConfig::Effort(Effort::Low)));
        last.reasoning = Some(ReasoningConfig::Effort(Effort::High));
        let r = suggest_request(&last, Message::assistant("ok"), &lv(&["off", "high"])).reasoning;
        assert_eq!(r, Some(ReasoningConfig::Off));
        // and the last request itself is left as it was sent
        assert_eq!(last.reasoning, Some(ReasoningConfig::Effort(Effort::High)));
    }

    #[test]
    fn levels_decide_only_where_the_vendor_table_made_them_and_the_names_decide_elsewhere() {
        let lv = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (gemini, anthropic, chat) = (nexo_providers::gemini::ID, nexo_providers::anthropic::ID, "openai-chat");
        assert!(always_thinks(gemini, &lv(&["low", "medium", "high"]), "gemini-3-pro"), "Gemini Pro lists no off");
        assert!(!always_thinks(gemini, &lv(&["off", "low", "medium", "high"]), "gemini-3-flash"));
        assert!(!always_thinks(anthropic, &lv(&["off", "low", "high"]), "claude-opus"));
        // the probe's default list for any reasoning model on a generic host has no off and proves nothing
        assert!(!always_thinks(chat, &lv(&["low", "medium", "high"]), "qwen3-235b"));
        assert!(!always_thinks(chat, &lv(&["low", "medium", "high"]), "anthropic/claude-sonnet"));
        assert!(always_thinks(chat, &lv(&["minimal", "low", "medium", "high"]), "gpt-5"));
        assert!(always_thinks(chat, &[], "deepseek-reasoner"));
        assert!(always_thinks(chat, &[], "openai/o3-mini"));
        assert!(!always_thinks(chat, &[], "llama3"));
    }

    /// A suggestion or a job review is a paid request: once `max_cost` or `max_cost_day` is reached it is not made.
    #[test]
    fn background_requests_wait_for_the_spend_limits() {
        // no limit set: never over
        assert!(!limit_reached(5.0, 5.0, None, None));
        // the session limit
        assert!(!limit_reached(0.5, 0.0, Some(1.0), None));
        assert!(limit_reached(1.0, 0.0, Some(1.0), None));
        // the day limit
        assert!(!limit_reached(0.0, 1.5, None, Some(2.0)));
        assert!(limit_reached(0.0, 2.0, None, Some(2.0)));
        // either one is enough
        assert!(limit_reached(0.1, 3.0, Some(1.0), Some(2.0)));
    }

    /// A process a command left running is not judged by its silence: a review would stop the person's server.
    #[test]
    fn a_job_a_command_left_running_is_not_reviewed() {
        use nexo_core::jobs::JobKind;
        let jobs = Jobs::new();
        let plain = jobs.start(JobKind::Shell, "sleep 100");
        let kept = jobs.start_held(JobKind::Shell, "python3 -m http.server 8000 &");
        let ids: Vec<u32> = reviewable(&jobs).iter().map(|j| j.id).collect();
        assert_eq!(ids, vec![plain.id()]);
        assert!(jobs.held(kept.id()) && !jobs.held(plain.id()));
        assert!(jobs.get(kept.id()).unwrap().status.running(), "it is a running job all the same");
    }

    #[test]
    fn only_a_short_request_shaped_line_is_shown_as_a_suggestion() {
        assert_eq!(tidy_suggestion("  \"run the\ntests\" ").as_deref(), Some("run the tests"));
        assert_eq!(tidy_suggestion("commit it").as_deref(), Some("commit it"));
        let apology = "I don't have enough conversation history in this session to reliably guess.";
        assert_eq!(tidy_suggestion(apology), None, "a refusal is not a request");
        assert_eq!(tidy_suggestion("Could you provide the previous messages?"), None, "a question is not either");
        assert_eq!(tidy_suggestion(&"word ".repeat(30)), None, "too long to be a next message");
        assert_eq!(tidy_suggestion("  "), None);
    }
}
