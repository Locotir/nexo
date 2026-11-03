//! A separate pass over a long tool output: a request of its own reads the output (the whole file,
//! when the call printed part of one) together with what the session is doing, and the session gets
//! its notes in place of the output. The output itself is kept in the session's record, and a call
//! that asks for fewer lines gets them as they are.
//!
//! Why: a long output stays in every later request of the session, and a model reads a long text
//! worse inside a long conversation than alone with one question. The notes are short, name the
//! lines they come from and are written for the request in progress.
//!
//! The request puts the material first and the question after, so that a second pass over the same
//! material, for another request, finds its start in the provider's cache.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use futures::StreamExt;
use nexo_core::message::{Message, Part, Role};
use nexo_core::provider::ModelRequest;
use nexo_core::stream::{Collector, StreamEvent, Usage};

use crate::client::{ModelClient, Target};

/// The system prompt of a reader request (a recorder can tell that request apart by it).
pub const READER_SYSTEM: &str = "You read material for another agent that works on a task. The agent will not see the \
material, only your notes, so they must hold every fact it needs, stated exactly. Do not guess: when something cannot \
be settled from the material, say so.";

/// Characters of material one reader request takes (larger material is not read this way).
pub const MATERIAL_CHARS: usize = 160_000;
/// Characters of the thread's first message and of the request in progress given to the reader.
const THREAD_CHARS: usize = 1_500;
const REQUEST_CHARS: usize = 3_000;
/// Characters of the agent's last words and of each recent call given to the reader.
const WHY_CHARS: usize = 600;
const STEP_CHARS: usize = 160;
/// Recent calls given to the reader.
const STEPS: usize = 4;
/// The longest reply of a reader request.
pub const NOTES_TOKENS: u32 = 1_500;

/// What a reader request is told.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ask {
    /// What the material is (`material/a.txt, whole file, 921 lines` or the call).
    pub label: String,
    /// The material, its lines numbered.
    pub material: String,
    /// The thread's first message, when it is not the request in progress.
    pub thread: String,
    /// The latest message of the person (or of the harness) the agent works on.
    pub request: String,
    /// The agent's last calls, oldest first.
    pub steps: Vec<String>,
    /// What the agent wrote with the call, if anything.
    pub why: String,
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// `text` with its lines numbered from `first`.
pub fn numbered(text: &str, first: usize) -> String {
    text.lines().enumerate().map(|(i, l)| format!("{:>6}│ {l}\n", i + first)).collect()
}

/// The text a message carries, tool results and calls aside.
fn words_of(m: &Message) -> String {
    m.content
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } if !text.trim_start().starts_with("[curator]") => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Ask {
    /// What the reader is told about `material`, from the history up to the call that produced it.
    pub fn from_history(history: &[Message], label: String, material: String) -> Ask {
        let said = |m: &&Message| m.role == Role::User && !words_of(m).trim().is_empty();
        let request = history.iter().rev().find(said).map(words_of).unwrap_or_default();
        let first = history.iter().find(said).map(words_of).unwrap_or_default();
        let thread = if first == request { String::new() } else { clip(&first, THREAD_CHARS) };
        let mut steps: Vec<String> = history
            .iter()
            .rev()
            .flat_map(|m| m.content.iter().rev())
            .filter_map(|p| match p {
                Part::ToolCall { name, args, .. } => Some(clip(&format!("{name} {args}"), STEP_CHARS)),
                _ => None,
            })
            .take(STEPS)
            .collect();
        steps.reverse();
        let why = history
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .map(|m| clip(&words_of(m), WHY_CHARS))
            .unwrap_or_default();
        Ask { label, material, thread, request: clip(&request, REQUEST_CHARS), steps, why }
    }

    /// The reader request's one message: the material first (the same for every request about it,
    /// so cached), then the thread.
    pub fn prompt(&self) -> String {
        let mut s = format!("<material of=\"{}\">\n{}</material>\n\n", self.label, self.material);
        if !self.thread.is_empty() {
            s.push_str(&format!("The thread began with:\n{}\n\n", self.thread));
        }
        s.push_str(&format!("The request in progress:\n{}\n\n", self.request));
        if !self.steps.is_empty() {
            s.push_str("The agent's last calls, the last one produced this material:\n");
            for st in &self.steps {
                s.push_str(&format!("- {st}\n"));
            }
            s.push('\n');
        }
        if !self.why.is_empty() {
            s.push_str(&format!("What the agent wrote with them:\n{}\n\n", self.why));
        }
        s.push_str(
            "Answer format: at most 12 lines `Finding: ...` with the facts from this material that the request in \
             progress needs, each citing its line numbers, then one line `Conclusion: ...` with what they imply for the \
             request, or `Conclusion: unknown`. If this material is not about the request, answer only \
             `Finding: not about this request.`",
        );
        s
    }

    /// The same material for the same request: the same notes.
    pub fn key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        (&self.material, &self.request).hash(&mut h);
        h.finish()
    }
}

/// What the session gets in place of the output.
pub fn shown(label: &str, notes: &str) -> String {
    format!(
        "[curator] {label}: read by a separate pass for the request in progress; the lines are kept but not shown here. \
         Its notes follow. To see exact lines, ask for a shorter range and you get them as they are.\n{}",
        notes.trim()
    )
}

/// One reader request: its notes, what it spent and the model.
pub async fn read(
    client: &dyn ModelClient,
    target: &Target,
    retries: u32,
    temperature: Option<f32>,
    ask: &Ask,
) -> anyhow::Result<(String, Usage, String)> {
    let (_, cfg, model) = target.get();
    let mut req = ModelRequest::new(model.clone(), READER_SYSTEM, vec![Message::user(ask.prompt())]);
    req.max_tokens = NOTES_TOKENS;
    req.temperature = temperature;
    let mut stream = client.stream(&cfg, &req, retries).await?;
    let mut c = Collector::new();
    while let Some(ev) = stream.next().await {
        if let StreamEvent::Error(e) = &ev {
            anyhow::bail!("the reader failed: {e}");
        }
        c.push(ev);
    }
    let text = c.message().text();
    if !text.contains("Finding") && !text.contains("Conclusion") {
        anyhow::bail!("the reader gave no notes");
    }
    Ok((text, c.usage, model))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_reader_is_told_the_request_the_thread_and_the_last_calls() {
        let history = vec![
            Message::user("Fix the build of the web app."),
            Message::new(
                Role::Assistant,
                vec![
                    Part::text("The error names src/app.ts; reading it."),
                    Part::ToolCall { id: "c1".into(), name: "bash".into(), args: json!({"command": "cat src/app.ts"}) },
                ],
            ),
        ];
        let a = Ask::from_history(&history, "src/app.ts, whole file, 3 lines".into(), numbered("a\nb\nc", 1));
        assert_eq!(a.request, "Fix the build of the web app.");
        assert_eq!(a.thread, "", "the first message is the request");
        assert_eq!(a.steps, vec![r#"bash {"command":"cat src/app.ts"}"#.to_string()]);
        assert_eq!(a.why, "The error names src/app.ts; reading it.");
        let p = a.prompt();
        assert!(p.starts_with("<material of=\"src/app.ts, whole file, 3 lines\">\n     1│ a\n"), "{p}");
        assert!(p.find("Fix the build").unwrap() > p.find("</material>").unwrap(), "material first, for the cache");
    }

    #[test]
    fn a_later_request_keeps_the_first_message_as_the_thread() {
        let history = vec![
            Message::user("Answer each objective about the material."),
            Message::new(Role::Assistant, vec![Part::text("Done: 3.")]),
            Message::user("[curator] old output left out"),
            Message::user("Current objective: the second function."),
        ];
        let a = Ask::from_history(&history, "x".into(), "1".into());
        assert_eq!(a.request, "Current objective: the second function.");
        assert_eq!(a.thread, "Answer each objective about the material.");
        let b = Ask { request: "another".into(), ..a.clone() };
        assert_ne!(a.key(), b.key(), "another request, other notes");
    }
}
