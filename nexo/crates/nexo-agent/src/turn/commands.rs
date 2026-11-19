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

/// Tells the tools the session that is now the conversation, so one that keeps a list for it (`todo`) takes up
/// what its messages and what was saved for it say, and lets go of what it held for the one before.
pub(super) fn session_opened(st: &State) {
    let id = st.session.id();
    for name in st.tools.names() {
        if let Some(t) = st.tools.get(name) {
            t.session_opened(id, st.session.messages());
        }
    }
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
    session_opened(st);
    crate::context::record(&mut st.session, st.policy.as_ref(), &st.config.context.policy);
    prefix::adopt(st);
    st.checkpoints.clear();
    st.history.replace(st.session.messages().to_vec());
    st.usage = st.session.usage();
    st.cost = st.session.cost();
    st.policy.reset();
    st.view_failed = false;
    st.fit_tokens = None;
    st.last_prompt = None;
    st.pauses = Default::default();
    st.last_call = None;
    st.last_request = None;
    st.repeats = Default::default();
    st.bin_told = crate::bin_hint::told(&st.history);
    st.dbg_told = crate::dbg_hint::told(&st.history);
    st.ask_told = crate::ask_hint::told(&st.history);
    st.ask_again = None;
    st.machine.forget();
    st.hidden.clear();
    st.hidden_told = 0;
    st.noticed.clear();
    st.checked_in = false;
    st.held = false;
    st.invalidate_directed();
    st.epoch += 1;
    flow::release_slot(st);
    st.turn_start = 0;
    st.turns = 0;
    st.goal_log = reopened_goal(&mut st.session, &st.events);
    st.goal = st.session.goal().map(String::from);
    st.goal_state = st.session.goal_state().clone();
    st.goal_round = 0;
    st.goal_cost0 = st.cost;
    st.goal_idle = GoalIdle::default();
    st.goal_counts = GoalCounts::new(st.cost);
    st.goal_unread = false;
    st.goal_pending = None;
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
    let _ = st.session.append(&msg);
    saving(st);
    let n = calls.len();
    notice(
        &st.events,
        Level::Info,
        format!("{n} tool call(s) left open when the session ended are answered as not run"),
    );
}

pub(super) async fn resume_session(state: &Shared, id: &str) {
    {
        let st = state.lock().await;
        if id == st.session.id() && !st.session.is_lost() {
            // a second copy would drop the lock this one holds when it replaced it (a lost session holds none)
            notice(&st.events, Level::Info, format!("session {id} is the one in use"));
            return;
        }
    }
    // The file is read and claimed with the state unlocked: the jobs, mail and sub-agents that need it meanwhile
    // are not held for the read. Commands are handled one at a time, so the session in use is still the one
    // checked above when the lock is taken again.
    // taken before anything is spent: a session another nexo writes is not resumed here
    let wanted = id.to_string();
    let opened = tokio::task::spawn_blocking(move || Session::open(&wanted).and_then(|mut s| s.claim().map(|_| s)))
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("the read did not finish: {e}")));
    let mut st = state.lock().await;
    match opened {
        Ok(s) => {
            skipped_note(&st.events, id, &s);
            install_session(&mut st, s);
            if !st.history.is_empty() {
                super::saved_state::load(&mut st);
                view_now(&mut st, 0).await;
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
                view_now(&mut st, 0).await;
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
    let (events, id, room) = {
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
        // what already waits (other than this session, which is read again) uses part of the one share's room
        let room = room_left(&st, std::slice::from_ref(&id));
        if room < MIN_SHARE_TOKENS {
            notice(&st.events, Level::Warn, ROOM_USED_UP);
            return;
        }
        (st.events.clone(), id, room)
    };
    // the other session's file is read with the state unlocked
    let read = recall_sessions(vec![id.clone()], focus, room).await.pop().map(|(_, r)| r);
    match read {
        Some(Ok(Some((title, block)))) => {
            let mut st = state.lock().await;
            st.shared.retain(|(i, _, _)| *i != id);
            st.shared.push((id, title, block));
            emit_pending(&st);
        }
        Some(Ok(None)) => notice(&events, Level::Info, format!("session {id} holds nothing to share")),
        Some(Err(e)) => notice(&events, Level::Error, e),
        None => {}
    }
}

/// Readies what each of several sessions knows to go with the next request, next to what already waits from other
/// sessions (as `share_memory` does). One share's room ([`SHARE_TOKENS`]) is divided among them and what waits, so
/// all of it together is no larger than one share was, however the shares were asked for.
pub(super) async fn share_memories(state: &Shared, ids: Vec<String>) {
    let (events, ids, each) = {
        let st = state.lock().await;
        let here = st.session.id().to_string();
        let mut ids: Vec<String> = ids.into_iter().fold(Vec::new(), |mut all, id| {
            if id != here && !all.contains(&id) {
                all.push(id);
            }
            all
        });
        if ids.is_empty() {
            notice(&st.events, Level::Warn, "no other session to take memory from");
            return;
        }
        let room = room_left(&st, &ids);
        let most = MAX_SHARED_SESSIONS.min(room / MIN_SHARE_TOKENS);
        if most == 0 {
            notice(&st.events, Level::Warn, ROOM_USED_UP);
            return;
        }
        if ids.len() > most {
            notice(
                &st.events,
                Level::Warn,
                format!("taking the first {most} sessions: the room does not stretch further"),
            );
            ids.truncate(most);
        }
        let each = room / ids.len();
        (st.events.clone(), ids, each)
    };
    // the sessions' files are read with the state unlocked
    let mut taken = Vec::new();
    for (id, read) in recall_sessions(ids, None, each).await {
        match read {
            Ok(Some((title, block))) => taken.push((id, title, block)),
            Ok(None) => notice(&events, Level::Info, format!("session {id} holds nothing to share")),
            Err(e) => notice(&events, Level::Error, e),
        }
    }
    if taken.is_empty() {
        return;
    }
    let mut st = state.lock().await;
    st.shared.retain(|(i, _, _)| !taken.iter().any(|(t, _, _)| t == i));
    st.shared.extend(taken);
    emit_pending(&st);
}

/// What is left of one share's room once the blocks waiting for sessions other than `replacing` are counted.
fn room_left(st: &State, replacing: &[String]) -> usize {
    let used: usize =
        st.shared.iter().filter(|(i, _, _)| !replacing.contains(i)).map(|(_, _, b)| crate::brief::tokens(b)).sum();
    SHARE_TOKENS.saturating_sub(used)
}

const ROOM_USED_UP: &str = "the room for shared memory is used up: `/shared-memory off` clears what waits";

/// The interface is told which sessions' memory waits and how large each block is.
fn emit_pending(st: &State) {
    let pending = st.shared.iter().map(|(i, t, b)| (i.clone(), t.clone(), crate::brief::tokens(b))).collect();
    emit(&st.events, AgentEvent::MemoryShared { pending });
}

/// [`recall_session`] for each of `ids`, read on a blocking thread so the state lock is not held while the files
/// are. A read that did not finish is told as that session's error.
async fn recall_sessions(
    ids: Vec<String>,
    focus: Option<String>,
    budget: usize,
) -> Vec<(String, Result<Option<(String, String)>, String>)> {
    let read = ids.clone();
    let done = tokio::task::spawn_blocking(move || {
        read.into_iter()
            .map(|id| {
                let got = recall_session(&id, focus.clone(), budget);
                (id, got)
            })
            .collect::<Vec<_>>()
    })
    .await;
    done.unwrap_or_else(|e| {
        ids.into_iter()
            .map(|id| (id.clone(), Err(format!("cannot read session {id}: the read did not finish: {e}"))))
            .collect()
    })
}

/// What session `id` knows, bearing on `focus` (else its goal or title), within about `budget` tokens: its
/// title and the block that names it. Ok(None) when it holds nothing; Err is the words for the person.
fn recall_session(id: &str, focus: Option<String>, budget: usize) -> Result<Option<(String, String)>, String> {
    let other = crate::session::Session::open(id).map_err(|e| format!("cannot read session {id}: {e}"))?;
    let title = other.meta().title.clone();
    let focus = focus.or_else(|| other.goal().map(str::to_string)).unwrap_or_default();
    let cwd = other.meta().cwd.clone();
    Ok(crate::brief::recall(id, &title, &focus, other.messages(), &cwd, budget).map(|(block, _)| (title, block)))
}

/// User-facing operations on the same locked coordination store used by the tools.
pub(super) async fn read_shared_memory(state: &Shared, since: u64) {
    let st = state.lock().await;
    match crate::share::read_since(since) {
        Ok(entries) if entries.is_empty() => {
            notice(&st.events, Level::Info, "No shared memory entries after this epoch.")
        }
        Ok(entries) => notice(
            &st.events,
            Level::Info,
            format!("Shared memory (continue after the last epoch):\n{}", crate::share::block(&entries)),
        ),
        Err(e) => notice(&st.events, Level::Error, format!("cannot read shared memory: {e}")),
    }
}

pub(super) async fn append_shared_memory(state: &Shared, text: &str) {
    let st = state.lock().await;
    match crate::share::append(st.session.id(), text) {
        Ok(entry) => notice(&st.events, Level::Info, format!("Shared: {}", crate::share::block(&[entry]))),
        Err(e) => notice(&st.events, Level::Error, format!("cannot write shared memory: {e}")),
    }
}

/// How much of another session one share brings, at most (tokens).
pub(super) const SHARE_TOKENS: usize = 4000;

/// Most sessions one share takes: each keeps at least this many tokens' worth of room (`recall` gives up under 200).
pub(super) const MAX_SHARED_SESSIONS: usize = 8;

/// The least room a share is worth reading with: `recall` gives up under this many tokens.
const MIN_SHARE_TOKENS: usize = 200;

/// Moves a recorded session to the trash; the one in use stays (it is still being written).
pub(super) async fn delete_session(state: &Shared, id: &str) {
    let events = {
        let st = state.lock().await;
        if st.session.id() == id {
            notice(&st.events, Level::Warn, "this is the session in use: /clear first, then delete it");
            return;
        }
        st.events.clone()
    };
    // The title is read as the list reads it (not the whole conversation), and the file moves, with the state
    // unlocked: a running turn is not held for either. Commands are handled one at a time, so the session in use
    // is still the one checked above.
    let doomed = id.to_string();
    let done = tokio::task::spawn_blocking(move || {
        let title = crate::session::meta_in(&crate::session::dir(), &doomed).map(|m| m.title).unwrap_or_default();
        crate::session::trash(&doomed).map(|_| title)
    })
    .await;
    match done {
        Ok(Ok(title)) => emit(&events, AgentEvent::SessionDeleted { id: id.to_string(), title }),
        Ok(Err(e)) => notice(&events, Level::Error, format!("cannot delete: {e}")),
        Err(e) => notice(&events, Level::Error, format!("cannot delete: {e}")),
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
    // the task list is the one the kept messages left: one set by what was dropped no longer holds
    session_opened(&st);
    st.policy.reset();
    st.view_failed = false;
    st.fit_tokens = None;
    st.last_prompt = None;
    st.last_call = None;
    st.last_request = None;
    st.repeats = Default::default();
    st.bin_told = crate::bin_hint::told(&st.history);
    st.dbg_told = crate::dbg_hint::told(&st.history);
    st.ask_told = crate::ask_hint::told(&st.history);
    st.ask_again = None;
    st.machine.forget();
    st.hidden.clear();
    st.hidden_told = 0;
    st.noticed.clear();
    st.checked_in = false;
    st.held = false;
    st.invalidate_directed();
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
        view_now(&mut st, 0).await;
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
    notice(&st.events, Level::Info, rewound_text(dropped, keep, &files));
}

/// The notice after a rewind; "kept" is said only when something was kept.
fn rewound_text(dropped: usize, keep: usize, files: &str) -> String {
    let kept = if keep == 0 { String::new() } else { format!(", {keep} kept") };
    format!("rewound: {} dropped{kept}{files}", nexo_core::count(dropped, "message"))
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
    {
        let s = &mut *st;
        crate::context::record(&mut s.session, s.policy.as_ref(), &s.config.context.policy);
    }
    session_opened(&st);
    st.system = full_system(&st);
    st.history.clear();
    st.checkpoints.clear();
    st.policy.reset();
    st.view_failed = false;
    st.fit_tokens = None;
    st.last_prompt = None;
    st.pauses = Default::default();
    st.last_call = None;
    st.last_request = None;
    st.repeats = Default::default();
    st.bin_told = crate::bin_hint::told(&st.history);
    st.dbg_told = crate::dbg_hint::told(&st.history);
    st.ask_told = crate::ask_hint::told(&st.history);
    st.ask_again = None;
    st.machine.forget();
    st.hidden.clear();
    st.hidden_told = 0;
    st.noticed.clear();
    st.checked_in = false;
    st.held = false;
    st.invalidate_directed();
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
    st.goal_unread = false;
    st.goal_pending = None;
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

#[cfg(test)]
mod rewound_tests {
    use super::rewound_text;

    #[test]
    fn a_rewind_to_nothing_does_not_say_zero_kept() {
        assert_eq!(rewound_text(2, 0, ""), "rewound: 2 messages dropped");
        assert_eq!(rewound_text(3, 1, ""), "rewound: 3 messages dropped, 1 kept");
    }
}
