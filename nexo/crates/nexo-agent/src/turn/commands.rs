//! Part of the agent loop; see the parent module.

use super::*;

/// The recorded conversation, as the interface would have seen it live:
/// every message, the calls with their outputs, and the session's usage.
pub(super) fn replay_history(events: &EventTx, session: &Session, tools: &ToolSet) {
    for (index, m) in session.messages().iter().enumerate() {
        emit(events, AgentEvent::History { index, message: m.clone() });
        if m.role == Role::Assistant {
            for (id, name, args) in m.tool_calls() {
                emit(
                    events,
                    AgentEvent::ToolCall {
                        id: id.into(),
                        name: name.into(),
                        args: args.clone(),
                        summary: summary_of(tools, name, args),
                    },
                );
                if let Some(out) = session.outputs().get(id) {
                    emit(events, AgentEvent::ToolDone { id: id.into(), output: out.clone(), elapsed_ms: 0 });
                }
            }
        }
    }
    emit(
        events,
        AgentEvent::Usage {
            turn: Usage::default(),
            call: Usage::default(),
            session: session.usage(),
            cost_usd: session.cost(),
        },
    );
}

/// Makes `session` the conversation: the interface starts over and gets the replay.
pub(super) fn install_session(st: &mut State, session: Session) {
    // the jobs of the session left behind stop, and none of them is reported to this one
    st.jobs.kill_all();
    st.jobs.take_ended();
    st.jobs.subs().clear();
    // its scratch folder (`$NEXO_TMP`) goes with it
    nexo_tools::bash::wipe_tmp(st.session.id());
    st.session = session;
    prefix::adopt(st);
    st.checkpoints.clear();
    st.history.replace(st.session.messages().to_vec());
    st.usage = st.session.usage();
    st.cost = st.session.cost();
    st.policy.reset();
    st.last_prompt = None;
    st.last_call = None;
    st.last_request = None;
    st.repeats = Default::default();
    st.bin_told = crate::bin_hint::told(&st.history);
    st.hidden.clear();
    st.hidden_told = 0;
    st.noticed.clear();
    st.checked_in = false;
    st.epoch += 1;
    flow::release_slot(st);
    st.turn_start = 0;
    st.turns = 0;
    st.goal = st.session.goal().map(String::from);
    st.goal_state = st.session.goal_state().clone();
    st.goal_round = 0;
    st.goal_log = resumed_log(st.goal.as_deref());
    st.goal_cost0 = st.cost;
    emit(&st.events, AgentEvent::HistoryCleared);
    emit(
        &st.events,
        AgentEvent::SessionChanged { session_id: st.session.id().to_string(), title: st.session.meta().title.clone() },
    );
    mail_sync(st);
    emit_goal(st);
    replay_history(&st.events, &st.session, &st.tools);
    close_open_calls(st);
}

/// A session that stopped between its last calls and their results (closed, killed, cut off): the
/// calls are answered as not run, and the answer is saved, so the history goes on as every protocol
/// takes it instead of failing each request after.
pub(super) fn close_open_calls(st: &mut State) {
    let calls: Vec<(String, String, Value)> = match st.history.last() {
        Some(m) if m.role == Role::Assistant => {
            m.tool_calls().map(|(i, n, a)| (i.to_string(), n.to_string(), a.clone())).collect()
        }
        _ => return,
    };
    if calls.is_empty() {
        return;
    }
    let msg = Message::tool_results(cancelled_results(&calls, "the session ended before the tools ran"));
    st.history.push(msg.clone());
    if let Err(e) = st.session.append(&msg) {
        notice(&st.events, Level::Warn, format!("cannot save the session: {e}"));
    }
    let n = calls.len();
    notice(
        &st.events,
        Level::Info,
        format!("{n} tool call(s) left open when the session ended are answered as not run"),
    );
}

pub(super) async fn resume_session(state: &Shared, id: &str) {
    let mut st = state.lock().await;
    if id == st.session.id() && !st.session.is_lost() {
        // a second copy would drop the lock this one holds when it replaced it (a lost session holds none)
        notice(&st.events, Level::Info, format!("session {id} is the one in use"));
        return;
    }
    // taken before anything is spent: a session another nexo writes is not resumed here
    match Session::open(id).and_then(|mut s| s.claim().map(|_| s)) {
        Ok(s) => {
            install_session(&mut st, s);
            if !st.history.is_empty() {
                view_now(&mut st).await;
                // a view shown, not sent: the next request finds no cache of it
                st.last_sent.clear();
            }
            drop(st);
            restore_model(state, false).await;
        }
        Err(e) => notice_failure(
            &st.events,
            format!("Could not resume session {id}"),
            e,
            &["See your sessions with `/resume`."],
        ),
    }
}

/// `/fork`: a new session from this one up to before message `index` (all of it when None), and
/// the conversation moves to it. The session left behind stays as it was.
pub(super) async fn fork_session(state: &Shared, index: Option<usize>) {
    let mut st = state.lock().await;
    let keep = index.map(|i| cut_point(&st.history, i)).unwrap_or(st.history.len());
    let from = st.session.id().to_string();
    match st.session.fork(keep) {
        Ok(s) => {
            install_session(&mut st, s);
            if !st.history.is_empty() {
                view_now(&mut st).await;
                // a view shown, not sent: the next request finds no cache of it
                st.last_sent.clear();
            }
            let id = st.session.id().to_string();
            notice(&st.events, Level::Info, format!("forked {from} into {id} (/resume tree to jump back)"));
        }
        Err(e) => notice(&st.events, Level::Error, format!("cannot fork the session: {e}")),
    }
}

/// Readies what session `id` knows (bearing on `focus`, or on its goal or title) to go with the next request.
pub(super) async fn share_memory(state: &Shared, id: Option<String>, focus: Option<String>) {
    let mut st = state.lock().await;
    let Some(id) = id else {
        st.shared.clear();
        emit(&st.events, AgentEvent::MemoryShared { pending: Vec::new() });
        return;
    };
    if st.session.id() == id {
        notice(&st.events, Level::Warn, "that is this session: its memory is already here");
        return;
    }
    let other = match crate::session::Session::open(&id) {
        Ok(s) => s,
        Err(e) => return notice(&st.events, Level::Error, format!("cannot read session {id}: {e}")),
    };
    let title = other.meta().title.clone();
    let focus = focus.or_else(|| other.goal().map(str::to_string)).unwrap_or_default();
    let cwd = other.meta().cwd.clone();
    match crate::brief::recall(&id, &title, &focus, other.messages(), &cwd, SHARE_TOKENS) {
        Some((block, _)) => {
            st.shared.retain(|(i, _, _)| *i != id);
            st.shared.push((id, title, block));
            let pending = st.shared.iter().map(|(i, t, b)| (i.clone(), t.clone(), crate::brief::tokens(b))).collect();
            emit(&st.events, AgentEvent::MemoryShared { pending });
        }
        None => notice(&st.events, Level::Info, format!("session {id} holds nothing to share")),
    }
}

/// How much of another session one share brings, at most (tokens).
pub(super) const SHARE_TOKENS: usize = 4000;

/// Moves a recorded session to the trash; the one in use stays (it is still being written).
pub(super) async fn delete_session(state: &Shared, id: &str) {
    let st = state.lock().await;
    if st.session.id() == id {
        notice(&st.events, Level::Warn, "this is the session in use: /clear first, then delete it");
        return;
    }
    let title = crate::session::Session::open(id).map(|s| s.meta().title.clone()).unwrap_or_default();
    match crate::session::trash(id) {
        Ok(_) => emit(&st.events, AgentEvent::SessionDeleted { id: id.to_string(), title }),
        Err(e) => notice(&st.events, Level::Error, format!("cannot delete: {e}")),
    }
}

/// Where a rewind to `index` really cuts: never between an assistant's
/// calls and their results, so the kept history stays well-formed.
pub(super) fn cut_point(history: &[Message], index: usize) -> usize {
    let mut i = index.min(history.len());
    while i > 0
        && history[i - 1].role == Role::Assistant
        && history[i - 1].has_tool_calls()
        && history.get(i).map(Message::is_tool_results).unwrap_or(true)
    {
        i -= 1;
    }
    i
}

pub(super) async fn rewind(state: &Shared, index: usize) {
    let mut st = state.lock().await;
    let keep = cut_point(&st.history, index);
    if let Err(e) = st.session.rewind(keep) {
        notice(&st.events, Level::Error, format!("cannot rewind the session: {e}"));
        return;
    }
    let dropped = st.history.len().saturating_sub(keep);
    st.history.truncate(keep);
    let restored = restore_files(&mut st, keep);
    st.policy.reset();
    st.last_prompt = None;
    st.last_call = None;
    st.last_request = None;
    st.repeats = Default::default();
    st.bin_told = crate::bin_hint::told(&st.history);
    st.hidden.clear();
    st.hidden_told = 0;
    st.noticed.clear();
    st.checked_in = false;
    st.epoch += 1;
    st.turn_start = 0;
    emit(&st.events, AgentEvent::HistoryCleared);
    emit(
        &st.events,
        AgentEvent::SessionChanged { session_id: st.session.id().to_string(), title: st.session.meta().title.clone() },
    );
    // the view dropped its runs with the history: it is told where each one stands
    for e in sub_resync(&st.jobs.subs()) {
        emit(&st.events, e);
    }
    mail_sync(&st);
    replay_history(&st.events, &st.session, &st.tools);
    if !st.history.is_empty() {
        view_now(&mut st).await;
        // a view shown, not sent: the next request finds no cache of it
        st.last_sent.clear();
    }
    let files = match restored.len() {
        0 => String::new(),
        n => {
            let names: Vec<String> =
                restored.iter().take(4).map(|p| p.strip_prefix(&st.cwd).unwrap_or(p).display().to_string()).collect();
            let more = if n > 4 { format!(" and {} more", n - 4) } else { String::new() };
            format!("; {n} file{} put back as they were: {}{more}", if n == 1 { "" } else { "s" }, names.join(", "))
        }
    };
    notice(
        &st.events,
        Level::Info,
        format!("rewound: {} dropped, {keep} kept{files}", nexo_core::count(dropped, "message")),
    );
}

/// A file as it is before a write, for a rewind: `Some(None)` when it did not exist, `Some(Some(bytes))`
/// when it was read, and `None` when it could not be read (no checkpoint then: a rewind would delete it).
pub(super) fn pre_image(path: &Path) -> Option<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(b) => Some(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(None),
        Err(_) => None,
    }
}

/// Puts back every file the agent wrote after `keep` messages as it was before the first
/// such write (a file that did not exist is removed); the checkpoints after `keep` go.
/// Changes made by shell commands are not known and stay.
pub(super) fn restore_files(st: &mut State, keep: usize) -> Vec<PathBuf> {
    let mut seen: Vec<PathBuf> = Vec::new();
    for (at, path, before) in &st.checkpoints {
        if *at <= keep || seen.contains(path) {
            continue;
        }
        let done = match before {
            Some(bytes) => std::fs::write(path, bytes),
            None => std::fs::remove_file(path).or_else(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(e)
                }
            }),
        };
        match done {
            Ok(()) => seen.push(path.clone()),
            Err(e) => notice(&st.events, Level::Warn, format!("could not put back {}: {e}", path.display())),
        }
    }
    st.checkpoints.retain(|(at, _, _)| *at <= keep);
    seen
}

pub(super) async fn export(state: &Shared, path: Option<PathBuf>, format: Option<String>) {
    use crate::export::{render, Format};
    let st = state.lock().await;
    let format = match (&format, &path) {
        (Some(f), _) => Format::parse(f).unwrap_or(Format::Markdown),
        (None, Some(p)) => Format::of_path(p),
        (None, None) => Format::Markdown,
    };
    let path = match path {
        Some(p) if p.is_absolute() => p,
        Some(p) => st.cwd.join(p),
        None => st.session.export_path().with_extension(format.extension()),
    };
    // secrets are always taken out of what the interface writes; `nexo export --no-redact` is the opt-out
    let text = render(&st.session, format, true, &|name, args| summary_of(&st.tools, name, args));
    let written =
        path.parent().map(std::fs::create_dir_all).unwrap_or(Ok(())).and_then(|_| std::fs::write(&path, text));
    match written {
        Ok(()) => notice(&st.events, Level::Info, format!("exported to {}", path.display())),
        Err(e) => notice(&st.events, Level::Error, format!("cannot export to {}: {e}", path.display())),
    }
}

pub(super) async fn clear(state: &Shared) {
    let mut st = state.lock().await;
    let (provider, _, model) = st.target.get();
    nexo_tools::bash::wipe_tmp(st.session.id());
    st.session = match Session::create(&st.cwd, &provider, &model) {
        Ok(s) => s,
        Err(e) => {
            notice(&st.events, Level::Warn, format!("the session is not being saved: {e}"));
            Session::ephemeral(&st.cwd, &provider, &model)
        }
    };
    {
        let s = &mut *st;
        s.prefix = prefix::fresh(&mut s.session, &s.cwd, &s.config.agent, &s.tools);
    }
    st.system = full_system(&st);
    st.history.clear();
    st.checkpoints.clear();
    st.policy.reset();
    st.last_prompt = None;
    st.last_call = None;
    st.last_request = None;
    st.repeats = Default::default();
    st.bin_told = crate::bin_hint::told(&st.history);
    st.hidden.clear();
    st.hidden_told = 0;
    st.noticed.clear();
    st.checked_in = false;
    st.epoch += 1;
    flow::release_slot(&st);
    st.reads.clear();
    st.digests.clear();
    st.inbox.clear();
    st.usage = Usage::default();
    st.cost = 0.0;
    st.turn_start = 0;
    st.turns = 0;
    let had_goal = st.goal.take().is_some();
    st.goal_round = 0;
    st.goal_log = GoalLog::default();
    st.goal_state = GoalState::Active;
    if had_goal {
        emit_goal(&st);
    }
    emit(
        &st.events,
        AgentEvent::Ready {
            session_id: st.session.id().to_string(),
            provider,
            model,
            cwd: st.cwd.clone(),
            permission_mode: st.perm.mode,
        },
    );
    emit(&st.events, AgentEvent::HistoryCleared);
    emit(&st.events, AgentEvent::SessionChanged { session_id: st.session.id().to_string(), title: String::new() });
    mail_sync(&st);
}

/// A `!` command: run outside the model's view, its output shown.
pub(super) fn run_shell(cwd: PathBuf, command: String, events: EventTx) {
    tokio::spawn(async move {
        let shell = nexo_core::shell::shell();
        let run =
            tokio::process::Command::new(&shell.program).args(shell.args_for(&command)).current_dir(&cwd).output();
        let (output, exit_code) = match tokio::time::timeout(Duration::from_secs(120), run).await {
            Ok(Ok(o)) => {
                let mut s = String::from_utf8_lossy(&o.stdout).to_string();
                let err = String::from_utf8_lossy(&o.stderr);
                if !err.trim().is_empty() {
                    if !s.is_empty() && !s.ends_with('\n') {
                        s.push('\n');
                    }
                    s.push_str(&err);
                }
                (s, o.status.code())
            }
            Ok(Err(e)) => (format!("cannot run: {e}"), None),
            Err(_) => ("timed out after 120 s".to_string(), None),
        };
        let output = if output.chars().count() > SHELL_CHARS {
            let head: String = output.chars().take(SHELL_CHARS / 2).collect();
            let tail: String =
                output.chars().rev().take(SHELL_CHARS / 2).collect::<Vec<_>>().into_iter().rev().collect();
            format!("{head}\n[... output shortened ...]\n{tail}")
        } else {
            output
        };
        emit(&events, AgentEvent::ShellOutput { command, output, exit_code });
    });
}
