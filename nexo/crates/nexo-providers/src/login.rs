//! Subscription logins: sign in once in the browser with a code (the OAuth
//! device flow), and nexo keeps the tokens in the credential store
//! ([`nexo_core::auth`]) and refreshes them before they expire.
//!
//! | flow      | what you pay for                  | endpoint it unlocks                        |
//! |-----------|-----------------------------------|--------------------------------------------|
//! | `copilot` | GitHub Copilot (Free, Pro, ...)   | `api.githubcopilot.com` (protocol `copilot`) |
//! | `codex`   | ChatGPT Plus / Pro / Team         | `chatgpt.com/backend-api/codex` (openai-responses) |
//! | `xai`     | SuperGrok / X Premium             | `api.x.ai/v1` (openai-chat)                  |
//!
//! [`login`] runs a flow, [`prepare`] turns a provider's configuration into
//! the one a request goes out with: the key, refreshed when needed, and what
//! the login learned (the Copilot endpoint, the ChatGPT account).

mod choices;
mod codex;
mod copilot;
mod form;
mod oauth;
mod renew;
mod xai;

#[cfg(test)]
mod tests;

pub use choices::{choices, choices_in, open_url, Choice};
pub use codex::account_id;
pub use form::form;
pub(crate) use oauth::{fail, post, text, tokens, Body};
pub(crate) use renew::refresh_plain;
pub use renew::{apply, prepare, prepare_in, refresh, refresh_at};

use std::time::Duration;

use codex::codex;
use copilot::copilot;
use xai::xai;

use nexo_core::auth::{self, Credential};
use nexo_core::ProviderError;

/// The flows nexo can run, with a line on each.
pub const FLOWS: &[(&str, &str)] = &[
    ("copilot", "GitHub Copilot subscription (github.com device code)"),
    ("codex", "ChatGPT Plus/Pro/Team subscription: the Codex CLI login if there is one, else the browser"),
    ("codex-browser", "ChatGPT subscription: sign in in the browser (PKCE, callback on localhost:1455)"),
    ("codex-cli", "ChatGPT subscription: reuse the login of the Codex CLI ($CODEX_HOME/auth.json)"),
    ("codex-device", "ChatGPT subscription: device code, for a machine without a browser"),
    ("xai", "SuperGrok / X Premium subscription (auth.x.ai device code)"),
];

pub fn is_flow(name: &str) -> bool {
    FLOWS.iter().any(|(f, _)| *f == name)
}

/// Where each flow talks to; tests point these at a local server.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub github: String,
    pub github_api: String,
    pub openai_issuer: String,
    pub xai_issuer: String,
    /// Where SuperGrok's plan credits are read: the Grok CLI's chat proxy.
    pub xai_billing: String,
    /// The Codex CLI's login file (None: its usual place).
    pub codex_auth: Option<std::path::PathBuf>,
    /// The ports tried for the browser callback.
    pub callback_ports: Vec<u16>,
    /// Added to every wait between two polls (GitHub asks for a margin).
    pub pad: Duration,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            github: "https://github.com".into(),
            github_api: "https://api.github.com".into(),
            openai_issuer: "https://auth.openai.com".into(),
            xai_issuer: "https://auth.x.ai/oauth2".into(),
            xai_billing: "https://cli-chat-proxy.grok.com/v1".into(),
            codex_auth: None,
            callback_ports: crate::codex::CALLBACK_PORTS.to_vec(),
            pad: Duration::from_secs(3),
        }
    }
}
pub(crate) const CODEX_CLIENT: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

pub const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

/// What the person has to do: open `url`, type `code` (empty: the browser comes back by itself, only the
/// address is to be opened).
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCode {
    pub url: String,
    pub code: String,
}

/// Runs `flow`; `show` is told the address and the code to type there.
pub async fn login(flow: &str, show: &(dyn Fn(&DeviceCode) + Sync)) -> Result<Credential, ProviderError> {
    login_at(&Endpoints::default(), flow, show).await
}

pub async fn login_at(
    at: &Endpoints,
    flow: &str,
    show: &(dyn Fn(&DeviceCode) + Sync),
) -> Result<Credential, ProviderError> {
    match flow {
        "copilot" => copilot(at, show).await,
        "codex" => {
            let file = at.codex_auth.clone().unwrap_or_else(crate::codex::cli_file);
            if crate::codex::cli_login_in(&file) {
                // a CLI login that ended long ago may hold a refresh token the server no longer takes:
                // it is renewed now, and when that fails the browser sign-in runs instead of keeping it
                let cred = crate::codex::from_cli(&file)?;
                if !cred.expiring(auth::now(), 60) {
                    return Ok(cred);
                }
                match crate::codex::renew_shared(at, &cred, &file).await {
                    Ok(renewed) => Ok(renewed),
                    Err(e) => {
                        tracing::warn!("the Codex CLI login could not be renewed ({e}): signing in in the browser");
                        crate::codex::browser(at, show).await
                    }
                }
            } else {
                crate::codex::browser(at, show).await
            }
        }
        "codex-browser" => crate::codex::browser(at, show).await,
        "codex-cli" => crate::codex::from_cli(&at.codex_auth.clone().unwrap_or_else(crate::codex::cli_file)),
        "codex-device" => codex(at, show).await,
        "xai" => xai(at, show).await,
        other => Err(fail(format!(
            "no login flow {other:?} (known: {})",
            FLOWS.iter().map(|(f, _)| *f).collect::<Vec<_>>().join(", ")
        ))),
    }
}
