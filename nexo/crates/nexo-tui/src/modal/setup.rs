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

    /// The first list: one fold per provider that is ready (signed in, key found, or a local
    /// server running), then one fold of the providers still to set up, then the row for a
    /// server of one's own. A fold shows its rows only when open (or while filtering).
    fn fill_providers(&mut self) {
        self.signed = None;
        let in_use = self.current.split_once('/').map(|(p, _)| p.to_string());
        let count = |name: &str| self.models.iter().filter(|(p, _)| p == name).count();
        // key and sign-in facts of a provider: (ready, what to say about it, hint of its set-up row)
        let facts = |name: &str, c: &ProviderConfig| {
            let running = self.local.iter().find(|(p, _)| p == name).map(|(_, w)| w.clone());
            // a key kept with `nexo auth login` counts too; a command is not run just to list
            let stored = || nexo_core::auth::get(c.auth.as_deref().unwrap_or(name)).is_some();
            let (key, ok) = match &c.api_key_env {
                _ if c.login.is_some() => {
                    if stored() {
                        ("signed in ✓", true)
                    } else {
                        ("sign in", false)
                    }
                }
                _ if c.api_key_cmd.is_some() || stored() => ("key ✓", true),
                Some(v) if std::env::var(v).is_ok_and(|k| !k.trim().is_empty()) => ("key ✓", true),
                _ => ("", false),
            };
            let need = match &c.api_key_env {
                _ if !key.is_empty() => key.to_string(),
                Some(_) if c.login.is_none() => "needs a key".to_string(),
                _ => String::new(),
            };
            // what the person must do comes first, and no address unless it is all there is
            let hint = match (&running, need.is_empty()) {
                (Some(w), true) => format!("● running · {w}"),
                (Some(w), false) => format!("● running · {w} · {need}"),
                (None, true) if LOCAL.iter().any(|(l, _)| *l == name) => "on this computer".to_string(),
                (None, true) => c.base_url.clone(),
                // the address is for the one who chose to type it: a person is told what to do
                (None, false) => need.clone(),
            };
            let status = if running.is_some() {
                "running"
            } else if c.login.is_some() && ok {
                "signed in"
            } else if ok {
                "key found"
            } else {
                "configured"
            };
            (provider_ready(name, c, &self.local), status, hint)
        };
        // the providers with a model of their own first (the one in use at the top), then the
        // other ready ones, running servers before the rest
        let mut order: Vec<String> = Vec::new();
        for (p, _) in &self.models {
            if !order.contains(p) {
                order.push(p.clone());
            }
        }
        if let Some(pos) = in_use.as_ref().and_then(|u| order.iter().position(|p| p == u)) {
            let p = order.remove(pos);
            order.insert(0, p);
        }
        let mut extra: Vec<(bool, String)> = self
            .providers
            .iter()
            .filter(|(n, c)| !order.contains(n) && facts(n, c).0)
            .map(|(n, _)| (!self.local.iter().any(|(l, _)| l == n), n.clone()))
            .collect();
        extra.sort_by_key(|(not_running, _)| *not_running);
        order.extend(extra.into_iter().map(|(_, n)| n));

        let mut groups: Vec<Group> = Vec::new();
        for name in &order {
            let cfg = self.providers.iter().find(|(n, _)| n == name).map(|(_, c)| c);
            let status = match self.local.iter().find(|(p, _)| p == name) {
                Some((_, w)) => format!("● running · {w}"),
                None => cfg.map_or("configured", |c| facts(name, c).1).to_string(),
            };
            let n = count(name);
            let used = in_use.as_deref() == Some(name.as_str());
            let mut parts: Vec<String> = Vec::new();
            if n > 0 {
                parts.push(format!("{n} model{}", if n == 1 { "" } else { "s" }));
            }
            parts.push(if used { "● in use".to_string() } else { status });
            let mut rows: Vec<(PickItem, String)> = self
                .models
                .iter()
                .filter(|(p, _)| p == name)
                .map(|(p, m)| {
                    let v = format!("{p}/{m}");
                    let hint = if v == self.current { "● in use" } else { "switch" };
                    (PickItem { label: format!("   {m}"), value: format!("use:{v}"), hint: hint.into() }, v)
                })
                .collect();
            if let Some(c) = cfg {
                let again = if c.login.is_some() { "sign in again" } else { "set up again" };
                rows.push((
                    PickItem { label: format!("   {again}"), value: format!("new:{name}"), hint: facts(name, c).2 },
                    format!("set up {name} {again}"),
                ));
            }
            groups.push(Group { key: name.clone(), label: name.clone(), hint: parts.join(" · "), rows });
        }
        let rest: Vec<(PickItem, String)> = self
            .providers
            .iter()
            .filter(|(n, _)| !order.contains(n))
            .map(|(n, c)| {
                (
                    PickItem { label: format!("   set up {n}"), value: format!("new:{n}"), hint: facts(n, c).2 },
                    format!("set up {n}"),
                )
            })
            .collect();
        if !rest.is_empty() {
            let hint = format!("{} provider{}", rest.len(), if rest.len() == 1 { "" } else { "s" });
            groups.push(Group { key: OTHERS.into(), label: "others".into(), hint, rows: rest });
        }
        self.groups = groups;
        let keep = format!("use:{}", self.current);
        self.rebuild(Some(&keep));
    }

    /// The visible rows of the first list from the folds: the open ones show their rows, and
    /// while something is typed every fold with a match is open and shows only what matches.
    fn rebuild(&mut self, keep: Option<&str>) {
        let q = self.list.query.trim().to_string();
        let filtering = !q.is_empty();
        let flat: Vec<(usize, usize)> =
            self.groups.iter().enumerate().flat_map(|(g, grp)| (0..grp.rows.len()).map(move |r| (g, r))).collect();
        let searches: Vec<String> = flat.iter().map(|&(g, r)| self.groups[g].rows[r].1.clone()).collect();
        let order = fuzzy::rank(&q, &searches);
        let mut items: Vec<PickItem> = Vec::new();
        let mut owner: Vec<Option<usize>> = Vec::new();
        for (g, grp) in self.groups.iter().enumerate() {
            let kids: Vec<usize> = order.iter().filter(|&&f| flat[f].0 == g).map(|&f| flat[f].1).collect();
            if filtering && kids.is_empty() {
                continue;
            }
            let open = filtering || self.open.contains(&grp.key);
            items.push(PickItem {
                label: format!("{} {}", if open { "▾" } else { "▸" }, grp.label),
                value: format!("group:{}", grp.key),
                hint: grp.hint.clone(),
            });
            owner.push(Some(g));
            if open {
                for r in kids {
                    items.push(grp.rows[r].0.clone());
                    owner.push(Some(g));
                }
            }
        }
        let own = PickItem { label: "set up your own server".into(), value: CUSTOM.into(), hint: "own server".into() };
        if !filtering || !fuzzy::rank(&q, &[format!("{} {}", own.label, own.hint)]).is_empty() {
            items.push(own);
            owner.push(None);
        }
        // typing lands on the best match, so Enter picks it; else on the row asked to keep
        let want = if filtering {
            order.first().map(|&f| self.groups[flat[f].0].rows[flat[f].1].0.value.clone())
        } else {
            keep.map(str::to_string)
        };
        self.list.selected = want.and_then(|v| items.iter().position(|i| i.value == v)).unwrap_or(0);
        self.list.ranked = (0..items.len()).collect();
        self.list.items = items;
        self.owner = owner;
    }

    /// Opens or closes the fold of the row under the cursor (the fold a model or set-up row is in).
    fn fold(&mut self, open: bool) {
        if self.step != Step::Provider || !self.list.query.trim().is_empty() {
            return;
        }
        let Some(g) = self.owner.get(self.list.selected).copied().flatten() else { return };
        let on_header = self.list.current().is_some_and(|i| i.value.starts_with("group:"));
        let key = self.groups[g].key.clone();
        if open {
            if !on_header {
                return;
            }
            self.open.insert(key.clone());
        } else {
            self.open.remove(&key);
        }
        self.rebuild(Some(&format!("group:{key}")));
    }

    /// The first list is rebuilt from its folds; the others are filtered as they are.
    fn refilter(&mut self) {
        if self.step == Step::Provider {
            self.rebuild(None);
        } else {
            self.list.refilter();
        }
    }

    /// How many rows the first list would have with every fold open: the room the dialog asks
    /// for, so what is opened later is not cut.
    pub fn rows_if_open(&self) -> usize {
        self.groups.iter().map(|g| 1 + g.rows.len()).sum::<usize>() + 1
    }

    /// The local servers found running; they become folds of their own.
    pub fn found_local(&mut self, found: Vec<(String, String)>) {
        self.local = found;
        self.scanned = true;
        if self.step == Step::Start {
            self.fill_start();
        } else if self.step == Step::Provider {
            let sel = self.list.current().map(|i| i.value.clone());
            self.fill_providers();
            if let Some(v) = sel {
                if let Some(pos) = self.list.items.iter().position(|i| i.value == v) {
                    self.list.selected = pos;
                }
            }
        }
    }

    fn probe_cmd(&mut self) -> SetupOut {
        self.error = None;
        self.busy = Some(self.wait_text());
        self.seq = next_seq();
        SetupOut::Send(AgentCommand::Probe {
            name: self.name.clone(),
            cfg: self.cfg.clone(),
            key: self.key.clone(),
            seq: self.seq,
        })
    }

    /// The answer to a probe, applied only when it belongs to the probe that is open now.
    pub fn probed_from(&mut self, name: &str, seq: u64, result: Result<ServerProbe, String>) {
        if name == self.name && seq == self.seq && self.step != Step::Done {
            self.probed(result);
        }
    }

    /// The answer to a check of a model, applied only when it belongs to the check that is open now.
    pub fn checked_from(&mut self, name: &str, model: &str, seq: u64, check: ModelCheck) {
        if name == self.name && model == self.model && seq == self.seq {
            self.checked(check);
        }
    }

    /// The list of the sign-in step: the saved login (when there is one), then the ways to sign in.
    fn fill_login(&mut self) {
        self.signed = None;
        let flow = self.cfg.login.clone().unwrap_or_default();
        let mut items = Vec::new();
        if nexo_core::auth::get(self.cfg.auth.as_deref().unwrap_or(&self.name)).is_some() {
            items.push(PickItem {
                label: "Keep the saved login".into(),
                value: "keep".into(),
                hint: "already signed in on this machine".into(),
            });
        }
        items.extend(nexo_providers::login::choices(&flow).into_iter().map(|c| PickItem {
            label: c.label,
            value: c.flow,
            hint: c.hint,
        }));
        self.list = Picker::new(PickKind::Model, "", items, None);
        self.prompt = None;
        self.step = Step::Login;
    }

    /// The page a sign-in waits on arrived.
    pub fn login_prompt(&mut self, url: String, code: String) {
        if self.step == Step::Login && self.busy.is_some() {
            self.prompt = Some((url, code));
        }
    }

    /// A sign-in ended: on success the provider is asked what it serves.
    pub fn logged_in(&mut self, result: Result<(), String>) -> SetupOut {
        if self.step != Step::Login || self.busy.is_none() {
            return SetupOut::Nothing;
        }
        self.busy = None;
        self.prompt = None;
        match result {
            Ok(()) => {
                self.signed = Some(format!("signed in to {}", self.name));
                self.step = Step::Address;
                self.field = self.cfg.base_url.clone();
                self.probe_cmd()
            }
            Err(e) => {
                self.error = Some(e);
                SetupOut::Nothing
            }
        }
    }

    /// The answer to a probe of the address.
    pub fn probed(&mut self, result: Result<ServerProbe, String>) {
        if self.busy.is_none() {
            return;
        }
        self.busy = None;
        match result {
            Err(e) if self.cfg.login.is_some() => {
                self.fill_login();
                self.error = Some(e);
            }
            Err(e) => {
                let again = if self.step == Step::Address { Again::Address } else { Again::Probe };
                self.trouble(ProviderError::new(ErrorKind::Network, e), again);
            }
            Ok(p) => {
                self.cfg.protocol = p.protocol.clone();
                self.cfg.base_url = p.base_url.clone();
                if p.needs_key && p.models.is_empty() {
                    let refused = self.has_key();
                    self.probe = Some(p);
                    if refused {
                        // the key that was sent did not open it: nothing is kept
                        let e = ProviderError::new(ErrorKind::Auth, "the key was refused").with_status(401);
                        self.trouble(e, Again::Key);
                    } else {
                        self.field.clear();
                        self.step = Step::Key;
                    }
                    return;
                }
                let items: Vec<PickItem> = p
                    .models
                    .iter()
                    .map(|m| {
                        let mut hint = Vec::new();
                        if let Some(n) = m.context_window {
                            hint.push(format!("{} ctx", fmt_k(n as u64)));
                        }
                        if self.cfg.login.is_some() {
                            // a subscription: there is no per-token price to compare
                            hint.push("plan".into());
                        } else if let Some(pr) = m.pricing {
                            hint.push(format!("${} / ${}", fmt_price(pr.input), fmt_price(pr.output)));
                        }
                        if m.reasoning {
                            hint.push("reasons".into());
                        }
                        PickItem { label: m.id.clone(), value: m.id.clone(), hint: hint.join(" · ") }
                    })
                    .collect();
                let preferred = self.cfg.default_model.clone();
                self.list = Picker::new(PickKind::Model, "", items, preferred.as_deref());
                self.step = Step::Model;
                self.probe = Some(p);
            }
        }
    }

    /// The answer to the check of a model: the review, prefilled.
    pub fn checked(&mut self, check: ModelCheck) {
        if self.busy.is_none() {
            return;
        }
        self.busy = None;
        let i = &check.info;
        // the last word on a fact is the one that set it
        let src = |what: &str| {
            check
                .sources
                .iter()
                .rev()
                .find(|s| s.contains(what))
                .map(|s| s.split(" from ").nth(1).unwrap_or("").to_string())
                .unwrap_or_default()
        };
        let num = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
        let price = |f: fn(&Pricing) -> f64| i.pricing.map(|p| fmt_price(f(&p))).unwrap_or_default();
        let unknown = |v: &str, s: String| if v.is_empty() { "unknown: type it if you know it".to_string() } else { s };
        let ctx = num(i.context_window);
        let out = num(i.max_output);
        let (pi, po, pc) = (price(|p| p.input), price(|p| p.output), price(|p| p.cache_read));
        self.fields = vec![
            Field { label: "name", value: self.name.clone(), source: "how nexo calls this provider".into() },
            Field { label: "context window", source: unknown(&ctx, src("context")), value: ctx },
            Field { label: "largest reply", source: unknown(&out, src("largest reply")), value: out },
            Field { label: "price in", source: unknown(&pi, src("prices")), value: pi },
            Field { label: "price out", source: "per million tokens".into(), value: po },
            Field { label: "price cached", source: "per million tokens".into(), value: pc },
        ];
        self.efforts = i.efforts.clone();
        self.effort = None;
        self.selected = 0;
        self.details = false;
        let failed = check.error.clone();
        self.check = Some(check);
        self.step = Step::Review;
        // the request failed: why, in plain words; the facts stay behind it for `S`
        if let Some(e) = failed {
            self.trouble(error_of(&e), Again::Check);
        }
    }

    /// The configuration the review describes, ready to save.
    pub fn result(&self) -> (String, ProviderConfig) {
        let mut cfg = self.cfg.clone();
        let get = |label: &str| {
            self.fields.iter().find(|f| f.label == label).map(|f| f.value.trim().to_string()).unwrap_or_default()
        };
        let name = Some(get("name")).filter(|n| !n.is_empty()).unwrap_or_else(|| self.name.clone());
        let m = self.model.clone();
        if let Ok(n) = get("context window").replace('_', "").parse::<u32>() {
            cfg.context_window.insert(m.clone(), n);
        }
        if let Ok(n) = get("largest reply").replace('_', "").parse::<u32>() {
            cfg.max_output.insert(m.clone(), n);
        }
        let f = |label: &str| get(label).trim_start_matches('$').parse::<f64>().ok();
        if let (Some(input), Some(output)) = (f("price in"), f("price out")) {
            cfg.pricing.insert(
                m.clone(),
                Pricing { input, output, cache_read: f("price cached").unwrap_or(0.0), cache_write: 0.0 },
            );
        }
        if !self.efforts.is_empty() {
            cfg.efforts.insert(m.clone(), self.efforts.clone());
        }
        if let Some(e) = self.effort.and_then(|i| self.efforts.get(i)) {
            cfg.effort.insert(m.clone(), e.clone());
        }
        (name, cfg)
    }

    /// Back one step; `Close` from the first.
    fn back(&mut self) -> SetupOut {
        self.error = None;
        if self.busy.take().is_some() {
            // a sign-in waiting for the browser is dropped
            if self.step == Step::Login {
                self.prompt = None;
                return SetupOut::Send(AgentCommand::CancelLogin);
            }
            // the running check is stopped, and whatever it answers later belongs to no one
            self.seq = next_seq();
            return SetupOut::Send(AgentCommand::CancelCheck);
        }
        match self.step {
            // text typed (hidden) on the first screen goes first; a second Esc closes
            Step::Start if !self.field.is_empty() => self.field.clear(),
            Step::Start | Step::Done => return SetupOut::Close,
            // a pasted text that was not taken for a key is dropped, and the person is told
            Step::Provider if self.pending_key.take().is_some() => {
                self.fill_providers();
                if self.from_start {
                    self.fill_start();
                }
                self.error = Some(HELD_DROPPED.into());
            }
            Step::Provider if self.from_start => self.fill_start(),
            Step::Provider => return SetupOut::Close,
            Step::Login | Step::Address | Step::Key => self.go_origin(),
            Step::Trouble if self.again == Again::Check => {
                self.problem = None;
                self.step = Step::Model;
            }
            Step::Trouble => self.go_origin(),
            Step::Model if self.cfg.login.is_some() => self.fill_login(),
            // the key was typed on the step before: it is asked for again, the old one is dropped
            Step::Model if self.key.is_some() && self.cfg.api_key_env.is_some() => {
                self.key = None;
                self.field.clear();
                self.step = Step::Key;
            }
            Step::Model => self.go_origin(),
            Step::Review => {
                self.check = None;
                self.step = Step::Model;
            }
        }
        SetupOut::Nothing
    }

    pub fn key(&mut self, key: KeyEvent) -> SetupOut {
        if key.code == KeyCode::Esc {
            return self.back();
        }
        // while the browser is waited for, `c` copies the whole link
        if self.step == Step::Login && key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::NONE {
            if let Some((url, _)) = &self.prompt {
                return SetupOut::Copy(url.clone());
            }
        }
        if self.busy.is_some() {
            return SetupOut::Nothing;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match self.step {
            Step::Start => self.start_key(key),
            Step::Trouble => self.trouble_key(key.code),
            Step::Done => match key.code {
                KeyCode::Enter => SetupOut::Close,
                _ => SetupOut::Nothing,
            },
            Step::Provider | Step::Model | Step::Login => match key.code {
                KeyCode::Enter | KeyCode::Tab => self.pick(),
                KeyCode::Up => {
                    self.move_list(-1);
                    SetupOut::Nothing
                }
                KeyCode::Down => {
                    self.move_list(1);
                    SetupOut::Nothing
                }
                KeyCode::PageUp => {
                    self.move_list_clamped(-8);
                    SetupOut::Nothing
                }
                KeyCode::PageDown => {
                    self.move_list_clamped(8);
                    SetupOut::Nothing
                }
                KeyCode::Home => {
                    self.list.selected = 0;
                    SetupOut::Nothing
                }
                KeyCode::End => {
                    self.list.selected = self.list.ranked.len().saturating_sub(1);
                    SetupOut::Nothing
                }
                // the folds of the first list
                KeyCode::Right if self.step == Step::Provider => {
                    self.fold(true);
                    SetupOut::Nothing
                }
                KeyCode::Left if self.step == Step::Provider => {
                    self.fold(false);
                    SetupOut::Nothing
                }
                KeyCode::Char(' ')
                    if self.step == Step::Provider
                        && self.list.query.is_empty()
                        && self.list.current().is_some_and(|i| i.value.starts_with("group:")) =>
                {
                    self.toggle_current();
                    SetupOut::Nothing
                }
                KeyCode::Backspace => {
                    self.list.query.pop();
                    self.refilter();
                    SetupOut::Nothing
                }
                KeyCode::Char('u') if ctrl => {
                    self.list.query.clear();
                    self.refilter();
                    SetupOut::Nothing
                }
                KeyCode::Char(c) if !ctrl => {
                    self.list.query.push(c);
                    self.refilter();
                    SetupOut::Nothing
                }
                _ => SetupOut::Nothing,
            },
            Step::Address | Step::Key => match key.code {
                KeyCode::Enter => self.submit_field(),
                // the page where the key is made
                KeyCode::Char('o') if ctrl && self.step == Step::Key => {
                    if let Some(url) = keys::page(&self.name) {
                        crate::links::open(url);
                    }
                    SetupOut::Nothing
                }
                KeyCode::Backspace => {
                    self.field.pop();
                    SetupOut::Nothing
                }
                KeyCode::Char('u') if ctrl => {
                    self.field.clear();
                    SetupOut::Nothing
                }
                KeyCode::Char(c) if !ctrl => {
                    self.field.push(c);
                    SetupOut::Nothing
                }
                _ => SetupOut::Nothing,
            },
            Step::Review => self.review_key(key),
        }
    }

    /// Typed text pasted in one go (a key, an address, a search).
    pub fn paste(&mut self, text: &str) -> SetupOut {
        // escapes and control characters out; a key wrapped over lines is joined back; spaces and quotes trimmed
        let text = crate::render::wrap::clean(&text.replace('\r', "\n")).replace('\n', "");
        let text = keys::clean(&text);
        match self.step {
            // a key pasted before any provider was picked: the service is read from its start
            Step::Start | Step::Provider if keys::looks_like_key(&text) && self.busy.is_none() => {
                return self.key_pasted(text)
            }
            Step::Start => {
                self.error = None;
                self.field.push_str(&text);
            }
            Step::Address | Step::Key => self.field.push_str(&text),
            Step::Provider | Step::Model | Step::Login => {
                self.list.query.push_str(&text);
                self.refilter();
            }
            Step::Review => {
                if let Some(f) = self.fields.get_mut(self.selected) {
                    f.value.push_str(&text);
                }
            }
            Step::Trouble | Step::Done => {}
        }
        SetupOut::Nothing
    }

    fn move_list(&mut self, d: i32) {
        let n = self.list.ranked.len() as i32;
        if n > 0 {
            self.list.selected = (self.list.selected as i32 + d).rem_euclid(n) as usize;
        }
    }

    /// A page up or down: stops at the ends instead of wrapping round.
    fn move_list_clamped(&mut self, d: i32) {
        let last = self.list.ranked.len().saturating_sub(1) as i32;
        self.list.selected = (self.list.selected as i32 + d).clamp(0, last) as usize;
    }

    /// Opens a closed fold under the cursor, closes an open one.
    fn toggle_current(&mut self) {
        let Some(g) = self.owner.get(self.list.selected).copied().flatten() else { return };
        let key = self.groups[g].key.clone();
        self.fold(!self.open.contains(&key));
    }

    fn pick(&mut self) -> SetupOut {
        match self.step {
            Step::Provider => {
                // a key typed key by key (no bracketed paste) is a key, taken as a paste is
                if keys::starts_like_key(&self.list.query, 1) && keys::looks_like_key(&self.list.query) {
                    let typed = std::mem::take(&mut self.list.query);
                    self.refilter();
                    return self.key_pasted(keys::clean(&typed));
                }
                let Some(it) = self.list.current().cloned() else { return SetupOut::Nothing };
                // a header opens or closes (while filtering every fold with a match is open)
                if it.value.starts_with("group:") {
                    self.toggle_current();
                    return SetupOut::Nothing;
                }
                if let Some(pm) = it.value.strip_prefix("use:") {
                    // a model already set up needs no key: a pasted text held is let go, and the person
                    // is told before anything is sent (the wizard stays; Enter again switches)
                    if self.pending_key.take().is_some() {
                        self.error = Some(HELD_DROPPED.into());
                        return SetupOut::Nothing;
                    }
                    let (provider, model) = match pm.split_once('/') {
                        Some((p, m)) => (Some(p.to_string()), m.to_string()),
                        None => (None, pm.to_string()),
                    };
                    return SetupOut::SendAndClose(AgentCommand::SetModel { provider, model });
                }
                self.choose(&it.value)
            }
            Step::Start => {
                let Some(it) = self.list.current().cloned() else { return SetupOut::Nothing };
                if it.value == "more" {
                    self.came_from = Some(it.value.clone());
                    self.show_providers();
                    return SetupOut::Nothing;
                }
                self.choose(&it.value)
            }
            Step::Login => {
                let Some(it) = self.list.current().cloned() else { return SetupOut::Nothing };
                self.error = None;
                if it.value == "keep" {
                    self.step = Step::Address;
                    self.field = self.cfg.base_url.clone();
                    return self.probe_cmd();
                }
                self.busy = Some("waiting for the browser".to_string());
                if it.value == "codex-cli" {
                    self.busy = Some("reading the Codex CLI login".into());
                }
                SetupOut::Send(AgentCommand::Login { name: self.name.clone(), flow: it.value })
            }
            Step::Model => {
                let typed = self.list.query.trim().to_string();
                // a key typed here is not a model name: it is not sent as one
                if looks_like_secret(&typed) {
                    self.list.query.clear();
                    self.refilter();
                    self.error = Some("That looks like a key, not a model name. It was not kept.".into());
                    return SetupOut::Nothing;
                }
                let model = match self.list.current() {
                    Some(it) => it.value.clone(),
                    None if !typed.is_empty() => typed,
                    None => return SetupOut::Nothing,
                };
                self.model = model.clone();
                self.error = None;
                let Some(probe) = self.probe.clone() else { return SetupOut::Nothing };
                self.busy = Some(format!("testing {model}"));
                self.seq = next_seq();
                SetupOut::Send(AgentCommand::CheckModel {
                    name: self.name.clone(),
                    probe,
                    cfg: self.cfg.clone(),
                    model,
                    key: self.key.clone(),
                    seq: self.seq,
                })
            }
            _ => SetupOut::Nothing,
        }
    }

    fn submit_field(&mut self) -> SetupOut {
        let text = keys::clean(&self.field);
        match self.step {
            Step::Address => {
                if text.is_empty() {
                    self.error = Some("type its address, e.g. localhost:11434".into());
                    return SetupOut::Nothing;
                }
                self.cfg.base_url = normalize_url(&text);
                self.probe_cmd()
            }
            Step::Key => {
                if let Some(var) = text.strip_prefix('$') {
                    if !std::env::var(var).is_ok_and(|k| !k.trim().is_empty()) {
                        self.error = Some(format!("${var} is not set in this shell"));
                        return SetupOut::Nothing;
                    }
                    self.cfg.api_key_env = Some(var.to_string());
                    self.cfg.api_key_cmd = None;
                    self.key = None;
                } else if let Some(cmd) = text.strip_prefix('!') {
                    self.cfg.api_key_cmd = Some(cmd.trim().to_string());
                    self.key = None;
                } else if !text.is_empty() {
                    self.key = Some(text);
                } else if self.cfg.resolve_key().is_none() {
                    self.error = Some("paste the key first".into());
                    return SetupOut::Nothing;
                }
                self.field.clear();
                self.probe_cmd()
            }
            _ => SetupOut::Nothing,
        }
    }

    fn review_key(&mut self, key: KeyEvent) -> SetupOut {
        let effort_row = self.fields.len();
        let rows = self.fields.len() + usize::from(!self.efforts.is_empty());
        match key.code {
            KeyCode::Enter => {
                let (name, cfg) = self.result();
                // the setup stays open on the last screen until the save is confirmed
                self.expect_done(&name);
                SetupOut::Send(AgentCommand::SaveModel { name, cfg, model: self.model.clone(), key: self.key.clone() })
            }
            // the facts the test found, to change before saving
            KeyCode::Tab if !self.details => {
                self.details = true;
                SetupOut::Nothing
            }
            _ if !self.details => SetupOut::Nothing,
            KeyCode::Up | KeyCode::BackTab => {
                self.selected = (self.selected + rows - 1) % rows.max(1);
                SetupOut::Nothing
            }
            KeyCode::Down | KeyCode::Tab => {
                self.selected = (self.selected + 1) % rows.max(1);
                SetupOut::Nothing
            }
            KeyCode::Left | KeyCode::Right if self.selected == effort_row => {
                // the default, then each level
                let n = self.efforts.len() + 1;
                let at = self.effort.map(|i| i + 1).unwrap_or(0);
                let next = if key.code == KeyCode::Right { (at + 1) % n } else { (at + n - 1) % n };
                self.effort = next.checked_sub(1);
                SetupOut::Nothing
            }
            KeyCode::Backspace => {
                if let Some(f) = self.fields.get_mut(self.selected) {
                    f.value.pop();
                }
                SetupOut::Nothing
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(f) = self.fields.get_mut(self.selected) {
                    f.value.clear();
                }
                SetupOut::Nothing
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(f) = self.fields.get_mut(self.selected) {
                    f.value.push(c);
                }
                SetupOut::Nothing
            }
            _ => SetupOut::Nothing,
        }
    }

    pub fn title(&self) -> String {
        match self.step.name() {
            "" => " set up a model ".to_string(),
            n => format!(" set up a model · {n} "),
        }
    }

    /// The row of the text being typed, if the step has one: it stays in view when the body scrolls.
    pub fn prompt_row(lines: &[Line<'_>]) -> Option<usize> {
        lines.iter().position(|l| l.spans.first().is_some_and(|s| s.content == "› "))
    }

    /// The row of the field being edited at the review step (the one drawn bold, after a space).
    pub fn selected_row(lines: &[Line<'_>]) -> Option<usize> {
        lines.iter().position(|l| {
            l.spans.first().is_some_and(|s| s.content.starts_with(' ') && s.style.add_modifier.contains(Modifier::BOLD))
        })
    }

    pub fn hints(&self) -> &'static str {
        if self.busy.is_some() && self.step != Step::Login {
            return " Esc cancel";
        }
        match self.step {
            Step::Start => " ↑↓ choose · Enter pick · paste a key · Esc close",
            Step::Provider if self.from_start => " type to search · ↑↓ choose · Enter pick · Esc back to the choices",
            Step::Provider => " type to filter · ↑↓ choose · ←→ fold · Enter open or pick · Esc close",
            Step::Login if self.busy.is_some() => " finish in the browser · Esc cancel",
            Step::Login => " ↑↓ choose · Enter sign in · Esc back",
            Step::Model => " type to filter (or a name not listed) · ↑↓ choose · Enter test it · Esc back",
            Step::Address => " type · Enter check · Ctrl+U clear · Esc back",
            Step::Key if keys::page(&self.name).is_some() => {
                " paste the key · Enter test it · Ctrl+O open the page · Esc back"
            }
            Step::Key => " paste the key · Enter test it · Esc back",
            Step::Review if !self.details => " Enter save and use · Tab change details · Esc back",
            Step::Review => " ↑↓ field · type to change · ←→ effort · Enter save and use · Esc back",
            Step::Trouble => match self.again {
                Again::Key => " Enter paste the key again · Tab details · Esc pick another provider",
                Again::Address => " Enter edit the address · Tab details · Esc pick another provider",
                Again::Probe => " Enter try again · Tab details · Esc pick another provider",
                Again::Check => " Enter try again · S save anyway · Tab details · Esc pick another model",
            },
            Step::Done => " Enter or Esc to start",
        }
    }

    /// The body: the lines above the list, and the list itself for the steps that have one.
    /// Returns the lines and the index of the first list row.
    pub fn lines(&self, w: usize, theme: &Theme, spinner: usize, short: bool) -> (Vec<Line<'static>>, Option<usize>) {
        let dim = theme.dim();
        let mut lines: Vec<Line<'static>> = Vec::new();
        let prompt = |text: String, masked: bool| {
            let shown = if masked { keys::mask(&text) } else { text };
            Line::from(vec![
                Span::styled("› ", theme.style(theme.accent)),
                Span::raw(shown),
                Span::styled("▏", theme.dim()),
            ])
        };
        let say = |lines: &mut Vec<Line<'static>>, text: &str, style: Style| {
            lines.extend(wrap_plain(text, w, style, &[]));
        };
        match self.step {
            Step::Start => {
                lines.push(self.head_line(
                    "nexo needs a model to talk to. Choose where to get one.",
                    w,
                    theme,
                    spinner,
                ));
                if !self.scanned && self.busy.is_none() && self.error.is_none() {
                    say(&mut lines, "Looking for local models…", dim);
                }
                if !self.field.is_empty() {
                    lines.push(prompt(self.field.clone(), true));
                }
            }
            // on a short screen the list comes first: the intro is dropped
            Step::Provider if short && self.busy.is_none() && self.error.is_none() => {}
            Step::Provider => {
                let ask = match (self.pending_key.is_some(), self.from_start) {
                    (true, _) => "Which service is this key from? Type to search.",
                    (false, true) => "Choose a provider. Type to search.",
                    (false, false) => "Pick a model, or set up a provider. Type to search.",
                };
                lines.push(self.head_line(ask, w, theme, spinner));
                if !self.scanned && !self.from_start && self.busy.is_none() && self.error.is_none() {
                    say(&mut lines, "Looking for local models…", dim);
                }
            }
            Step::Login => {
                lines.push(Line::from(vec![
                    Span::styled(format!("{} ", self.name), theme.bold(theme.accent)),
                    Span::styled("sign in with your subscription", dim),
                ]));
                if let Some(cost) = self.cost_line() {
                    say(&mut lines, cost, dim);
                }
                if let Some((url, code)) = &self.prompt {
                    lines.push(Line::default());
                    // the host only; the whole link is long, and `c` copies it
                    lines.push(Line::from(vec![
                        Span::styled("opened ", dim),
                        Span::styled(host_of(url).to_string(), theme.style(theme.accent)),
                        Span::styled(" in your browser · c copies the link", dim),
                    ]));
                    if !code.is_empty() {
                        let spaced = code.chars().map(String::from).collect::<Vec<_>>().join(" ");
                        lines.push(Line::default());
                        lines.push(Line::from(vec![
                            Span::styled("type this code  ", dim),
                            Span::styled(spaced, theme.bold(theme.accent)),
                        ]));
                    }
                }
            }
            Step::Address => {
                lines.push(Line::from(vec![
                    Span::styled(format!("{} ", self.name), theme.bold(theme.accent)),
                    Span::styled("address of the server", dim),
                ]));
                if let Some(s) = &self.signed {
                    lines.push(Line::from(Span::styled(format!("✓ {s}"), theme.style(theme.success))));
                }
                // the reason it failed takes the place of the example, so the dialog does not grow
                match &self.error {
                    Some(e) => lines.push(Line::from(Span::styled(
                        truncate(&format!("✗ {e}"), w.max(8)),
                        theme.style(theme.error),
                    ))),
                    None => say(&mut lines, "e.g. localhost:11434 (Ollama) or localhost:8000/v1 (vLLM)", dim),
                }
                lines.push(prompt(self.field.clone(), false));
            }
            Step::Key => lines.extend(self.key_lines(w, theme)),
            Step::Trouble => lines.extend(self.trouble_lines(w, theme)),
            Step::Done => lines.extend(self.done_lines(w, theme)),
            Step::Model => {
                if let Some(s) = &self.signed {
                    lines.push(Line::from(Span::styled(format!("✓ {s}"), theme.style(theme.success))));
                }
                if let Some(p) = &self.probe {
                    let n = p.models.len();
                    let on_this_computer = LOCAL.iter().any(|(l, _)| *l == self.name);
                    let mut head = if on_this_computer {
                        let mut h = format!("✓ {}", kind_label(&p.kind));
                        if let Some(v) = &p.version {
                            h.push_str(&format!(" {v}"));
                        }
                        h.push_str(" on this computer");
                        h
                    } else if self.name == "custom" {
                        format!("✓ {} at {}", kind_label(&p.kind), p.base_url)
                    } else if self.has_key() && self.cfg.login.is_none() && p.key_proved {
                        "✓ The key works".to_string()
                    } else if self.has_key() && self.cfg.login.is_none() {
                        // a list anyone can read says nothing of the key: the test request will
                        format!("✓ Models found at {}", self.name)
                    } else {
                        format!("✓ Connected to {}", self.name)
                    };
                    head.push_str(&match n {
                        0 => String::new(),
                        1 => " · 1 model".to_string(),
                        n => format!(" · {n} models"),
                    });
                    lines.push(Line::from(Span::styled(truncate(&head, w.max(8)), theme.style(theme.success))));
                    if n == 0 {
                        lines.extend(self.empty_note(w, theme));
                    } else if let Some(about) = self.model_line() {
                        say(&mut lines, &about, dim);
                    }
                }
                lines.push(prompt(self.list.query.clone(), looks_like_secret(&self.list.query)));
            }
            Step::Review if !self.details => {
                lines.push(Line::from(vec![Span::styled(
                    format!("{}/{}", self.name, self.model),
                    theme.bold(theme.accent),
                )]));
                if let Some(c) = &self.check {
                    let ms = c.latency_ms.map(crate::render::blocks::fmt_ms).unwrap_or_default();
                    lines.push(Line::from(Span::styled(
                        format!("✓ Works: {} answered in {ms}", self.model),
                        theme.style(theme.success),
                    )));
                    if !c.called_tool {
                        say(
                            &mut lines,
                            "It did not use the test tool, so tools may not work.",
                            theme.style(theme.warn),
                        );
                    }
                }
            }
            Step::Review => {
                lines.push(Line::from(vec![Span::styled(
                    format!("{}/{}", self.name, self.model),
                    theme.bold(theme.accent),
                )]));
                if let Some(c) = &self.check {
                    match (&c.error, c.latency_ms) {
                        (Some(e), _) => {
                            say(&mut lines, &format!("✗ the test request failed: {e}"), theme.style(theme.error))
                        }
                        (None, Some(ms)) => {
                            let mut s = format!("✓ answered in {}", crate::render::blocks::fmt_ms(ms));
                            s.push_str(if c.called_tool {
                                " · calls tools ✓"
                            } else {
                                " · did not call the test tool: tools may not work"
                            });
                            if c.reasoned {
                                s.push_str(" · reasons");
                            }
                            let style =
                                if c.called_tool { theme.style(theme.success) } else { theme.style(theme.warn) };
                            say(&mut lines, &s, style);
                        }
                        _ => {}
                    }
                    for s in &c.sources {
                        say(&mut lines, &format!("  {s}"), dim);
                    }
                }
                lines.push(Line::default());
                for (i, f) in self.fields.iter().enumerate() {
                    lines.push(self.row(i, f.label, &f.value, &f.source, w, theme));
                }
                if !self.efforts.is_empty() {
                    let v = match self.effort {
                        Some(i) => format!("‹ {} ›", self.efforts[i]),
                        None => "‹ default ›".to_string(),
                    };
                    let src = format!("takes {}", self.efforts.join(", "));
                    lines.push(self.row(self.fields.len(), "effort", &v, &src, w, theme));
                }
            }
        }
        let in_head = matches!(self.step, Step::Start | Step::Provider);
        if let (Some(b), false) = (&self.busy, in_head) {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                format!("{} {b}…", SPINNER[spinner % SPINNER.len()]),
                theme.style(theme.accent),
            )));
        }
        let own_line = in_head || matches!(self.step, Step::Address | Step::Key);
        if let (Some(e), false) = (&self.error, own_line) {
            lines.push(Line::default());
            if self.step == Step::Login {
                // the sign-in failed: one line for the reason, then what the keys do
                let one = e.lines().next().unwrap_or("");
                lines.push(Line::from(Span::styled(truncate(&format!("✗ {one}"), w.max(8)), theme.style(theme.error))));
                lines.push(Line::from(Span::styled("Enter retry · Esc back", dim)));
            } else {
                say(&mut lines, &format!("✗ {e}"), theme.style(theme.error));
            }
        }
        // a server with no models yet has said so above: no "no match" under an empty prompt, no list to follow
        let nothing_listed = self.step == Step::Model && self.list.items.is_empty() && self.list.query.is_empty();
        let list_at = (matches!(self.step, Step::Start | Step::Provider | Step::Model | Step::Login)
            && !nothing_listed)
            .then(|| {
                lines.push(Line::default());
                lines.len()
            });
        if list_at.is_some() {
            if self.list.ranked.is_empty() {
                let none = if self.step == Step::Model && !self.list.query.trim().is_empty() {
                    "no match · Enter try it as typed"
                } else {
                    "no match"
                };
                lines.push(Line::from(Span::styled(none, dim)));
            }
            for (i, &idx) in self.list.ranked.iter().enumerate() {
                let it = &self.list.items[idx];
                let label = truncate(&it.label, w.saturating_sub(2));
                let style = if i == self.list.selected {
                    theme.selection().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                // a header and the row of one's own server: the name, then its facts after it
                if self.step == Step::Provider && (it.value.starts_with("group:") || it.value == CUSTOM) {
                    let used = crate::render::wrap::width(&label) + 5;
                    let hint = truncate(&it.hint, w.saturating_sub(used));
                    let pad = w.saturating_sub(used + crate::render::wrap::width(&hint));
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {label}"), style.patch(theme.bold(theme.accent)).patch(style)),
                        Span::styled(format!("  · {hint}"), style.patch(theme.dim())),
                        Span::styled(" ".repeat(pad), style),
                    ]));
                    continue;
                }
                let hint = truncate(&it.hint, w.saturating_sub(crate::render::wrap::width(&label) + 4));
                let pad = w.saturating_sub(crate::render::wrap::width(&label) + crate::render::wrap::width(&hint) + 2);
                lines.push(Line::from(vec![
                    Span::styled(format!(" {label}"), style),
                    Span::styled(" ".repeat(pad), style),
                    Span::styled(hint, style.patch(theme.dim())),
                    Span::styled(" ", style),
                ]));
            }
        }
        (lines, list_at)
    }

    fn row(&self, i: usize, label: &str, value: &str, source: &str, w: usize, theme: &Theme) -> Line<'static> {
        let sel = i == self.selected;
        let style = if sel { theme.selection().add_modifier(Modifier::BOLD) } else { Style::default() };
        let cursor = if sel { "▏" } else { "" };
        let left = format!(" {label:<15}{value}{cursor}");
        let src = truncate(source, w.saturating_sub(crate::render::wrap::width(&left) + 3));
        let pad = w.saturating_sub(crate::render::wrap::width(&left) + crate::render::wrap::width(&src) + 1);
        Line::from(vec![
            Span::styled(left, style),
            Span::styled(" ".repeat(pad), style),
            Span::styled(src, style.patch(theme.dim())),
            Span::styled(" ", style),
        ])
    }

    /// The list row under a click, when the list is shown.
    pub fn click_row(&mut self, i: usize) -> SetupOut {
        if matches!(self.step, Step::Start | Step::Provider | Step::Model | Step::Login) && i < self.list.ranked.len() {
            self.list.selected = i;
            return self.pick();
        }
        SetupOut::Nothing
    }

    pub fn scroll_by(&mut self, d: i32) {
        match self.step {
            Step::Start | Step::Provider | Step::Model | Step::Login => self.move_list(d),
            Step::Review if self.details => {
                let rows = (self.fields.len() + usize::from(!self.efforts.is_empty())) as i32;
                if rows > 0 {
                    self.selected = (self.selected as i32 + d).rem_euclid(rows) as usize;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::ModelInfo;

    fn press(s: &mut Setup, code: KeyCode) -> SetupOut {
        s.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn typing(s: &mut Setup, text: &str) {
        for c in text.chars() {
            press(s, KeyCode::Char(c));
        }
    }

    fn setup() -> Setup {
        let mut ds = ProviderConfig::new("openai-chat", "https://api.deepseek.com", Some("NEXO_TEST_NO_SUCH_KEY"));
        ds.default_model = Some("deepseek-chat".into());
        let ol = ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None);
        Setup::new(
            vec![("deepseek".into(), ds), ("ollama".into(), ol)],
            vec![("deepseek".into(), "deepseek-chat".into())],
            "deepseek/deepseek-chat",
        )
    }

    #[test]
    fn a_pasted_key_or_address_loses_escapes_controls_and_line_breaks() {
        let mut s = setup();
        for step in [Step::Key, Step::Address] {
            s.step = step;
            s.field.clear();
            s.paste("  key-abc\x1b[2J\r\ndef\x07\r\n");
            assert_eq!(s.field, "key-abcdef", "{step:?}");
        }
    }

    #[test]
    fn the_scripted_mock_provider_needs_no_key_or_address() {
        let m = ProviderConfig::new("mock", "", None);
        assert!(provider_ready("mock", &m, &[]));
    }

    /// Every test of the setup reads a login store that is not there, not the person's own.
    fn no_auth() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| std::env::set_var("NEXO_AUTH_FILE", "/nonexistent/nexo-test-auth.json"));
    }

    #[test]
    fn a_key_kept_with_auth_login_counts_as_found() {
        no_auth();
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("auth.json");
        let mut map = std::collections::BTreeMap::new();
        map.insert("acme-auth".to_string(), nexo_core::auth::Credential::Api { key: "stored".into() });
        nexo_core::auth::write_to(&f, &map).unwrap();
        std::env::set_var("NEXO_AUTH_FILE", &f);
        let acme = ProviderConfig::new("openai-chat", "https://acme.example", Some("NEXO_TEST_NO_SUCH_KEY"));
        let s = Setup::new(vec![("acme-auth".into(), acme)], vec![("acme-auth".into(), "m".into())], "acme-auth/m");
        // a store that is not there, not the person's own, for the tests that run beside this one
        std::env::set_var("NEXO_AUTH_FILE", "/nonexistent/nexo-test-auth.json");
        let row = s.list.items.iter().find(|i| i.value == "new:acme-auth").unwrap();
        assert!(row.hint.ends_with("key ✓"), "{}", row.hint);
    }

    #[test]
    fn an_unset_variable_at_the_key_step_is_named_not_probed() {
        let mut s = setup();
        s.step = Step::Key;
        typing(&mut s, "$NEXO_TEST_NO_SUCH_KEY");
        assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Nothing);
        assert_eq!(s.error.as_deref(), Some("$NEXO_TEST_NO_SUCH_KEY is not set in this shell"));
        assert_eq!(s.step, Step::Key);
    }

    #[test]
    fn a_configured_model_switches_at_once() {
        let mut s = setup();
        assert_eq!(s.list.current().unwrap().value, "use:deepseek/deepseek-chat");
        let out = press(&mut s, KeyCode::Enter);
        assert_eq!(
            out,
            SetupOut::SendAndClose(AgentCommand::SetModel {
                provider: Some("deepseek".into()),
                model: "deepseek-chat".into()
            })
        );
    }

    #[test]
    fn a_running_local_server_moves_up_and_is_asked_at_once() {
        let mut s = setup();
        s.found_local(vec![("ollama".into(), "Ollama 0.12.6".into())]);
        let labels: Vec<&str> = s.list.ranked.iter().map(|&i| s.list.items[i].label.as_str()).collect();
        assert_eq!(
            labels,
            ["▾ deepseek", "   deepseek-chat", "   set up again", "▸ ollama", "set up your own server"],
            "{labels:?}"
        );
        assert!(s.list.items[3].hint.contains("● running · Ollama 0.12.6"));
        typing(&mut s, "set up oll");
        let out = press(&mut s, KeyCode::Enter);
        assert!(matches!(out, SetupOut::Send(AgentCommand::Probe { ref name, .. }) if name == "ollama"), "{out:?}");
        assert!(s.busy.is_some());
    }

    #[test]
    fn the_whole_setup_of_a_keyed_provider() {
        let mut s = setup();
        typing(&mut s, "set up deep");
        // a named provider goes straight to its key: no address to type
        assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Nothing);
        assert_eq!((s.step, s.field.as_str()), (Step::Key, ""));
        typing(&mut s, "sk-secret");
        let rendered: String = s
            .lines(80, &Theme::nexo(), 0, false)
            .0
            .iter()
            .map(|l| l.spans.iter().map(|x| x.content.to_string()).collect::<String>())
            .collect();
        assert!(!rendered.contains("sk-secret") && rendered.contains("•••"), "the key is masked");
        let out = press(&mut s, KeyCode::Enter);
        assert!(matches!(out, SetupOut::Send(AgentCommand::Probe { key: Some(ref k), .. }) if k == "sk-secret"));
        s.probed(Ok(ServerProbe {
            kind: "openai-compatible".into(),
            protocol: "openai-chat".into(),
            base_url: "https://api.deepseek.com".into(),
            models: vec![
                ModelInfo { id: "deepseek-chat".into(), context_window: Some(128_000), ..Default::default() },
                ModelInfo { id: "deepseek-reasoner".into(), reasoning: true, ..Default::default() },
            ],
            ..Default::default()
        }));
        assert_eq!(s.step, Step::Model);
        assert_eq!(s.list.current().unwrap().value, "deepseek-chat", "the provider's default model comes first");
        press(&mut s, KeyCode::Down);
        let out = press(&mut s, KeyCode::Enter);
        assert!(
            matches!(out, SetupOut::Send(AgentCommand::CheckModel { ref model, .. }) if model == "deepseek-reasoner")
        );
        s.checked(ModelCheck {
            info: ModelInfo {
                id: "deepseek-reasoner".into(),
                context_window: Some(128_000),
                reasoning: true,
                efforts: vec!["low".into(), "high".into()],
                ..Default::default()
            },
            sources: vec!["context 128000 from the models.dev catalogue".into()],
            latency_ms: Some(900),
            called_tool: true,
            reasoned: true,
            ..Default::default()
        });
        assert_eq!(s.step, Step::Review);
        assert_eq!(s.fields[1].value, "128000");
        assert_eq!(s.fields[1].source, "the models.dev catalogue");
        // the facts wait behind Tab; a price typed in, the effort set to high
        press(&mut s, KeyCode::Tab);
        for _ in 0..3 {
            press(&mut s, KeyCode::Down);
        }
        typing(&mut s, "0.28");
        press(&mut s, KeyCode::Down);
        typing(&mut s, "0.42");
        press(&mut s, KeyCode::Down);
        press(&mut s, KeyCode::Down);
        press(&mut s, KeyCode::Left);
        assert_eq!(s.effort, Some(1));
        let out = press(&mut s, KeyCode::Enter);
        let SetupOut::Send(AgentCommand::SaveModel { name, cfg, model, key }) = out else { panic!("{out:?}") };
        assert_eq!(
            (name.as_str(), model.as_str(), key.as_deref()),
            ("deepseek", "deepseek-reasoner", Some("sk-secret"))
        );
        assert_eq!(cfg.context_window["deepseek-reasoner"], 128_000);
        assert_eq!(cfg.pricing["deepseek-reasoner"].output, 0.42);
        assert_eq!(cfg.effort["deepseek-reasoner"], "high");
        // the last screen waits for the save to be confirmed
        assert_eq!(s.step, Step::Review);
        s.saved("deepseek", "deepseek-reasoner");
        assert_eq!(s.step, Step::Done);
        assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Close);
    }

    #[test]
    fn a_dead_address_stays_with_its_reason_and_esc_walks_back() {
        let mut s = setup();
        typing(&mut s, "your own");
        press(&mut s, KeyCode::Enter);
        assert_eq!((s.step, s.name.as_str()), (Step::Address, "custom"));
        typing(&mut s, "localhost:9");
        press(&mut s, KeyCode::Enter);
        assert_eq!(s.cfg.base_url, "http://localhost:9");
        s.probed(Err("nothing answers at http://localhost:9: connection refused".into()));
        // the reason is a card, with a way on: Enter edits the address, kept as typed
        assert_eq!(s.step, Step::Trouble);
        assert!(s.problem.as_ref().unwrap().title.contains("localhost"), "{:?}", s.problem);
        assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Nothing);
        assert_eq!((s.step, s.field.as_str()), (Step::Address, "localhost:9"));
        s.probed(Err("x".into()));
        assert_eq!(s.step, Step::Address, "nothing is waiting: the answer is ignored");
        press(&mut s, KeyCode::Enter);
        s.probed(Err("nothing answers at http://localhost:9: connection refused".into()));
        assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
        assert_eq!(s.step, Step::Provider);
        assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Close);
    }

    #[test]
    fn a_model_the_list_lacks_can_be_typed() {
        let mut s = setup();
        s.step = Step::Model;
        s.probe = Some(ServerProbe { kind: "llama.cpp".into(), ..Default::default() });
        s.list = Picker::new(PickKind::Model, "", Vec::new(), None);
        typing(&mut s, "my-model");
        let out = press(&mut s, KeyCode::Enter);
        assert!(matches!(out, SetupOut::Send(AgentCommand::CheckModel { ref model, .. }) if model == "my-model"));
    }

    fn subscription() -> Setup {
        let mut cx = ProviderConfig::new("openai-responses", "https://chatgpt.example/backend-api/codex", None);
        cx.login = Some("codex".into());
        cx.models = vec!["gpt-x".into()];
        Setup::new(vec![("codex".into(), cx)], Vec::new(), "")
    }

    #[test]
    fn a_subscription_provider_asks_how_to_sign_in_before_its_address() {
        let mut s = subscription();
        // the first choices offer the sign-in; the row is on the cursor
        assert_eq!((s.step, s.list.current().unwrap().value.as_str()), (Step::Start, "new:codex"));
        assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Nothing);
        assert_eq!(s.step, Step::Login);
        assert_eq!(s.cfg.auth.as_deref(), Some("codex"), "the login is kept under the provider's name");
        let values: Vec<&str> = s.list.items.iter().map(|i| i.value.as_str()).collect();
        assert!(values.contains(&"codex-browser") && values.contains(&"codex-device"), "{values:?}");

        // the browser way: the command goes out, the page shown, Esc drops it
        typing(&mut s, "browser");
        let out = press(&mut s, KeyCode::Enter);
        assert_eq!(out, SetupOut::Send(AgentCommand::Login { name: "codex".into(), flow: "codex-browser".into() }));
        assert!(s.busy.is_some());
        s.login_prompt("https://auth.example/oauth/authorize?state=abc".into(), String::new());
        let (lines, _) = s.lines(80, &Theme::nexo(), 0, false);
        let text: String = lines.iter().flat_map(|l| l.spans.iter().map(|sp| sp.content.to_string())).collect();
        assert!(text.contains("auth.example") && !text.contains("oauth/authorize"), "{text}");
        assert_eq!(
            press(&mut s, KeyCode::Char('c')),
            SetupOut::Copy("https://auth.example/oauth/authorize?state=abc".into())
        );
        assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Send(AgentCommand::CancelLogin));
        assert!(s.busy.is_none() && s.prompt.is_none() && s.step == Step::Login);

        // a refused sign-in stays on the list with its reason
        s.list.query.clear();
        s.list.refilter();
        typing(&mut s, "browser");
        press(&mut s, KeyCode::Enter);
        assert_eq!(s.logged_in(Err("sign-in was declined".into())), SetupOut::Nothing);
        assert_eq!((s.step, s.error.as_deref()), (Step::Login, Some("sign-in was declined")));

        // a good one asks the provider what it serves, and Esc from the models goes back to the sign-in
        s.list.query.clear();
        s.list.refilter();
        typing(&mut s, "browser");
        press(&mut s, KeyCode::Enter);
        let out = s.logged_in(Ok(()));
        assert!(matches!(out, SetupOut::Send(AgentCommand::Probe { ref name, .. }) if name == "codex"), "{out:?}");
        s.probed(Ok(ServerProbe {
            kind: "subscription".into(),
            protocol: "openai-responses".into(),
            base_url: "https://chatgpt.example/backend-api/codex".into(),
            models: vec![ModelInfo { id: "gpt-x".into(), ..Default::default() }],
            ..Default::default()
        }));
        assert_eq!(s.step, Step::Model);
        press(&mut s, KeyCode::Esc);
        assert_eq!(s.step, Step::Login);
        assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
        assert_eq!(s.step, Step::Start);
    }

    #[test]
    fn a_login_that_arrives_after_the_setup_moved_on_is_ignored() {
        let mut s = subscription();
        assert_eq!(s.logged_in(Ok(())), SetupOut::Nothing);
        s.login_prompt("https://x".into(), String::new());
        assert!(s.prompt.is_none());
        assert_eq!(s.step, Step::Start);
    }

    // ---- the first run: a few choices, a pasted key tested before it is kept ----

    /// A first run: nothing set up, a keyed provider for each of two services, a subscription, a local server.
    fn first_run() -> Setup {
        no_auth();
        let mut ds = ProviderConfig::new("openai-chat", "https://api.deepseek.com", Some("NEXO_T_FIRST_DS"));
        ds.default_model = Some("deepseek-chat".into());
        let an = ProviderConfig::new("anthropic", "https://api.anthropic.com", Some("NEXO_T_FIRST_AN"));
        let ol = ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None);
        let mut cx = ProviderConfig::new("openai-responses", "https://chatgpt.example/backend-api/codex", None);
        cx.login = Some("codex".into());
        Setup::new(
            vec![("deepseek".into(), ds), ("anthropic".into(), an), ("codex".into(), cx), ("ollama".into(), ol)],
            Vec::new(),
            "",
        )
    }

    fn row_text(s: &Setup, w: usize) -> String {
        s.lines(w, &Theme::nexo(), 0, false)
            .0
            .iter()
            .map(|l| l.spans.iter().map(|x| x.content.to_string()).collect::<String>() + "\n")
            .collect()
    }

    fn deepseek_probe() -> ServerProbe {
        ServerProbe {
            kind: "openai-compatible".into(),
            protocol: "openai-chat".into(),
            base_url: "https://api.deepseek.com".into(),
            models: vec![ModelInfo { id: "deepseek-chat".into(), context_window: Some(128_000), ..Default::default() }],
            ..Default::default()
        }
    }

    #[test]
    fn the_first_screen_has_a_few_choices_and_one_is_recommended() {
        let s = first_run();
        assert_eq!(s.step, Step::Start);
        let labels: Vec<&str> = s.list.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["Use deepseek", "Sign in with your ChatGPT plan", "More providers"], "{labels:?}");
        assert_eq!(s.list.items.iter().filter(|i| i.hint.contains("recommended")).count(), 1);
        assert!(s.list.items[0].hint.starts_with("recommended"), "{}", s.list.items[0].hint);
        // nothing a newcomer has not met
        let t = row_text(&s, 80);
        for word in ["endpoint", "token", "slug", "base URL"] {
            assert!(!t.contains(word), "{word}: {t}");
        }
        assert!(
            t.lines().filter(|l| !l.trim().is_empty()).count() <= 5,
            "one sentence, the scan note, three choices: {t}"
        );
    }

    #[test]
    fn models_on_this_computer_come_first_and_ask_for_no_key() {
        let mut s = first_run();
        s.found_local(vec![("ollama".into(), "Ollama 0.12.6".into())]);
        assert_eq!(s.step, Step::Start);
        let first = s.list.current().unwrap();
        assert_eq!(
            (first.label.as_str(), first.value.as_str()),
            ("Use the models on this computer (free)", "local:ollama")
        );
        assert!(first.hint.starts_with("recommended"), "{}", first.hint);
        assert!(s.list.items.len() <= 4);
        let out = press(&mut s, KeyCode::Enter);
        assert!(
            matches!(out, SetupOut::Send(AgentCommand::Probe { ref name, key: None, .. }) if name == "ollama"),
            "{out:?}"
        );
        assert_ne!(s.step, Step::Key);
    }

    #[test]
    fn a_key_found_in_the_environment_is_offered_and_used_as_it_is() {
        no_auth();
        // its own variable, so no other test sees it
        std::env::set_var("NEXO_T_ENV_OFFER_AN", "made-up-key-for-the-test-0000");
        let an = ProviderConfig::new("anthropic", "https://api.anthropic.com", Some("NEXO_T_ENV_OFFER_AN"));
        let ds = ProviderConfig::new("openai-chat", "https://api.deepseek.com", Some("NEXO_T_ENV_OFFER_DS"));
        let mut s = Setup::new(vec![("deepseek".into(), ds), ("anthropic".into(), an)], Vec::new(), "");
        let first = s.list.current().unwrap();
        assert_eq!(first.label, "Found a key for anthropic, use it?");
        assert_eq!(first.value, "env:anthropic");
        assert!(s.list.items.len() <= 4);
        // the offered row is the one that is recommended; deepseek is still one row down
        assert!(first.hint.starts_with("recommended"), "{}", first.hint);
        assert!(s.list.items.iter().any(|i| i.label == "Use deepseek" && !i.hint.contains("recommended")));
        let out = press(&mut s, KeyCode::Enter);
        std::env::remove_var("NEXO_T_ENV_OFFER_AN");
        assert!(
            matches!(out, SetupOut::Send(AgentCommand::Probe { ref name, key: None, .. }) if name == "anthropic"),
            "the key is read from where it is, not copied: {out:?}"
        );
        assert_eq!(s.name, "anthropic");
    }

    #[test]
    fn a_pasted_key_names_its_service_when_only_that_service_starts_so() {
        let mut s = first_run();
        // quotes, spaces and a line break come with a paste; none of them is the key
        let out = s.paste("  \"sk-ant-api03-madeupmadeupmade\"  \r\n");
        assert_eq!(s.name, "anthropic");
        assert!(
            matches!(out, SetupOut::Send(AgentCommand::Probe { key: Some(ref k), .. }) if k == "sk-ant-api03-madeupmadeupmade"),
            "{out:?}"
        );
        assert_eq!(s.busy.as_deref(), Some("testing the key"));
    }

    #[test]
    fn a_pasted_key_with_a_start_two_services_share_asks_which_one() {
        let mut s = first_run();
        let out = s.paste("sk-0123456789abcdef0123456789abcdef");
        assert_eq!(out, SetupOut::Nothing);
        assert_eq!(s.step, Step::Provider, "the person is asked");
        assert!(row_text(&s, 80).contains("deepseek"));
        typing(&mut s, "set up deep");
        let out = press(&mut s, KeyCode::Enter);
        assert!(
            matches!(out, SetupOut::Send(AgentCommand::Probe { ref name, key: Some(ref k), .. }) if name == "deepseek" && k == "sk-0123456789abcdef0123456789abcdef"),
            "the pasted key goes with the choice: {out:?}"
        );
    }

    #[test]
    fn a_key_on_the_key_screen_shows_dots_and_its_last_four_and_the_page_to_get_one() {
        let mut s = first_run();
        press(&mut s, KeyCode::Enter);
        assert_eq!((s.step, s.name.as_str()), (Step::Key, "deepseek"));
        s.paste(" 'abcdefgh12345678' ");
        assert_eq!(s.field, "abcdefgh12345678");
        let t = row_text(&s, 80);
        assert!(
            t.contains(
                "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}5678"
            ),
            "{t}"
        );
        assert!(!t.contains("abcdefgh"), "{t}");
        assert!(t.contains("https://platform.deepseek.com/api_keys"), "{t}");
        for word in ["endpoint", "token", "slug", "base URL"] {
            assert!(!t.contains(word), "{word}: {t}");
        }
        let _ = crate::links::take_opened();
        press_ctrl(&mut s, 'o');
        assert_eq!(crate::links::take_opened(), ["https://platform.deepseek.com/api_keys"]);
        // a provider whose key page is not known shows no link
        s.name = "ollama".into();
        assert!(!row_text(&s, 80).contains("Get one at"));
    }

    fn press_ctrl(s: &mut Setup, c: char) -> SetupOut {
        s.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    #[test]
    fn a_refused_key_is_a_card_with_a_way_back_and_nothing_is_kept() {
        let mut s = first_run();
        press(&mut s, KeyCode::Enter);
        typing(&mut s, "sk-wrong-key-0000");
        let out = press(&mut s, KeyCode::Enter);
        assert!(matches!(out, SetupOut::Send(AgentCommand::Probe { key: Some(_), .. })), "{out:?}");
        s.probed(Ok(ServerProbe { needs_key: true, ..Default::default() }));
        assert_eq!(s.step, Step::Trouble);
        let t = row_text(&s, 80);
        assert!(t.contains("401") || t.to_lowercase().contains("key"), "{t}");
        assert!(t.contains("paste the key again"), "{t}");
        assert!(t.lines().filter(|l| !l.trim().is_empty()).count() <= 4, "a card of 3 lines and a hint: {t}");
        assert!(t.lines().all(|l| l.chars().count() < 72), "{t}");
        assert!(s.key.is_some(), "the wrong key is in memory only, never written");
        // Enter: the key is asked again, empty
        assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Nothing);
        assert_eq!((s.step, s.field.as_str(), s.key.is_none()), (Step::Key, "", true));
        // Esc: back to the first choices, with the key forgotten
        s.probed(Ok(ServerProbe { needs_key: true, ..Default::default() }));
        typing(&mut s, "x");
        assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
        assert_eq!(s.step, Step::Start);
        assert!(s.pending_key.is_none() && s.field.is_empty());
    }

    #[test]
    fn a_server_that_does_not_answer_is_a_card_with_retry() {
        let mut s = first_run();
        s.found_local(vec![("ollama".into(), "Ollama 0.12.6".into())]);
        press(&mut s, KeyCode::Enter);
        s.probed(Err("nothing answers at http://localhost:11434/v1: connection refused".into()));
        assert_eq!(s.step, Step::Trouble);
        let out = press(&mut s, KeyCode::Enter);
        assert!(
            matches!(out, SetupOut::Send(AgentCommand::Probe { ref name, .. }) if name == "ollama"),
            "retry asks again: {out:?}"
        );
        s.probed(Err("nothing answers".into()));
        assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
        assert_eq!(s.step, Step::Start, "or another way in");
    }

    #[test]
    fn a_local_server_with_no_models_says_what_to_do_instead_of_an_empty_list() {
        let mut s = first_run();
        s.found_local(vec![("ollama".into(), "Ollama 0.12.6".into())]);
        press(&mut s, KeyCode::Enter);
        s.probed(Ok(ServerProbe {
            kind: "ollama".into(),
            protocol: "openai-chat".into(),
            base_url: "http://localhost:11434/v1".into(),
            ..Default::default()
        }));
        assert_eq!(s.step, Step::Model);
        let t = row_text(&s, 80);
        assert!(t.contains("ollama pull") && !t.contains("no match"), "{t}");
    }

    #[test]
    fn a_default_model_is_chosen_with_a_line_on_what_it_is_for() {
        let mut s = first_run();
        press(&mut s, KeyCode::Enter);
        typing(&mut s, "sk-good-key-0000");
        press(&mut s, KeyCode::Enter);
        s.probed(Ok(deepseek_probe()));
        assert_eq!(s.step, Step::Model);
        assert_eq!(s.list.current().unwrap().value, "deepseek-chat");
        assert!(row_text(&s, 80).contains("suggests for everyday work"), "{}", row_text(&s, 80));
    }

    #[test]
    fn the_key_is_tested_with_a_model_then_saved_then_the_last_screen_says_where() {
        let mut s = first_run();
        press(&mut s, KeyCode::Enter);
        typing(&mut s, "sk-good-key-0000");
        press(&mut s, KeyCode::Enter);
        s.probed(Ok(deepseek_probe()));
        let out = press(&mut s, KeyCode::Enter);
        assert!(matches!(out, SetupOut::Send(AgentCommand::CheckModel { .. })), "{out:?}");
        assert_eq!(s.busy.as_deref(), Some("testing deepseek-chat"));
        s.checked(ModelCheck {
            info: ModelInfo { id: "deepseek-chat".into(), context_window: Some(128_000), ..Default::default() },
            latency_ms: Some(640),
            ..Default::default()
        });
        assert_eq!(s.step, Step::Review);
        let t = row_text(&s, 80);
        assert!(t.contains("Works") && t.contains("deepseek-chat"), "{t}");
        // saving is asked of the agent; the last screen waits for its word
        let out = press(&mut s, KeyCode::Enter);
        assert!(matches!(out, SetupOut::Send(AgentCommand::SaveModel { .. })), "{out:?}");
        assert_eq!(s.step, Step::Review);
        s.saved("other", "model");
        assert_eq!(s.step, Step::Review, "a model changed that is not this one is not our save");
        s.saved("deepseek", "deepseek-chat");
        assert_eq!(s.step, Step::Done);
        let t = row_text(&s, 80);
        assert!(t.contains("deepseek/deepseek-chat is ready"), "{t}");
        assert!(t.contains("Saved in") && t.contains("The key is in"), "{t}");
        assert!(t.contains("/model") && t.contains("/help") && t.contains("Enter"), "three things to try: {t}");
        assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Close);
    }

    #[test]
    fn a_failed_check_is_a_card_and_esc_returns_to_the_models() {
        let mut s = first_run();
        press(&mut s, KeyCode::Enter);
        typing(&mut s, "sk-good-key-0000");
        press(&mut s, KeyCode::Enter);
        s.probed(Ok(deepseek_probe()));
        press(&mut s, KeyCode::Enter);
        s.checked(ModelCheck { error: Some("HTTP 402: insufficient balance".into()), ..Default::default() });
        assert_eq!(s.step, Step::Trouble);
        assert!(s.problem.is_some());
        assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
        assert_eq!(s.step, Step::Model);
    }

    #[test]
    fn esc_goes_back_one_step_at_every_step_and_closes_from_the_first() {
        let mut s = first_run();
        // Start -> More providers -> Key -> (typed) back -> Provider -> Start -> closed
        press(&mut s, KeyCode::Down);
        press(&mut s, KeyCode::Down);
        press(&mut s, KeyCode::Enter);
        assert_eq!(s.step, Step::Provider);
        typing(&mut s, "set up deep");
        press(&mut s, KeyCode::Enter);
        assert_eq!(s.step, Step::Key);
        assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
        assert_eq!(s.step, Step::Provider);
        assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
        assert_eq!(s.step, Step::Start);
        assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Close);
        // Key -> Model -> Review: each Esc is one step
        let mut s = first_run();
        press(&mut s, KeyCode::Enter);
        typing(&mut s, "sk-good-key-0000");
        press(&mut s, KeyCode::Enter);
        s.probed(Ok(deepseek_probe()));
        press(&mut s, KeyCode::Enter);
        s.checked(ModelCheck {
            info: ModelInfo { id: "deepseek-chat".into(), ..Default::default() },
            ..Default::default()
        });
        assert_eq!(s.step, Step::Review);
        press(&mut s, KeyCode::Esc);
        assert_eq!(s.step, Step::Model);
        press(&mut s, KeyCode::Esc);
        assert_eq!(s.step, Step::Key, "the key typed is asked for again");
        press(&mut s, KeyCode::Esc);
        assert_eq!(s.step, Step::Start);
        // while a request is out, Esc cancels it and stays
        press(&mut s, KeyCode::Enter);
        typing(&mut s, "sk-good-key-0000");
        press(&mut s, KeyCode::Enter);
        assert!(s.busy.is_some());
        assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Send(AgentCommand::CancelCheck));
        assert!(s.busy.is_none() && s.step == Step::Key);
    }

    #[test]
    fn the_list_of_providers_says_what_to_do_not_where_they_live() {
        let mut s = first_run();
        s.show_providers();
        let hint = |v: &str| {
            s.list.items.iter().find(|i| i.value == v).map(|i| i.hint.clone()).unwrap_or_else(|| panic!("{v}"))
        };
        assert_eq!(hint("new:deepseek"), "needs a key");
        assert_eq!(hint("new:codex"), "sign in");
        assert_eq!(hint("new:ollama"), "on this computer");
        assert!(s.list.items.iter().all(|i| !i.hint.contains("://")), "no address in a row");
    }

    #[test]
    fn the_key_line_of_every_step_says_what_esc_does() {
        let mut s = first_run();
        for step in [
            Step::Start,
            Step::Provider,
            Step::Login,
            Step::Model,
            Step::Address,
            Step::Key,
            Step::Review,
            Step::Trouble,
            Step::Done,
        ] {
            s.step = step;
            s.from_start = false;
            let h = s.hints();
            assert!(h.contains("Esc") || step == Step::Done, "{step:?}: {h}");
            if !matches!(step, Step::Start | Step::Provider | Step::Done) {
                assert!(h.contains("Esc back") || h.contains("Esc pick another"), "{step:?}: {h}");
            }
        }
        s.step = Step::Provider;
        s.from_start = true;
        assert!(s.hints().contains("Esc back to the choices"));
        s.busy = Some("testing the key".into());
        assert_eq!(s.hints(), " Esc cancel");
    }

    #[test]
    fn esc_from_a_provider_screen_lands_on_the_row_it_came_from() {
        let mut s = first_run();
        s.fill_start();
        press(&mut s, KeyCode::Down);
        let want = s.list.current().map(|i| i.value.clone()).unwrap_or_default();
        press(&mut s, KeyCode::Enter);
        press(&mut s, KeyCode::Esc);
        assert_eq!(s.step, Step::Start);
        assert_eq!(s.list.current().map(|i| i.value.clone()).unwrap_or_default(), want);
        // and from the whole list of providers it lands on "More providers"
        press(&mut s, KeyCode::Down);
        press(&mut s, KeyCode::Enter);
        assert_eq!(s.step, Step::Provider);
        press(&mut s, KeyCode::Esc);
        assert_eq!(s.step, Step::Start);
        assert_eq!(s.list.current().map(|i| i.value.as_str()), Some("more"));
    }

    // ---- the key leaves no residue, and the screens claim only what happened ----

    const HELD: &str = "sk-0123456789abcdef0123456789abcdef";

    /// A paste of unknown kind, the person on the list of providers.
    fn holding() -> Setup {
        let mut s = first_run();
        assert_eq!(s.paste(HELD), SetupOut::Nothing);
        assert_eq!(s.step, Step::Provider);
        s
    }

    #[test]
    fn an_answer_of_a_check_that_was_left_changes_nothing() {
        let mut s = first_run();
        let SetupOut::Send(AgentCommand::Probe { name: a, seq: seq_a, .. }) = s.paste("sk-ant-api03-madeupmadeupmade")
        else {
            panic!("a probe")
        };
        assert_eq!(a, "anthropic");
        // Esc stops the check; the agent is told
        assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Send(AgentCommand::CancelCheck));
        assert!(s.busy.is_none());
        // a check on another provider
        assert_eq!(s.paste(HELD), SetupOut::Nothing);
        typing(&mut s, "set up deep");
        let SetupOut::Send(AgentCommand::Probe { name: b, seq: seq_b, .. }) = press(&mut s, KeyCode::Enter) else {
            panic!("a probe")
        };
        assert_eq!(b, "deepseek");
        assert_ne!(seq_a, seq_b);
        // the first one answers late: wrong name, then the right name with the old counter
        s.probed_from("anthropic", seq_a, Ok(deepseek_probe()));
        s.probed_from("deepseek", seq_a, Ok(deepseek_probe()));
        assert!(s.busy.is_some() && s.probe.is_none(), "the screen is still the second check's");
        s.probed_from("deepseek", seq_b, Ok(deepseek_probe()));
        assert!(s.busy.is_none() && s.probe.is_some());
    }

    #[test]
    fn a_model_check_answer_of_another_check_is_ignored() {
        let mut s = first_run();
        press(&mut s, KeyCode::Enter);
        typing(&mut s, "sk-good-key-0000");
        press(&mut s, KeyCode::Enter);
        s.probed(Ok(deepseek_probe()));
        let SetupOut::Send(AgentCommand::CheckModel { seq, .. }) = press(&mut s, KeyCode::Enter) else {
            panic!("a check")
        };
        let ok =
            ModelCheck { info: ModelInfo { id: "deepseek-chat".into(), ..Default::default() }, ..Default::default() };
        s.checked_from("deepseek", "deepseek-chat", seq + 1, ok.clone());
        s.checked_from("deepseek", "other", seq, ok.clone());
        assert!(s.busy.is_some(), "not ours");
        s.checked_from("deepseek", "deepseek-chat", seq, ok);
        assert!(s.busy.is_none());
    }

    #[test]
    fn a_save_that_failed_keeps_the_step_says_why_and_never_reaches_the_end_screen() {
        let mut s = first_run();
        press(&mut s, KeyCode::Enter);
        typing(&mut s, "sk-good-key-0000");
        press(&mut s, KeyCode::Enter);
        s.probed(Ok(deepseek_probe()));
        press(&mut s, KeyCode::Enter);
        s.checked(ModelCheck {
            info: ModelInfo { id: "deepseek-chat".into(), ..Default::default() },
            ..Default::default()
        });
        assert!(matches!(press(&mut s, KeyCode::Enter), SetupOut::Send(AgentCommand::SaveModel { .. })));
        s.save_failed(
            "deepseek",
            "deepseek-chat",
            "Not saved: config.toml could not be written. Make it writable, then press Enter.".into(),
        );
        assert_eq!(s.step, Step::Review);
        assert!(s.busy.is_none() && s.done.is_none());
        let t = row_text(&s, 100);
        assert!(t.contains("Not saved") && !t.contains("is ready"), "{t}");
        // a failure that is not ours changes nothing; a later success of the same save still works
        assert!(!s.saved("deepseek", "deepseek-chat"), "nothing was waiting");
        assert_eq!(s.step, Step::Review);
    }

    #[test]
    fn the_key_works_only_when_a_request_that_needs_the_key_passed() {
        let mut s = first_run();
        press(&mut s, KeyCode::Enter);
        typing(&mut s, "sk-good-key-0000");
        press(&mut s, KeyCode::Enter);
        s.probed(Ok(deepseek_probe()));
        let t = row_text(&s, 100);
        assert!(!t.contains("The key works") && t.contains("Models found"), "a public list proves nothing: {t}");
        let mut s2 = first_run();
        press(&mut s2, KeyCode::Enter);
        typing(&mut s2, "sk-good-key-0000");
        press(&mut s2, KeyCode::Enter);
        s2.probed(Ok(ServerProbe { key_proved: true, ..deepseek_probe() }));
        assert!(row_text(&s2, 100).contains("The key works"));
    }

    #[test]
    fn a_held_paste_is_dropped_with_a_line_when_a_local_server_is_picked() {
        let mut s = holding();
        typing(&mut s, "set up oll");
        s.found_local(vec![("ollama".into(), "Ollama 0.12.6".into())]);
        let out = press(&mut s, KeyCode::Enter);
        assert!(
            matches!(out, SetupOut::Send(AgentCommand::Probe { ref name, key: None, .. }) if name == "ollama"),
            "{out:?}"
        );
        assert!(s.key.is_none() && s.pending_key.is_none());
        assert_eq!(s.error.as_deref(), Some(HELD_DROPPED));
    }

    #[test]
    fn a_held_paste_is_dropped_with_a_line_when_the_custom_server_is_picked_and_when_esc_closes() {
        let mut s = holding();
        typing(&mut s, "your own");
        press(&mut s, KeyCode::Enter);
        assert_eq!((s.step, s.name.as_str()), (Step::Address, "custom"));
        assert!(s.key.is_none() && s.pending_key.is_none());
        assert_eq!(s.error.as_deref(), Some(HELD_DROPPED));
        // Esc on the list with a held paste drops it first
        let mut s = holding();
        assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
        assert!(s.pending_key.is_none());
        assert_eq!(s.error.as_deref(), Some(HELD_DROPPED));
    }

    #[test]
    fn a_held_paste_still_goes_to_the_cloud_provider_the_person_picks() {
        let mut s = holding();
        typing(&mut s, "set up deep");
        let out = press(&mut s, KeyCode::Enter);
        assert!(matches!(out, SetupOut::Send(AgentCommand::Probe { key: Some(ref k), .. }) if k == HELD), "{out:?}");
        assert!(s.error.is_none());
    }

    /// A server that records the head of every request it gets.
    fn recording_server() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("127.0.0.1:{}", l.local_addr().unwrap().port());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { break };
                let mut buf = [0u8; 4096];
                let n = c.read(&mut buf).unwrap_or(0);
                log.lock().unwrap().push(String::from_utf8_lossy(&buf[..n]).to_string());
                let _ = c.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
        });
        (addr, seen)
    }

    #[test]
    fn a_held_paste_never_reaches_a_server_at_an_address_the_person_typed() {
        let (addr, seen) = recording_server();
        let mut s = holding();
        typing(&mut s, "your own");
        press(&mut s, KeyCode::Enter);
        s.field.clear();
        typing(&mut s, &addr);
        let SetupOut::Send(AgentCommand::Probe { cfg, key, .. }) = press(&mut s, KeyCode::Enter) else {
            panic!("a probe")
        };
        assert!(key.is_none(), "the command carries no key");
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _ = rt.block_on(nexo_providers::probe::server(&cfg, key.as_deref()));
        let heads = seen.lock().unwrap().clone();
        assert!(!heads.is_empty(), "the server was asked");
        for h in heads {
            let h = h.to_ascii_lowercase();
            assert!(!h.contains("authorization") && !h.contains("x-api-key") && !h.contains(&HELD[..12]), "{h}");
        }
    }

    #[test]
    fn a_key_typed_in_the_search_box_is_masked_and_taken_as_a_key() {
        let mut s = first_run();
        s.paste("a");
        s.step = Step::Provider;
        s.fill_providers();
        let typed = "sk-ant-api03-madeupmadeupmade";
        typing(&mut s, typed);
        assert_eq!(s.list.query, typed, "all of it is kept until Enter");
        assert!(!row_text(&s, 100).contains(&typed[..14]), "never drawn in clear");
        let out = press(&mut s, KeyCode::Enter);
        assert!(matches!(out, SetupOut::Send(AgentCommand::Probe { key: Some(ref k), .. }) if k == typed), "{out:?}");
        assert!(s.list.query.is_empty());
    }

    #[test]
    fn typed_text_is_a_key_only_with_a_known_start_and_is_masked_from_the_first_character_after_it() {
        let mut s = first_run();
        s.paste("a");
        s.step = Step::Provider;
        s.fill_providers();
        // a model id of 17 characters is a model id: shown as typed, and Enter does not hold it as a key
        typing(&mut s, "deepseek-reasoner");
        assert!(!looks_like_secret("deepseek-reasoner"), "shown as typed");
        press(&mut s, KeyCode::Enter);
        assert!(s.pending_key.is_none(), "not a key");
        assert_eq!(s.list.query, "deepseek-reasoner", "the search stays what it was");
        s.list.query.clear();
        s.refilter();
        // a known start: the start may be drawn, nothing after it ever is
        typing(&mut s, "gsk_ab");
        assert!(looks_like_secret(&s.list.query), "masked as soon as one character follows the known start");
        assert!(!looks_like_secret("gsk_"), "a start alone is only a start");
        typing(&mut s, "cdefghijklmnop");
        press(&mut s, KeyCode::Enter);
        assert_eq!(s.pending_key.as_deref(), Some("gsk_abcdefghijklmnop"), "a known start and long enough: a key");
    }

    #[test]
    fn an_answer_with_the_open_counter_but_another_name_is_ignored() {
        let mut s = first_run();
        let SetupOut::Send(AgentCommand::Probe { seq, .. }) = s.paste("sk-ant-api03-madeupmadeupmade") else {
            panic!("a probe")
        };
        s.probed_from("deepseek", seq, Ok(deepseek_probe()));
        assert!(s.busy.is_some() && s.probe.is_none(), "only the name told it apart");
    }

    #[test]
    fn esc_makes_the_answer_of_the_stopped_check_late_even_for_a_wait_that_began_without_a_new_check() {
        let mut s = first_run();
        let SetupOut::Send(AgentCommand::Probe { seq, .. }) = s.paste("sk-ant-api03-madeupmadeupmade") else {
            panic!("a probe")
        };
        assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Send(AgentCommand::CancelCheck));
        // another wait begins on the same provider without a check of its own (the screen is waiting again)
        s.busy = Some("waiting".into());
        s.probed_from("anthropic", seq, Ok(deepseek_probe()));
        assert!(s.busy.is_some() && s.probe.is_none(), "the stopped check's answer is late");
    }

    #[test]
    fn picking_a_model_already_set_up_with_a_paste_held_says_the_one_line_and_waits() {
        let mut s = setup();
        s.step = Step::Provider;
        s.fill_providers();
        s.pending_key = Some(HELD.into());
        typing(&mut s, "deepseek-chat");
        for _ in 0..4 {
            if s.list.current().is_some_and(|i| i.value.starts_with("use:")) {
                break;
            }
            press(&mut s, KeyCode::Down);
        }
        assert!(s.list.current().is_some_and(|i| i.value.starts_with("use:")), "{:?}", s.list.current());
        assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Nothing, "the wizard does not close on a dropped paste");
        assert!(s.pending_key.is_none());
        assert_eq!(s.error.as_deref(), Some(HELD_DROPPED));
        assert!(matches!(press(&mut s, KeyCode::Enter), SetupOut::SendAndClose(AgentCommand::SetModel { .. })));
    }

    #[test]
    fn a_held_paste_goes_only_to_the_services_of_nexos_own_list_by_the_entry_picked() {
        // a provider of the person's own on a host name of the local network: not in the list
        let lan = ProviderConfig::new("openai-chat", "https://gpu-box.corp.example/v1", Some("NEXO_T_LAN"));
        let mut ds = ProviderConfig::new("openai-chat", "https://api.deepseek.com", Some("NEXO_T_LAN_DS"));
        ds.default_model = Some("deepseek-chat".into());
        no_auth();
        for (name, cfg, goes) in [("gpu-box", lan.clone(), false), ("deepseek", ds, true)] {
            let mut s = Setup::new(vec![(name.into(), cfg)], Vec::new(), "");
            s.paste("a");
            s.step = Step::Provider;
            s.fill_providers();
            s.pending_key = Some(HELD.into());
            let out = s.choose(&format!("new:{name}"));
            let sent = matches!(out, SetupOut::Send(AgentCommand::Probe { key: Some(_), .. })) || s.key.is_some();
            assert_eq!(sent, goes, "{name}: {out:?}");
            if !goes {
                assert_eq!(s.error.as_deref(), Some(HELD_DROPPED));
            }
        }
        // a built-in name pointed at a host of the person's own is not the built-in service either
        let moved = ProviderConfig::new("openai-chat", "http://nas.local:8000/v1", Some("NEXO_T_LAN"));
        let mut s = Setup::new(vec![("deepseek".into(), moved)], Vec::new(), "");
        s.pending_key = Some(HELD.into());
        s.choose("new:deepseek");
        assert!(s.key.is_none(), "kept nothing for an address that is not the service's");
    }

    #[test]
    fn a_key_typed_where_a_model_name_goes_is_masked_and_not_sent_as_a_name() {
        let mut s = first_run();
        press(&mut s, KeyCode::Enter);
        typing(&mut s, "sk-good-key-0000");
        press(&mut s, KeyCode::Enter);
        s.probed(Ok(deepseek_probe()));
        assert_eq!(s.step, Step::Model);
        let k = "sk-ant-0123456789abcdef0123456789abcdef";
        s.list.query = k.into();
        assert!(!row_text(&s, 100).contains(k), "masked on screen");
        assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Nothing, "not sent as a model");
        assert!(s.list.query.is_empty() && s.error.is_some());
    }

    #[test]
    fn the_last_screen_says_only_you_can_read_it_only_where_the_mode_is_set() {
        let mut s = first_run();
        s.done =
            Some(Done { model: "deepseek/m".into(), config: "/c.toml".into(), key: Some("/k".into()), private: false });
        s.step = Step::Done;
        let t = row_text(&s, 100);
        assert!(t.contains("The key is in /k") && !t.contains("only you can read it"), "{t}");
        s.done.as_mut().unwrap().private = true;
        assert!(row_text(&s, 100).contains("only you can read it"));
    }

    #[test]
    fn esc_on_the_first_screen_clears_hidden_text_first_and_a_second_esc_closes() {
        let mut s = first_run();
        s.paste("some words");
        assert!(!s.field.is_empty());
        assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
        assert!(s.field.is_empty());
        assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Close);
    }

    #[test]
    fn nothing_is_busy_when_the_probe_to_check_with_does_not_exist() {
        let mut s = first_run();
        s.step = Step::Model;
        s.probe = None;
        s.list.query = "some-model".into();
        let _ = press(&mut s, KeyCode::Enter);
        assert!(s.busy.is_none(), "{:?}", s.busy);
    }
}
