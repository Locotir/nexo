//! The look at a long background command (`job_review`): a request of its own, apart from the
//! conversation and with no tools, weighs whether a shell job that has run past its time is still worth
//! waiting for. It can answer one word only, so it cannot start a diagnosis of its own: it reads the
//! command, how long it ran, whether it still prints and the end of what it printed, and the request in
//! progress, and says CONTINUE or STOP.

use futures::StreamExt;
use nexo_core::message::{Message, Role};
use nexo_core::provider::ModelRequest;
use nexo_core::stream::{Collector, StreamEvent, Usage};

use crate::client::{ModelClient, Target};

/// The system prompt of a review request (a recorder can tell that request apart by it).
pub const REVIEW_SYSTEM: &str = "You watch a shell command that an agent left running in the background while it \
works on a request. Decide whether it is still worth waiting for. Answer with exactly one word: CONTINUE or STOP. \
STOP when it looks stuck (no new output for long and nothing it is known to compute silently), waiting for input, \
looping on the same lines, failing over and over, or no longer of use to the request. CONTINUE when it is making \
progress or is the kind of work that takes long (a build, a test suite, a download, a training run).";

/// Seconds between two looks at the running jobs.
pub const TICK_S: u64 = 15;
/// Characters of the end of the output, of the command and of the request a review is given.
const TAIL_CHARS: usize = 3000;
const COMMAND_CHARS: usize = 600;
const REQUEST_CHARS: usize = 600;

fn clip_end(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    format!("…{}", s.chars().skip(n - max).collect::<String>())
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!("{}…", s.chars().take(max).collect::<String>())
}

/// The request in progress: the latest message of the person with words in it.
pub fn request_of(history: &[Message]) -> String {
    history
        .iter()
        .rev()
        .filter(|m| m.role == Role::User)
        .map(Message::text)
        .find(|t| !t.trim().is_empty())
        .unwrap_or_default()
}

/// The one message of a review request.
pub fn prompt(command: &str, secs: u64, lines: usize, new: usize, tail: &str, request: &str) -> String {
    let tail = if tail.trim().is_empty() { "(nothing)".to_string() } else { clip_end(tail, TAIL_CHARS) };
    format!(
        "Request in progress:\n{}\n\nBackground command:\n{}\n\nRunning for {} s; {} lines of output in all, {} of them \
         since the last look.\n\nEnd of its output:\n{}\n\nCONTINUE or STOP?",
        clip(request.trim(), REQUEST_CHARS),
        clip(command, COMMAND_CHARS),
        secs,
        lines,
        new,
        tail
    )
}

/// Whether the answer says STOP: the first of the two words it holds; neither is CONTINUE.
pub fn says_stop(answer: &str) -> bool {
    let a = answer.to_ascii_uppercase();
    match (a.find("STOP"), a.find("CONTINUE")) {
        (Some(s), Some(c)) => s < c,
        (Some(_), None) => true,
        _ => false,
    }
}

/// One review request: whether to stop, what it spent and the model.
pub async fn ask(
    client: &dyn ModelClient,
    target: &Target,
    retries: u32,
    prompt: String,
) -> anyhow::Result<(bool, Usage, String)> {
    let (_, cfg, model) = target.get();
    let mut req = ModelRequest::new(model.clone(), REVIEW_SYSTEM, vec![Message::user(prompt)]);
    // room for a model that reasons before it answers; the answer itself is one word
    req.max_tokens = 1024;
    req.temperature = Some(0.0);
    let mut stream = client.stream(&cfg, &req, retries).await?;
    let mut c = Collector::new();
    while let Some(ev) = stream.next().await {
        if let StreamEvent::Error(e) = &ev {
            anyhow::bail!("the review failed: {e}");
        }
        c.push(ev);
    }
    Ok((says_stop(&c.message().text()), c.usage, model))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_clear_stop_stops() {
        assert!(says_stop("STOP"));
        assert!(says_stop("stop."));
        assert!(says_stop("STOP: it waits for input, not CONTINUE"));
        assert!(!says_stop("CONTINUE"));
        assert!(!says_stop("CONTINUE, do not STOP it"));
        assert!(!says_stop(""));
        assert!(!says_stop("maybe"));
    }

    #[test]
    fn the_prompt_holds_the_command_the_time_the_progress_and_the_end_of_the_output() {
        let tail = format!("{}\nlast line", "x".repeat(5000));
        let p = prompt("cargo test --workspace", 640, 1200, 0, &tail, "Fix the failing test");
        assert!(p.contains("cargo test --workspace") && p.contains("640 s") && p.contains("0 of them"));
        assert!(p.contains("last line") && p.contains("Fix the failing test"));
        assert!(p.len() < TAIL_CHARS + COMMAND_CHARS + REQUEST_CHARS + 600);
        assert!(prompt("sleep 900", 600, 0, 0, "", "").contains("(nothing)"));
    }
}
