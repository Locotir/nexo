//! `/model`: a guided setup. Pick a configured model to switch to it, or a
//! provider to set one up: its address is checked by asking it, the key is
//! asked for only when the server wants one, its models come from the server,
//! and the chosen model is tried with a real request before anything is saved.
//! Every fact the setup found is shown, with where it came from, and can be
//! changed before saving.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_core::agent::AgentCommand;
use nexo_core::provider::{kind_label, normalize_url, LOCAL};
use nexo_core::{ModelCheck, Pricing, ProviderConfig, ServerProbe};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{PickItem, PickKind, Picker};
use crate::fuzzy;
use crate::render::blocks::{fmt_k, fmt_price, SPINNER};
use crate::render::wrap::{truncate, wrap_plain};
use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Provider,
    /// How to sign in to a subscription provider.
    Login,
    Address,
    Key,
    Model,
    Review,
}

impl Step {
    fn number(self) -> usize {
        match self {
            Step::Provider => 1,
            Step::Login | Step::Address => 2,
            Step::Key => 3,
            Step::Model => 4,
            Step::Review => 5,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Step::Provider => "provider",
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
        };
        s.fill_providers();
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
                Some(v) if c.login.is_none() => format!("needs {v}"),
                _ => String::new(),
            };
            // what the person must do comes first: a narrow window cuts the end, and the address is the part to lose
            let hint = match (&running, need.is_empty()) {
                (Some(w), true) => format!("● running · {w}"),
                (Some(w), false) => format!("● running · {w} · {need}"),
                (None, true) => c.base_url.clone(),
                (None, false) if need.starts_with("needs") => format!("{need} · {}", c.base_url),
                (None, false) => format!("{} · {need}", c.base_url),
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
        let own = PickItem {
            label: "set up your own server".into(),
            value: CUSTOM.into(),
            hint: "OpenAI-compatible (Ollama, vLLM, LM Studio)".into(),
        };
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
        if self.step == Step::Provider {
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
        self.busy = Some(format!("asking {}", self.cfg.base_url));
        SetupOut::Send(AgentCommand::Probe { name: self.name.clone(), cfg: self.cfg.clone(), key: self.key.clone() })
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
            Err(e) => {
                self.error = Some(e);
                self.step = match self.step {
                    Step::Key => Step::Key,
                    _ if self.cfg.login.is_some() => {
                        self.fill_login();
                        Step::Login
                    }
                    _ => Step::Address,
                };
            }
            Ok(p) => {
                self.cfg.protocol = p.protocol.clone();
                self.cfg.base_url = p.base_url.clone();
                if p.needs_key && p.models.is_empty() {
                    if self.step == Step::Key {
                        self.error = Some("the server refused that key".into());
                    }
                    self.field.clear();
                    self.step = Step::Key;
                    self.probe = Some(p);
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
        self.check = Some(check);
        self.step = Step::Review;
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
            return SetupOut::Nothing;
        }
        self.step = match self.step {
            Step::Provider => return SetupOut::Close,
            Step::Login | Step::Address => {
                self.fill_providers();
                Step::Provider
            }
            Step::Key | Step::Model if self.cfg.login.is_some() => {
                self.fill_login();
                Step::Login
            }
            Step::Key | Step::Model => {
                self.field = self.cfg.base_url.clone();
                Step::Address
            }
            Step::Review => {
                self.check = None;
                Step::Model
            }
        };
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

    /// Typed text pasted in one go.
    pub fn paste(&mut self, text: &str) {
        // escapes and control characters out; a key wrapped over lines is joined back
        let text = crate::render::wrap::clean(&text.replace('\r', "\n")).replace('\n', "");
        let text = text.trim();
        match self.step {
            Step::Address | Step::Key => self.field.push_str(text),
            Step::Provider | Step::Model | Step::Login => {
                self.list.query.push_str(text);
                self.refilter();
            }
            Step::Review => {
                if let Some(f) = self.fields.get_mut(self.selected) {
                    f.value.push_str(text);
                }
            }
        }
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
                let Some(it) = self.list.current().cloned() else { return SetupOut::Nothing };
                // a header opens or closes (while filtering every fold with a match is open)
                if it.value.starts_with("group:") {
                    self.toggle_current();
                    return SetupOut::Nothing;
                }
                if let Some(pm) = it.value.strip_prefix("use:") {
                    let (provider, model) = match pm.split_once('/') {
                        Some((p, m)) => (Some(p.to_string()), m.to_string()),
                        None => (None, pm.to_string()),
                    };
                    return SetupOut::SendAndClose(AgentCommand::SetModel { provider, model });
                }
                self.signed = None;
                let name = it.value.trim_start_matches("new:").to_string();
                self.cfg = self
                    .providers
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, c)| c.clone())
                    .unwrap_or_else(|| ProviderConfig::new("openai-chat", "", None));
                self.name = if name.is_empty() { "custom".into() } else { name };
                self.key = None;
                // a subscription: the login first, kept under the provider's name
                if self.cfg.login.is_some() {
                    let own = self.name.clone();
                    self.cfg.auth.get_or_insert(own);
                    self.fill_login();
                    return SetupOut::Nothing;
                }
                // a server found running here is asked at once
                if let Some(root) = LOCAL
                    .iter()
                    .find(|(p, _)| *p == self.name && self.local.iter().any(|(l, _)| l == p))
                    .map(|(_, r)| r.to_string())
                {
                    if self.cfg.base_url.starts_with(&root) || self.cfg.base_url.is_empty() {
                        self.field = self.cfg.base_url.clone();
                        self.step = Step::Address;
                        return self.probe_cmd();
                    }
                }
                self.field = self.cfg.base_url.clone();
                self.step = Step::Address;
                SetupOut::Nothing
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
                let model = match self.list.current() {
                    Some(it) => it.value.clone(),
                    None if !typed.is_empty() => typed,
                    None => return SetupOut::Nothing,
                };
                self.model = model.clone();
                self.error = None;
                self.busy = Some(format!("trying {model} with a real request"));
                let Some(probe) = self.probe.clone() else { return SetupOut::Nothing };
                SetupOut::Send(AgentCommand::CheckModel {
                    name: self.name.clone(),
                    probe,
                    cfg: self.cfg.clone(),
                    model,
                    key: self.key.clone(),
                })
            }
            _ => SetupOut::Nothing,
        }
    }

    fn submit_field(&mut self) -> SetupOut {
        let text = self.field.trim().to_string();
        match self.step {
            Step::Address => {
                if text.is_empty() {
                    self.error =
                        Some("type the server's address, e.g. localhost:11434 or https://api.example.com/v1".into());
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
                    self.error = Some("paste the key, or $VARIABLE that holds it, or !command that prints it".into());
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
                SetupOut::SendAndClose(AgentCommand::SaveModel {
                    name,
                    cfg,
                    model: self.model.clone(),
                    key: self.key.clone(),
                })
            }
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
        format!(" set up a model · {}/5 {} ", self.step.number(), self.step.name())
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
        match self.step {
            Step::Provider => " type to filter · ↑↓ choose · ←→ fold · Enter open or pick · Esc close",
            Step::Login if self.busy.is_some() => " finish in the browser · Esc cancel",
            Step::Login => " ↑↓ choose · Enter sign in · Esc back",
            Step::Model => " type to filter (or a name not listed) · ↑↓ choose · Enter try it · Esc back",
            Step::Address | Step::Key => " type · Enter check · Ctrl+U clear · Esc back",
            Step::Review => " ↑↓ field · type to change · ←→ effort · Enter save and use · Esc back",
        }
    }

    /// The body: the lines above the list, and the list itself for the steps that have one.
    /// Returns the lines and the index of the first list row.
    pub fn lines(&self, w: usize, theme: &Theme, spinner: usize, short: bool) -> (Vec<Line<'static>>, Option<usize>) {
        let dim = theme.dim();
        let mut lines: Vec<Line<'static>> = Vec::new();
        let prompt = |text: String, masked: bool| {
            let shown = if masked { "•".repeat(text.chars().count()) } else { text };
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
            // on a short screen the list comes first: the intro is dropped
            Step::Provider if short => {}
            Step::Provider => {
                say(&mut lines, "Switch to a model you have, or set up a provider: nexo asks the server what it is and what it serves.", dim);
                if !self.scanned {
                    say(&mut lines, "Looking for Ollama, LM Studio, vLLM and llama.cpp on this machine…", dim);
                }
            }
            Step::Login => {
                lines.push(Line::from(vec![
                    Span::styled(format!("{} ", self.name), theme.bold(theme.accent)),
                    Span::styled("sign in with your subscription", dim),
                ]));
                say(
                    &mut lines,
                    "The login is kept in a file only you can read; it is never shown and never put in the config.",
                    dim,
                );
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
                say(
                    &mut lines,
                    "e.g. localhost:11434 (Ollama), localhost:8000/v1 (vLLM), https://api.deepseek.com",
                    dim,
                );
                lines.push(prompt(self.field.clone(), false));
            }
            Step::Key => {
                let p = self.probe.as_ref();
                let what = p.map(|p| kind_label(&p.kind).to_string()).unwrap_or_default();
                lines.push(Line::from(vec![
                    Span::styled(format!("{} ", self.name), theme.bold(theme.accent)),
                    Span::styled(format!("{what} at {} wants a key", self.cfg.base_url), dim),
                ]));
                if let Some(v) = &self.cfg.api_key_env {
                    let set = std::env::var(v).is_ok_and(|k| !k.trim().is_empty());
                    say(
                        &mut lines,
                        &if set { format!("${v} is set · Enter try it") } else { format!("${v} is not set") },
                        dim,
                    );
                }
                say(&mut lines, "Paste the key (kept in a file only you can read, never in the config), or $VARIABLE, or !command that prints it.", dim);
                let masked = !self.field.starts_with('$') && !self.field.starts_with('!');
                lines.push(prompt(self.field.clone(), masked));
            }
            Step::Model => {
                if let Some(s) = &self.signed {
                    lines.push(Line::from(Span::styled(format!("✓ {s}"), theme.style(theme.success))));
                }
                if let Some(p) = &self.probe {
                    let mut head = format!("✓ {}", kind_label(&p.kind));
                    if let Some(v) = &p.version {
                        head.push_str(&format!(" {v}"));
                    }
                    head.push_str(&format!(" at {} · {} models", p.base_url, p.models.len()));
                    lines.push(Line::from(Span::styled(head, theme.style(theme.success))));
                    for t in &p.trail {
                        say(&mut lines, &format!("  {t}"), dim);
                    }
                }
                lines.push(prompt(self.list.query.clone(), false));
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
        if let Some(b) = &self.busy {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                format!("{} {b}…", SPINNER[spinner % SPINNER.len()]),
                theme.style(theme.accent),
            )));
        }
        if let Some(e) = &self.error {
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
        let list_at = matches!(self.step, Step::Provider | Step::Model | Step::Login).then(|| {
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
        if matches!(self.step, Step::Provider | Step::Model | Step::Login) && i < self.list.ranked.len() {
            self.list.selected = i;
            return self.pick();
        }
        SetupOut::Nothing
    }

    pub fn scroll_by(&mut self, d: i32) {
        match self.step {
            Step::Provider | Step::Model | Step::Login => self.move_list(d),
            Step::Review => {
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

    #[test]
    fn a_key_kept_with_auth_login_counts_as_found() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("auth.json");
        let mut map = std::collections::BTreeMap::new();
        map.insert("deepseek".to_string(), nexo_core::auth::Credential::Api { key: "stored".into() });
        nexo_core::auth::write_to(&f, &map).unwrap();
        std::env::set_var("NEXO_AUTH_FILE", &f);
        let s = setup();
        std::env::remove_var("NEXO_AUTH_FILE");
        let row = s.list.items.iter().find(|i| i.value == "new:deepseek").unwrap();
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
        assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Nothing);
        assert_eq!((s.step, s.field.as_str()), (Step::Address, "https://api.deepseek.com"));
        let out = press(&mut s, KeyCode::Enter);
        assert!(matches!(out, SetupOut::Send(AgentCommand::Probe { key: None, .. })));
        // the server wants a key
        s.probed(Ok(ServerProbe {
            kind: "openai-compatible".into(),
            protocol: "openai-chat".into(),
            base_url: "https://api.deepseek.com".into(),
            needs_key: true,
            ..Default::default()
        }));
        assert_eq!(s.step, Step::Key);
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
        // a price typed in, the effort set to high
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
        let SetupOut::SendAndClose(AgentCommand::SaveModel { name, cfg, model, key }) = out else { panic!("{out:?}") };
        assert_eq!(
            (name.as_str(), model.as_str(), key.as_deref()),
            ("deepseek", "deepseek-reasoner", Some("sk-secret"))
        );
        assert_eq!(cfg.context_window["deepseek-reasoner"], 128_000);
        assert_eq!(cfg.pricing["deepseek-reasoner"].output, 0.42);
        assert_eq!(cfg.effort["deepseek-reasoner"], "high");
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
        assert_eq!(s.step, Step::Address);
        assert!(s.error.as_deref().unwrap().contains("nothing answers"));
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
        typing(&mut s, "codex");
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
        assert_eq!(s.step, Step::Provider);
    }

    #[test]
    fn a_login_that_arrives_after_the_setup_moved_on_is_ignored() {
        let mut s = subscription();
        assert_eq!(s.logged_in(Ok(())), SetupOut::Nothing);
        s.login_prompt("https://x".into(), String::new());
        assert!(s.prompt.is_none());
        assert_eq!(s.step, Step::Provider);
    }
}
