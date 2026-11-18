//! What every protocol parser has in common: it is fed the data of one
//! event at a time, told when the transport failed, and told when the
//! stream ended, so a response that said nothing is reported as an error
//! instead of passing for an empty reply.

use futures::{Stream, StreamExt};
use nexo_core::{ErrorKind, EventStream, FinishReason, ProviderError, StreamEvent};
use std::sync::{Arc, Mutex};

use crate::sse::SseEvent;

pub trait Feed: Send + 'static {
    /// The data of one event.
    fn feed(&mut self, data: &str) -> Vec<StreamEvent>;
    /// The transport failed with `e`.
    fn fail(&mut self, e: String) -> Vec<StreamEvent>;
    /// The stream ended; what is still open is closed here.
    fn finish(&mut self) -> Vec<StreamEvent>;
}

/// Runs `parser` over the events and ends the stream with `Done`.
pub fn run<P: Feed>(events: impl Stream<Item = Result<SseEvent, String>> + Send + 'static, parser: P) -> EventStream {
    let p = Arc::new(Mutex::new(parser));
    let p2 = p.clone();
    let s = events
        .flat_map(move |ev| {
            let out = match p.lock() {
                Ok(mut p) => match ev {
                    Ok(ev) => p.feed(&ev.data),
                    Err(e) => p.fail(e),
                },
                Err(_) => Vec::new(),
            };
            futures::stream::iter(out)
        })
        .chain(
            futures::stream::once(async move { p2.lock().map(|mut p| p.finish()).unwrap_or_default() })
                .flat_map(futures::stream::iter),
        )
        .chain(futures::stream::once(async { StreamEvent::Done }));
    crate::openai_chat::terminated(s)
}

/// Leaves a trace in the log of an event whose data was not JSON, so a hole in a reply can be explained
/// afterwards. A blank event or an SSE comment is keep-alive, not loss, and stays silent.
pub fn note_dropped(data: &str) {
    let t = data.trim();
    if t.is_empty() || t.starts_with(':') {
        return;
    }
    let mut end = t.len().min(200);
    while !t.is_char_boundary(end) {
        end -= 1;
    }
    tracing::warn!(bytes = data.len(), head = %&t[..end], "dropped an event that is not JSON");
}

/// The state the parsers share at the end of a stream.
#[derive(Debug, Default, Clone, Copy)]
pub struct Progress {
    /// A chunk of the protocol was understood.
    pub started: bool,
    /// Text, reasoning or a tool call reached the caller.
    pub content: bool,
    /// A `Finish` or an `Error` was emitted.
    pub finished: bool,
    pub saw_tool: bool,
}

impl Progress {
    /// What the end of the stream means, given what came before: nothing at
    /// all is a response that was not the protocol (an HTML page, an empty
    /// body); a start without content is a reply cut before it began (both
    /// retryable by the client); content without a finish is taken as complete (see `at_end_strict` for
    /// the protocols that always say how a reply ended).
    pub fn at_end(&mut self) -> Option<StreamEvent> {
        if self.finished {
            return None;
        }
        self.finished = true;
        Some(if !self.started {
            StreamEvent::Error(ProviderError::new(
                ErrorKind::Parse,
                "the response was not an event stream of this protocol (nothing in it could be read)",
            ))
        } else if !self.content {
            StreamEvent::Error(ProviderError::new(ErrorKind::Network, "the stream ended before the reply began"))
        } else {
            StreamEvent::Finish(if self.saw_tool { FinishReason::ToolUse } else { FinishReason::Stop })
        })
    }

    /// Like `at_end_with_calls`, for a protocol that always closes a reply with an explicit stop: content
    /// with no stop means the connection closed early, so the end is a truncation error, not a finish.
    /// The caller keeps the partial text and sees the error, as it does for a cut tool call.
    pub fn at_end_strict(&mut self, open: bool) -> Option<StreamEvent> {
        if !open && !self.finished && self.content {
            self.finished = true;
            return Some(StreamEvent::Error(ProviderError::new(
                ErrorKind::Network,
                "the connection closed before the reply was complete (truncated)",
            )));
        }
        self.at_end_with_calls(open)
    }

    /// The end of a stream that left tool calls open (`open`): without a
    /// proper stop their arguments may be cut, so this is an error, not a
    /// finish, and the caller drops the open calls instead of closing them.
    pub fn at_end_with_calls(&mut self, open: bool) -> Option<StreamEvent> {
        if open && !self.finished {
            self.finished = true;
            return Some(StreamEvent::Error(ProviderError::new(
                ErrorKind::Network,
                "the stream ended in the middle of a tool call (truncated)",
            )));
        }
        self.at_end()
    }
}

/// The kind of an error a server reports inside its stream, from the
/// error's type (`rate_limit_error`, `overloaded_error`, `RESOURCE_EXHAUSTED`,
/// ...) and message.
pub fn stream_error_kind(error_type: &str, message: &str) -> ErrorKind {
    let t = error_type.to_ascii_lowercase();
    let m = message.to_ascii_lowercase();
    // an empty balance is not a limit that lifts: waiting never refills it, so it is not retried
    if nexo_core::says_no_credit(&t) || nexo_core::says_no_credit(&m) {
        ErrorKind::BadRequest
    } else if t.contains("rate_limit") || t.contains("resource_exhausted") || t == "429" || m.contains("rate limit") {
        ErrorKind::RateLimit
    } else if t.contains("overloaded") || t.contains("unavailable") || m.contains("overloaded") {
        ErrorKind::Overloaded
    } else if t.contains("authentication")
        || t.contains("permission")
        || t.contains("unauthenticated")
        || t.contains("invalid_api_key")
    {
        ErrorKind::Auth
    } else if nexo_core::is_context_error(None, error_type, message) {
        ErrorKind::ContextLength
    } else if t.contains("invalid_request") || t.contains("invalid_argument") || t.contains("bad_request") {
        ErrorKind::BadRequest
    } else {
        ErrorKind::Server
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_end_of_a_stream_is_read_from_what_came_before() {
        let mut p = Progress::default();
        assert!(matches!(p.at_end(), Some(StreamEvent::Error(e)) if e.kind == ErrorKind::Parse));
        assert_eq!(p.at_end(), None, "once");
        let mut p = Progress { started: true, ..Default::default() };
        assert!(matches!(p.at_end(), Some(StreamEvent::Error(e)) if e.kind == ErrorKind::Network && e.is_retryable()));
        let mut p = Progress { started: true, content: true, saw_tool: true, ..Default::default() };
        assert_eq!(p.at_end(), Some(StreamEvent::Finish(FinishReason::ToolUse)));
        let mut p = Progress { finished: true, ..Default::default() };
        assert_eq!(p.at_end(), None);
    }

    #[test]
    fn classifies_errors_reported_in_a_stream() {
        assert_eq!(stream_error_kind("rate_limit_error", ""), ErrorKind::RateLimit);
        assert_eq!(stream_error_kind("RESOURCE_EXHAUSTED", "quota"), ErrorKind::RateLimit);
        assert_eq!(stream_error_kind("overloaded_error", ""), ErrorKind::Overloaded);
        assert_eq!(stream_error_kind("", "This model's maximum context length is 8192"), ErrorKind::ContextLength);
        assert_eq!(stream_error_kind("authentication_error", ""), ErrorKind::Auth);
        assert_eq!(stream_error_kind("server_error", "boom"), ErrorKind::Server);
    }

    #[test]
    fn an_empty_balance_reported_in_a_stream_is_not_retried_but_a_busy_one_is() {
        let empty = stream_error_kind("insufficient_quota", "You exceeded your current quota");
        assert_eq!(empty, ErrorKind::BadRequest);
        assert!(!ProviderError::new(empty, "x").is_retryable());
        assert_eq!(stream_error_kind("", "Your credit balance is too low"), ErrorKind::BadRequest);
        assert_eq!(stream_error_kind("rate_limit_error", "slow down"), ErrorKind::RateLimit);
        assert_eq!(stream_error_kind("RESOURCE_EXHAUSTED", "You exceeded your current quota"), ErrorKind::RateLimit);
    }

    #[test]
    fn a_too_long_error_in_a_stream_is_the_context_kind_with_or_without_a_number() {
        assert_eq!(stream_error_kind("context_length_exceeded", "Your input is too long."), ErrorKind::ContextLength);
        assert_eq!(stream_error_kind("exceed_context_size_error", ""), ErrorKind::ContextLength);
        assert_eq!(stream_error_kind("", "Input exceeds the model's context window"), ErrorKind::ContextLength);
        assert_eq!(
            stream_error_kind(
                "INVALID_ARGUMENT",
                "The input token count (300000) exceeds the maximum number of tokens allowed (262144)."
            ),
            ErrorKind::ContextLength
        );
        assert_eq!(
            stream_error_kind("", "request (9000 tokens) exceeds the available context size (8192 tokens)"),
            ErrorKind::ContextLength
        );
    }

    #[test]
    fn open_calls_at_the_end_of_a_stream_are_a_truncation_error() {
        let mut p = Progress { started: true, content: true, saw_tool: true, ..Default::default() };
        assert!(matches!(p.at_end_with_calls(true), Some(StreamEvent::Error(e)) if e.message.contains("truncated")));
        assert_eq!(p.at_end_with_calls(true), None, "once");
        let mut p = Progress { started: true, content: true, saw_tool: true, ..Default::default() };
        assert_eq!(p.at_end_with_calls(false), Some(StreamEvent::Finish(FinishReason::ToolUse)));
        let mut p = Progress { finished: true, ..Default::default() };
        assert_eq!(p.at_end_with_calls(true), None, "a stream that already stopped");
    }
}
