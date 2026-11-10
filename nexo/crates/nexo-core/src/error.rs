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

impl ErrorKind {
    /// A plain lowercase phrase for the kind.
    pub fn phrase(self) -> &'static str {
        match self {
            ErrorKind::Auth => "key missing or rejected",
            ErrorKind::RateLimit => "rate limited",
            ErrorKind::Overloaded => "provider overloaded",
            ErrorKind::ContextLength => "request too long for the model",
            ErrorKind::BadRequest => "request refused",
            ErrorKind::Network => "network error",
            ErrorKind::Parse => "unreadable answer",
            ErrorKind::Server => "provider error",
            ErrorKind::Cancelled => "cancelled",
            ErrorKind::Other => "error",
        }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.phrase(), self.message)?;
        if self.kind == ErrorKind::Auth {
            write!(f, " (run `nexo auth login <provider>`)")?;
        }
        Ok(())
    }
}

impl std::error::Error for ProviderError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

    /// What the person reads: a plain sentence for the kind, with the server's own message when it
    /// gave one (the `message` of a JSON body), never the enum name or the raw body.
    pub fn human(&self) -> String {
        let said = server_message(&self.message);
        let code = self.status.map(|s| format!(" ({s})")).unwrap_or_default();
        let base = match self.kind {
            ErrorKind::Auth if self.status.is_some() => format!("Key rejected{code}"),
            ErrorKind::Auth => "Key missing or rejected".to_string(),
            ErrorKind::RateLimit => "Rate limited".to_string(),
            ErrorKind::Overloaded => "The provider is overloaded".to_string(),
            ErrorKind::ContextLength => "The request is longer than the model's window".to_string(),
            ErrorKind::BadRequest => format!("The provider refused the request{code}"),
            ErrorKind::Network => "Could not reach the provider".to_string(),
            ErrorKind::Parse => "The provider's answer could not be read".to_string(),
            ErrorKind::Server => format!("The provider failed{code}"),
            ErrorKind::Cancelled => return "Cancelled".to_string(),
            ErrorKind::Other => return said.unwrap_or_else(|| self.message.clone()),
        };
        match said {
            Some(m) if !m.is_empty() => format!("{base}: {m}"),
            _ => base,
        }
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

/// The `message` of a JSON error body (`{"error":{"message":..}}`, `{"message":..}`, `{"error":".."}`),
/// or the text itself when it is not JSON; None when it is empty or only `HTTP 500`.
fn server_message(body: &str) -> Option<String> {
    let t = body.trim();
    let found = match serde_json::from_str::<serde_json::Value>(t) {
        Ok(v) => {
            let e = v.get("error").unwrap_or(&v);
            e.get("message")
                .or_else(|| v.get("message"))
                .and_then(|m| m.as_str())
                .or_else(|| e.as_str())
                .map(str::to_string)
        }
        Err(_) if t.starts_with('{') || t.starts_with('[') => None,
        Err(_) => Some(t.to_string()),
    };
    found.map(|m| shorten(m.lines().next().unwrap_or(""), 240)).filter(|m| !m.is_empty() && !m.starts_with("HTTP "))
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
    fn the_person_reads_a_sentence_not_the_enum_and_the_body() {
        let e = ProviderError::from_http(
            429,
            r#"{"error":{"type":"rate_limit_error","message":"Number of requests exceeded"}}"#,
        );
        assert_eq!(e.human(), "Rate limited: Number of requests exceeded");
        let e = ProviderError::from_http(401, r#"{"error":"bad key"}"#);
        assert_eq!(e.human(), "Key rejected (401): bad key");
        assert_eq!(ProviderError::from_http(500, "").human(), "The provider failed (500)");
        assert_eq!(ProviderError::from_http(502, r#"{"x":1}"#).human(), "The provider failed (502)");
        assert_eq!(
            ProviderError::new(ErrorKind::Network, "connection reset").human(),
            "Could not reach the provider: connection reset"
        );
        assert!(!ProviderError::from_http(429, "{\"a\":1}").human().contains('{'));
    }

    #[test]
    fn a_request_timeout_is_retried_as_a_network_error() {
        let e = ProviderError::from_http(408, "Request Timeout");
        assert_eq!(e.kind, ErrorKind::Network);
        assert!(e.is_retryable());
    }
}
