use std::time::Duration;

use nexo_core::auth;
use nexo_core::auth::Credential;
use nexo_core::{ErrorKind, ProviderConfig, ProviderError};

use super::form;
use super::oauth::{fail, post, said, tokens, Body};
use super::xai::XAI_CLIENT;
use super::{Endpoints, CODEX_CLIENT};

/// A fresh access token for `cred`, when its flow has one to give.
pub async fn refresh(cred: &Credential) -> Result<Credential, ProviderError> {
    refresh_at(&Endpoints::default(), cred).await
}

pub async fn refresh_at(at: &Endpoints, cred: &Credential) -> Result<Credential, ProviderError> {
    if cred.metadata("flow") == Some("claude") {
        return super::claude::refresh(cred).await;
    }
    // a login that lives in the Codex CLI's file is renewed together with that file
    if cred.metadata("flow") == Some("codex") {
        if let Some(f) = cred.metadata("source_file") {
            return crate::codex::renew_shared(at, cred, std::path::Path::new(f)).await;
        }
    }
    refresh_plain(at, cred).await
}

/// The renewal request itself.
pub(crate) async fn refresh_plain(at: &Endpoints, cred: &Credential) -> Result<Credential, ProviderError> {
    let Credential::Oauth { refresh, .. } = cred else { return Ok(cred.clone()) };
    let flow = cred.metadata("flow").unwrap_or_default().to_string();
    let (url, client) = match flow.as_str() {
        "codex" => (format!("{}/oauth/token", at.openai_issuer), CODEX_CLIENT),
        "xai" => (format!("{}/token", at.xai_issuer), XAI_CLIENT),
        _ => return Ok(cred.clone()),
    };
    if refresh.is_empty() {
        return Err(fail(format!("the {flow} login has expired: run `nexo auth login` again")));
    }
    let (st, v) = post(
        &url,
        Body::Form(form(&[("grant_type", "refresh_token"), ("refresh_token", refresh), ("client_id", client)])),
    )
    .await?;
    if !(200..300).contains(&st) {
        return Err(fail(format!(
            "the {flow} login could not be renewed ({st}: {}); run `nexo auth login` again",
            said(&v)
        )));
    }
    tokens(&flow, st, &v, Some(cred))
}

/// The key `cfg` names directly (variable, file or command), not the credential store. A key command may
/// wait for the person, so it runs on a blocking thread: the request waits for it, a worker does not.
pub async fn direct_key(cfg: &ProviderConfig) -> Option<String> {
    let direct = ProviderConfig { auth: None, ..cfg.clone() };
    if direct.api_key_cmd.is_none() {
        return direct.resolve_key();
    }
    tokio::task::spawn_blocking(move || direct.resolve_key()).await.ok().flatten()
}

/// The configuration and key a request to `cfg` goes out with. A key from
/// the environment, a file or a command wins; else the store's entry (under
/// `auth`) is used, renewed first when it is about to expire, and what its
/// login learned is applied.
pub async fn prepare(cfg: &ProviderConfig) -> Result<(ProviderConfig, Option<String>), ProviderError> {
    prepare_in(&auth::file(), &Endpoints::default(), cfg).await
}

pub async fn prepare_in(
    store: &std::path::Path,
    at: &Endpoints,
    cfg: &ProviderConfig,
) -> Result<(ProviderConfig, Option<String>), ProviderError> {
    let claude_only = cfg.auth.as_deref() == Some("anthropic-oauth") || cfg.login.as_deref() == Some("claude");
    if !claude_only {
        if let Some(k) = direct_key(cfg).await {
            return Ok((cfg.clone(), Some(k)));
        }
    }
    let Some(name) = cfg.auth.as_deref() else { return Ok((cfg.clone(), None)) };
    // a store that cannot be parsed is said so: reading it as empty would send the person to log in again,
    // and that login would then fail to write over the file. Not an `Auth` error: those are shown with the
    // advice to log in again, which is the wrong one here.
    let unreadable = |e: std::io::Error| ProviderError::new(ErrorKind::Other, e.to_string());
    let Some(mut cred) = auth::read_checked(store).map_err(unreadable)?.remove(name) else {
        return Ok((cfg.clone(), None));
    };
    if claude_only && cred.metadata("flow") != Some("claude") {
        return Err(fail("anthropic-oauth requires a Claude OAuth login; run `nexo auth login anthropic-oauth`"));
    }
    validate_claude_destination(cfg, &cred)?;
    if cred.expiring(auth::now(), 60) {
        cred = renew_locked(store, at, name, cfg).await?.unwrap_or(cred);
    }
    if claude_only && cred.metadata("flow") != Some("claude") {
        return Err(fail("anthropic-oauth requires a Claude OAuth login"));
    }
    validate_claude_destination(cfg, &cred)?;
    Ok((apply(cfg, &cred), Some(cred.secret().to_string())))
}

// Keep this deliberately strict: no userinfo, alternate ports, queries or proxy paths.
// This check runs before refresh as well as before returning the access token.
fn validate_claude_destination(cfg: &ProviderConfig, cred: &Credential) -> Result<(), ProviderError> {
    if cred.metadata("flow") == Some("claude")
        && (cfg.protocol != "anthropic"
            || !matches!(
                cfg.base_url.as_str(),
                "https://api.anthropic.com"
                    | "https://api.anthropic.com/"
                    | "https://api.anthropic.com/v1"
                    | "https://api.anthropic.com/v1/"
            ))
    {
        return Err(fail("Claude OAuth requires the official Anthropic API endpoint and protocol"));
    }
    Ok(())
}

/// The whole renewal request, inside the task that holds the store's lock: whatever the server does, the
/// lock is let go within it, well before the others give up waiting for it (`Lock::WAIT`, 60s).
pub(super) const RENEWAL_LIMIT: Duration = if cfg!(test) { Duration::from_secs(1) } else { Duration::from_secs(40) };

/// Renews the entry `name` holding the store's lock from the read to the write, so two sessions never
/// spend the same refresh token: after the lock is taken the entry is read again, and one another
/// process renewed meanwhile is used as it is. None when the entry is gone.
pub(super) async fn renew_locked(
    store: &std::path::Path,
    at: &Endpoints,
    name: &str,
    cfg: &ProviderConfig,
) -> Result<Option<Credential>, ProviderError> {
    let f = store.to_path_buf();
    let lock = tokio::task::spawn_blocking(move || auth::lock_in(&f))
        .await
        .map_err(|e| fail(format!("the credential store could not be locked: {e}")))?
        .map_err(|e| fail(format!("the credential store could not be locked: {e}")))?;
    let Some(cred) = lock.get(name) else { return Ok(None) };
    validate_claude_destination(cfg, &cred)?;
    if !cred.expiring(auth::now(), 60) {
        return Ok(Some(cred));
    }
    // the server may spend the old refresh token before it answers: the renewal and its save go on
    // in a task of their own, holding the lock, even when this request stops waiting or is dropped
    let (at, owned) = (at.clone(), name.to_string());
    let task = tokio::spawn(async move {
        let renewed = tokio::time::timeout(RENEWAL_LIMIT, refresh_at(&at, &cred)).await.map_err(|_| {
            fail(format!("the server did not answer the renewal of {owned} in {}s", RENEWAL_LIMIT.as_secs()))
        })??;
        // a store that cannot be written still leaves this request working
        if let Err(e) = lock.set(&owned, renewed.clone()) {
            tracing::warn!("could not save the renewed login for {owned}: {e}");
        }
        Ok::<_, ProviderError>(renewed)
    });
    // bounded: on timeout only this request fails; the next one reads what the task saved
    let renewed = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .map_err(|_| fail(format!("renewing the login for {name} took over 30s; try again")))?
        .map_err(|e| fail(format!("renewing the login for {name} failed: {e}")))??;
    Ok(Some(renewed))
}

/// What a login adds to the provider's configuration.
pub fn apply(cfg: &ProviderConfig, cred: &Credential) -> ProviderConfig {
    let mut c = cfg.clone();
    match cred.metadata("flow") {
        Some("claude") if matches!(cred, Credential::Oauth { .. }) => {
            c.extra.insert("bearer".into(), toml::Value::Boolean(true));
            let key = c
                .headers
                .keys()
                .find(|k| k.eq_ignore_ascii_case("anthropic-beta"))
                .cloned()
                .unwrap_or_else(|| "anthropic-beta".into());
            let beta = c.headers.entry(key).or_default();
            if !beta.split(',').any(|v| v.trim() == "oauth-2025-04-20") {
                if !beta.trim().is_empty() {
                    beta.push(',');
                }
                beta.push_str("oauth-2025-04-20");
            }
        }
        Some("copilot") => {
            if let Some(base) = cred.metadata("base_url") {
                if c.base_url == crate::copilot::BASE_URL {
                    c.base_url = base.to_string();
                }
            }
        }
        Some("codex") => {
            let mut set = |k: &str, v: &str| {
                c.headers.entry(k.to_string()).or_insert_with(|| v.to_string());
            };
            set("originator", "nexo");
            set("x-codex-beta-features", "remote_compaction_v2");
            if let Some(id) = cred.metadata("account_id") {
                set("chatgpt-account-id", id);
            }
            c.extra.entry("omit_max_tokens".into()).or_insert(toml::Value::Boolean(true));
        }
        _ => {}
    }
    c
}
