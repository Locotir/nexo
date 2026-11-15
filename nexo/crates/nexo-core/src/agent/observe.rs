//! Ephemeral, bounded observation of a single agent channel. Not a transport or an authorization boundary.
//! Conversation and tool data can contain secrets; callers must protect access to snapshots.
use std::collections::{HashSet, VecDeque};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use super::AgentEvent;
use crate::message::{Message, Part};
use serde::{Deserialize, Serialize};

const MAX_EVENTS: usize = 512;
const MAX_BYTES: usize = 512 * 1024;
const MAX_EVENT: usize = 16 * 1024;

/// A JSON fragment, possibly cut at a UTF-8 boundary. A truncated fragment is not necessarily valid JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservedEvent {
    pub sequence: u64,
    pub kind: String,
    pub json: String,
    pub truncated: bool,
}

/// An atomic copy. `instance_id` distinguishes registries across process restarts; `generation`
/// changes on transcript resets and identity changes. If `dropped` is
/// nonzero, the snapshot is only a suffix; `sequence` is the last assigned event number in this generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationSnapshot {
    /// Random identity of this in-memory registry, stable across its resets.
    pub instance_id: String,
    pub session_id: Option<String>,
    pub generation: u64,
    pub sequence: u64,
    pub dropped: u64,
    /// Current read-only wait indicators, independent of event retention.
    #[serde(default)]
    pub permission_waiting: bool,
    #[serde(default)]
    pub question_waiting: bool,
    pub events: Vec<ObservedEvent>,
}

struct State {
    instance_id: String,
    session_id: Option<String>,
    generation: u64,
    sequence: u64,
    dropped: u64,
    bytes: usize,
    events: VecDeque<ObservedEvent>,
    permissions: HashSet<String>,
    questions: HashSet<String>,
    // Unknown outstanding IDs cannot be safely counted after overflow (duplicate asks and
    // unrelated resolutions are possible). Stay conservatively true until a reset.
    permission_overflow: bool,
    question_overflow: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            instance_id: uuid::Uuid::new_v4().to_string(),
            session_id: None,
            generation: 0,
            sequence: 0,
            dropped: 0,
            bytes: 0,
            events: VecDeque::new(),
            permissions: HashSet::new(),
            questions: HashSet::new(),
            permission_overflow: false,
            question_overflow: false,
        }
    }
}

/// Cloneable reader of the channel's in-memory registry. Snapshots and writes use the same lock.
#[derive(Clone, Default)]
pub struct Observation(Arc<Mutex<State>>);

impl Observation {
    pub fn snapshot(&self) -> ObservationSnapshot {
        let s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        ObservationSnapshot {
            instance_id: s.instance_id.clone(),
            session_id: s.session_id.clone(),
            generation: s.generation,
            sequence: s.sequence,
            dropped: s.dropped,
            permission_waiting: !s.permissions.is_empty() || s.permission_overflow,
            question_waiting: !s.questions.is_empty() || s.question_overflow,
            events: s.events.iter().cloned().collect(),
        }
    }
    /// Read projected events newer than an earlier snapshot, atomically with metadata.
    /// Independent readers do not consume one another's events. `Err` returns a fresh
    /// snapshot for explicit resynchronization after identity/reset, a future cursor, or
    /// eviction of unread events. Inspect `dropped` and `truncated` for earlier loss.
    /// This is bounded polling, not a lossless subscription or an authorization boundary.
    pub fn snapshot_since(&self, cursor: &ObservationSnapshot) -> Result<ObservationSnapshot, ObservationSnapshot> {
        let mut next = self.snapshot();
        let same = next.instance_id == cursor.instance_id
            && next.session_id == cursor.session_id
            && next.generation == cursor.generation;
        // Retained sequences are contiguous, beginning at dropped + 1.
        if !same || cursor.sequence > next.sequence || cursor.sequence < next.dropped {
            return Err(next);
        }
        next.events.retain(|event| event.sequence > cursor.sequence);
        Ok(next)
    }
    fn reset(s: &mut State, id: Option<String>) {
        s.session_id = id;
        s.generation = s.generation.checked_add(1).expect("observation generation exhausted");
        s.sequence = 0;
        s.dropped = 0;
        s.bytes = 0;
        s.events.clear();
        s.permissions.clear();
        s.questions.clear();
        s.permission_overflow = false;
        s.question_overflow = false;
    }
    pub(super) fn publish(&self, event: &AgentEvent) {
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match event {
            AgentEvent::HistoryCleared => {
                Self::reset(&mut s, None);
                return;
            }
            AgentEvent::Ready { session_id, .. } | AgentEvent::SessionChanged { session_id, .. } => {
                if s.session_id.as_deref() != Some(session_id) {
                    Self::reset(&mut s, Some(session_id.clone()));
                }
                return;
            }
            _ => {}
        }
        if s.session_id.is_none() {
            return;
        }
        match event {
            AgentEvent::PermissionAsk { request } => {
                if s.permissions.len() < 128 && request.id.len() <= 1024 {
                    s.permissions.insert(request.id.clone());
                } else if !s.permissions.contains(&request.id) {
                    s.permission_overflow = true;
                }
            }
            AgentEvent::PermissionResolved { id, .. } => {
                s.permissions.remove(id);
            }
            AgentEvent::Question { question } => {
                if s.questions.len() < 128 && question.id.len() <= 1024 {
                    s.questions.insert(question.id.clone());
                } else if !s.questions.contains(&question.id) {
                    s.question_overflow = true;
                }
            }
            AgentEvent::QuestionResolved { id, .. } => {
                s.questions.remove(id);
            }
            _ => {}
        }
        let Some((kind, json, truncated)) = project(event) else {
            return;
        };
        s.sequence = s.sequence.checked_add(1).expect("observation sequence exhausted");
        let item = ObservedEvent { sequence: s.sequence, kind: kind.into(), json, truncated };
        s.bytes += item.json.len() + item.kind.len();
        s.events.push_back(item);
        while s.events.len() > MAX_EVENTS || s.bytes > MAX_BYTES {
            let old = s.events.pop_front().expect("nonempty observation");
            s.bytes -= old.json.len() + old.kind.len();
            s.dropped += 1;
        }
    }
}

// Limit serialization while it runs rather than constructing arbitrarily large event copies.
struct Limited {
    bytes: Vec<u8>,
    truncated: bool,
}
impl Write for Limited {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let room = MAX_EVENT.saturating_sub(self.bytes.len());
        let n = room.min(buf.len());
        self.bytes.extend_from_slice(&buf[..n]);
        if n < buf.len() {
            self.truncated = true;
            return Err(io::Error::other("observation limit"));
        }
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encode<T: Serialize + ?Sized>(value: &T) -> (String, bool) {
    let mut out = Limited { bytes: Vec::new(), truncated: false };
    let result = serde_json::to_writer(&mut out, value);
    let truncated = out.truncated || result.is_err();
    while std::str::from_utf8(&out.bytes).is_err() {
        out.bytes.pop();
    }
    (String::from_utf8(out.bytes).expect("validated UTF-8"), truncated)
}

// Only readable message parts; never include images, opaque reasoning, signatures or provider metadata.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ReadablePart<'a> {
    Text { text: &'a str },
    Reasoning { text: &'a str },
    ToolCall { id: &'a str, name: &'a str, args: &'a serde_json::Value },
    ToolResult { call_id: &'a str, is_error: bool, content: Vec<ReadablePart<'a>> },
    Omitted,
}
fn parts<'a>(input: &'a [Part], depth: usize, budget: &mut usize, limited: &mut bool) -> Vec<ReadablePart<'a>> {
    // Bound structure allocation as well as the final serialized bytes.
    if depth >= 8 && !input.is_empty() {
        *limited = true;
        return vec![ReadablePart::Omitted];
    }
    let mut out = Vec::new();
    for p in input {
        if *budget == 0 {
            *limited = true;
            out.push(ReadablePart::Omitted);
            break;
        }
        *budget -= 1;
        out.push(match p {
            Part::Text { text } => ReadablePart::Text { text },
            Part::Reasoning { text, .. } => ReadablePart::Reasoning { text },
            Part::ToolCall { id, name, args } => ReadablePart::ToolCall { id, name, args },
            Part::ToolResult { call_id, is_error, content, .. } => ReadablePart::ToolResult {
                call_id,
                is_error: *is_error,
                content: parts(content, depth + 1, budget, limited),
            },
            _ => ReadablePart::Omitted,
        });
    }
    out
}
fn message(m: &Message) -> ((crate::message::Role, Vec<ReadablePart<'_>>), bool) {
    let mut limited = false;
    let p = parts(&m.content, 0, &mut 128, &mut limited);
    ((m.role, p), limited)
}
fn project(e: &AgentEvent) -> Option<(&'static str, String, bool)> {
    let (kind, (json, truncated)) = match e {
        AgentEvent::History { index, message: m } => {
            let (msg, limited) = message(m);
            let (json, truncated) = encode(&(*index, msg));
            ("history", (json, truncated || limited))
        }
        AgentEvent::AssistantDone { message: m } => {
            let (msg, limited) = message(m);
            let (json, truncated) = encode(&msg);
            ("assistant_done", (json, truncated || limited))
        }
        AgentEvent::TextDelta { text } => ("text_delta", encode(text)),
        AgentEvent::ReasoningDelta { text } => ("reasoning_delta", encode(text)),
        AgentEvent::Preparing { what } => ("preparing", encode(what)),
        AgentEvent::Notice { level, text, .. } => ("notice", encode(&(level, text))),
        AgentEvent::ShellOutput { command, output, exit_code } => {
            ("shell_output", encode(&(command, output, exit_code)))
        }
        // No request IDs, prompts, decisions or answers: this endpoint cannot authorize actions.
        AgentEvent::PermissionAsk { .. } => ("permission_wait", encode(&true)),
        AgentEvent::PermissionResolved { .. } => ("permission_wait", encode(&false)),
        AgentEvent::Question { .. } => ("question_wait", encode(&true)),
        AgentEvent::QuestionResolved { .. } => ("question_wait", encode(&false)),
        AgentEvent::AssistantStart => ("assistant_start", encode(&())),
        AgentEvent::TurnStart { turn } => ("turn_start", encode(turn)),
        AgentEvent::TurnEnd { reason } => ("turn_end", encode(reason)),
        AgentEvent::ToolCall { id, name, args, summary } => ("tool_call", encode(&(id, name, args, summary))),
        AgentEvent::ToolStart { id } => ("tool_start", encode(id)),
        AgentEvent::ToolProgress { id, line } => ("tool_progress", encode(&(id, line))),
        AgentEvent::ToolDone { id, output, elapsed_ms } => {
            // ToolOutput.content is plain text, not Part; images and display are deliberately excluded.
            ("tool_done", encode(&(id, &output.content, output.is_error, elapsed_ms)))
        }
        _ => return None,
    };
    Some((kind, json, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::channels;
    use crate::message::{ImageSource, Message, Part, Role};

    fn identity(tx: &super::super::EventTx, id: &str) {
        tx.send(AgentEvent::SessionChanged { session_id: id.into(), title: String::new() }).unwrap();
    }
    #[test]
    fn filters_resets_and_isolates() {
        let (a, ends) = channels();
        let (b, _) = channels();
        identity(&ends.events, "one");
        ends.events.send(AgentEvent::SystemPrompt { text: "private".into() }).unwrap();
        ends.events
            .send(AgentEvent::History {
                index: 0,
                message: Message::new(
                    Role::User,
                    vec![Part::text("hello"), Part::Image { source: ImageSource::Url { url: "secret-image".into() } }],
                ),
            })
            .unwrap();
        let snap = a.observation.snapshot();
        assert_eq!(snap.sequence, 1);
        assert!(snap.events[0].json.contains("hello"));
        assert!(!snap.events[0].json.contains("secret-image"));
        assert!(b.observation.snapshot().session_id.is_none());
        ends.events.send(AgentEvent::HistoryCleared).unwrap();
        let cleared = a.observation.snapshot();
        assert!(cleared.session_id.is_none());
        assert!(cleared.events.is_empty());
        identity(&ends.events, "two");
        assert!(a.observation.snapshot().generation > snap.generation);
    }
    #[test]
    fn bounded_with_visible_loss() {
        let (handle, ends) = channels();
        identity(&ends.events, "one");
        ends.events.send(AgentEvent::TextDelta { text: "é".repeat(MAX_EVENT) }).unwrap();
        let snap = handle.observation.snapshot();
        assert!(snap.events[0].truncated);
        assert!(snap.events[0].json.len() <= MAX_EVENT);
        for i in 0..MAX_EVENTS + 10 {
            ends.events.send(AgentEvent::TextDelta { text: i.to_string() }).unwrap();
        }
        let snap = handle.observation.snapshot();
        assert!(snap.dropped > 0);
        assert_eq!(snap.events.len(), MAX_EVENTS);
        assert_eq!(snap.events.last().unwrap().sequence, snap.sequence);
        assert_eq!(snap.events[0].sequence, snap.dropped + 1);
    }
    #[test]
    fn projected_waits_are_private_and_survive_event_eviction() {
        use crate::permission::{PermissionDecision, PermissionRequest, Question};
        use crate::tool::{ToolDisplay, ToolKind};
        let (handle, ends) = channels();
        identity(&ends.events, "one");
        ends.events
            .send(AgentEvent::Question {
                question: Question {
                    id: "secret-id".into(),
                    header: "secret-header".into(),
                    question: "secret-question".into(),
                    options: vec![],
                    multi: false,
                    plan: Some("secret-plan".into()),
                },
            })
            .unwrap();
        ends.events
            .send(AgentEvent::PermissionAsk {
                request: PermissionRequest {
                    id: "permission-id".into(),
                    tool: "secret-tool".into(),
                    kind: ToolKind::Read,
                    summary: "secret-summary".into(),
                    detail: ToolDisplay::Plain,
                    suggested_rules: vec!["secret-rule".into()],
                    agent: None,
                },
            })
            .unwrap();
        let snap = handle.observation.snapshot();
        assert!(snap.question_waiting && snap.permission_waiting);
        assert!(!serde_json::to_string(&snap).unwrap().contains("secret"));
        assert!(!serde_json::to_string(&snap).unwrap().contains("permission-id"));
        for _ in 0..MAX_EVENTS + 1 {
            ends.events.send(AgentEvent::TextDelta { text: "x".into() }).unwrap();
        }
        let snap = handle.observation.snapshot();
        assert!(snap.dropped > 0 && snap.question_waiting && snap.permission_waiting);
        ends.events
            .send(AgentEvent::QuestionResolved { id: "secret-id".into(), answer: Some("secret-answer".into()) })
            .unwrap();
        ends.events
            .send(AgentEvent::PermissionResolved { id: "permission-id".into(), decision: PermissionDecision::Allow })
            .unwrap();
        let snap = handle.observation.snapshot();
        assert!(!snap.question_waiting && !snap.permission_waiting);
        assert!(!serde_json::to_string(&snap).unwrap().contains("secret-answer"));
    }

    #[test]
    fn structure_limits_mark_truncation() {
        let many = Message::new(Role::User, (0..129).map(|_| Part::text("x")).collect());
        let (_, json, truncated) = project(&AgentEvent::AssistantDone { message: many }).unwrap();
        assert!(truncated && json.contains("omitted"));
        let mut part = Part::text("leaf");
        for _ in 0..8 {
            part = Part::ToolResult { call_id: "id".into(), is_error: false, content: vec![part], name: None };
        }
        let (_, json, truncated) =
            project(&AgentEvent::AssistantDone { message: Message::new(Role::User, vec![part]) }).unwrap();
        assert!(truncated && json.contains("omitted"));
    }

    #[test]
    fn tool_output_excludes_images() {
        use crate::tool::ToolOutput;
        let mut output = ToolOutput::ok("visible");
        output.images.push(ImageSource::Base64 { media_type: "image/png".into(), data: "secret-image".into() });
        let (_, json, truncated) = project(&AgentEvent::ToolDone { id: "tool".into(), output, elapsed_ms: 4 }).unwrap();
        assert!(!truncated);
        assert!(!json.contains("secret-image"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap(),
            serde_json::json!(["tool", "visible", false, 4])
        );
    }

    #[test]
    fn instance_distinguishes_registries_and_survives_reset() {
        let (a, ae) = channels();
        let (b, be) = channels();
        identity(&ae.events, "same");
        identity(&be.events, "same");
        let first = a.observation.snapshot();
        let other = b.observation.snapshot();
        assert_eq!((first.generation, first.sequence), (other.generation, other.sequence));
        assert_ne!(first.instance_id, other.instance_id);
        ae.events.send(AgentEvent::HistoryCleared).unwrap();
        identity(&ae.events, "same");
        let reset = a.observation.snapshot();
        assert_eq!(first.instance_id, reset.instance_id);
        assert_ne!(first.generation, reset.generation);
    }

    #[test]
    fn overflow_waits_remain_conservative_until_reset() {
        use crate::permission::Question;
        let (handle, ends) = channels();
        identity(&ends.events, "one");
        for n in 0..129 {
            ends.events
                .send(AgentEvent::Question {
                    question: Question {
                        id: n.to_string(),
                        header: String::new(),
                        question: String::new(),
                        options: vec![],
                        multi: false,
                        plan: None,
                    },
                })
                .unwrap();
        }
        for n in 0..129 {
            ends.events.send(AgentEvent::QuestionResolved { id: n.to_string(), answer: None }).unwrap();
        }
        assert!(handle.observation.snapshot().question_waiting);
        ends.events.send(AgentEvent::HistoryCleared).unwrap();
        assert!(!handle.observation.snapshot().question_waiting);
    }

    #[test]
    fn independent_cursors_detect_loss_reset_and_other_instances() {
        let (handle, ends) = channels();
        identity(&ends.events, "one");
        let start = handle.observation.snapshot();
        for n in 0..3 {
            ends.events.send(AgentEvent::TextDelta { text: n.to_string() }).unwrap();
        }
        let a = handle.observation.snapshot_since(&start).unwrap();
        let b = handle.observation.snapshot_since(&start).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.events.iter().map(|e| e.sequence).collect::<Vec<_>>(), [1, 2, 3]);
        assert!(handle.observation.snapshot_since(&a).unwrap().events.is_empty());
        for _ in 0..MAX_EVENTS {
            ends.events.send(AgentEvent::TextDelta { text: "x".into() }).unwrap();
        }
        let current = handle.observation.snapshot_since(&a).unwrap();
        assert_eq!(current.events.len(), MAX_EVENTS);
        let resync = handle.observation.snapshot_since(&start).unwrap_err();
        assert_eq!(resync.dropped, 3);
        let (other, other_ends) = channels();
        identity(&other_ends.events, "one");
        assert!(other.observation.snapshot_since(&start).is_err());
        ends.events.send(AgentEvent::HistoryCleared).unwrap();
        identity(&ends.events, "one");
        assert!(handle.observation.snapshot_since(&current).is_err());
        let mut future = handle.observation.snapshot();
        future.sequence = 1;
        assert!(handle.observation.snapshot_since(&future).is_err());
    }

    #[test]
    fn concurrent_sends_and_snapshots_have_consistent_sequence() {
        let (handle, ends) = channels();
        identity(&ends.events, "one");
        let mut threads = Vec::new();
        for _ in 0..4 {
            let tx = ends.events.clone();
            threads.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    tx.send(AgentEvent::TextDelta { text: "x".into() }).unwrap();
                }
            }));
        }
        for _ in 0..100 {
            let s = handle.observation.snapshot();
            assert_eq!(s.events.last().map_or(0, |e| e.sequence), s.sequence);
        }
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(handle.observation.snapshot().sequence, 400);
    }
}
