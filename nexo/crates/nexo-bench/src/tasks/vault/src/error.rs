//! The error type shared by every layer, and the status code each kind maps to.

use std::fmt;

/// Everything that can go wrong while serving a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultError {
    /// The thing asked for does not exist.
    NotFound(String),
    /// The caller is known but may not do this.
    Denied(String),
    /// The caller is not logged in, or the credentials are wrong.
    Unauthenticated(String),
    /// The input is well-formed but not acceptable.
    Invalid(String),
    /// The input is not well-formed at all.
    Malformed(String),
    /// A per-user limit was reached.
    QuotaExceeded(String),
    /// The disk said no.
    Io(String),
}

/// The result type of the whole crate.
pub type Result<T> = std::result::Result<T, VaultError>;

impl VaultError {
    /// The status code a front end should answer with.
    pub fn status(&self) -> u16 {
        match self {
            VaultError::NotFound(_) => 404,
            VaultError::Denied(_) => 403,
            VaultError::Unauthenticated(_) => 401,
            VaultError::Invalid(_) | VaultError::Malformed(_) => 400,
            VaultError::QuotaExceeded(_) => 413,
            VaultError::Io(_) => 500,
        }
    }

    /// The human-readable part of the error.
    pub fn message(&self) -> &str {
        match self {
            VaultError::NotFound(m)
            | VaultError::Denied(m)
            | VaultError::Unauthenticated(m)
            | VaultError::Invalid(m)
            | VaultError::Malformed(m)
            | VaultError::QuotaExceeded(m)
            | VaultError::Io(m) => m,
        }
    }
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            VaultError::NotFound(_) => "not found",
            VaultError::Denied(_) => "denied",
            VaultError::Unauthenticated(_) => "unauthenticated",
            VaultError::Invalid(_) => "invalid",
            VaultError::Malformed(_) => "malformed",
            VaultError::QuotaExceeded(_) => "quota exceeded",
            VaultError::Io(_) => "i/o error",
        };
        write!(f, "{kind}: {}", self.message())
    }
}

impl std::error::Error for VaultError {}

impl From<std::io::Error> for VaultError {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::NotFound {
            VaultError::NotFound(e.to_string())
        } else {
            VaultError::Io(e.to_string())
        }
    }
}
