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

pub(super) fn command(rt: &mut Runtime, input: &str) {
    let input = input.trim();
    if input == "list" {
        if rt.loops.tasks.is_empty() {
            notice(&rt.events, Level::Info, "loop: no active loops");
        }
        for task in &rt.loops.tasks {
            notice(&rt.events, Level::Info, format!("loop #{} every {}s: {}", task.id, task.seconds, task.prompt));
        }
        return;
    }
    if let Some(id) = input.strip_prefix("cancel ") {
        if let Ok(id) = id.trim().parse::<u64>() {
            let before = rt.loops.tasks.len();
            rt.loops.tasks.retain(|t| t.id != id);
            notice(
                &rt.events,
                Level::Info,
                if before == rt.loops.tasks.len() {
                    format!("loop #{id} not found")
                } else {
                    format!("loop #{id} cancelled; an already running turn continues")
                },
            );
            return;
        }
    }
    let Some((duration, prompt)) = input.split_once(char::is_whitespace) else {
        notice(&rt.events, Level::Warn, USAGE);
        return;
    };
    let Some(seconds) = interval(duration) else {
        notice(&rt.events, Level::Warn, USAGE);
        return;
    };
    let prompt = prompt.trim();
    if prompt.is_empty() || prompt.len() > 16_000 {
        notice(&rt.events, Level::Warn, "loop: prompt must be 1–16000 bytes");
        return;
    }
    if rt.loops.tasks.len() >= MAX_LOOPS {
        notice(&rt.events, Level::Warn, "loop: limit of 8 active loops reached");
        return;
    }
    let Some(id) = rt.loops.next_id.checked_add(1) else {
        notice(&rt.events, Level::Warn, "loop: ID limit reached");
        return;
    };
    rt.loops.next_id = id;
    rt.loops.tasks.push(Task { id, seconds, prompt: prompt.to_string(), due: Instant::now() });
    notice(&rt.events, Level::Info, format!("loop #{id} scheduled: runs as soon as the session is idle, then every {seconds}s. Closing or switching session stops loops; model calls and tools consume resources. /loop cancel {id} stops future runs."));
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
    let (id, prompt) = (task.id, task.prompt.clone());
    notice(&rt.events, Level::Info, format!("loop #{id}: running scheduled prompt: {prompt}"));
    rt.turn = start_prompt_item(&rt.state, prompt, Vec::new(), false, rt.cancel.child_token(), None).await;
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
