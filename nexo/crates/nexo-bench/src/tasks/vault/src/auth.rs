//! Passwords and tokens.
//!
//! A token is `ID.MAC`, where the MAC is a keyed digest of the id under the server secret. The
//! digest is a 64-bit FNV-1a: a stand-in that keeps the crate free of dependencies.

use crate::error::{Result, VaultError};
use crate::users::{Directory, UserId};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// A digest of `parts`, each followed by a separator so `["ab", "c"]` and `["a", "bc"]` differ.
pub fn digest(parts: &[&str]) -> u64 {
    let mut h = FNV_OFFSET;
    for part in parts {
        for b in part.bytes().chain(std::iter::once(0xff)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(FNV_PRIME);
        }
    }
    h
}

/// The stored form of a password.
pub fn hash_password(salt: &str, password: &str) -> u64 {
    digest(&[salt, password])
}

/// The MAC part of the token of user `id`, as 16 hex digits.
pub fn token_mac(secret: &str, id: UserId) -> String {
    format!("{:016x}", digest(&[secret, &id.to_string()]))
}

/// The token a user presents with every request.
pub fn issue_token(secret: &str, id: UserId) -> String {
    format!("{id}.{}", token_mac(secret, id))
}

/// Whether two secrets are equal, taking the same time wherever they differ.
pub fn tokens_match(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// The user a token belongs to, or why it is no good.
pub fn verify_token(dir: &Directory, secret: &str, token: &str) -> Result<UserId> {
    let bad = || VaultError::Unauthenticated("invalid token".into());
    let (id, mac) = token.split_once('.').ok_or_else(bad)?;
    let id: UserId = id.parse().map_err(|_| bad())?;
    dir.get(id).ok_or_else(bad)?;
    if tokens_match(&token_mac(secret, id), mac) {
        Ok(id)
    } else {
        Err(bad())
    }
}

/// Checks a name and password and returns a token. An unknown name and a wrong password give the
/// same error, so a caller cannot tell which accounts exist.
pub fn login(dir: &Directory, secret: &str, name: &str, password: &str) -> Result<String> {
    let bad = || VaultError::Unauthenticated("wrong name or password".into());
    let user = dir.by_name(name).ok_or_else(bad)?;
    let given = format!("{:016x}", hash_password(&user.salt, password));
    let stored = format!("{:016x}", user.password_hash);
    if tokens_match(&given, &stored) {
        Ok(issue_token(secret, user.id))
    } else {
        Err(bad())
    }
}
