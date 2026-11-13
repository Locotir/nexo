//! Finding the person's browsers and getting their consent: the part of [`RelayBrowser`] before the first tab.

use super::RelayBrowser;
use crate::{Error, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use nexo_relay::client::{self, Client};
use nexo_relay::Dirs;
use serde::Serialize;

/// What nexo calls itself to the browser (the host shows it next to the agent's own name).
const AGENT: &str = "nexo";

/// One browser with the nexo extension running, as listed by [`browsers`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrowserEntry {
    /// Pass this to [`RelayBrowser::connect`].
    pub id: String,
    /// What the person sees, e.g. `Google Chrome`.
    pub name: String,
    pub os: String,
    pub version: String,
    /// Other agents that are using it right now (empty when it is free).
    pub in_use: Vec<String>,
}

/// The browsers that answer right now, and the ones that are there but did not (id and why).
pub async fn browsers(dirs: &Dirs) -> (Vec<BrowserEntry>, Vec<(String, String)>) {
    let (found, bad) = client::list(dirs).await;
    let list = found
        .into_iter()
        .map(|l| BrowserEntry {
            id: l.info.id,
            name: l.info.name,
            os: l.info.os,
            version: l.info.version,
            in_use: l.others,
        })
        .collect();
    (list, bad)
}

fn none_running() -> Error {
    Error::NotConnected(
        "no browser with the nexo extension is running; run `nexo chrome-setup`, load the extension in Chrome \
         (chrome://extensions, Developer mode, Load unpacked) and keep that Chrome open"
            .into(),
    )
}

/// A session with this browser that the person allowed.
async fn allowed(dirs: &Dirs, id: &str, name: &str) -> Result<Client> {
    let client = Client::open(dirs, id, AGENT, name).await?;
    // Nothing is controlled before the person clicks Allow in the browser; this waits for that click.
    client.consent().await?;
    Ok(client)
}

/// Asks every running browser at once; the first one whose person clicks Allow is the one used, and the prompts of
/// the others are withdrawn when their sessions drop.
async fn any(dirs: &Dirs, name: &str) -> Result<Client> {
    let (found, bad) = client::list(dirs).await;
    match found.as_slice() {
        [] => {
            let why: Vec<String> = bad.iter().map(|(id, why)| format!("{id}: {why}")).collect();
            if why.is_empty() {
                Err(none_running())
            } else {
                Err(Error::NotConnected(format!("a browser is there but did not answer ({})", why.join("; "))))
            }
        }
        [one] => allowed(dirs, &one.info.id, name).await,
        many => {
            let mut asking: FuturesUnordered<_> = many.iter().map(|l| allowed(dirs, &l.info.id, name)).collect();
            let mut last = None;
            while let Some(answer) = asking.next().await {
                match answer {
                    Ok(client) => return Ok(client),
                    Err(e) => last = Some(e),
                }
            }
            Err(last.unwrap_or_else(none_running))
        }
    }
}

impl RelayBrowser {
    /// Connects to the person's own browser and opens a first tab. With an `id` (from [`browsers`]) that browser is
    /// asked; without one every running browser is asked and the first to be allowed is used. `name` is how this
    /// agent shows in the browser. Waits up to two minutes for the person to click Allow; if they click Deny, or do
    /// not click, this is an error and nothing was controlled.
    pub async fn connect(dirs: &Dirs, id: Option<&str>, name: &str) -> Result<RelayBrowser> {
        let client = match id {
            Some(id) => allowed(dirs, id, name).await?,
            None => any(dirs, name).await?,
        };
        RelayBrowser::start(client).await
    }
}
