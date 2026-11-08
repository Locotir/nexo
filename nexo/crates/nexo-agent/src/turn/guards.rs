//! Part of the agent loop; see the parent module.

use super::*;

/// What `checkpoint_now` reads of the loop's locals.
pub(super) struct Standing<'a> {
    pub(super) rounds: &'a [u64],
    pub(super) guard: u32,
    pub(super) stale: u32,
    pub(super) stale_guard: u32,
}

/// The most checkpoints this request may still take: none for a spend limit that is in force (the limit
/// stops the request), `agent.max_checkpoints` or 5 otherwise. A limit counts only once this request has spent
/// something: on a model with no prices, or when the day's spend cannot be kept, it would never fire.
pub(super) fn checkpoint_cap(st: &State, cost0: f64) -> Option<u32> {
    if let Some(n) = st.config.agent.max_checkpoints {
        return Some(n);
    }
    let limited = st.budget.is_some_and(|b| b > 0.0) || st.config.agent.max_cost_day.is_some_and(|d| d > 0.0);
    if limited && st.cost > cost0 {
        None
    } else {
        Some(checkpoint::DEFAULT_MAX)
    }
}

/// A request reached `agent.max_turns` with `on_limit = "checkpoint"`: the message to add to the tool results,
/// or the end of the request when a cap or the lack of progress says so (the model is not asked then).
pub(super) async fn checkpoint_now(
    state: &Shared,
    events: &EventTx,
    ck: &mut checkpoint::Tracker,
    ctx: &Standing<'_>,
) -> Result<String, TurnEnd> {
    let mut st = state.lock().await;
    let (used, cap) = (st.ck_used, checkpoint_cap(&st, ck.cost0));
    if cap.is_some_and(|c| used >= c) {
        let limited = if st.config.agent.max_checkpoints.is_some() { "" } else { "; no spend limit is set" };
        notice(
            events,
            Level::Warn,
            format!("stopped after {used} checkpoints (agent.max_checkpoints{limited}); ask to continue"),
        );
        st.ck_stop = Some(format!("stopped after {used} checkpoints"));
        return Err(TurnEnd::MaxTurns);
    }
    let todo = checkpoint::todo_of(&st.history[st.turn_start.min(st.history.len())..]);
    let now = checkpoint::Mark {
        writes: st.checkpoints.len(),
        wrote: ck.wrote(),
        fresh: ck.fresh,
        todo: todo.as_ref().map_or(0, |t| t.hash),
    };
    // a message of the person that joins now is steering: it counts as progress
    if checkpoint::progress(&now, &ck.prev) || !st.inbox.is_empty() {
        ck.idle = 0;
    } else {
        ck.idle += 1;
    }
    if ck.idle >= 2 {
        notice(
            events,
            Level::Warn,
            "stopped: two checkpoints in a row with no file written, no new output line and no change in the todo list; ask to continue",
        );
        st.ck_stop = Some("two checkpoints in a row without progress".to_string());
        return Err(TurnEnd::MaxTurns);
    }
    let from = st.turn_start.min(st.history.len());
    let distinct = |from: usize| {
        let mut out: Vec<String> = Vec::new();
        for (_, p, _) in st.checkpoints.iter().skip(from) {
            let p = p.display().to_string();
            if !out.contains(&p) {
                out.push(p);
            }
        }
        out
    };
    let first = st.history.get(from).map(Message::text).unwrap_or_default();
    let request = if first.starts_with(GOAL_CONTINUE_MARK) { "(goal continuation)".to_string() } else { first };
    let (u, u0) = (st.usage, ck.usage0);
    let spend = {
        let mut s = format!("${:.2} this request, ${:.2} this session", st.cost - ck.cost0, st.cost);
        if let Some(b) = st.budget.filter(|b| *b > 0.0) {
            s.push_str(&format!(" of ${b:.2} (max_cost)"));
        }
        if let Some(d) = st.config.agent.max_cost_day.filter(|d| *d > 0.0) {
            s.push_str(&format!("; ${:.2} of ${d:.2} today (max_cost_day)", budget::spent_today()));
        }
        match checkpoint_cap(&st, ck.cost0) {
            Some(c) if st.config.agent.max_checkpoints.is_none() => {
                s.push_str(&format!(" | no spend limit is set: at most {c} checkpoints"))
            }
            Some(c) => s.push_str(&format!(" | at most {c} checkpoints")),
            None => s.push_str(" | a spend limit is set: it ends the request"),
        }
        s
    };
    let loop_hits = (2..ctx.guard.max(2)).rev().find(|t| looping(ctx.rounds, *t).is_some()).unwrap_or(0);
    let report = checkpoint::Report {
        number: used + 1,
        cap,
        cost: st.cost - ck.cost0,
        calls: checkpoint::calls_in(&st.history[from..]),
        rounds: ck.rounds,
        input: u.total_input().saturating_sub(u0.total_input()),
        output: u.output.saturating_sub(u0.output),
        cached: u.cache_read.saturating_sub(u0.cache_read),
        elapsed: ck.started.elapsed(),
        request,
        goal: st.goal.clone(),
        todo: todo.clone(),
        prev_todo: ck.prev_todo.clone(),
        files: distinct(ck.writes0),
        new_files: distinct(ck.prev.writes),
        fresh_total: ck.fresh,
        fresh_new: ck.fresh - ck.prev.fresh,
        build: ck.build(),
        errors: ck.repeated(),
        loop_hits,
        loop_limit: ctx.guard,
        stale: ctx.stale,
        stale_limit: ctx.stale_guard,
        spend,
        idle: ck.idle,
    };
    let line = checkpoint::stats(&report);
    notice(events, Level::Info, format!("⟳ checkpoint {} · {line} · asking", report.number));
    st.ck_used += 1;
    ck.pending = Some((report.number, line));
    ck.nudged = false;
    ck.prev = now;
    ck.prev_todo = todo;
    Ok(checkpoint::render(&report))
}

/// Notes the files the write tools of a round left: only a write that succeeded, with content this request had
/// not left in that file before, is progress (a rewrite of the same bytes, a denied or failed write is not).
pub(super) async fn note_writes(
    state: &Shared,
    ck: &mut checkpoint::Tracker,
    asked: &[(String, String)],
    results: &[Part],
) {
    use std::hash::{Hash, Hasher};
    let outs: Vec<bool> = results
        .iter()
        .filter_map(|p| if let Part::ToolResult { is_error, .. } = p { Some(*is_error) } else { None })
        .collect();
    if outs.len() != asked.len() {
        return;
    }
    let st = state.lock().await;
    for ((name, args), failed) in asked.iter().zip(outs) {
        if failed || !st.tools.get(name).is_some_and(|t| t.kind() == ToolKind::Write) {
            continue;
        }
        let args: Value = serde_json::from_str(args).unwrap_or(Value::Null);
        let Some(p) = args.get("path").and_then(Value::as_str) else { continue };
        let path = if Path::new(p).is_absolute() { PathBuf::from(p) } else { st.cwd.join(p) };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        match std::fs::read(&path) {
            Ok(bytes) => bytes.hash(&mut h),
            Err(_) => args.to_string().hash(&mut h),
        }
        ck.note_write(&path.display().to_string(), h.finish());
    }
}

/// The model chose to stop at a checkpoint: an active goal pauses there, unless the reply meets or awaits it.
pub(super) async fn checkpoint_stopped(state: &Shared, why: &str, reply: &str) {
    let mut st = state.lock().await;
    if st.goal.is_some() && st.goal_state.is_active() && !goal_done(reply) && !goal_wait(reply) {
        let say = format!("goal paused: stopped at a checkpoint ({why}) · /goal resume goes on");
        goal_to(&mut st, GoalState::Paused(format!("stopped at a checkpoint: {why}")), say);
    }
}

pub(super) fn then(answer: bool) -> &'static str {
    if answer {
        "; one last call without tools asks for the answer"
    } else {
        "; ask to continue"
    }
}

/// One tool round as a hash of what it asked (tool and arguments) and what came back, call ids aside: two
/// rounds with the same hash brought the agent nothing the other did not.
pub(super) fn round_hash(asked: &[(String, String)], results: &[Part]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    asked.hash(&mut h);
    for r in results {
        if let Part::ToolResult { name, content, is_error, .. } = r {
            (name, serde_json::to_string(content).unwrap_or_default(), is_error).hash(&mut h);
        }
    }
    h.finish()
}

/// Puts the output lines of `parts` among the `known` ones and says whether any was not there yet, or None
/// when there was no line (`agent.stale_guard`). A line counts trimmed and from four characters on: blank
/// lines, braces and short markers come back in any output and tell nothing of what it brought.
pub(super) fn fresh_lines(known: &mut HashSet<u64>, parts: &[Part]) -> Option<bool> {
    match fresh_count(known, parts, false) {
        (0, _) => None,
        (_, new) => Some(new > 0),
    }
}

/// [`fresh_lines`] as numbers: the lines counted and how many of them were new. With `norm` a line counts
/// with its digit runs read as one mark, so a clock, a counter or a pid does not make an old line new.
pub(super) fn fresh_count(known: &mut HashSet<u64>, parts: &[Part], norm: bool) -> (usize, usize) {
    use std::hash::{Hash, Hasher};
    let (mut lines, mut fresh) = (0, 0);
    for p in parts {
        if let Part::ToolResult { content, .. } = p {
            for line in content.iter().filter_map(Part::as_text).flat_map(str::lines) {
                let line = line.trim();
                if line.chars().count() >= 4 {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    if norm {
                        numberless(line).hash(&mut h);
                    } else {
                        line.hash(&mut h);
                    }
                    lines += 1;
                    fresh += usize::from(known.insert(h.finish()));
                }
            }
        }
    }
    (lines, fresh)
}

/// `line` with each run of digits turned into one `#`.
pub(super) fn numberless(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        if c.is_ascii_digit() {
            if !out.ends_with('#') {
                out.push('#');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The lines a conversation has brought, digits aside: what the checkpoints call new.
pub(super) fn known_numberless(history: &[Message]) -> HashSet<u64> {
    let mut known = HashSet::new();
    for m in history {
        let _ = fresh_count(&mut known, &m.content, true);
    }
    known
}

/// The output lines a conversation has already brought, as `fresh_lines` counts them.
pub(super) fn known_lines(history: &[Message]) -> HashSet<u64> {
    let mut known = HashSet::new();
    for m in history {
        let _ = fresh_lines(&mut known, &m.content);
    }
    known
}

/// `agent.loop_guard`: the length `p` of the block of rounds that closes `rounds` and ran `times` times in a
/// row, the same calls with the same outputs each time (the shortest such block). A person watching would
/// see the agent going down the same path again with nothing new on it: a command run over and over, or a
/// cycle of reads that never changes. None below two times, or when the latest rounds are not such a loop.
pub(super) fn looping(rounds: &[u64], times: u32) -> Option<usize> {
    let (n, t) = (rounds.len(), times as usize);
    if t < 2 {
        return None;
    }
    (1..=n / t).find(|&p| (1..t).all(|k| (0..p).all(|i| rounds[n - 1 - i] == rounds[n - 1 - i - k * p])))
}
