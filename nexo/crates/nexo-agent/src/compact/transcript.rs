//! The conversation as plain text for the summariser, fitted to the model's window.

use super::*;
use nexo_core::message::{Message, Part, Role};

/// Characters of transcript the summary request may carry: what the window leaves after the
/// summary's own reply (`reply` tokens), with room for the estimate running short, and never more
/// than [`TRANSCRIPT_CHARS`].
pub fn transcript_chars(window: Option<u32>, reply: u32) -> usize {
    match window {
        Some(w) => {
            let room = u64::from(w.saturating_sub(reply).max(w / 2));
            let chars = room * 3 / 5 * WINDOW_CHARS_PER_TOKEN;
            (chars as usize).min(TRANSCRIPT_CHARS)
        }
        None => TRANSCRIPT_CHARS,
    }
}

/// The conversation as plain text, tool results shortened, in `max_chars` characters at most (about):
/// past it, the first message (the task, or the summary that stands for the older work) and the
/// newest messages that fit, with a mark where the middle is left out.
pub fn transcript(msgs: &[Message], max_chars: usize) -> String {
    let lines: Vec<String> = msgs.iter().map(rendered).collect();
    let total: usize = lines.iter().map(|l| l.chars().count()).sum();
    if total <= max_chars || lines.is_empty() {
        return lines.concat();
    }
    const GAP: &str = "[... the middle of the transcript is left out ...]\n";
    let mut out = shorten(&lines[0], max_chars / 4);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    let mut left = max_chars.saturating_sub(out.chars().count() + GAP.len());
    let mut tail = Vec::new();
    for l in lines[1..].iter().rev() {
        let n = l.chars().count();
        if n > left {
            if tail.is_empty() {
                // the newest message alone is past the room left: its end
                tail.push(l.chars().skip(n - left).collect::<String>());
            }
            break;
        }
        left -= n;
        tail.push(l.clone());
    }
    tail.reverse();
    out.push_str(GAP);
    out.push_str(&tail.concat());
    out
}

/// One message of the transcript.
pub(super) fn rendered(m: &Message) -> String {
    let mut out = String::new();
    for p in &m.content {
        match p {
            Part::Text { text } => {
                let who = if m.role == Role::User { "person" } else { "assistant" };
                out.push_str(&format!("[{who}] {text}\n"));
            }
            Part::ToolCall { name, args, .. } => {
                out.push_str(&format!("[call {name}] {}\n", shorten(&args.to_string(), 400)));
            }
            Part::ToolResult { content, is_error, .. } => {
                let t = nexo_core::message::result_text(content);
                let tag = if *is_error { "result, error" } else { "result" };
                out.push_str(&format!("[{tag}] {}\n", shorten(&t, RESULT_CHARS)));
            }
            _ => {}
        }
    }
    out
}

pub(super) fn shorten(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let head: String = s.chars().take(n / 2).collect();
    let tail: String = s.chars().rev().take(n / 2).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head} [...] {tail}")
}
