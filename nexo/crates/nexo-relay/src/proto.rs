//! The vocabulary the agent, the host and the extension share. Every message is a JSON object with a `t` (type).
//!
//! Agent <-> host, on the local socket (the host adds `sid`, the session, when it passes one on to the extension):
//!
//! ```text
//! host  -> agent  {t:"challenge", v, nonce}
//! agent -> host   {t:"hello", client, name, nonce, proof}          proof = proof("agent", token, host nonce, agent nonce)
//! host  -> agent  {t:"welcome", proof, browser:{id,name,os,version,ext}, ready}   or {t:"denied", why}
//! agent -> host   {t:"connect", id}                                 asks the person for consent in the browser
//! host  -> agent  {t:"consent", id, ok, why?}
//! agent -> host   {t:"req", id, m, p}                               m: tabs.list tabs.create tabs.close tabs.activate
//!                                                                      window.resize cdp
//! host  -> agent  {t:"res", id, r}  or  {t:"res", id, e}
//! host  -> agent  {t:"ev", tab, m, p}                               a CDP event of one of the session's tabs
//! host  -> agent  {t:"gone", why}                                   the browser went away
//! ```
//!
//! Host <-> extension, on Chrome's stdio: the same `req`, `res` and `ev` with a `sid`, plus `hello` (the browser says
//! who it is), `session` (an agent came or left), `consent` and `consent_result`, `ping` and `pong`.

use sha2::{Digest, Sha256};
use uuid::Uuid;

/// The protocol version in `challenge`.
pub const VERSION: u32 = 1;

/// Longest a person gets to answer the consent prompt; the agent waits a little longer than the extension does.
pub const CONSENT_SECS: u64 = 120;

/// A fresh random string, 32 hex characters.
pub fn nonce() -> String {
    Uuid::new_v4().simple().to_string()
}

/// The proof that one side knows the token, without sending it: the hash of the role, the token and both nonces.
/// The host proves itself too, so an agent never talks to a squatter on the socket or pipe name.
pub fn proof(role: &str, token: &str, host_nonce: &str, agent_nonce: &str) -> String {
    let mut h = Sha256::new();
    for part in [role, token, host_nonce, agent_nonce] {
        h.update((part.len() as u32).to_be_bytes());
        h.update(part.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Browser ids become file and pipe names: letters, digits, `-` and `_` only, 1 to 64 of them.
pub fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// What a browser tells about itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BrowserInfo {
    pub id: String,
    pub name: String,
    pub os: String,
    pub version: String,
    pub ext: String,
}

impl BrowserInfo {
    /// From the `browser` object of a `hello` or `welcome`. Every text is cut to a sane length: it comes from the
    /// browser, so it is shown to the person, never trusted.
    pub fn from_json(v: &serde_json::Value) -> Option<BrowserInfo> {
        let text = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .chars()
                .filter(|c| !c.is_control())
                .take(80)
                .collect::<String>()
        };
        let id = text("id");
        valid_id(&id).then(|| BrowserInfo {
            id,
            name: text("name"),
            os: text("os"),
            version: text("version"),
            ext: text("ext"),
        })
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({"id": self.id, "name": self.name, "os": self.os, "version": self.version, "ext": self.ext})
    }
}
