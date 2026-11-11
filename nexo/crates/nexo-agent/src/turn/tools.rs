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

/// The result of a call that was skipped because the call before it in its message failed.
const NOT_RUN: &str =
    "not run: the call before it in this message failed. Read its result, then send this call again if it is still wanted.";

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
    // every verdict is in by now: reads, searches, page fetches and pure checks can overlap
    let overlaps: Vec<bool> = calls
        .iter()
        .map(|(_, n, a)| {
            tools
                .get(n)
                .map(|t| {
                    matches!(t.call_kind(a), ToolKind::Read | ToolKind::Search | ToolKind::Network)
                        || is_pure_check(n, a)
                })
                .unwrap_or(false)
        })
        .collect();
    let mut results: Vec<Option<Part>> = (0..calls.len()).map(|_| None).collect();
    let mut cancelled = false;
    // a call that ran alone and failed: the calls after it in this message are not run
    let mut stopped = false;
    let step = step_before(state, &tools, &cwd, &calls).await;
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
        } else if stopped {
            for (j, (id, name, _)) in calls.iter().enumerate().take(end).skip(i) {
                let out = ToolOutput::error(NOT_RUN);
                results[j] = Some(Part::ToolResult {
                    call_id: id.clone(),
                    name: Some(name.clone()),
                    content: vec![Part::text(NOT_RUN)],
                    is_error: true,
                });
                // it wrote nothing: the step check must not count it as a call that ran
                verdicts[j] = (Verdict::Denied(NOT_RUN.into()), Why::denied("rule"));
                emit(events, AgentEvent::ToolDone { id: id.clone(), output: out, elapsed_ms: 0 });
            }
        } else {
            let futs = calls[i..end].iter().zip(verdicts[i..end].iter()).map(|((id, name, args), (v, why))| {
                run_one(state, events, &tools, &cwd, &session_id, id, name, args, v, *why, cancel)
            });
            for (k, (part, c)) in futures::future::join_all(futs).await.into_iter().enumerate() {
                results[i + k] = Some(part);
                cancelled |= c;
            }
            // only a call that ran alone stops the rest: reads, searches and pure checks are independent
            if !overlaps[i] && !cancelled {
                stopped = matches!(&results[i], Some(Part::ToolResult { is_error: true, .. }));
            }
        }
        i = end;
    }
    if let (Some(step), false) = (step, cancelled) {
        step_check(state, &tools, &cwd, &calls, &verdicts, &mut results, step, cancel).await;
    }
    (results.into_iter().flatten().collect(), cancelled)
}

/// A plain `bash` call that only looks or runs a build, test or lint (the classifier of `verify`): it changes
/// nothing a neighbour call depends on, so it may overlap with other calls of its kind.
fn is_pure_check(name: &str, args: &Value) -> bool {
    let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
    name == "bash"
        && !flag("background")
        && !flag("detach")
        && !flag("unsandboxed")
        && args.get("command").and_then(Value::as_str).is_some_and(verify::is_check_command)
}

/// The answer to a check command the step-level check ran already, when no source file has changed since.
async fn repeated_check(state: &Shared, name: &str, args: &Value, cwd: &Path) -> Option<ToolOutput> {
    let command = args.get("command").and_then(Value::as_str).filter(|_| name == "bash")?;
    if !nexo_tools::project::is_check_command(command) {
        return None;
    }
    let (guard, on) = {
        let st = state.lock().await;
        (st.post.clone(), st.config.tools.post_write_check)
    };
    if !on {
        return None;
    }
    let root = cwd.to_path_buf();
    let now = tokio::task::spawn_blocking(move || nexo_tools::postcheck::snapshot(&root)).await.ok()?;
    let text = guard.recorded(command, nexo_tools::postcheck::fingerprint(&now))?;
    Some(ToolOutput::ok(format!(
        "(unchanged since the check that ran after your last change; no source file differs, so this is its result)\n{}",
        text.trim_start()
    )))
}

/// The paths an `edit` or `write` call names.
fn changed_paths(args: &Value) -> Vec<String> {
    let one = args.get("path").and_then(Value::as_str).map(str::to_string);
    let many = args.get("files").and_then(Value::as_array).into_iter().flatten();
    one.into_iter().chain(many.filter_map(|f| f.get("path").and_then(Value::as_str).map(str::to_string))).collect()
}

/// The files before a step that may change code, when a check after it is wanted.
struct StepBefore {
    files: nexo_tools::postcheck::Snapshot,
    secs: u64,
}

/// Takes the snapshot when the post-write check is on, the folder is a project and a call can change files.
async fn step_before(
    state: &Shared,
    tools: &ToolSet,
    cwd: &Path,
    calls: &[(String, String, Value)],
) -> Option<StepBefore> {
    let secs = {
        let st = state.lock().await;
        if !st.config.tools.post_write_check {
            return None;
        }
        st.config.tools.post_write_check_secs
    };
    let writes = calls.iter().any(|(_, n, a)| {
        tools.get(n).is_some_and(|t| matches!(t.call_kind(a), ToolKind::Write | ToolKind::Exec)) && n != "recall"
    });
    // without a project check, only a command can change a data file unseen by the tools that report it
    let runs_command = calls.iter().any(|(_, n, a)| tools.get(n).is_some_and(|t| t.call_kind(a) == ToolKind::Exec));
    let has_check = nexo_tools::project::check_command_within(cwd, Some(cwd)).is_some();
    if !writes || !(has_check || runs_command) {
        return None;
    }
    let root = cwd.to_path_buf();
    let files = tokio::task::spawn_blocking(move || nexo_tools::postcheck::snapshot(&root)).await.ok()?;
    Some(StepBefore { files, secs })
}

/// After a step: when it changed source files and did not prove itself, the project's check runs once and its
/// result joins the last call that changed files.
#[allow(clippy::too_many_arguments)]
async fn step_check(
    state: &Shared,
    tools: &ToolSet,
    cwd: &Path,
    calls: &[(String, String, Value)],
    verdicts: &[(Verdict, Why)],
    results: &mut [Option<Part>],
    before: StepBefore,
    cancel: &CancellationToken,
) {
    use nexo_tools::{postcheck, project};
    // the model ran a check itself in this step (or asked for one with `check`): that is the proof
    let proved = calls.iter().zip(verdicts).any(|((_, n, a), (v, _))| {
        matches!(v, Verdict::Run)
            && (a.get("check").and_then(Value::as_str).is_some_and(|c| !c.trim().is_empty())
                || (n == "bash" && a.get("command").and_then(Value::as_str).is_some_and(project::is_check_command)))
    });
    if proved {
        return;
    }
    let root = cwd.to_path_buf();
    let Ok(after) = tokio::task::spawn_blocking(move || postcheck::snapshot(&root)).await else { return };
    let changed = postcheck::changed(&before.files, &after);
    let Some(first) = changed.first() else { return };
    // the call that changed them: the last that ran as an edit, a write or a command
    let Some(at) = calls.iter().zip(verdicts).enumerate().rev().find_map(|(i, ((_, n, a), (v, _)))| {
        let kind = tools.get(n).map(|t| t.call_kind(a));
        (matches!(v, Verdict::Run) && matches!(kind, Some(ToolKind::Write | ToolKind::Exec))).then_some(i)
    }) else {
        return;
    };
    let dir = first.parent().unwrap_or(cwd);
    let Some(check) = project::check_command_within(dir, Some(cwd)) else {
        data_check(cwd, calls, &changed, results, at);
        return;
    };
    let (guard, perm) = {
        let st = state.lock().await;
        (st.post.clone(), st.perm.clone())
    };
    let cap = std::time::Duration::from_secs(before.secs.max(1));
    let text = if let Some(report) =
        without_tests(&guard, &perm, &check, cwd, cap, tools.get("bash").is_some(), &changed).await
    {
        report
    } else if let Some(bash) = tools.get("bash") {
        let mut ctx = ToolContext::new(check.dir.clone());
        ctx.sandbox = state.lock().await.sandbox.clone();
        ctx.cancel = cancel.child_token();
        let t0 = Instant::now();
        let out = bash.run(serde_json::json!({"command": check.command, "timeout": before.secs.max(1)}), &ctx).await;
        let timed_out = out.content.contains("[killed: timed out after");
        guard.record_run(&check.command, &check.dir, timed_out);
        let exit = match &out.display {
            ToolDisplay::Shell { exit_code, .. } => *exit_code,
            _ => None,
        };
        let text = guard.report(&check.command, exit, timed_out, t0.elapsed().as_secs_f64(), cap, &out.content);
        guard.remember(&check.command, postcheck::fingerprint(&after), &text);
        text
    } else {
        return;
    };
    if let Some(Part::ToolResult { content, .. }) = results[at].as_mut() {
        match content.first_mut() {
            Some(Part::Text { text: t }) => t.push_str(&text),
            _ => content.push(Part::text(text.trim_start().to_string())),
        }
    }
}

/// A project without a check command: the data files a command changed, checked by their format and
/// reported with the result of the call that changed them (those `edit` and `write` changed report themselves).
fn data_check(
    cwd: &Path,
    calls: &[(String, String, Value)],
    changed: &[PathBuf],
    results: &mut [Option<Part>],
    at: usize,
) {
    let own: Vec<PathBuf> = calls
        .iter()
        .filter(|(_, n, _)| n == "edit" || n == "write")
        .flat_map(|(_, _, a)| changed_paths(a))
        .map(|p| if Path::new(&p).is_absolute() { PathBuf::from(p) } else { cwd.join(p) })
        .collect();
    let text: String = changed
        .iter()
        .filter(|f| nexo_tools::data_check::is_data(f) && f.is_file() && !own.contains(f))
        .take(5)
        .map(|f| nexo_tools::data_check::report(f))
        .collect();
    if text.is_empty() {
        return;
    }
    if let Some(Part::ToolResult { content, .. }) = results[at].as_mut() {
        match content.first_mut() {
            Some(Part::Text { text: t }) => t.push_str(&text),
            _ => content.push(Part::text(text.trim_start().to_string())),
        }
    }
}

/// When the project's command is not run after a step: why, with the compiler's word on each changed file
/// in its place (what the edit alone would have given). None where the command runs.
async fn without_tests(
    guard: &nexo_tools::postcheck::Guard,
    perm: &Policy,
    check: &nexo_tools::project::Check,
    cwd: &Path,
    cap: std::time::Duration,
    has_bash: bool,
    changed: &[PathBuf],
) -> Option<String> {
    // the project's command is a command like any other: it runs only where the policy lets the model run it
    let note = if let Some(note) = guard.skip_note(&check.command, &check.dir, cap) {
        note
    } else {
        match perm.decide("bash", ToolKind::Exec, &serde_json::json!({"command": check.command}), cwd) {
            Decision::Allow if has_bash => return None,
            Decision::Allow => format!("\ncheck: {} | not run: no command tool", check.command),
            Decision::Ask => {
                format!("\ncheck: {} | not run: commands are asked about in this mode; run it yourself", check.command)
            }
            Decision::Deny(why) => format!("\ncheck: {} | not run: denied by a rule ({why})", check.command),
        }
    };
    let mut diag = String::new();
    for f in changed.iter().take(5) {
        diag.push_str(&nexo_tools::edit_check::report(f).await);
    }
    Some(format!("{diag}{note}"))
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
            // a file a running sub-agent is changing is its own until it ends
            if tool.kind() == ToolKind::Write {
                let claims = state.lock().await.claims.clone();
                for p in nexo_core::tool::call_paths(args) {
                    if let Some(label) = claims.holder(&nexo_core::claims::plain_path(&p, cwd)) {
                        let out = ToolOutput::error(format!(
                            "`{p}` is being changed by a sub-agent that is still running ({label}); leave it to that \
                             one until it ends, or stop it with the `jobs` tool"
                        ));
                        emit(events, AgentEvent::ToolDone { id: id.into(), output: out.clone(), elapsed_ms: 0 });
                        return (result(&out), false);
                    }
                }
            }
            // what a file looked like before the agent writes it, for a rewind
            if tool.kind() == ToolKind::Write {
                for p in nexo_core::tool::call_paths(args).iter().map(String::as_str) {
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
            let (jobs, claims, detach, seen, perm, rules_cwd, sandbox, vision, post_check, check_secs) = {
                let mut st = state.lock().await;
                count_step(&mut st);
                let (provider, _, model) = st.target.get();
                // a picture is withheld only where the model catalogue says the model cannot see it
                let vision = nexo_providers::catalogue::cached()
                    .and_then(|c| c.lookup(&provider, &model).and_then(|e| e.vision))
                    .unwrap_or(true);
                let check_secs = st.config.tools.post_write_check_secs;
                // the per-edit diagnostics give way only where the step-level check will run
                let post_check = st.config.tools.post_write_check
                    && nexo_tools::project::check_command_within(cwd, Some(cwd)).is_some();
                (
                    st.jobs.clone(),
                    st.claims.clone(),
                    st.detach.clone(),
                    st.seen.clone(),
                    st.perm.clone(),
                    cwd.to_path_buf(),
                    st.sandbox.clone(),
                    vision,
                    post_check,
                    check_secs,
                )
            };
            let ctx = ToolContext {
                cwd: cwd.to_path_buf(),
                session_id: session_id.to_string(),
                call_id: id.to_string(),
                cancel: cancel.child_token(),
                progress: Some(ptx),
                env: Default::default(),
                jobs,
                claims,
                detach: Some(detach),
                seen,
                sandbox,
                vision,
                post_check,
                check_secs,
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
            } else if let Some(recorded) = repeated_check(state, name, args, cwd).await {
                recorded
            } else {
                if tool.handles_cancel() {
                    // it ends itself, with its cleanup done (a sub-agent's worktree)
                    tool.run(args.clone(), &ctx).await
                } else {
                    tokio::select! {
                        o = tool.run(args.clone(), &ctx) => o,
                        _ = cancel.cancelled() => ToolOutput::error("interrupted by the person"),
                    }
                }
            };
            drop(ctx);
            let _ = forwarder.await;
            if name == "bash" {
                // a sandboxed command changed a path that runs outside the sandbox later: the person is told too
                if let Some(note) = nexo_tools::guarded::note_in(&out.content) {
                    let st = state.lock().await;
                    notice(&st.events, Level::Warn, note.trim_matches(['[', ']']).to_string());
                }
            }
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
                    ("edit" | "write", _) if !out.is_error => {
                        // the result carries a short file whole: a later read of it, unchanged, needs no lines
                        for p in changed_paths(args) {
                            let path = if Path::new(&p).is_absolute() { PathBuf::from(&p) } else { cwd.join(&p) };
                            st.reads.after_change(&path, id);
                        }
                        None
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

#[cfg(test)]
mod step_check_tests {
    use super::*;
    use crate::permission::Policy;
    use nexo_tools::postcheck::Guard;

    fn crate_with_error() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("Cargo.toml"),
            "[package]\nname = \"c\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir(d.path().join("src")).unwrap();
        let f = d.path().join("src/lib.rs");
        std::fs::write(&f, "pub fn f() -> i32 { \"x\" }\n").unwrap();
        (d, f)
    }

    fn policy(mode: PermissionMode, deny: &[&str]) -> Policy {
        Policy { mode, allow: vec![], deny: deny.iter().map(|s| s.to_string()).collect() }
    }

    #[tokio::test]
    async fn the_step_check_is_not_run_where_commands_are_asked_about_or_denied() {
        let (d, f) = crate_with_error();
        let check = nexo_tools::project::check_command_within(d.path(), Some(d.path())).unwrap();
        let cap = Duration::from_secs(45);
        let g = Guard::new();
        let ask = policy(PermissionMode::AcceptEdits, &[]);
        let r = without_tests(&g, &ask, &check, d.path(), cap, true, std::slice::from_ref(&f)).await.unwrap();
        assert!(r.contains("| not run: commands are asked about"), "{r}");
        let deny = policy(PermissionMode::Auto, &["bash(cargo test*)"]);
        let r = without_tests(&g, &deny, &check, d.path(), cap, true, std::slice::from_ref(&f)).await.unwrap();
        assert!(r.contains("| not run: denied"), "{r}");
        for p in [policy(PermissionMode::Auto, &[]), Policy { allow: vec!["bash(cargo test*)".into()], ..ask }] {
            assert!(without_tests(&g, &p, &check, d.path(), cap, true, std::slice::from_ref(&f)).await.is_none());
        }
    }

    #[tokio::test]
    async fn a_skipped_check_still_carries_the_compiler_diagnostics() {
        let (d, f) = crate_with_error();
        let check = nexo_tools::project::check_command_within(d.path(), Some(d.path())).unwrap();
        // cargo's first run in a new crate (lock file, build folder) is not what this test is about, and on
        // a busy machine it can use up the compiler check's 20 s
        let _ = std::process::Command::new("cargo").args(["check", "-q", "--offline"]).current_dir(d.path()).output();
        let cap = Duration::from_secs(45);
        let g = Guard::new();
        g.record_run(&check.command, &check.dir, true);
        let auto = policy(PermissionMode::Auto, &[]);
        let r = without_tests(&g, &auto, &check, d.path(), cap, true, &[f]).await.unwrap();
        assert!(r.contains("diagnostics:") && r.contains("| skipped |"), "{r}");
    }
}
