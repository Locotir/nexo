//! Part of the agent loop; see the parent module.
//!
//! The commands that reach a model server or an MCP server for the interface: the model setup's scan, probe,
//! sign-in and model check, and an MCP server's toggle, reconnection and sign-in. Each runs in a task of its own,
//! so the loop never waits on a server, a browser or a person signing in.

use nexo_core::provider::ServerProbe;

use super::runtime::Runtime;
use super::*;

/// The model setup opened: local model servers are looked for, and so are the models the vendors added.
pub(super) fn on_scan_local(rt: &mut Runtime, own: Vec<(String, String)>) {
    // the setup opening is also the time to look for models the vendors added
    spawn_model_lists(&rt.state);
    let events = rt.events.clone();
    // trial keys a killed run left behind: the setup opening is the time to clear them
    tokio::task::spawn_blocking(setup::sweep_trial_keys);
    tokio::spawn(async move {
        let found = nexo_providers::probe::scan_local(&own).await;
        emit(&events, AgentEvent::LocalServers { found });
    });
}

/// A server the setup names is asked what it is; a newer probe or check replaces the one running.
pub(super) fn on_probe(rt: &mut Runtime, name: String, cfg: ProviderConfig, key: Option<String>, seq: u64) {
    let events = rt.events.clone();
    if let Some(prev) = rt.checking.take() {
        prev.abort();
    }
    rt.checking = Some(
        tokio::spawn(async move {
            // a stored subscription login is renewed and applied like in a request
            let (cfg, stored) = match (&key, cfg.auth.is_some()) {
                (None, true) => nexo_providers::login::prepare(&cfg).await.unwrap_or((cfg.clone(), None)),
                _ => (cfg, None),
            };
            let key = key.or_else(|| cfg.resolve_key()).or(stored);
            let result = nexo_providers::probe::server(&cfg, key.as_deref()).await;
            emit(&events, AgentEvent::Probed { name, seq, result });
        })
        .abort_handle(),
    );
}

/// A subscription sign-in in the browser; a newer one replaces the one waiting.
pub(super) fn on_login(rt: &mut Runtime, name: String, flow: String) {
    if let Some(prev) = rt.signing_in.take() {
        prev.abort();
    }
    let events = rt.events.clone();
    rt.signing_in = Some(
        tokio::spawn(async move {
            let shown = events.clone();
            let show = move |d: &nexo_providers::login::DeviceCode| {
                nexo_providers::login::open_url(&d.url);
                emit(&shown, AgentEvent::LoginPrompt { url: d.url.clone(), code: d.code.clone() });
            };
            let result = match nexo_providers::login::login(&flow, &show).await {
                Ok(cred) => nexo_core::auth::set(&name, cred).map_err(|e| format!("the login could not be kept: {e}")),
                Err(e) => Err(e.message),
            };
            emit(&events, AgentEvent::LoggedIn { name, result });
        })
        .abort_handle(),
    );
}

/// A model the setup is about to save is tried with one small request; a newer probe or check replaces the one
/// running.
pub(super) async fn on_check_model(
    rt: &mut Runtime,
    name: String,
    probe: ServerProbe,
    cfg: ProviderConfig,
    model: String,
    key: Option<String>,
    seq: u64,
) {
    let events = rt.events.clone();
    let client = rt.state.lock().await.client.clone();
    if let Some(prev) = rt.checking.take() {
        prev.abort();
    }
    rt.checking = Some(
        tokio::spawn(async move {
            let mut cfg = cfg;
            cfg.protocol = probe.protocol.clone();
            cfg.base_url = probe.base_url.clone();
            // only for the test request: the key's own file waits for the save, and
            // the guard removes the trial file when the check ends or is stopped
            let mut _trial = None;
            if let Some(k) = key.as_deref().filter(|k| !k.trim().is_empty()) {
                match setup::trial_key(&name, k, &mut cfg) {
                    Ok(t) => _trial = Some(t),
                    Err(e) => notice(&events, Level::Warn, format!("the key could not be tried: {e}")),
                }
            }
            // bounded, so the trial file of a check never outlives the sweep's patience
            let check = tokio::time::timeout(setup::CHECK_LIMIT, setup::check(client, &name, &probe, &cfg, &model))
                .await
                .unwrap_or_else(|_| nexo_core::ModelCheck {
                    error: Some(format!("no answer in {} s", setup::CHECK_LIMIT.as_secs())),
                    ..Default::default()
                });
            emit(&events, AgentEvent::ModelChecked { name, model, seq, check });
        })
        .abort_handle(),
    );
}

/// An MCP server turned on or off, for this session or (`persist`) in the configuration.
pub(super) async fn on_mcp_toggle(rt: &mut Runtime, server: String, on: bool, persist: bool) {
    let hub = rt.state.lock().await.mcp.clone();
    let events = rt.events.clone();
    tokio::spawn(async move {
        let r = match hub {
            Some(h) => h.set_enabled(&server, on, persist).await,
            None => Err("no MCP servers are configured".to_string()),
        };
        mcp_result(&events, r);
    });
}

/// An MCP server connected again.
pub(super) async fn on_mcp_reconnect(rt: &mut Runtime, server: String) {
    let hub = rt.state.lock().await.mcp.clone();
    let events = rt.events.clone();
    tokio::spawn(async move {
        let r = match hub {
            Some(h) => h.reconnect(&server).await,
            None => Err("no MCP servers are configured".to_string()),
        };
        mcp_result(&events, r);
    });
}

/// An MCP server's sign-in in the browser.
pub(super) async fn on_mcp_login(rt: &mut Runtime, server: String) {
    let hub = rt.state.lock().await.mcp.clone();
    let events = rt.events.clone();
    tokio::spawn(async move {
        let open = {
            let events = events.clone();
            move |url: &str| {
                if nexo_providers::login::open_url(url) {
                    notice(
                        &events,
                        Level::Info,
                        format!("Opened your browser to sign in. If nothing opened, go to:\n{url}"),
                    );
                } else {
                    notice(&events, Level::Info, format!("Open this address in a browser to sign in:\n{url}"));
                }
            }
        };
        let r = match hub {
            Some(h) => h.login(&server, &open).await,
            None => Err("no MCP servers are configured".to_string()),
        };
        mcp_result(&events, r);
    });
}
