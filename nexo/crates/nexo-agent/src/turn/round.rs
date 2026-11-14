//! Part of the agent loop; see the parent module.

use super::*;

pub(super) async fn run_turn(state: Shared, cancel: CancellationToken) -> TurnEnd {
    let (events, max_turns, guard, stale_guard, guard_answer, turn_no, mut known, ck_on, mut ck, mut known_ck, alone) = {
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
            st.alone,
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
    // how many of them are asked to go on before the request ends there
    let mute_asks = if alone { MUTE_ASKS_ALONE } else { MUTE_ASKS };
    // the last reply was cut at `agent.reasoning_budget`: the asks after it, until a reply calls or says something,
    // go out at the least reasoning the model lists, so a retry does not think as long again at the same level
    let mut calm = false;
    // times in a row this request was sent again because its stream broke inside the reasoning
    let mut recut = 0u32;
    // times in a row a Stop hook asked the agent to go on (capped at STOP_HOOK_CAP)
    let mut stop_blocks = 0u32;
    // an edit, a write or a command that changes things has run in this turn (`agent.close_on_verified`)
    let mut changed = false;
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
            // a signal to send sub-agents to the background belongs to the round it came in: a round that
            // went on without `take_inbox` (a nudge, a retry) leaves none for the next one
            st.detach.clear();
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
        // this request asks for less reasoning than the first: a cut past the asks stops the turn
        let mut lowered = false;
        if calm && req.reasoning.is_none() {
            let mut st = state.lock().await;
            let (_, pcfg, model) = st.target.get();
            let levels = pcfg.efforts.get(&model).map(Vec::as_slice).unwrap_or(&[]);
            req.reasoning = calm_level(levels, nexo_providers::probe::off_thinks_less(&pcfg.protocol, &model));
            // a level is set only where it changes the request: none left means the same request again
            lowered = req.reasoning.is_some();
            if let Some(last) = st.last_request.as_mut() {
                last.reasoning = req.reasoning;
            }
        }
        if cancel.is_cancelled() {
            return TurnEnd::Interrupted;
        }
        emit(&events, AgentEvent::AssistantStart);
        // the request is out: until its first word the wait is the model's
        preparing(&events, PREP_MODEL);
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
                    let p = problem_of(&state, &e).await.after_compaction_failed();
                    notice_problem(&events, p);
                    return TurnEnd::Error;
                }
                Lesson::Output if !capped => {
                    capped = true;
                    continue;
                }
                _ => {
                    notice_problem(&events, problem_of(&state, &e).await);
                    return TurnEnd::Error;
                }
            },
        };
        // reasoning tokens a reply may stream before any text or call; off when the person set a level. It stays
        // on after a cut while the asks think less. A model with no level to lower (an always-thinking one) would
        // be asked the same way and cut the same way each time: after two asks its reply runs free, up to
        // max_tokens, as the one way to an answer short of editing the config
        let budget = {
            let st = state.lock().await;
            let (_, pcfg, model) = st.target.get();
            match setup::effort_of(&pcfg, &model, &st.config.agent.reasoning) {
                Some(_) => 0,
                None if mute >= 2 && !lowered => 0,
                None => u64::from(st.config.agent.reasoning_budget),
            }
        };
        let (mut thought, mut reasoned, mut spoke, mut over_budget) = (0u64, false, false, false);
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
                                spoke |= !t.is_empty();
                                let text = match &mut marker {
                                    Some(f) => f.push(t),
                                    None => t.clone(),
                                };
                                if !text.is_empty() {
                                    emit(&events, AgentEvent::TextDelta { text });
                                }
                            }
                            StreamEvent::ReasoningDelta(t) => {
                                emit(&events, AgentEvent::ReasoningDelta { text: t.clone() });
                                reasoned = true;
                                thought += t.chars().count() as u64;
                            }
                            StreamEvent::ToolCallStart { .. }
                            | StreamEvent::ToolCallDelta { .. }
                            | StreamEvent::ToolCallEnd { .. } => spoke = true,
                            _ => {}
                        }
                        c.push(ev);
                        // thinking past the budget with nothing said: stop reading, so the provider stops generating
                        if budget > 0 && !spoke && thought / 4 > budget {
                            over_budget = true;
                            break;
                        }
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
        let estimated = over_budget && c.usage.is_empty();
        if estimated {
            // the stream was dropped before the provider's usage event: the output is what was streamed, and
            // the input is what was sent (estimated, in the provider's count), split between cached and not
            // like the last billed call; with none before, all of it is uncached
            let st = state.lock().await;
            let sent = (st.est_sent as f64 * st.tok_ratio.unwrap_or(1.0)).round() as u64;
            let cached = match st.last_call {
                Some(l) if l.total_input() > 0 => {
                    (sent as f64 * l.cache_read as f64 / l.total_input() as f64).round() as u64
                }
                _ => 0,
            };
            drop(st);
            for u in [&mut c.usage, &mut c.call] {
                u.output = thought / 4;
                u.cache_read = cached;
                u.input = sent - cached;
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
        // the stream broke inside the reasoning, before any text or call: the client leaves it alone (the
        // reasoning is on the screen by then), and ending the turn here loses the request to one bad
        // connection. What was thought is dropped, neither kept nor sent back, and the request goes out again
        // through the checks at the top of the loop; what the attempt was billed stays spent
        let lost = c.error.as_ref().filter(|e| e.is_retryable() && reasoned && !spoke && !interrupted);
        let again = RECUT_RETRIES.min(retries);
        match lost {
            Some(e) if recut < again => {
                recut += 1;
                bill_attempt(&state, &events, &req.model, usage, call).await;
                let why =
                    "the stream was cut while the model reasoned; that reasoning is dropped and the request sent again";
                notice(&events, Level::Warn, format!("{why} ({recut}/{again})"));
                tokio::select! {
                    _ = tokio::time::sleep(client.backoff(recut, e)) => {}
                    _ = cancel.cancelled() => return TurnEnd::Interrupted,
                }
                continue;
            }
            _ => recut = 0,
        }
        // a reply cut at the budget said nothing: its thinking is not kept, so a provider that sends reasoning
        // back does not carry thousands of tokens of it into every retry and pull the model back into it
        let kept = if over_budget { without_reasoning(&msg) } else { msg.clone() };
        calm |= over_budget;
        {
            let mut st = state.lock().await;
            if !kept.content.is_empty() {
                st.history.push(kept.clone());
                st.hidden.clear();
                st.hidden_told = 0;
                if let Err(e) = st.session.append(&kept) {
                    notice(&events, Level::Warn, format!("cannot save the session: {e}"));
                }
            }
            if !usage.is_empty() {
                let model = req.model.clone();
                add_usage(&mut st, &events, &model, usage, call, UsageKind::Main);
                // an estimate has no input side: it says nothing about the cache or the token ratio
                if !estimated {
                    st.policy.usage(call.cache_read, call.total_input());
                    st.tok_ratio = learn_ratio(st.tok_ratio, st.est_sent, call.total_input());
                    st.last_call = Some(call);
                }
            }
            if !kept.content.is_empty() && call.total_input() > 0 {
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
            notice_problem(&events, problem_of(&state, &e).await);
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
        if calls.is_empty() && msg.text().trim().is_empty() && !msg.reasoning().trim().is_empty() && mute < mute_asks {
            // the model thought and stopped before writing anything: ending here would leave the request
            // undone with nothing said, so it is asked to go on from its reasoning
            mute += 1;
            // more room only bought more reasoning, so the limit stays; a budget cut says so
            let why = if over_budget {
                format!("the reply reasoned past agent.reasoning_budget ({budget} tokens) with no answer and no call; cut and asked for the next step with less reasoning")
            } else {
                "the reply held only reasoning, with no answer and no call; asked to go on".to_string()
            };
            notice(&events, Level::Warn, why);
            let note = if over_budget {
                // the cut thinking is gone: the ask names why and what to do, and repeats none of it
                budget_note(budget, mute > 1)
            } else {
                // a reply that ended inside its reasoning on its own: the end of it, short, as a lead to go on from
                let note = if mute == 1 { MUTE_NOTE } else { MUTE_NOTE_AGAIN };
                format!("{note}{}", reasoning_tail(&msg.reasoning()))
            };
            push_results(&state, vec![Part::text(note)]).await;
            continue;
        }
        mute = 0;
        // the reply called or said something: the next requests go out at the person's level again
        calm = false;
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
            if msg.text().trim().is_empty() && !msg.reasoning().trim().is_empty() && ck.stopping.is_none() {
                let n = mute_asks + 1;
                let why = if over_budget {
                    let then = if alone {
                        "Raise agent.reasoning_budget or split the request."
                    } else {
                        "Send \"continue\" or rephrase the request."
                    };
                    format!("The model reasoned past agent.reasoning_budget ({budget} tokens) {n} times in a row with no answer; stopped. {then}")
                } else if alone {
                    format!("The model reasoned without answering {n} times in a row; stopped. Raise agent.max_tokens or agent.reasoning_budget.")
                } else {
                    "The model reasoned without answering. Send \"continue\" or raise agent.reasoning_budget."
                        .to_string()
                };
                notice(&events, Level::Warn, why);
            }
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
        // whether each call succeeded, by its result, for what an active goal counts as a change
        let ok: Vec<bool> = kept
            .iter()
            .map(|(id, _, _)| {
                results.iter().any(|p| matches!(p, Part::ToolResult { call_id, .. } if call_id == id) && succeeded(p))
            })
            .collect();
        let (lines, new) = if stale_guard > 0 {
            // the curator's view leaves outputs out: what it did not send is not something the model has seen
            let mut shown = {
                let st = state.lock().await;
                (st.policy.recall() && !st.last_sent.is_empty()).then(|| known_lines(&st.last_sent))
            };
            fresh_count_in(&mut known, shown.as_mut(), &results, false)
        } else {
            (0, 0)
        };
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
        // and, once in the session, the line that says `nexo bin` exists, when a command read a compiled program by hand
        let results = {
            let mut st = state.lock().await;
            let cwd = st.cwd.clone();
            let offer = offer_of(&st);
            let results = crate::bin_hint::round(&mut st.bin_told, &kept, results, &cwd, offer);
            // and the `debug` call that explains a crash, a gdb or strace run, when the tool is offered
            if offer.debug {
                crate::dbg_hint::round(&mut st.dbg_told, &kept, results, &cwd)
            } else {
                results
            }
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
        // `agent.close_on_verified`: this turn changed something, and the calls of this reply include a check that
        // succeeded after the last change; with the answer in the same reply, it is the end of the turn
        let checked = {
            let st = state.lock().await;
            if st.config.agent.close_on_verified {
                let ok: Vec<bool> = if results.len() == kept.len() {
                    results.iter().map(succeeded).collect()
                } else {
                    vec![false; kept.len()]
                };
                let j = verify::judge(&kept, &ok, |n, a| st.tools.get(n).map(|t| t.call_kind(a)));
                changed |= j.changed;
                j.checked && ok.iter().all(|&b| b)
            } else {
                false
            }
        };
        let closing = stop.is_none()
            && !cancelled
            && answer == Answer::No
            && ck.stopping.is_none()
            && !verify::is_lead_in(&msg.text())
            && checked
            && changed;
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
        let last_results = results.clone();
        push_results(&state, results).await;
        if !cancelled {
            watch_round(&state).await;
        }
        if cancelled {
            return TurnEnd::Interrupted;
        }
        // an active goal whose work brings nothing new for too long: the turn ends and the goal pauses, with the facts
        if stop.is_none() {
            let mut guard = state.lock().await;
            let st = &mut *guard;
            if st.goal.is_some() && st.goal_state.is_active() {
                let mut idle = std::mem::take(&mut st.goal_idle);
                let was = goal_calls(st, &mut idle, &kept, &ok, &last_results);
                let step = goal_watch(&mut idle, &was);
                st.goal_idle = idle;
                if let GoalStep::Stop(why) = step {
                    st.ck_stop = Some(why.clone());
                    notice(&events, Level::Warn, format!("stopped: {why} while working on the goal"));
                    return TurnEnd::MaxTurns;
                }
            }
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

/// How many times one request is sent again after its stream broke inside the reasoning (`agent.retries`
/// lower than this, or 0, lowers it).
pub(super) const RECUT_RETRIES: u32 = 2;

/// How many replies in a row that held only reasoning are asked to go on: after them the person is told, and
/// can send "continue".
pub(super) const MUTE_ASKS: u32 = 2;

/// The same with no person there (`nexo -p`): nobody can send "continue" and the request would be lost, so it
/// is asked twice more. Every reply stays under `agent.reasoning_budget`.
pub(super) const MUTE_ASKS_ALONE: u32 = 4;

/// The least reasoning a model lists among its levels (`efforts`): off, then minimal, then low; for the asks
/// after a cut at `agent.reasoning_budget`. Off only where the request acts on it (`off_thinks_less`): an
/// OpenAI protocol sends nothing for it, the same request again, and Opus 5.5 thinks at the server's default
/// effort, more than with no level. None when nothing lower is left, as a provider that takes no level may
/// refuse one; a level returned always asks for less thinking than the first request.
pub(super) fn calm_level(levels: &[String], off_thinks_less: bool) -> Option<ReasoningConfig> {
    let has = |l: &str| levels.iter().any(|x| x == l);
    if off_thinks_less && (has("off") || has("none")) {
        Some(ReasoningConfig::Off)
    } else if has("minimal") {
        Some(ReasoningConfig::Effort(Effort::Minimal))
    } else if has("low") {
        Some(ReasoningConfig::Effort(Effort::Low))
    } else {
        None
    }
}

/// The reply without its reasoning: what a reply cut at the budget keeps.
fn without_reasoning(msg: &Message) -> Message {
    let content =
        msg.content.iter().filter(|p| !matches!(p, Part::Reasoning { .. } | Part::RedactedReasoning { .. })).cloned();
    Message { role: msg.role, content: content.collect() }
}

/// Sent after a reply was cut at `agent.reasoning_budget` with nothing said: the cause, and one thing to do now
/// that needs little thought whatever the request is. `again` is the second cut in a row.
fn budget_note(budget: u64, again: bool) -> String {
    let what = if again {
        format!(
            "Your reasoning passed the {budget}-token limit again and was cut, with no answer and no tool call. \
         Reply now with almost no reasoning: one tool call, or a short answer, or one short question to the person \
         if the request is unclear."
        )
    } else {
        format!(
            "Your reasoning passed the {budget}-token limit and was cut, with no answer and no tool call; that \
         reasoning is gone. Do not think it all over again: make the next tool call now, or answer in a few lines. \
         If the request is a new task, start it with one small step (for example, write the first file)."
        )
    };
    format!("<system-reminder>{what}</system-reminder>")
}

/// Sent when a reply held only reasoning (often cut at max_tokens while thinking): not a request of the person.
pub(super) const MUTE_NOTE: &str =
    "<system-reminder>Your last reply held only reasoning: no answer and no tool call reached the person. \
     Go on from it now with a tool call or the answer, keeping the reasoning short.</system-reminder>";

/// How much of the end of a reasoning-only reply the next ask repeats: a lead to go on from, not the plan again
/// (a provider that sends reasoning back has all of it already).
const MUTE_TAIL: usize = 500;

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
    format!("\n<system-reminder>The end of that reasoning:\n{dots}{}</system-reminder>", &r[from..])
}

/// The second time in a row: planning longer is what keeps failing, so the model is told to build first.
pub(super) const MUTE_NOTE_AGAIN: &str =
    "<system-reminder>Your replies keep ending inside the reasoning, before any answer or tool call. Stop \
     planning: make a tool call now that builds the simplest version that could work (write the file, run the \
     command), then test it and improve it step by step.</system-reminder>";

pub(super) const ANSWER_NOW: &str =
    "Tool calls are off until you answer: they will not run. Give your answer now, from what you have already seen. \
     They are on again after your answer, or when the person writes.";

/// Sent after `CHECKPOINT STOP` with no summary: the request ends on the model's account of its work.
pub(super) const SUMMARY_NOW: &str =
    "<system-reminder>You chose to stop at the checkpoint. Tool calls will not run now. \
     Write your final summary for the person: what was done, what is left and how to go on.</system-reminder>";

/// Sent once when the model chose to continue at a checkpoint but called no tool.
pub(super) const CHECKPOINT_GO_ON: &str =
    "<system-reminder>You chose to continue at the checkpoint. Go on with the next tool call.</system-reminder>";

/// Whether a tool result is a success: not an error, and not a shell command that ended with a non-zero
/// code (that is no failure to show, but a check that answered no is not a check that passed).
fn succeeded(p: &Part) -> bool {
    let Part::ToolResult { content, is_error: false, .. } = p else { return false };
    let text = content.iter().filter_map(Part::as_text).collect::<Vec<_>>().join("\n");
    nexo_core::shell::exit_code_in(&text).is_none_or(|c| c == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(levels: &[&str]) -> Option<ReasoningConfig> {
        calm_level(&levels.iter().map(|l| l.to_string()).collect::<Vec<_>>(), true)
    }

    fn pick_no_off(levels: &[&str]) -> Option<ReasoningConfig> {
        calm_level(&levels.iter().map(|l| l.to_string()).collect::<Vec<_>>(), false)
    }

    /// Where off changes nothing in the request (an OpenAI protocol) or thinks more (Opus 5.5), it is not the
    /// calm level: the next lowest is, and a model with only off and on gets none, so its asks run free.
    #[test]
    fn off_is_the_calm_level_only_where_it_thinks_less() {
        let low = Some(ReasoningConfig::Effort(Effort::Low));
        assert_eq!(pick_no_off(&["off", "low", "medium", "high"]), low);
        assert_eq!(pick_no_off(&["none", "minimal", "low"]), Some(ReasoningConfig::Effort(Effort::Minimal)));
        assert_eq!(pick_no_off(&["off", "on"]), None);
        assert_eq!(pick(&["off", "on"]), Some(ReasoningConfig::Off));
    }

    #[test]
    fn the_calm_level_is_the_least_the_model_lists() {
        let (off, min, low) = (
            Some(ReasoningConfig::Off),
            Some(ReasoningConfig::Effort(Effort::Minimal)),
            Some(ReasoningConfig::Effort(Effort::Low)),
        );
        assert_eq!(pick(&["minimal", "low", "medium", "high"]), min);
        assert_eq!(pick(&["off", "low", "high"]), off);
        assert_eq!(pick(&["high", "none", "minimal"]), off);
        assert_eq!(pick(&["low", "medium", "high"]), low);
        // nothing below medium, or no level at all: none is sent
        assert_eq!(pick(&["medium", "high"]), None);
        assert_eq!(pick(&[]), None);
    }
}
