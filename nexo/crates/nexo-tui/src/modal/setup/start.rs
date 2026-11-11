//! The screens of the setup that a first run adds around the old ones: the first choices, a key pasted
//! before a provider was picked, the card of what went wrong, and the last screen that says what was saved.

use nexo_core::provider::LOCAL;
use nexo_core::{ErrorKind, Problem, ProblemCtx, ProviderConfig, ProviderError};
use nexo_providers::keys;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{host_of, PickItem, PickKind, Picker, Setup, SetupOut, Step, OTHERS};
use crate::render::blocks::SPINNER;
use crate::render::wrap::{truncate, wrap_plain};
use crate::theme::Theme;

/// The provider offered first to someone with nothing: the one the README starts with.
const RECOMMENDED: &str = "deepseek";

/// What Enter does on the screen of a problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Again {
    /// Ask the server again (the same address, the same key).
    Probe,
    /// Type the key again.
    Key,
    /// Edit the address.
    Address,
    /// Send the test request again.
    Check,
}

/// What the last screen says: what was saved and where.
#[derive(Debug, Clone, PartialEq)]
pub struct Done {
    /// `provider/model`.
    pub model: String,
    pub config: String,
    /// The file the key went to (none when there was no typed key).
    pub key: Option<String>,
    /// The file mode that keeps others out is set (Unix); the line says "only you can read it" only then.
    pub private: bool,
}

/// The reason of a failed test request, as an error the problem card knows: the status in
/// parentheses ("(402)") and the words of the message tell what kind it was.
pub(super) fn error_of(text: &str) -> ProviderError {
    let lower = text.to_ascii_lowercase();
    let status = lower
        .split('(')
        .skip(1)
        .filter_map(|s| s.split(')').next())
        .find_map(|s| s.trim().parse::<u16>().ok().filter(|n| (400..600).contains(n)));
    let kind = match status {
        Some(401 | 403) => ErrorKind::Auth,
        Some(429) => ErrorKind::RateLimit,
        Some(500..=599) => ErrorKind::Server,
        Some(_) => ErrorKind::BadRequest,
        None if lower.contains("key rejected") || lower.contains("key missing") => ErrorKind::Auth,
        None if lower.contains("could not reach") || lower.contains("nothing answers") => ErrorKind::Network,
        None if lower.contains("rate limited") => ErrorKind::RateLimit,
        None => ErrorKind::Other,
    };
    let e = ProviderError::new(kind, text.to_string());
    match status {
        Some(s) => e.with_status(s),
        None => e,
    }
}

impl Setup {
    /// The first screen of someone with no model: at most three ways in, then "More providers". The
    /// first row is the one nexo suggests.
    pub(super) fn fill_start(&mut self) {
        self.signed = None;
        self.problem = None;
        self.pending_key = None;
        self.from_start = false;
        // the cursor stays where the person put it; on the first row it follows a better row that arrives
        let keep = self
            .came_from
            .take()
            .or_else(|| self.list.current().filter(|_| self.list.selected > 0).map(|i| i.value.clone()));
        let mut rows: Vec<PickItem> = Vec::new();
        let item = |label: String, value: String, hint: &str| PickItem { label, value, hint: hint.to_string() };
        if let Some((name, what)) = self.local.first() {
            rows.push(item("Use the models on this computer (free)".into(), format!("local:{name}"), what));
        }
        let env = self.found_key();
        if let Some(name) = &env {
            rows.push(item(
                format!("Found a key for {name}, use it?"),
                format!("env:{name}"),
                "already on this computer",
            ));
        }
        let has = |n: &str| self.providers.iter().find(|(p, _)| p == n);
        if let Some((_, c)) = has(RECOMMENDED).filter(|(n, _)| env.as_deref() != Some(n.as_str())) {
            let hint = if c.pricing.is_empty() { "your own key" } else { "your own key · pay as you go" };
            rows.push(item(format!("Use {RECOMMENDED}"), format!("new:{RECOMMENDED}"), hint));
        }
        if has("codex").is_some_and(|(_, c)| c.login.is_some()) {
            rows.push(item("Sign in with your ChatGPT plan".into(), "new:codex".into(), "included in your plan"));
        }
        rows.truncate(3);
        if let Some(first) = rows.first_mut() {
            first.hint =
                if first.hint.is_empty() { "recommended".into() } else { format!("recommended · {}", first.hint) };
        }
        rows.push(item(
            "More providers".into(),
            "more".into(),
            &format!("all {} · search by name", self.providers.len()),
        ));
        self.list = Picker::new(PickKind::Model, "", rows, keep.as_deref());
        self.step = Step::Start;
    }

    /// The first provider with a key already in the environment, if there is one.
    pub(super) fn found_key(&self) -> Option<String> {
        self.providers
            .iter()
            .filter(|(n, c)| c.login.is_none() && !LOCAL.iter().any(|(l, _)| l == n))
            .find(|(_, c)| {
                c.api_key_env.as_deref().is_some_and(|v| std::env::var(v).is_ok_and(|k| !k.trim().is_empty()))
            })
            .map(|(n, _)| n.clone())
    }

    /// "More providers": the whole list, open, to search.
    pub(super) fn show_providers(&mut self) {
        self.from_start = true;
        self.open.insert(OTHERS.into());
        self.fill_providers();
        self.list.query.clear();
        self.step = Step::Provider;
    }

    /// Back to the list the person chose the provider from.
    pub(super) fn go_origin(&mut self) {
        self.problem = None;
        self.prompt = None;
        // what was typed for the step left is not carried to the one before
        self.field.clear();
        self.key = None;
        self.pending_key = None;
        if self.origin == Step::Start {
            self.fill_start();
        } else {
            self.fill_providers();
            self.step = Step::Provider;
        }
    }

    /// A provider was chosen (from either list): where it goes next depends on what it needs.
    pub(super) fn choose(&mut self, value: &str) -> SetupOut {
        self.origin = self.step;
        if self.step == Step::Start {
            self.came_from = Some(value.to_string());
        }
        self.signed = None;
        self.problem = None;
        self.details = false;
        let name = value.trim_start_matches("new:").trim_start_matches("local:").trim_start_matches("env:").to_string();
        self.cfg = self
            .providers
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, c)| c.clone())
            .unwrap_or_else(|| ProviderConfig::new("openai-chat", "", None));
        self.name = if name.is_empty() { "custom".into() } else { name };
        // a pasted text of unknown kind goes only to a cloud service the person picks: a server on this
        // computer or of one's own never gets it
        let held = self.pending_key.take();
        let dropped = held.is_some() && !self.cloud();
        self.key = held.filter(|_| !dropped);
        let out = self.choose_on();
        if dropped {
            self.error = Some(super::HELD_DROPPED.into());
        }
        out
    }

    /// Whether the entry picked is a service of nexo's own list of cloud services, reached at the address
    /// that list gives it. Decided by the entry, never by how a name or an address looks: a server of
    /// one's own, on any host name, is not in that list.
    fn cloud(&self) -> bool {
        let local = LOCAL.iter().any(|(p, _)| *p == self.name);
        nexo_providers::presets::presets()
            .get(&self.name)
            .is_some_and(|p| p.login.is_none() && p.base_url.starts_with("https://") && p.base_url == self.cfg.base_url)
            && !local
    }

    fn choose_on(&mut self) -> SetupOut {
        // a subscription: the login first, kept under the provider's name
        if self.cfg.login.is_some() {
            let own = self.name.clone();
            self.cfg.auth.get_or_insert(own);
            self.fill_login();
            return SetupOut::Nothing;
        }
        // only one's own server needs its address typed
        if self.name == "custom" {
            self.field = self.cfg.base_url.clone();
            self.step = Step::Address;
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
                return self.probe_cmd();
            }
        }
        // a key already there (typed, in the environment, in a file) is tested; else it is asked for
        let kept = self.key.is_some()
            || self.cfg.api_key_cmd.is_some()
            || self.cfg.api_key_file.is_some()
            || self.cfg.resolve_key_for(&self.name).is_some();
        if kept {
            return self.probe_cmd();
        }
        self.field.clear();
        self.step = if self.cfg.api_key_env.is_some() {
            Step::Key
        } else {
            self.field = self.cfg.base_url.clone();
            Step::Address
        };
        SetupOut::Nothing
    }

    /// A key arrived before a provider was chosen: its start names the service when only one uses it;
    /// else the person is asked which one.
    pub(super) fn key_pasted(&mut self, key: String) -> SetupOut {
        self.field.clear();
        self.error = None;
        let known = keys::recognise(&key).filter(|n| self.providers.iter().any(|(p, _)| p == n));
        self.pending_key = Some(key);
        match known {
            Some(name) => self.choose(&format!("new:{name}")),
            None => {
                self.show_providers();
                SetupOut::Nothing
            }
        }
    }

    /// What went wrong, as the card of a failed request, with what Enter does about it.
    pub(super) fn trouble(&mut self, e: ProviderError, again: Again) {
        let ctx = ProblemCtx::from_config(&self.name, &self.model, &self.cfg);
        let mut p = Problem::of(&e, &ctx);
        // the card says to run a command; in the setup the next move is to paste the key again
        if matches!(e.kind, ErrorKind::Auth) && e.status.is_some() {
            p.steps = vec!["Enter: paste the key again. Esc: pick another provider.".into()];
        }
        self.busy = None;
        self.error = None;
        self.problem = Some(p);
        self.details = false;
        self.again = again;
        self.step = Step::Trouble;
    }

    /// Enter on a problem: try again where it went wrong.
    fn retry(&mut self) -> SetupOut {
        self.problem = None;
        self.details = false;
        match self.again {
            Again::Key => {
                self.key = None;
                self.field.clear();
                self.step = Step::Key;
                SetupOut::Nothing
            }
            Again::Address => {
                self.step = Step::Address;
                SetupOut::Nothing
            }
            Again::Probe => {
                if self.origin == Step::Start {
                    self.fill_start();
                } else {
                    self.step = Step::Provider;
                }
                self.probe_cmd()
            }
            Again::Check => {
                self.step = Step::Model;
                self.pick()
            }
        }
    }

    pub(super) fn trouble_key(&mut self, code: crossterm::event::KeyCode) -> SetupOut {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Enter => self.retry(),
            KeyCode::Tab => {
                self.details = !self.details;
                SetupOut::Nothing
            }
            // a test request that failed does not always mean the model is unusable
            KeyCode::Char('s' | 'S') if self.again == Again::Check => {
                self.problem = None;
                self.details = true;
                self.step = Step::Review;
                SetupOut::Nothing
            }
            _ => SetupOut::Nothing,
        }
    }

    pub(super) fn start_key(&mut self, key: crossterm::event::KeyEvent) -> SetupOut {
        use crossterm::event::{KeyCode, KeyModifiers};
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        self.error = None;
        match key.code {
            KeyCode::Enter if !self.field.is_empty() => {
                let typed = keys::clean(&self.field);
                self.field.clear();
                // typed text is a key only when it starts the way a known key does (a paste is told apart by
                // the terminal and takes the other road); anything else is dropped, never held or sent
                if keys::starts_like_key(&typed, 1) && keys::looks_like_key(&typed) {
                    self.key_pasted(typed)
                } else {
                    self.error =
                        Some("That is not a key nexo knows. Pick its service below, then paste the key.".into());
                    SetupOut::Nothing
                }
            }
            KeyCode::Enter | KeyCode::Tab => self.pick(),
            KeyCode::Up => {
                self.move_list(-1);
                SetupOut::Nothing
            }
            KeyCode::Down => {
                self.move_list(1);
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
            // text typed here is a key being pasted without brackets: it waits, hidden, for Enter
            KeyCode::Char(c) if !ctrl => {
                self.field.push(c);
                SetupOut::Nothing
            }
            _ => SetupOut::Nothing,
        }
    }

    /// The save went through: the last screen.
    pub fn saved(&mut self, provider: &str, model: &str) -> bool {
        let mine = self.done.as_ref().is_some_and(|d| d.model == format!("{provider}/{model}"));
        if mine {
            self.busy = None;
            self.error = None;
            self.step = Step::Done;
        }
        mine
    }

    /// The save did not go through: the screen stays where it was and says what failed and what to do.
    /// True when this wizard was waiting for that save (the line is its own); false: nobody is.
    pub fn save_failed(&mut self, provider: &str, model: &str, reason: String) -> bool {
        let mine = self.done.as_ref().is_some_and(|d| d.model == format!("{provider}/{model}"));
        if mine {
            self.done = None;
            self.busy = None;
            self.error = Some(reason);
        }
        mine
    }

    /// What the last screen will say once the save is confirmed.
    pub(super) fn expect_done(&mut self, name: &str) {
        let config = nexo_core::paths::shown(&nexo_core::paths::user_config_file());
        let key = self.key.as_ref().map(|_| nexo_core::paths::shown(&nexo_agent::setup::key_file(name)));
        self.done = Some(Done { model: format!("{name}/{}", self.model), config, key, private: cfg!(unix) });
        self.busy = Some("saving".into());
    }

    /// The line over a list: the wait, the reason it failed, or the question.
    pub(super) fn head_line(&self, text: &str, w: usize, theme: &Theme, spinner: usize) -> Line<'static> {
        if let Some(b) = &self.busy {
            return Line::from(Span::styled(
                format!("{} {b}…", SPINNER[spinner % SPINNER.len()]),
                theme.style(theme.accent),
            ));
        }
        if let Some(e) = &self.error {
            return Line::from(Span::styled(truncate(&format!("✗ {e}"), w.max(8)), theme.style(theme.error)));
        }
        Line::from(Span::raw(truncate(text, w.max(8))))
    }

    /// One plain line on what using this provider costs, only where the preset says so.
    pub(super) fn cost_line(&self) -> Option<&'static str> {
        if self.cfg.login.is_some() {
            Some("Included in your subscription.")
        } else if LOCAL.iter().any(|(p, _)| *p == self.name) {
            Some("Free: it runs on this computer.")
        } else if !self.cfg.pricing.is_empty() {
            Some("Pay as you go: you pay for what you use.")
        } else {
            None
        }
    }

    /// The key step: who wants a key, where to make one, what it costs, the key as dots.
    pub(super) fn key_lines(&self, w: usize, theme: &Theme) -> Vec<Line<'static>> {
        let dim = theme.dim();
        let mut lines = vec![Line::from(vec![
            Span::styled(format!("{} ", self.name), theme.bold(theme.accent)),
            Span::styled("needs a key to answer", dim),
        ])];
        if let Some(url) = keys::page(&self.name) {
            lines.push(Line::from(vec![
                Span::styled("Get one at ", dim),
                Span::styled(url.to_string(), theme.style(theme.accent).add_modifier(Modifier::UNDERLINED)),
            ]));
        }
        match (&self.error, self.cost_line()) {
            (Some(e), _) => {
                lines.push(Line::from(Span::styled(truncate(&format!("✗ {e}"), w.max(8)), theme.style(theme.error))))
            }
            (None, Some(cost)) => lines.push(Line::from(Span::styled(cost, dim))),
            (None, None) => {}
        }
        let shown = if self.field.starts_with('$') || self.field.starts_with('!') {
            self.field.clone()
        } else {
            keys::mask(&self.field)
        };
        lines.push(Line::from(vec![
            Span::styled("› ", theme.style(theme.accent)),
            Span::raw(shown),
            Span::styled("▏", dim),
        ]));
        lines
    }

    /// The card of a problem, padded to the room it takes open, so opening it never cuts it.
    pub(super) fn trouble_lines(&self, w: usize, theme: &Theme) -> Vec<Line<'static>> {
        let Some(p) = &self.problem else { return Vec::new() };
        let (mut lines, _) = crate::render::problem::card(p, 1, self.details, w, theme);
        let room = crate::render::problem::card(p, 1, true, w, theme).0.len();
        while lines.len() < room {
            lines.push(Line::default());
        }
        lines
    }

    /// The last screen: what was saved, where, and three things to try.
    pub(super) fn done_lines(&self, w: usize, theme: &Theme) -> Vec<Line<'static>> {
        let dim = theme.dim();
        let Some(d) = &self.done else { return Vec::new() };
        let mut lines = vec![Line::from(vec![
            Span::styled("✓ ", theme.style(theme.success)),
            Span::styled(truncate(&d.model, w.saturating_sub(12).max(8)), theme.bold(theme.accent)),
            Span::raw(" is ready"),
        ])];
        lines.push(Line::from(Span::styled(truncate(&format!("Saved in {}", d.config), w.max(8)), dim)));
        if let Some(k) = &d.key {
            let text = if d.private {
                format!("The key is in {k}, only you can read it")
            } else {
                format!("The key is in {k}")
            };
            lines.push(Line::from(Span::styled(truncate(&text, w.max(8)), dim)));
        }
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("Try it", theme.bold(theme.fg))));
        let code = theme.code_inline();
        let tip = |head: &'static str, rest: &'static str| {
            Line::from(vec![Span::raw("  "), Span::styled(head, code), Span::styled(rest, Style::default())])
        };
        lines.push(Line::from(vec![Span::raw("  Type a request and press "), Span::styled("Enter", code)]));
        lines.push(tip("/model", " switches to another model"));
        lines.push(tip("/help", " lists every command"));
        lines
    }

    /// The text of the "no models" note of a local server that has nothing installed yet.
    pub(super) fn empty_note(&self, w: usize, theme: &Theme) -> Vec<Line<'static>> {
        let ollama = self.probe.as_ref().is_some_and(|p| p.kind == "ollama") || self.name == "ollama";
        let text = if ollama {
            "No models yet. In a terminal run: ollama pull <name>"
        } else {
            "No models listed. Type a model name to try it."
        };
        wrap_plain(text, w, theme.dim(), &[])
    }

    /// A key is at hand for the request: typed, from a command, a file, the environment or a saved login.
    pub(super) fn has_key(&self) -> bool {
        self.key.is_some() || self.cfg.api_key_cmd.is_some() || self.cfg.resolve_key_for(&self.name).is_some()
    }

    /// What the wait says while the server is asked.
    pub(super) fn wait_text(&self) -> String {
        if self.has_key() {
            "testing the key".to_string()
        } else {
            format!("connecting to {}", host_of(&self.cfg.base_url))
        }
    }

    /// What the highlighted model is, in one line, from what the server and the preset said.
    pub(super) fn model_line(&self) -> Option<String> {
        let probe = self.probe.as_ref()?;
        let id = self.list.current().map(|i| i.value.clone())?;
        let info = probe.models.iter().find(|m| m.id == id)?;
        let suggested = self.cfg.default_model.as_deref() == Some(id.as_str());
        let local = LOCAL.iter().any(|(p, _)| *p == self.name);
        let mut s = String::new();
        if info.reasoning {
            s.push_str("Thinks before it answers: slower, better on hard problems.");
        } else if suggested {
            s.push_str(&format!("The model {} suggests for everyday work.", self.name));
        } else if local {
            s.push_str("Runs on this computer, free.");
        }
        Some(s).filter(|s| !s.is_empty())
    }
}
