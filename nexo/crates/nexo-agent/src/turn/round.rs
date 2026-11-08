//! Part of the agent loop; see the parent module.

use super::*;

pub(super) async fn run_turn(state: Shared, cancel: CancellationToken) -> TurnEnd {
    let (events, max_turns, guard, stale_guard, guard_answer, turn_no, mut known, ck_on, mut ck, mut known_ck) = {
        let st = state.lock().await;
        let a = &st.config.agent;
        let ck_on = a.on_limit == OnLimit::Checkpoint && a.max_checkpoints != Some(0);
        // the output lines the session has already brought (`agent.stale_guard`, and the checkpoints' progress)
        let known = if a.stale_guard > 0 { known_lines(&st.history) } else { HashSet::new() };
        let known_ck = if ck_on { known_numberless(&st.history) } else { HashSet::new() };
        let mark = checkpoint::Mark { writes: st.checkpoints.len(), wrote: 0, fresh: 0, todo: 0 };
        let todo = checkpoint::todo_of(&st.history[st.turn_start.min(st.history.len())..]);
        let mark = checkpoint::Mark { todo: todo.as_ref().map_or(0, |t| t.hash), ..mark };
        let ck = checkpoint::Tracker::new(st.cost, st.usage, mark, todo);
        (
            st.events.clone(),
            a.max_turns,
            a.loop_guard,
            a.stale_guard,
            a.guard_answer,
            st.turns,
            known,
            ck_on,
            ck,
            known_ck,
        )
    };
    emit(&events, AgentEvent::TurnStart { turn: turn_no });
    let mut compacted = false;
    // the view was asked again after the window was learned smaller (once, before any compaction)
    let mut refitted = false;
    let mut capped = false;
    let mut rounds = 0u32;
    // each tool round of this request as one hash of its calls and outputs (`agent.loop_guard`)
    let mut seen: Vec<u64> = Vec::new();
    // tool rounds in a row that wrote nothing and brought no new output line (`agent.stale_guard`)
    let mut stale = 0u32;
    // a guard stopped the tool rounds and the next call may only answer (`agent.guard_answer`)
    let mut answer = Answer::No;
    // replies in a row cut at max_tokens while writing their calls
    let mut cut_calls = 0u32;
    // replies in a row that held only reasoning: no answer and no call
    let mut mute = 0u32;
    // times in a row a Stop hook asked the agent to go on (capped at STOP_HOOK_CAP)
    let mut stop_blocks = 0u32;
    loop {
        // with checkpoints the limit is met after the tool round that reaches it; here only a request that went
        // on without tools (a guard's answer) stops
        if rounds >= max_turns && (!ck_on || answer != Answer::No) {
            notice(
                &events,
                Level::Warn,
                format!("stopped after {max_turns} tool rounds (agent.max_turns); ask to continue"),
            );
            return TurnEnd::MaxTurns;
        }
        rounds += 1;
        // the spend limits are checked before the call, so none is passed by more than one call
        {
            let mut st = state.lock().await;
            match budget::verdict(st.cost, budget::spent_today(), st.budget, st.config.agent.max_cost_day) {
                budget::Verdict::Stop(why) => {
                    notice(&events, Level::Error, why);
                    return TurnEnd::Error;
                }
                budget::Verdict::Warn(why) if !st.budget_warned => {
                    st.budget_warned = true;
                    notice(&events, Level::Warn, why);
                }
                _ => {}
            }
        }
        let (mut req, cfg, client, retries) = match prepare(&state, &cancel, answer == Answer::Forced).await {
            Ok(x) => x,
            Err(e) => {
                notice(&events, Level::Error, e.to_string());
                return TurnEnd::Error;
            }
        };
        if cut_calls > 0 {
            req.temperature = retry_temperature(req.temperature);
        }
        if cancel.is_cancelled() {
            return TurnEnd::Interrupted;
        }
        emit(&events, AgentEvent::AssistantStart);
        {
            let mut st = state.lock().await;
            if !st.mail_turn.is_empty() {
                let mails = std::mem::take(&mut st.mail_turn);
                if let Some(m) = st.mail.clone() {
                    emit(&events, m.read(&mails));
                }
            }
        }
        let started = tokio::select! {
            r = client.stream(&cfg, &req, retries) => r,
            _ = cancel.cancelled() => return TurnEnd::Interrupted,
        };
        let mut stream = match started {
            Ok(s) => s,
            Err(e) => match learn(&state, &e, &req.model).await {
                Lesson::Refit if !refitted => {
                    refitted = true;
                    continue;
                }
                Lesson::Context | Lesson::Refit if !compacted => {
                    compacted = true;
                    if compact_now(&state, &events, Some(()), &cancel).await {
                        continue;
                    }
                    notice(
                        &events,
                        Level::Error,
                        format!(
                            "the request does not fit the model's window and could not be compacted: {}",
                            e.human()
                        ),
                    );
                    return TurnEnd::Error;
                }
                Lesson::Output if !capped => {
                    capped = true;
                    continue;
                }
                _ => {
                    notice(&events, Level::Error, explain(&e));
                    return TurnEnd::Error;
                }
            },
        };
        let mut c = Collector::for_model(req.model.clone());
        let mut interrupted = false;
        let mut marker = ck.pending.is_some().then(checkpoint::MarkerFilter::start);
        loop {
            tokio::select! {
                ev = stream.next() => match ev {
                    None => break,
                    Some(ev) => {
                        match &ev {
                            StreamEvent::TextDelta(t) => {
                                let text = match &mut marker {
                                    Some(f) => f.push(t),
                                    None => t.clone(),
                                };
                                if !text.is_empty() {
                                    emit(&events, AgentEvent::TextDelta { text });
                                }
                            }
                            StreamEvent::ReasoningDelta(t) => emit(&events, AgentEvent::ReasoningDelta { text: t.clone() }),
                            _ => {}
                        }
                        c.push(ev);
                    }
                },
                _ = cancel.cancelled() => { interrupted = true; break }
            }
        }
        if let Some(f) = &mut marker {
            let text = f.finish();
            if !text.is_empty() {
                emit(&events, AgentEvent::TextDelta { text });
            }
        }
        drop(stream);
        let mut msg = c.message();
        if c.error.is_none() && !interrupted {
            // a reply that wrote its calls as text, in the model's own markup, makes them as calls
            let specs = state.lock().await.tools.specs();
            // ids unique in the session: turns count from 0 again after a resume or a fork
            let prefix = format!("text_call_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
            if let Some(m) = leak::recover(&msg, &specs, &prefix) {
                let n = m.tool_calls().count();
                notice(&events, Level::Info, format!("the model wrote {n} tool call(s) as text; they run as calls"));
                msg = m;
            }
        }
        let (usage, call) = (c.usage, c.call);
        let cut = c.finish == Some(FinishReason::Length);
        if cut && c.error.is_none() {
            leave_out_cut_args(&mut msg);
        }
        if let Some(e) = &c.error {
            if msg.content.is_empty() {
                match learn(&state, e, &req.model).await {
                    Lesson::Refit if !refitted => {
                        bill_attempt(&state, &events, &req.model, usage, call).await;
                        refitted = true;
                        continue;
                    }
                    Lesson::Context | Lesson::Refit if !compacted => {
                        compacted = true;
                        if compact_now(&state, &events, Some(()), &cancel).await {
                            bill_attempt(&state, &events, &req.model, usage, call).await;
                            continue;
                        }
                    }
                    Lesson::Output if !capped => {
                        capped = true;
                        bill_attempt(&state, &events, &req.model, usage, call).await;
                        continue;
                    }
                    _ => {}
                }
            }
        }
        {
            let mut st = state.lock().await;
            if !msg.content.is_empty() {
                st.history.push(msg.clone());
                st.hidden.clear();
                if let Err(e) = st.session.append(&msg) {
                    notice(&events, Level::Warn, format!("cannot save the session: {e}"));
                }
            }
            if !usage.is_empty() {
                let model = req.model.clone();
                add_usage(&mut st, &events, &model, usage, call, UsageKind::Main);
                st.policy.usage(call.cache_read, call.total_input());
                st.tok_ratio = learn_ratio(st.tok_ratio, st.est_sent, call.total_input());
            }
            if !msg.content.is_empty() && call.total_input() > 0 {
                let len = st.history.len();
                st.last_prompt = Some((call.total_input() + call.output, len));
            }
            if !msg.content.is_empty() {
                emit(&events, AgentEvent::AssistantDone { message: msg.clone() });
            }
        }
        let calls: Vec<(String, String, Value)> =
            msg.tool_calls().map(|(i, n, a)| (i.to_string(), n.to_string(), a.clone())).collect();
        if let Some(e) = c.error {
            notice(&events, Level::Error, explain(&e));
            if !calls.is_empty() {
                push_results(&state, cancelled_results(&calls, "the model call failed before the tools ran")).await;
            }
            return TurnEnd::Error;
        }
        if interrupted {
            if !calls.is_empty() {
                push_results(&state, cancelled_results(&calls, "interrupted by the person")).await;
            }
            return TurnEnd::Interrupted;
        }
        if cut && !calls.is_empty() {
            // the last call stopped half written: its arguments are not what the model meant, and the
            // calls before it belong to a plan cut short. None runs; the model is told why and asks again
            push_results(&state, cancelled_results(&calls, CUT_NOTE)).await;
            cut_calls += 1;
            if cut_calls >= 2 {
                notice(&events, Level::Error, "two replies in a row were cut at max_tokens while writing their calls; stopped (raise agent.max_tokens or ask for smaller steps)");
                return TurnEnd::Error;
            }
            notice(&events, Level::Warn, "the reply was cut at max_tokens while writing its calls; they did not run");
            continue;
        }
        cut_calls = 0;
        if calls.is_empty() && msg.text().trim().is_empty() && !msg.reasoning().trim().is_empty() && mute < 2 {
            // the model thought and stopped before writing anything: ending here would leave the request
            // undone with nothing said, so it is asked to go on from its reasoning
            mute += 1;
            let room = if cut {
                let mut st = state.lock().await;
                st.reply_boost = (st.reply_boost.max(1) * 2).min(4);
                format!(" with room for {} tokens", reply_tokens(&st))
            } else {
                String::new()
            };
            notice(
                &events,
                Level::Warn,
                format!("the reply held only reasoning, with no answer and no call; asked to go on{room}"),
            );
            let note = if mute == 1 { MUTE_NOTE } else { MUTE_NOTE_AGAIN };
            // most providers do not take reasoning back, so the next request would think from nothing
            // and be cut again: the end of it travels in the note
            let note = format!("{note}{}", reasoning_tail(&msg.reasoning()));
            push_results(&state, vec![Part::text(note)]).await;
            continue;
        }
        mute = 0;
        if let Some((n, line)) = ck.pending.take() {
            let text = msg.text();
            match checkpoint::parse_decision(&text) {
                Some(d) if !d.go_on => {
                    notice(&events, Level::Info, format!("⟳ checkpoint {n} · {line} · stop: {}", d.reason));
                    ck.stopping = Some(d.reason.clone());
                    if d.rest.is_empty() {
                        // the reason alone is no summary: one more call asks for it, the tools staying declared
                        let mut out = cancelled_results(&calls, "stopped at a checkpoint");
                        out.push(Part::text(SUMMARY_NOW));
                        push_results(&state, out).await;
                        answer = Answer::Asked;
                        continue;
                    }
                    if !calls.is_empty() {
                        push_results(&state, cancelled_results(&calls, "stopped at a checkpoint")).await;
                    }
                    checkpoint_stopped(&state, &d.reason, &text).await;
                    return TurnEnd::Done;
                }
                Some(d) => {
                    notice(&events, Level::Info, format!("⟳ checkpoint {n} · {line} · continue: {}", d.reason));
                    if calls.is_empty() && !ck.nudged {
                        ck.nudged = true;
                        push_results(&state, vec![Part::text(CHECKPOINT_GO_ON)]).await;
                        continue;
                    }
                }
                None if !calls.is_empty() => {
                    notice(&events, Level::Info, format!("⟳ checkpoint {n} · {line} · continue (no decision line)"));
                }
                None if calls.is_empty() => {
                    notice(&events, Level::Info, format!("⟳ checkpoint {n} · {line} · ended without a decision line"));
                }
                None => {}
            }
        }
        if calls.is_empty() {
            if cut {
                notice(&events, Level::Warn, "the reply was cut at max_tokens (agent.max_tokens)");
            }
            if let Some(why) = ck.stopping.take() {
                checkpoint_stopped(&state, &why, &msg.text()).await;
            }
            // a Stop hook may send the agent back to work, with its reason as a note; a few times only
            if stop_blocks < STOP_HOOK_CAP {
                let fields =
                    serde_json::json!({"stop_hook_active": stop_blocks > 0, "last_assistant_message": msg.text()});
                if let Some(why) = fire(&state, "stop", None, fields).await.blocked {
                    stop_blocks += 1;
                    notice(
                        &events,
                        Level::Info,
                        format!("a Stop hook asked the agent to go on ({stop_blocks}/{STOP_HOOK_CAP}): {why}"),
                    );
                    push_results(&state, vec![Part::text(format!("[Stop hook] {why}"))]).await;
                    continue;
                }
            }
            return TurnEnd::Done;
        }
        if answer != Answer::No {
            // the model called a tool all the same: the call does not run; the next call is told by the protocol
            // that none may be called, and if it calls one even so, the request stops
            push_results(&state, cancelled_results(&calls, "a guard stopped the tool rounds")).await;
            if answer == Answer::Forced {
                if let Some(why) = ck.stopping.take() {
                    // it chose to stop and would not write the summary: the stop stands, not the round limit
                    checkpoint_stopped(&state, &why, &msg.text()).await;
                    return TurnEnd::Done;
                }
                return TurnEnd::MaxTurns;
            }
            answer = Answer::Forced;
            continue;
        }
        let asked = calls.iter().map(|(_, n, a)| (n.clone(), a.to_string())).collect::<Vec<_>>();
        let kept = calls.clone();
        let (results, cancelled) = run_tools(&state, &events, calls, &cancel).await;
        seen.push(round_hash(&asked, &results));
        let (lines, new) = if stale_guard > 0 { fresh_count(&mut known, &results, false) } else { (0, 0) };
        if ck_on {
            ck.fresh += fresh_count(&mut known_ck, &results, true).1;
            ck.round(&asked, &results);
            note_writes(&state, &mut ck, &asked, &results).await;
        }
        // the guards above weigh the outputs as they came; the line of a failure that came back is added after
        let results = {
            let mut st = state.lock().await;
            st.repeats.round(&kept, results)
        };
        if stale_guard > 0 {
            let wrote = {
                let st = state.lock().await;
                asked.iter().any(|(n, _)| st.tools.get(n).is_some_and(|t| t.kind() == ToolKind::Write))
            };
            // a round with no output line (a file written by a shell command, a silent step) is neither
            stale = match (lines, new) {
                _ if wrote => 0,
                (0, _) => stale,
                (_, 0) => stale + 1,
                _ => 0,
            };
        }
        // a guard that stops the tool rounds here, and what the person is told
        let stop = if cancelled {
            None
        } else if let Some(p) = looping(&seen, guard) {
            let what = if p == 1 { "the same tool round".to_string() } else { format!("the same {p} tool rounds") };
            Some(format!(
                "stopped: {what} ran {guard} times in a row with the same outputs (agent.loop_guard){}",
                then(guard_answer)
            ))
        } else if stale_guard > 0 && stale >= stale_guard {
            Some(format!("stopped: {stale} tool rounds in a row wrote nothing and brought no output line this session had not seen (agent.stale_guard){}", then(guard_answer)))
        } else {
            None
        };
        let mut results = results;
        if stop.is_some() && guard_answer {
            // the model is told, next to the outputs, that the tools are off: told nothing, it writes the next
            // call as text even when the protocol forbids calls (seen with DeepSeek, with or without tools)
            answer = Answer::Asked;
            results.push(Part::text(ANSWER_NOW));
        }
        if ck_on && stop.is_none() && !cancelled && answer == Answer::No && rounds >= max_turns {
            let ctx = Standing { rounds: &seen, guard, stale, stale_guard };
            match checkpoint_now(&state, &events, &mut ck, &ctx).await {
                Ok(text) => {
                    results.push(Part::text(text));
                    rounds = 0;
                }
                Err(end) => {
                    push_results(&state, results).await;
                    return end;
                }
            }
        }
        // `agent.close_on_verified`: the reply holds its answer and every call only confirmed finished work, and
        // all of them succeeded: this reply is the end of the turn, there is nothing left for another request to say
        let closing = stop.is_none()
            && !cancelled
            && answer == Answer::No
            && ck.stopping.is_none()
            && !verify::is_lead_in(&msg.text())
            && {
                let st = state.lock().await;
                st.config.agent.close_on_verified
                    && results.len() == kept.len()
                    && results.iter().all(|p| matches!(p, Part::ToolResult { is_error: false, .. }))
                    && verify::all_verification(&kept, |n, a| st.tools.get(n).map(|t| t.call_kind(a)))
            };
        if closing {
            let mut blocked = None;
            if stop_blocks < STOP_HOOK_CAP {
                let fields =
                    serde_json::json!({"stop_hook_active": stop_blocks > 0, "last_assistant_message": msg.text()});
                blocked = fire(&state, "stop", None, fields).await.blocked;
            }
            if let Some(why) = blocked {
                stop_blocks += 1;
                notice(
                    &events,
                    Level::Info,
                    format!("a Stop hook asked the agent to go on ({stop_blocks}/{STOP_HOOK_CAP}): {why}"),
                );
                results.push(Part::text(format!("[Stop hook] {why}")));
            } else {
                push_results(&state, results).await;
                return TurnEnd::Done;
            }
        }
        push_results(&state, results).await;
        if !cancelled {
            watch_round(&state).await;
        }
        if cancelled {
            return TurnEnd::Interrupted;
        }
        if let Some(why) = stop {
            notice(&events, Level::Warn, why);
            if !guard_answer {
                return TurnEnd::MaxTurns;
            }
        }
        if take_inbox(&state).await {
            // the person spoke: the new message is answered with tools on, and the guards start over
            answer = Answer::No;
            seen.clear();
            stale = 0;
        }
    }
}

/// What follows a guard's stop: an answer from what was gathered (`agent.guard_answer`), or the person's word.
/// What the model reads, after the outputs of the round a guard stopped, before the call that may only answer
/// (`agent.guard_answer`).
/// Where a request stands after a guard's stop (`agent.guard_answer`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Answer {
    /// No guard stopped the tool rounds.
    No,
    /// The model is asked in words (`ANSWER_NOW`) to answer. The call keeps the tools and the tool choice of
    /// every other call: a request that changes them misses the provider's prompt cache (DeepSeek leaves the
    /// tools out of the prompt, Anthropic drops the cached messages), and that one call then costs as much as
    /// the whole view sent anew.
    Asked,
    /// The model called a tool all the same: the protocol is told that none may be called.
    Forced,
}

/// Why the calls of a reply cut at max_tokens did not run, and how to fit the next one in a reply: a model at a
/// low temperature otherwise sends the same call again and is cut at the same place
pub(super) const CUT_NOTE: &str =
    "the reply was cut at max_tokens before its calls were complete. A call has to fit in one \
    reply: write a long file in parts, keep test scripts short, and do not spell out long lists of data";

/// The arguments of the calls of a reply cut at max_tokens, left out of the history: they never run, and a
/// model that looped into the cap (a test spelling out 1..100) copies its own loop when it sees it again
pub(super) fn leave_out_cut_args(msg: &mut Message) {
    for p in &mut msg.content {
        if let Part::ToolCall { args, .. } = p {
            let chars = args.to_string().chars().count();
            *args = serde_json::json!({ "left_out": format!("cut at max_tokens after {chars} characters") });
        }
    }
}

/// The lowest temperature of the request that follows a reply cut at max_tokens while writing its calls
pub(super) const CUT_TEMPERATURE: f32 = 0.6;

/// The temperature of the request after a cut: a low one is raised to `CUT_TEMPERATURE`, because a model at
/// temperature 0 that looped into the cap loops the same way again from the same history; none (the provider's
/// own) and a higher one stay
pub(super) fn retry_temperature(t: Option<f32>) -> Option<f32> {
    t.map(|t| t.max(CUT_TEMPERATURE))
}

/// Sent when a reply held only reasoning (often cut at max_tokens while thinking): not a request of the person.
pub(super) const MUTE_NOTE: &str =
    "<system-reminder>Your last reply held only reasoning: no answer and no tool call reached the person. \
     Go on from it now with a tool call or the answer, keeping the reasoning short.</system-reminder>";

/// How much of the end of a reasoning-only reply the next ask repeats.
const MUTE_TAIL: usize = 3000;

/// The end of the reasoning, as a quoted block for the ask to go on; empty when there is none.
fn reasoning_tail(r: &str) -> String {
    let r = r.trim();
    if r.is_empty() {
        return String::new();
    }
    let mut from = r.len().saturating_sub(MUTE_TAIL);
    while !r.is_char_boundary(from) {
        from += 1;
    }
    let dots = if from > 0 { "..." } else { "" };
    format!(
        "\n<system-reminder>The end of that reasoning, repeated because it is not kept:\n{dots}{}</system-reminder>",
        &r[from..]
    )
}

/// The second time in a row: planning longer is what keeps failing, so the model is told to build first.
pub(super) const MUTE_NOTE_AGAIN: &str =
    "<system-reminder>Your replies keep ending inside the reasoning, before any answer or tool call. Stop \
     planning: make a tool call now that builds the simplest version that could work (write the file, run the \
     command), then test it and improve it step by step.</system-reminder>";

pub(super) const ANSWER_NOW: &str =
    "Tool calls are off for the rest of this request: they will not run. Give your answer now, from what you have already seen. \
     The next message of the person turns them on again.";

/// Sent after `CHECKPOINT STOP` with no summary: the request ends on the model's account of its work.
pub(super) const SUMMARY_NOW: &str =
    "<system-reminder>You chose to stop at the checkpoint. Tool calls will not run now. \
     Write your final summary for the person: what was done, what is left and how to go on.</system-reminder>";

/// Sent once when the model chose to continue at a checkpoint but called no tool.
pub(super) const CHECKPOINT_GO_ON: &str =
    "<system-reminder>You chose to continue at the checkpoint. Go on with the next tool call.</system-reminder>";
