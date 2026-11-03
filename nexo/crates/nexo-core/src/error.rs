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
            400 | 413 | 422
                if lower.contains("context length")
                    || lower.contains("context_length")
                    || lower.contains("too many tokens")
                    || lower.contains("maximum context")
                    || lower.contains("prompt is too long")
                    || lower.contains("exceeds the model") =>
            {
                ErrorKind::ContextLength
            }
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
}
