//! Session-local recurring prompts. Never persisted or run concurrently with a turn.
use super::*;

const MAX_LOOPS: usize = 8;
const MAX_INTERVAL: u64 = 30 * 24 * 60 * 60;
const USAGE: &str =
    "usage: /loop <interval> <prompt> (e.g. 60s, 5m, 2h, 1d), /loop list, /loop cancel <id>; intervals: 60s–30d";

struct Task {
    id: u64,
    seconds: u64,
    prompt: String,
    due: Instant,
    person: bool,
}

#[derive(Default)]
pub(super) struct Loops {
    tasks: Vec<Task>,
    next_id: u64,
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
        let lines: Vec<_> =
            rt.loops.tasks.iter().map(|t| format!("loop #{} every {}s: {}", t.id, t.seconds, t.prompt)).collect();
        ToolOutput::ok(if lines.is_empty() { "loop: no active loops".into() } else { lines.join("\n") })
    } else if let Some(id) = input.strip_prefix("cancel ") {
        match id.trim().parse::<u64>() {
            Ok(id) => {
                let before = rt.loops.tasks.len();
                rt.loops.tasks.retain(|t| t.id != id);
                if before == rt.loops.tasks.len() {
                    ToolOutput::error(format!("loop #{id} not found"))
                } else {
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
                rt.loops.tasks.push(Task { id, seconds, prompt: prompt.to_string(), due: Instant::now(), person });
                ToolOutput::ok(format!("loop #{id} scheduled: runs as soon as the session is idle, then every {seconds}s. Closing or switching session stops loops; model calls and tools consume resources. /loop cancel {id} stops future runs."))
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
    if !rt.loops.tasks.is_empty() {
        rt.loops.tasks.clear();
        notice(&rt.events, Level::Info, "session change: all loops stopped");
    }
}

fn ready(busy: bool, held: bool, queued: bool) -> bool {
    !busy && !held && !queued
}

pub(super) async fn tick(rt: &mut Runtime) {
    let st = rt.state.lock().await;
    if !ready(rt.turn.is_some(), st.held, !st.inbox.is_empty()) {
        return;
    }
    drop(st);
    let now = Instant::now();
    let Some(task) = rt.loops.tasks.iter_mut().filter(|t| t.due <= now).min_by_key(|t| t.due) else {
        return;
    };
    task.due = now + Duration::from_secs(task.seconds);
    let (id, prompt, person) = (task.id, task.prompt.clone(), task.person);
    notice(&rt.events, Level::Info, format!("loop #{id}: running scheduled prompt: {prompt}"));
    rt.turn = start_scheduled_prompt(&rt.state, prompt, person, rt.cancel.child_token()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
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
