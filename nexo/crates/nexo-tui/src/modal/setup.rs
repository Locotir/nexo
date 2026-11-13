//! `/model`: a guided setup. Pick a configured model to switch to it, or a
//! provider to set one up: its address is checked by asking it, the key is
//! asked for only when the server wants one, its models come from the server,
//! and the chosen model is tried with a real request before anything is saved.
//! Every fact the setup found is shown, with where it came from, and can be
//! changed before saving.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_core::agent::AgentCommand;
use nexo_core::provider::{kind_label, normalize_url, LOCAL};
use nexo_core::{ErrorKind, ModelCheck, Pricing, Problem, ProviderConfig, ProviderError, ServerProbe};
use nexo_providers::keys;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{PickItem, PickKind, Picker};
use crate::fuzzy;
use crate::render::blocks::{fmt_k, fmt_price, SPINNER};
use crate::render::wrap::{truncate, wrap_plain};
use crate::theme::Theme;

mod start;
use start::{error_of, Again, Done};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The first screen of someone with no model: a few ways in, one suggested.
    Start,
    Provider,
    /// How to sign in to a subscription provider.
    Login,
    Address,
    Key,
    Model,
    Review,
    /// A key, a server or a test request that did not work: the card of why, and a way on.
    Trouble,
    /// What was saved and where.
    Done,
}

impl Step {
    fn name(self) -> &'static str {
        match self {
            Step::Start => "",
            Step::Trouble => "problem",
            Step::Done => "ready",
            Step::Provider => "providers",
            Step::Login => "sign in",
            Step::Address => "address",
            Step::Key => "key",
            Step::Model => "model",
            Step::Review => "save",
        }
    }
}

/// One editable line of the review.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub label: &'static str,
    pub value: String,
    /// Where the value came from, shown dim after it.
    pub source: String,
}

/// What a key in the setup asks the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum SetupOut {
    Nothing,
    Close,
    /// A command for the agent; the setup waits for its answer when `busy` is set.
    Send(AgentCommand),
    /// Send and close (a switch to a configured model, the final save).
    SendAndClose(AgentCommand),
    /// Text for the clipboard (the full sign-in link).
    Copy(String),
}

#[derive(Debug, Clone)]
pub struct Setup {
    pub step: Step,
    /// The list of the provider and model steps.
    pub list: Picker,
    /// The text being typed at the address and key steps.
    pub field: String,
    /// What the setup waits for, shown with a spinner (`checking http://…`).
    pub busy: Option<String>,
    pub error: Option<String>,
    /// The provider being set up and its configuration so far.
    pub name: String,
    pub cfg: ProviderConfig,
    /// A key typed at the key step (never shown again, kept in a file of its own on save).
    pub key: Option<String>,
    pub probe: Option<ServerProbe>,
    /// The page a sign-in waits on and the code to type there (empty: none).
    pub prompt: Option<(String, String)>,
    /// A sign-in that just worked: shown as one line until the setup moves on.
    pub signed: Option<String>,
    pub model: String,
    pub check: Option<ModelCheck>,
    pub fields: Vec<Field>,
    pub selected: usize,
    /// The reasoning levels the model takes; `effort` indexes it (`None`: the default).
    pub efforts: Vec<String>,
    pub effort: Option<usize>,
    /// Local servers found running: `(preset, what answered)`.
    pub local: Vec<(String, String)>,
    /// The scan for local servers has answered (empty or not): the "looking" line goes away.
    pub scanned: bool,
    /// The folds of the first list, and which of them are open (by key).
    groups: Vec<Group>,
    open: std::collections::HashSet<String>,
    /// The fold each visible row of the first list belongs to (`None`: the row of one's own server).
    owner: Vec<Option<usize>>,
    providers: Vec<(String, ProviderConfig)>,
    models: Vec<(String, String)>,
    current: String,
    /// The long list was reached from the first choices (Esc goes back to them).
    from_start: bool,
    /// The list a provider was picked from; Esc from its screens goes back there.
    origin: Step,
    /// A key pasted before a provider was picked: it waits for the pick.
    pending_key: Option<String>,
    /// The row of the first screen the person came from, so Esc lands on it again.
    came_from: Option<String>,
    /// What went wrong (the card of the `Trouble` step), and what Enter does about it.
    pub problem: Option<Problem>,
    again: Again,
    /// The card or the review shows everything (the raw text, the facts to edit).
    pub details: bool,
    /// What the last screen will say, once the save is confirmed.
    done: Option<Done>,
    /// The check (address probe or test request) that is open now: Esc and every new check change it,
    /// and an answer that carries another number is a late one.
    seq: u64,
}

/// The numbers of checks never repeat in a run, so an answer to a setup that was closed cannot fit a new one.
fn next_seq() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Whether a provider can be used as it is: a login saved, a key found, a local server that
/// answered, or a remote server that asks for nothing. The picker and the home screen share it.
pub fn provider_ready(name: &str, c: &ProviderConfig, local: &[(String, String)]) -> bool {
    if local.iter().any(|(p, _)| p == name) {
        return true;
    }
    // the scripted test provider has no server and no key
    if c.protocol == "mock" {
        return true;
    }
    // a key kept with `nexo auth login` counts too; a command is not run just to ask
    let stored = nexo_core::auth::get(c.auth.as_deref().unwrap_or(name)).is_some();
    if c.login.is_some() {
        return stored;
    }
    if c.api_key_cmd.is_some() || c.api_key_file.is_some() || stored {
        return true;
    }
    match &c.api_key_env {
        Some(v) => std::env::var(v).is_ok_and(|k| !k.trim().is_empty()),
        None => {
            // no key wanted: a server on this machine has to be found running first
            let url = c.base_url.to_ascii_lowercase();
            let host = url.split("://").last().unwrap_or("");
            let on_this_machine = ["localhost", "127.", "[::1]", "0.0.0.0"].iter().any(|h| host.starts_with(h));
            !url.is_empty() && !on_this_machine
        }
    }
}

/// Said when a pasted text the program could not place is let go.
const HELD_DROPPED: &str = "The pasted text was not kept or sent anywhere.";

/// Text typed in a search box that is surely a key: it starts like the key of a known service (the list
/// in `nexo_providers::keys`) and has something after that start. Nothing else is taken for a key, so a
/// model id such as `deepseek-reasoner` is never hidden or kept. From then on it is drawn as dots, so no
/// more than the start is ever drawn, and it is never taken for a model name.
fn looks_like_secret(text: &str) -> bool {
    keys::starts_like_key(text, 1)
}

/// The host of an address: `https://auth.openai.com/oauth?x=1` is `auth.openai.com`.
fn host_of(url: &str) -> &str {
    let rest = url.split("://").last().unwrap_or(url);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

/// A fold of the first list: a header and the rows under it, each with the text the filter reads.
#[derive(Debug, Clone)]
struct Group {
    key: String,
    label: String,
    hint: String,
    rows: Vec<(PickItem, String)>,
}

/// The key of the fold of providers not ready yet.
const OTHERS: &str = "@others";

/// The value of the "set up your own server" row.
const CUSTOM: &str = "new:";

impl Setup {
    pub fn new(providers: Vec<(String, ProviderConfig)>, models: Vec<(String, String)>, current: &str) -> Setup {
        let mut s = Setup {
            step: Step::Provider,
            list: Picker::new(PickKind::Model, "", Vec::new(), None),
            field: String::new(),
            busy: None,
            error: None,
            name: String::new(),
            cfg: ProviderConfig::default(),
            key: None,
            probe: None,
            prompt: None,
            signed: None,
            model: String::new(),
            check: None,
            fields: Vec::new(),
            selected: 0,
            efforts: Vec::new(),
            effort: None,
            local: Vec::new(),
            scanned: false,
            groups: Vec::new(),
            open: current.split_once('/').map(|(p, _)| p.to_string()).into_iter().collect(),
            owner: Vec::new(),
            providers,
            models,
            current: current.to_string(),
            from_start: false,
            origin: Step::Provider,
            pending_key: None,
            came_from: None,
            problem: None,
            again: Again::Probe,
            details: false,
            done: None,
            seq: 0,
        };
        s.fill_providers();
        // nothing to switch to yet: the first run starts with a few clear choices
        if s.models.is_empty() {
            s.fill_start();
        }
        s
    }
}

mod answers;
mod key_input;
mod lists;
mod view;

#[cfg(test)]
mod tests;
