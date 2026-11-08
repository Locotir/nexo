//! Part of the agent loop; see the parent module.

use super::*;

pub(super) fn cancelled_results(calls: &[(String, String, Value)], reason: &str) -> Vec<Part> {
    calls
        .iter()
        .map(|(id, name, _)| Part::ToolResult {
            call_id: id.clone(),
            name: Some(name.clone()),
            content: vec![Part::text(format!("The call did not run: {reason}."))],
            is_error: true,
        })
        .collect()
}

pub(super) async fn push_results(state: &Shared, results: Vec<Part>) {
    if results.is_empty() {
        return;
    }
    let mut st = state.lock().await;
    // the outputs, and after them the text the loop adds (`ANSWER_NOW`)
    let m = Message { role: Role::User, content: results };
    st.history.push(m.clone());
    if let Err(e) = st.session.append(&m) {
        notice(&st.events, Level::Warn, format!("cannot save the session: {e}"));
    }
}

/// Runs the calls of one assistant message; (results in order, interrupted).
pub(super) async fn run_tools(
    state: &Shared,
    events: &EventTx,
    calls: Vec<(String, String, Value)>,
    cancel: &CancellationToken,
) -> (Vec<Part>, bool) {
    let (tools, cwd, gate, session_id) = {
        let st = state.lock().await;
        (st.tools.clone(), st.cwd.clone(), st.gate.clone(), st.session.id().to_string())
    };
    // PreToolUse hooks first: one may block a call, decide its permission or replace its input
    let mut calls = calls;
    let mut pre: Vec<(Option<String>, Option<crate::hooks::Decision>)> = Vec::with_capacity(calls.len());
    for (_, name, args) in calls.iter_mut() {
        let o = fire(state, "pre_tool", Some(name), crate::hooks::tool_fields(name, args)).await;
        if let Some(u) = o.updated_input {
            *args = u;
        }
        pre.push((o.blocked, o.decision));
    }
    for (id, name, args) in &calls {
        emit(
            events,
            AgentEvent::ToolCall {
                id: id.clone(),
                name: name.clone(),
                args: args.clone(),
                summary: summary_of(&tools, name, args),
            },
        );
    }
    // permissions first, in order, so the asks come one at a time
    let mut verdicts: Vec<(Verdict, Why)> = Vec::with_capacity(calls.len());
    for ((id, name, args), (blocked, decision)) in calls.iter().zip(pre) {
        if cancel.is_cancelled() {
            verdicts.push((Verdict::Denied("interrupted by the person".into()), Why::denied("user")));
            continue;
        }
        if let Some(why) = blocked {
            verdicts.push((Verdict::Blocked(why), Why::denied("hook")));
            continue;
        }
        let Some(tool) = tools.get(name) else {
            verdicts.push((Verdict::Denied(format!("unknown tool `{name}`")), Why::denied("rule")));
            continue;
        };
        let v = permit_why(state, events, &gate, &cwd, id, name, tool.as_ref(), args, cancel, decision, None).await;
        verdicts.push(v);
    }
    // every verdict is in by now: reads, searches and page fetches can overlap
    let overlaps: Vec<bool> = calls
        .iter()
        .map(|(_, n, a)| {
            tools
                .get(n)
                .map(|t| matches!(t.call_kind(a), ToolKind::Read | ToolKind::Search | ToolKind::Network))
                .unwrap_or(false)
        })
        .collect();
    let mut results: Vec<Option<Part>> = (0..calls.len()).map(|_| None).collect();
    let mut cancelled = false;
    // in the order asked: each run of calls that can overlap goes at once, any other call alone, so one
    // command among three sub-agents does not make the sub-agents wait for each other
    let mut i = 0;
    while i < calls.len() {
        let end = if overlaps[i] { i + overlaps[i..].iter().take_while(|o| **o).count() } else { i + 1 };
        if cancelled || cancel.is_cancelled() {
            cancelled = true;
            for (j, (id, name, args)) in calls.iter().enumerate().take(end).skip(i) {
                results[j] = Some(
                    cancelled_results(&[(id.clone(), name.clone(), args.clone())], "interrupted by the person")
                        .remove(0),
                );
                emit(
                    events,
                    AgentEvent::ToolDone { id: id.clone(), output: ToolOutput::error("interrupted"), elapsed_ms: 0 },
                );
            }
        } else {
            let futs = calls[i..end].iter().zip(verdicts[i..end].iter()).map(|((id, name, args), (v, why))| {
                run_one(state, events, &tools, &cwd, &session_id, id, name, args, v, *why, cancel)
            });
            for (k, (part, c)) in futures::future::join_all(futs).await.into_iter().enumerate() {
                results[i + k] = Some(part);
                cancelled |= c;
            }
        }
        i = end;
    }
    (results.into_iter().flatten().collect(), cancelled)
}

/// One call: its permission result, its run, and its line in the audit log.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_one(
    state: &Shared,
    events: &EventTx,
    tools: &ToolSet,
    cwd: &Path,
    session_id: &str,
    id: &str,
    name: &str,
    args: &Value,
    verdict: &Verdict,
    why: Why,
    cancel: &CancellationToken,
) -> (Part, bool) {
    let t0 = Instant::now();
    let done = run_call(state, events, tools, cwd, session_id, id, name, args, verdict, cancel).await;
    let audit = state.lock().await.audit.clone();
    if let Some(audit) = audit {
        let (provider, model, exit_code) = {
            let st = state.lock().await;
            let (p, _, m) = st.target.get();
            let exit = match st.session.outputs().get(id).map(|o| &o.display) {
                Some(ToolDisplay::Shell { exit_code, .. }) => *exit_code,
                _ => None,
            };
            (p.to_string(), m.to_string(), exit)
        };
        let is_error = matches!(&done.0, Part::ToolResult { is_error: true, .. });
        let status = if matches!(verdict, Verdict::Run) {
            if is_error {
                "error"
            } else {
                "ok"
            }
        } else {
            "denied"
        };
        let summary = summary_of(tools, name, args);
        audit.tool(ToolRecord {
            session: session_id,
            cwd,
            provider: &provider,
            model: &model,
            tool: name,
            args,
            summary: &summary,
            why,
            status,
            exit_code,
            duration_ms: t0.elapsed().as_millis() as u64,
        });
    }
    done
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_call(
    state: &Shared,
    events: &EventTx,
    tools: &ToolSet,
    cwd: &Path,
    session_id: &str,
    id: &str,
    name: &str,
    args: &Value,
    verdict: &Verdict,
    cancel: &CancellationToken,
) -> (Part, bool) {
    let result = |out: &ToolOutput| {
        let mut content =
            vec![Part::text(if out.content.is_empty() { "(no output)".to_string() } else { out.content.clone() })];
        content.extend(out.images.iter().cloned().map(|source| Part::Image { source }));
        Part::ToolResult { call_id: id.to_string(), name: Some(name.to_string()), content, is_error: out.is_error }
    };
    let output = match verdict {
        Verdict::Denied(reason) => ToolOutput::error(format!("Permission denied: {reason}")),
        Verdict::Blocked(why) => ToolOutput::error(format!("Blocked by a hook: {why}")),
        Verdict::Run => {
            let tool = tools.get(name).cloned();
            let Some(tool) = tool else {
                let out = ToolOutput::error(format!("unknown tool `{name}`"));
                emit(events, AgentEvent::ToolDone { id: id.into(), output: out.clone(), elapsed_ms: 0 });
                return (result(&out), false);
            };
            // what a file looked like before the agent writes it, for a rewind
            if tool.kind() == ToolKind::Write {
                if let Some(p) = args.get("path").and_then(Value::as_str) {
                    let path = if Path::new(p).is_absolute() { PathBuf::from(p) } else { cwd.join(p) };
                    let before = pre_image(&path);
                    let mut st = state.lock().await;
                    match before {
                        Some(before) => {
                            let at = st.history.len();
                            st.checkpoints.push((at, path, before));
                        }
                        None => notice(
                            &st.events,
                            Level::Warn,
                            format!("could not read {} before the edit; a rewind will not restore it", path.display()),
                        ),
                    }
                }
            }
            emit(events, AgentEvent::ToolStart { id: id.into() });
            let (ptx, mut prx) = tokio::sync::mpsc::unbounded_channel::<ToolProgress>();
            let fwd_events = events.clone();
            let forwarder = tokio::spawn(async move {
                while let Some(p) = prx.recv().await {
                    emit(&fwd_events, AgentEvent::ToolProgress { id: p.call_id, line: p.line });
                }
            });
            // the rules as they stand now, for a tool that is led somewhere it was not asked about
            let (jobs, seen, perm, rules_cwd, sandbox, vision) = {
                let st = state.lock().await;
                let (provider, _, model) = st.target.get();
                // a picture is withheld only where the model catalogue says the model cannot see it
                let vision = nexo_providers::catalogue::cached()
                    .and_then(|c| c.lookup(&provider, &model).and_then(|e| e.vision))
                    .unwrap_or(true);
                (st.jobs.clone(), st.seen.clone(), st.perm.clone(), cwd.to_path_buf(), st.sandbox.clone(), vision)
            };
            let ctx = ToolContext {
                cwd: cwd.to_path_buf(),
                session_id: session_id.to_string(),
                call_id: id.to_string(),
                cancel: cancel.child_token(),
                progress: Some(ptx),
                env: Default::default(),
                jobs,
                seen,
                sandbox,
                vision,
                check: Some(Arc::new(move |tool: &str, kind, args: &Value| {
                    match perm.decide(tool, kind, args, &rules_cwd) {
                        Decision::Allow => nexo_core::tool::Check::Allow,
                        Decision::Ask => nexo_core::tool::Check::Ask,
                        Decision::Deny(_) => nexo_core::tool::Check::Deny,
                    }
                })),
            };
            let t0 = Instant::now();
            let out = if name == "recall" {
                crate::recall::answer(&state.lock().await.history, args)
            } else if name == "goal_brief" {
                goal_brief(state, args).await
            } else {
                tokio::select! {
                    o = tool.run(args.clone(), &ctx) => o,
                    _ = cancel.cancelled() => ToolOutput::error("interrupted by the person"),
                }
            };
            drop(ctx);
            let _ = forwarder.await;
            if name == "exit_plan" {
                let mut st = state.lock().await;
                let switch = st.plan.lock().unwrap_or_else(|e| e.into_inner()).switch.take();
                if let Some(mode) = switch {
                    set_mode(&mut st, mode);
                }
            }
            let elapsed_ms = t0.elapsed().as_millis() as u64;
            // the file the call printed (part of), if that is all it did
            let file =
                match (name, args.get("path").and_then(Value::as_str).filter(|_| !nexo_core::tool::has_paths(args))) {
                    ("read", Some(p)) => {
                        Some((if Path::new(p).is_absolute() { PathBuf::from(p) } else { cwd.join(p) }, p.to_string()))
                    }
                    ("bash", _) => args.get("command").and_then(Value::as_str).and_then(crate::reads::shell_print).map(
                        |(rel, _)| {
                            (
                                if rel.is_absolute() { rel.clone() } else { cwd.join(&rel) },
                                rel.to_string_lossy().into_owned(),
                            )
                        },
                    ),
                    _ => None,
                };
            let seen = {
                let mut st = state.lock().await;
                let _ = st.session.record_output(id, &out);
                match (name, &file) {
                    ("read", Some((path, p))) if !out.is_error => {
                        let offset = args.get("offset").and_then(Value::as_u64);
                        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(READ_LIMIT);
                        st.reads.after_read(path, p, offset, limit, id, crate::reads::printed(&out))
                    }
                    ("bash", Some((path, p))) if !out.is_error => {
                        let span = args.get("command").and_then(Value::as_str).and_then(crate::reads::shell_print);
                        let printed = crate::reads::printed(&out);
                        span.and_then(|(_, span)| st.reads.after_lines(path, p, span, id, printed))
                    }
                    _ => None,
                }
            };
            emit(events, AgentEvent::ToolDone { id: id.into(), output: out.clone(), elapsed_ms });
            if name == "todo" && !out.is_error {
                if let Some((done, total)) = todo_counts(args) {
                    emit(events, AgentEvent::TodoChanged { done, total });
                }
            }
            // PostToolUse hooks: their text (a note, or the reason of exit 2) goes to the model after the output
            let mut hook_notes: Vec<String> = Vec::new();
            {
                let mut fields = crate::hooks::tool_fields(name, args);
                fields["output"] = serde_json::json!(out.content);
                fields["tool_response"] = serde_json::json!(out.content);
                fields["is_error"] = serde_json::json!(out.is_error);
                fields["elapsed_ms"] = serde_json::json!(elapsed_ms);
                let o = fire(state, "post_tool", Some(name), fields).await;
                hook_notes.extend(o.context);
                hook_notes.extend(o.blocked);
            }
            // the person sees the lines; the model, which has them, a note
            let mut sent = match seen {
                Some(note) => ToolOutput::ok(note),
                None if !out.is_error => match digest(state, events, name, args, file, &out, cancel).await {
                    Some(notes) => ToolOutput::ok(notes),
                    None => out,
                },
                None => out,
            };
            if !hook_notes.is_empty() {
                sent.content = format!("{}\n\n[hook] {}", sent.content, hook_notes.join("\n[hook] "));
            }
            return (result(&sent), cancel.is_cancelled());
        }
    };
    emit(events, AgentEvent::ToolDone { id: id.into(), output: output.clone(), elapsed_ms: 0 });
    (result(&output), false)
}

/// With `[context] reader_at`, the notes a separate reader request takes from a long output (from the
/// whole file, when the call printed part of one), in the form the session gets them; None when the
/// output is short, too long for one request, or the reader failed (then the output goes as it is).
pub(super) async fn digest(
    state: &Shared,
    events: &EventTx,
    name: &str,
    args: &Value,
    file: Option<(PathBuf, String)>,
    out: &ToolOutput,
    cancel: &CancellationToken,
) -> Option<String> {
    let (ask, client, target, retries, temperature) = {
        let st = state.lock().await;
        let at = st.config.context.reader_at;
        if at == 0 || out.content.len() as u64 / 4 <= at {
            return None;
        }
        let whole = file.and_then(|(path, shown)| {
            let text = std::fs::read_to_string(&path).ok()?;
            let lines = text.lines().count();
            (text.len() <= reader::MATERIAL_CHARS)
                .then(|| (format!("{shown}, whole file, {lines} lines"), reader::numbered(&text, 1)))
        });
        let (label, material) = match whole {
            Some(w) => w,
            None if out.content.len() <= reader::MATERIAL_CHARS => {
                (format!("output of {}", summary_of(&st.tools, name, args)), reader::numbered(&out.content, 1))
            }
            None => return None,
        };
        let ask = reader::Ask::from_history(&st.history, label, material);
        if let Some(notes) = st.digests.get(&ask.key()) {
            return Some(reader::shown(&ask.label, notes));
        }
        let (retries, temperature) = (st.config.agent.retries, st.config.agent.temperature);
        (ask, st.client.clone(), st.target.clone(), retries, temperature)
    };
    let done = tokio::select! {
        r = reader::read(&*client, &target, retries, temperature, &ask) => r,
        _ = cancel.cancelled() => return None,
    };
    let mut st = state.lock().await;
    match done {
        Ok((notes, usage, model)) => {
            add_usage(&mut st, events, &model, usage, Usage::default(), UsageKind::Reader);
            st.digests.insert(ask.key(), notes.clone());
            Some(reader::shown(&ask.label, &notes))
        }
        Err(e) => {
            notice(events, Level::Warn, format!("{e}; the output goes as it is"));
            None
        }
    }
}
