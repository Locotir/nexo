//! Session-local recurring prompts. Never persisted or run concurrently with a turn.
use super::*;

const MAX_LOOPS: usize = 8;
const MAX_INTERVAL: u64 = 30 * 24 * 60 * 60;
const USAGE: &str = "usage: /loop <interval> <prompt> (e.g. 60s, 5m, 2h, 1d), /loop list, /loop cancel <id>, /loop resume <id>; intervals: 60s–30d";

struct Task {
    id: u64,
    seconds: u64,
    prompt: String,
    due: Instant,
    person: bool,
    paused: bool,
}

#[derive(Default)]
pub(super) struct Loops {
    tasks: Vec<Task>,
    next_id: u64,
    running: Option<u64>,
    pending: Option<Pending>,
}

struct Pending {
    id: u64,
    prompt: String,
    person: bool,
    config: nexo_core::config::Config,
    cwd: PathBuf,
    session: String,
    hook: Option<std::pin::Pin<Box<dyn std::future::Future<Output = crate::hooks::Outcome> + Send>>>,
    outcome: Option<crate::hooks::Outcome>,
    intervened: bool,
}

pub(super) fn preparing(loops: &Loops) -> bool {
    loops.pending.is_some()
}

pub(super) fn pending(loops: &Loops) -> bool {
    loops.pending.as_ref().is_some_and(|p| p.hook.is_some())
}

// A cancelled hook may already have performed effects. Require an explicit resume.
pub(super) fn abandon(rt: &mut Runtime) {
    if let Some(p) = rt.loops.pending.take() {
        if let Some(task) = rt.loops.tasks.iter_mut().find(|t| t.id == p.id) {
            task.paused = true;
            notice(
                &rt.events,
                Level::Warn,
                format!(
                    "loop #{} paused: preparation interrupted; review possible hook effects before /loop resume {}",
                    p.id, p.id
                ),
            );
        }
    }
}

// A competing turn can change both history and external policy state even if it
// finishes before the hook does. Never reuse this preparation afterwards.
pub(super) fn invalidate_for_intervention(rt: &mut Runtime) {
    if let Some(p) = rt.loops.pending.as_mut() {
        p.intervened = true;
    }
}

pub(super) async fn wait_hook(loops: &mut Loops) -> crate::hooks::Outcome {
    loops.pending.as_mut().unwrap().hook.as_mut().unwrap().as_mut().await
}

pub(super) async fn hook_done(rt: &mut Runtime, outcome: crate::hooks::Outcome) {
    let Some(p) = rt.loops.pending.as_mut() else { return };
    // A denial is authoritative even if a human turn or inbox arrived meanwhile.
    if let Some(why) = outcome.blocked.clone() {
        let id = p.id;
        rt.loops.pending = None;
        notice(&rt.events, Level::Warn, format!("your message was not sent: a hook stopped it ({why})"));
        if let Some(task) = rt.loops.tasks.iter_mut().find(|t| t.id == id) {
            task.paused = true;
        }
        notice(&rt.events, Level::Warn, format!("loop #{id} paused: prompt was blocked; /loop resume {id} to resume"));
        return;
    }
    if p.intervened {
        stale(rt);
        return;
    }
    p.hook = None;
    p.outcome = Some(outcome);
    // Admission is deferred to the tick, after queued commands have been considered.
    // Do not announce a hypothetical wait: admission or a concrete pause follows.
}

fn stale(rt: &mut Runtime) {
    if let Some(p) = rt.loops.pending.take() {
        if let Some(task) = rt.loops.tasks.iter_mut().find(|t| t.id == p.id) {
            task.paused = true;
            notice(&rt.events, Level::Warn, format!(
                "loop #{} paused: preparation invalidated by intervening activity; review possible hook effects before /loop resume {}",
                p.id, p.id
            ));
        }
    }
}

async fn admit(rt: &mut Runtime) {
    if !rt.loops.pending.as_ref().is_some_and(|p| p.outcome.is_some()) {
        return;
    }
    if rt.loops.pending.as_ref().is_some_and(|p| p.intervened) {
        stale(rt);
        return;
    }
    let st = rt.state.lock().await;
    if !ready(rt.turn.is_some(), st.held, !st.inbox.is_empty()) {
        return;
    }
    let p = rt.loops.pending.as_ref().unwrap();
    let changed = p.config != st.config || p.cwd != st.cwd || p.session != st.session.id();
    drop(st);
    if changed {
        abandon(rt);
        return;
    }
    let p = rt.loops.pending.take().unwrap();
    let Some(task) = rt.loops.tasks.iter_mut().find(|t| t.id == p.id && !t.paused) else { return };
    // Measure from admission, not hook start: a long hook cannot cause an immediate rerun.
    task.due = Instant::now() + Duration::from_secs(task.seconds);
    // Keep the prompt in its History message only, not in a second transcript notice.
    notice(&rt.events, Level::Info, format!("loop #{}: running scheduled prompt", p.id));
    rt.turn = Some(
        begin_scheduled_turn(&rt.state, p.prompt, p.person, rt.cancel.child_token(), p.outcome.unwrap().context).await,
    );
    rt.loops.running = Some(p.id);
}

impl Loops {
    fn finish(&mut self, reason: TurnEnd) -> Option<u64> {
        let id = self.running.take()?;
        if matches!(reason, TurnEnd::Error | TurnEnd::MaxTurns | TurnEnd::Interrupted) {
            if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
                task.paused = true;
                return Some(id);
            }
        }
        None
    }
}

fn interval(s: &str) -> Option<u64> {
    let (number, factor) = match s.as_bytes().last()? {
        b's' => (&s[..s.len() - 1], 1),
        b'm' => (&s[..s.len() - 1], 60),
        b'h' => (&s[..s.len() - 1], 3600),
        b'd' => (&s[..s.len() - 1], 86400),
        _ => return None,
    };
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = number.parse::<u64>().ok()?.checked_mul(factor)?;
    (60..=MAX_INTERVAL).contains(&n).then_some(n)
}

pub(super) fn command(rt: &mut Runtime, input: &str, person: bool) -> ToolOutput {
    let input = input.trim();
    let out = if input == "list" {
        let lines: Vec<_> = rt
            .loops
            .tasks
            .iter()
            .map(|t| {
                format!("loop #{} every {}s{}: {}", t.id, t.seconds, if t.paused { " [paused]" } else { "" }, t.prompt)
            })
            .collect();
        ToolOutput::ok(if lines.is_empty() { "loop: no active loops".into() } else { lines.join("\n") })
    } else if let Some(id) = input.strip_prefix("resume ") {
        if !person {
            ToolOutput::error("loop: only the person can resume a paused loop")
        } else if let Ok(id) = id.trim().parse::<u64>() {
            match rt.loops.tasks.iter_mut().find(|t| t.id == id) {
                Some(task) if task.paused => {
                    task.paused = false;
                    task.due = Instant::now() + Duration::from_secs(task.seconds);
                    ToolOutput::ok(format!("loop #{id} resumed; next run in {}s", task.seconds))
                }
                Some(_) => ToolOutput::error(format!("loop #{id} is not paused")),
                None => ToolOutput::error(format!("loop #{id} not found")),
            }
        } else {
            ToolOutput::error(USAGE)
        }
    } else if let Some(id) = input.strip_prefix("cancel ") {
        match id.trim().parse::<u64>() {
            Ok(id) => {
                let before = rt.loops.tasks.len();
                rt.loops.tasks.retain(|t| t.id != id);
                if before == rt.loops.tasks.len() {
                    ToolOutput::error(format!("loop #{id} not found"))
                } else {
                    if rt.loops.pending.as_ref().is_some_and(|p| p.id == id) {
                        rt.loops.pending = None;
                        notice(
                            &rt.events,
                            Level::Warn,
                            format!("loop #{id}: preparation cancelled; hook effects may already have occurred"),
                        );
                    }
                    ToolOutput::ok(format!("loop #{id} cancelled; an already running turn continues"))
                }
            }
            Err(_) => ToolOutput::error(USAGE),
        }
    } else if let Some((duration, prompt)) = input.split_once(char::is_whitespace) {
        let prompt = prompt.trim();
        if let Some(seconds) = interval(duration) {
            if prompt.is_empty() || prompt.len() > 16_000 {
                ToolOutput::error("loop: prompt must be 1–16000 bytes")
            } else if rt.loops.tasks.len() >= MAX_LOOPS {
                ToolOutput::error("loop: limit of 8 active loops reached")
            } else if let Some(id) = rt.loops.next_id.checked_add(1) {
                rt.loops.next_id = id;
                rt.loops.tasks.push(Task {
                    id,
                    seconds,
                    prompt: prompt.to_string(),
                    due: Instant::now(),
                    person,
                    paused: false,
                });
                ToolOutput::ok(format!(
                    "loop #{id} scheduled: runs as soon as the session is idle, then every {seconds}s. Closing or switching session stops loops; model calls and tools consume resources. /loop cancel {id} stops future runs."
                ))
            } else {
                ToolOutput::error("loop: ID limit reached")
            }
        } else {
            ToolOutput::error(USAGE)
        }
    } else {
        ToolOutput::error(USAGE)
    };
    notice(&rt.events, if out.is_error { Level::Warn } else { Level::Info }, out.content.clone());
    out
}

pub(super) fn clear(rt: &mut Runtime) {
    rt.loops.pending = None;
    rt.loops.running = None;
    if !rt.loops.tasks.is_empty() {
        rt.loops.tasks.clear();
        notice(&rt.events, Level::Info, "session change: all loops stopped");
    }
}

fn ready(busy: bool, held: bool, queued: bool) -> bool {
    !busy && !held && !queued
}

pub(super) async fn tick(rt: &mut Runtime) {
    if rt.loops.pending.is_some() {
        admit(rt).await;
        return;
    }
    let st = rt.state.lock().await;
    if !ready(rt.turn.is_some(), st.held, !st.inbox.is_empty()) {
        return;
    }
    let (config, cwd, session, events) =
        (st.config.clone(), st.cwd.clone(), st.session.id().to_string(), st.events.clone());
    drop(st);
    let now = Instant::now();
    let Some(task) = rt.loops.tasks.iter_mut().filter(|t| !t.paused && t.due <= now).min_by_key(|t| t.due) else {
        return;
    };
    let (id, prompt, person) = (task.id, task.prompt.clone(), task.person);
    // Hooks can take time (or block). Report preparation, not execution, and only
    // when a submit hook is configured. The full prompt appears once on admission.
    if crate::hooks::listens(&config.hooks, "user_prompt_submit") {
        notice(&rt.events, Level::Info, format!("loop #{id}: preparing scheduled prompt (submit hooks)"));
    }
    let text = prompt.clone();
    let (hooks, hook_cwd, hook_session) = (config.hooks.clone(), cwd.clone(), session.clone());
    let hook = Box::pin(async move {
        fire_with(
            &hooks,
            &events,
            &hook_cwd,
            &hook_session,
            "user_prompt_submit",
            None,
            &serde_json::json!({"prompt": text}),
        )
        .await
    });
    rt.loops.pending =
        Some(Pending { id, prompt, person, config, cwd, session, hook: Some(hook), outcome: None, intervened: false });
}

pub(super) fn turn_end(rt: &mut Runtime, reason: TurnEnd) {
    if let Some(id) = rt.loops.finish(reason) {
        notice(
            &rt.events,
            Level::Warn,
            format!("loop #{id} paused after {reason:?}; review partial effects before /loop resume {id}"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_the_failed_loop_is_paused_and_attribution_is_cleared() {
        let make =
            |id| Task { id, seconds: 60, prompt: "test".into(), due: Instant::now(), person: true, paused: false };
        for reason in [TurnEnd::Error, TurnEnd::MaxTurns, TurnEnd::Interrupted] {
            let mut loops = Loops { tasks: vec![make(1), make(2)], next_id: 2, running: Some(1), pending: None };
            assert_eq!(loops.finish(reason), Some(1));
            assert!(loops.tasks[0].paused);
            assert!(!loops.tasks[1].paused);
            assert_eq!(loops.finish(reason), None); // unrelated subsequent turn
        }
        {
            let reason = TurnEnd::Done;
            let mut loops = Loops { tasks: vec![make(1)], next_id: 1, running: Some(1), pending: None };
            assert_eq!(loops.finish(reason), None);
            assert!(!loops.tasks[0].paused);
            assert_eq!(loops.finish(TurnEnd::Error), None);
        }
        let mut loops = Loops { tasks: vec![make(2)], next_id: 2, running: Some(1), pending: None };
        assert_eq!(loops.finish(TurnEnd::Error), None); // cancelled while running
        assert!(!loops.tasks[0].paused);
    }

    #[test]
    fn held_busy_and_human_queue_block_execution() {
        assert!(ready(false, false, false));
        assert!(!ready(true, false, false));
        assert!(!ready(false, true, false));
        assert!(!ready(false, false, true));
    }

    #[test]
    fn intervals_are_bounded_and_checked() {
        assert_eq!(interval("60s"), Some(60));
        assert_eq!(interval("1m"), Some(60));
        assert_eq!(interval("2h"), Some(7200));
        assert_eq!(interval("30d"), Some(MAX_INTERVAL));
        for s in ["", "1", "59s", "0m", "31d", "-1m", "1.5m", "18446744073709551615d", "é"] {
            assert_eq!(interval(s), None, "{s}");
        }
    }
}
