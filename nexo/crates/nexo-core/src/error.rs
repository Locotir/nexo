//! Errors a provider call can end in, classified so the caller knows what
//! to do (retry, compact, ask for a key, give up).

use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Missing or rejected key.
    Auth,
    /// 429.
    RateLimit,
    /// 503, 529, "overloaded".
    Overloaded,
    /// The request is longer than the model's window.
    ContextLength,
    /// Any other 4xx.
    BadRequest,
    /// Could not connect, or the stream was cut.
    Network,
    /// The response could not be understood.
    Parse,
    /// 5xx not covered above.
    Server,
    /// Cancelled by the caller.
    Cancelled,
    Other,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error, Serialize, Deserialize)]
#[error("{kind:?}: {message}")]
pub struct ProviderError {
    pub kind: ErrorKind,
    pub status: Option<u16>,
    pub message: String,
    /// What the server asked to wait, when it said.
    pub retry_after: Option<Duration>,
}

impl ProviderError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> ProviderError {
        ProviderError { kind, status: None, message: message.into(), retry_after: None }
    }

    pub fn with_status(mut self, status: u16) -> ProviderError {
        self.status = Some(status);
        self
    }

    /// Classifies an HTTP status with the body's text.
    pub fn from_http(status: u16, body: &str) -> ProviderError {
        let lower = body.to_ascii_lowercase();
        let kind = match status {
            401 | 403 => ErrorKind::Auth,
            429 => ErrorKind::RateLimit,
            400 | 413 | 422 if is_context_error(Some(status), "", body) => ErrorKind::ContextLength,
            408 => ErrorKind::Network,
            400..=499 => ErrorKind::BadRequest,
            503 | 529 => ErrorKind::Overloaded,
            500..=599 => ErrorKind::Server,
            _ => ErrorKind::Other,
        };
        let kind = if lower.contains("overloaded") && kind == ErrorKind::Server { ErrorKind::Overloaded } else { kind };
        let message = if body.trim().is_empty() { format!("HTTP {status}") } else { shorten(body, 600) };
        ProviderError { kind, status: Some(status), message, retry_after: None }
    }

    pub fn is_retryable(&self) -> bool {
        matches!(self.kind, ErrorKind::RateLimit | ErrorKind::Overloaded | ErrorKind::Server | ErrorKind::Network)
    }
}

/// Whether an error says the request is longer than the model's window,
/// from its HTTP status (when there is one), its code or type, and its
/// message. The one test every protocol and the HTTP layer share.
pub fn is_context_error(status: Option<u16>, code: &str, message: &str) -> bool {
    if status == Some(413) {
        return true;
    }
    let c = code.to_ascii_lowercase();
    if c.contains("context_length") || c.contains("context_size") {
        return true;
    }
    let m = message.to_ascii_lowercase();
    [
        "context length",
        "context_length",
        "context window",
        "context size",
        "context_size",
        "maximum context",
        "too many tokens",
        "prompt is too long",
        "exceeds the model",
        "exceeds the maximum number of tokens",
    ]
    .iter()
    .any(|k| m.contains(k))
}

fn shorten(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n - 1).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_statuses() {
        assert_eq!(ProviderError::from_http(401, "").kind, ErrorKind::Auth);
        assert_eq!(ProviderError::from_http(429, "slow down").kind, ErrorKind::RateLimit);
        assert_eq!(
            ProviderError::from_http(400, "This model's maximum context length is 128000").kind,
            ErrorKind::ContextLength
        );
        assert_eq!(ProviderError::from_http(400, "bad json").kind, ErrorKind::BadRequest);
        assert_eq!(ProviderError::from_http(500, "{\"type\":\"overloaded_error\"}").kind, ErrorKind::Overloaded);
        assert!(ProviderError::from_http(502, "").is_retryable());
        assert!(!ProviderError::from_http(400, "").is_retryable());
    }

    #[test]
    fn one_test_knows_every_too_long_wording() {
        assert!(is_context_error(None, "context_length_exceeded", ""));
        assert!(is_context_error(None, "exceed_context_size_error", ""));
        assert!(is_context_error(
            None,
            "",
            "The input token count (300000) exceeds the maximum number of tokens allowed (262144)."
        ));
        assert!(is_context_error(
            None,
            "",
            "the request (9000 tokens) exceeds the available context size (8192 tokens)"
        ));
        assert!(is_context_error(Some(413), "", "Request Entity Too Large"));
        assert!(!is_context_error(None, "invalid_request_error", "max_tokens: 65536 > 8192"));
        assert!(!is_context_error(Some(400), "", "bad json"));
        assert_eq!(ProviderError::from_http(413, "").kind, ErrorKind::ContextLength);
        assert_eq!(
            ProviderError::from_http(400, r#"{"error":{"code":"exceed_context_size_error","message":"too big"}}"#).kind,
            ErrorKind::ContextLength
        );
    }

    #[test]
    fn a_request_timeout_is_retried_as_a_network_error() {
        let e = ProviderError::from_http(408, "Request Timeout");
        assert_eq!(e.kind, ErrorKind::Network);
        assert!(e.is_retryable());
    }
}
