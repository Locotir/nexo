//! PKCE (RFC 7636): the verifier and the S256 challenge an OAuth browser
//! login sends in its place.

use base64::Engine;
use sha2::{Digest, Sha256};

/// A code verifier and the challenge sent in its place.
#[derive(Debug, Clone, PartialEq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// `BASE64URL(SHA256(verifier))`, no padding (RFC 7636, S256).
pub fn challenge_of(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A fresh verifier: 64 characters from 256 random bits, within the 43..128 the RFC allows.
pub fn pkce() -> Pkce {
    let verifier = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    Pkce { challenge: challenge_of(&verifier), verifier }
}
