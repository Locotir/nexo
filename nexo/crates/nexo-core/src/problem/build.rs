//! The card itself: `Problem`, the context it is built with, and the builders for the errors that are not a provider's.

use serde::{Deserialize, Serialize};

use crate::ProviderConfig;

use super::*;

/// A failure for a person: a title, the likely cause, steps, and the raw text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    /// What failed, in a few words.
    pub title: String,
    /// The likely cause, one or two sentences.
    #[serde(default)]
    pub why: String,
    /// What to do next, in order; a command or a key goes between backticks.
    #[serde(default)]
    pub steps: Vec<String>,
    /// The provider's or the tool's own text (status included), kept but never the headline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What the session knows about the call that failed.
#[derive(Debug, Clone, Default)]
pub struct ProblemCtx {
    /// The provider's name in the config (`openai`, `ollama`).
    pub provider: String,
    pub model: String,
    pub base_url: String,
    /// The environment variable the key is read from.
    pub key_env: Option<String>,
    /// The provider takes a subscription login (`nexo auth login <provider>`).
    pub login: bool,
    /// The model's window in tokens, when known.
    pub window: Option<u32>,
    /// Part of the reply (text, reasoning or a call) had already arrived when the call failed. Only the caller
    /// knows this; it decides whether a dropped connection can say that something was shown and kept.
    pub shown: bool,
}

impl ProblemCtx {
    pub fn from_config(provider: &str, model: &str, cfg: &ProviderConfig) -> ProblemCtx {
        ProblemCtx {
            provider: provider.to_string(),
            model: model.to_string(),
            base_url: cfg.base_url.clone(),
            key_env: cfg.api_key_env.clone(),
            login: cfg.login.is_some(),
            window: cfg.context_window.get(model).copied(),
            shown: false,
        }
    }

    /// The host of the base URL, without scheme, credentials, port or path.
    pub fn host(&self) -> String {
        host_of(&self.base_url)
    }

    /// The server runs on this machine (or on its network): `localhost`, a loopback or private address,
    /// or the usual port of Ollama or LM Studio.
    pub fn is_local(&self) -> bool {
        let h = self.host();
        h == "localhost"
            || h.ends_with(".local")
            || h == "::1"
            || h.starts_with("127.")
            || h.starts_with("192.168.")
            || h.starts_with("10.")
            || h == "0.0.0.0"
            || self.base_url.contains(":11434")
            || self.base_url.contains(":1234")
    }

    /// How the provider is named in a sentence.
    pub(super) fn who(&self) -> String {
        if !self.provider.is_empty() {
            self.provider.clone()
        } else if !self.host().is_empty() {
            self.host()
        } else {
            "the provider".to_string()
        }
    }

    /// The address as shown: no credentials, no query.
    pub(super) fn shown_url(&self) -> String {
        let u = self.base_url.trim();
        let (scheme, rest) = match u.split_once("://") {
            Some((s, r)) => (format!("{s}://"), r),
            None => (String::new(), u),
        };
        let rest = rest.split(['?', '#']).next().unwrap_or("");
        let rest = match rest.split_once('@') {
            Some((a, r)) if !a.contains('/') => r,
            _ => rest,
        };
        format!("{scheme}{rest}")
    }
}

impl Problem {
    /// A problem with a title and the cause, for the errors that are not a provider's.
    pub fn new(title: impl Into<String>, why: impl Into<String>) -> Problem {
        Problem { title: safe(&title.into()), why: safe(&why.into()), steps: Vec::new(), detail: None }
    }

    pub fn step(mut self, step: impl Into<String>) -> Problem {
        self.steps.push(step.into());
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Problem {
        let d = safe(&detail.into());
        self.detail = Some(d).filter(|d| !d.trim().is_empty());
        self
    }

    /// Only text, no structure: its first line is the title. A text that is longer than the title row
    /// shows (or has more lines) is kept whole in the detail, so the card opens to all of it.
    pub fn from_text(text: &str) -> Problem {
        let text = safe(text.trim());
        let title = text.lines().next().unwrap_or("").trim_end().to_string();
        let whole = text.contains('\n') || title.chars().count() > TITLE_ROW;
        let p = Problem::new(title, "");
        if whole {
            p.detail(text)
        } else {
            p
        }
    }

    /// The card as plain lines: title, why, the steps as numbered lines, the detail last.
    pub fn plain(&self) -> String {
        let detail = self.detail.as_deref().filter(|d| !d.is_empty());
        // a detail that opens with the title already holds it (a card made of a plain text)
        let held = detail.is_some_and(|d| d.starts_with(self.title.as_str())) && self.why.is_empty();
        let mut out = Vec::new();
        if !held || !self.steps.is_empty() {
            out.push(self.title.clone());
        }
        if !self.why.is_empty() {
            out.push(self.why.clone());
        }
        out.extend(self.steps.iter().enumerate().map(|(i, s)| format!("{}. {s}", i + 1)));
        if let Some(d) = detail {
            out.push(d.to_string());
        }
        out.join("\n")
    }

    /// The card on one line (`title: cause`), for the places that hold one message: a script reads it.
    pub fn one_line(&self) -> String {
        let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        match (flat(&self.title), flat(&self.why)) {
            (t, w) if w.is_empty() => t,
            (t, w) => format!("{t}: {w}"),
        }
    }

    /// The card for a request that is too long after compacting did not help: the cause is said short, and
    /// the first step is no longer "compact".
    pub fn after_compaction_failed(mut self) -> Problem {
        self.why = "Compacting did not help.".to_string();
        self.steps.retain(|s| !s.contains("/compact"));
        if let Some(first) = self.steps.first_mut() {
            if let Some(rest) = first.strip_prefix("Or ") {
                let mut cs = rest.chars();
                *first = cs.next().map(|c| c.to_uppercase().chain(cs).collect()).unwrap_or_default();
            }
        }
        self
    }
}
