//! A failure as the person reads it: what failed in plain words, the likely cause, what to do next,
//! and the provider's own text kept apart as the detail. Built from a [`ProviderError`] and what the
//! session knows (provider, model, address, key source), or by hand for the other errors.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{ErrorKind, ProviderError};
use crate::ProviderConfig;

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
    fn who(&self) -> String {
        if !self.provider.is_empty() {
            self.provider.clone()
        } else if !self.host().is_empty() {
            self.host()
        } else {
            "the provider".to_string()
        }
    }

    /// The address as shown: no credentials, no query.
    fn shown_url(&self) -> String {
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

fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit('@').next().unwrap_or("");
    if let Some(v6) = auth.strip_prefix('[') {
        return v6.split(']').next().unwrap_or("").to_string();
    }
    auth.split(':').next().unwrap_or("").to_ascii_lowercase()
}

fn shorten(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// "45 seconds", "3 minutes".
fn wait_words(d: Duration) -> String {
    let s = d.as_secs().max(1);
    if s < 120 {
        format!("{s} s")
    } else if s < 7200 {
        format!("{} min", s.div_ceil(60))
    } else {
        format!("{} h", s.div_ceil(3600))
    }
}

fn thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A title longer than this many characters is cut on the closed card, so its whole text goes in the detail.
const TITLE_ROW: usize = 60;

fn has(text: &str, words: &[&str]) -> bool {
    words.iter().any(|w| text.contains(w))
}

fn skip_csi(it: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for d in it.by_ref() {
        if ('@'..='~').contains(&d) {
            break;
        }
    }
}

fn skip_string(it: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(d) = it.next() {
        if d == '\x07' || d == '\u{9c}' {
            break;
        }
        if d == '\x1b' {
            it.next_if_eq(&'\\');
            break;
        }
    }
}

/// `s` as text for a screen: escape sequences (colours, links, title changes) and control bytes taken out,
/// a tab as a space; line breaks stay. What a server sends is never trusted to be plain.
pub fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\x1b' => match it.next() {
                Some('[') => skip_csi(&mut it),
                Some(']' | 'P' | '_' | '^' | 'X') => skip_string(&mut it),
                _ => {}
            },
            '\u{9b}' => skip_csi(&mut it),
            '\u{9d}' | '\u{90}' | '\u{9e}' | '\u{9f}' | '\u{98}' => skip_string(&mut it),
            '\t' => out.push(' '),
            // a carriage return alone is a line break; before a line feed it is nothing
            '\r' => {
                if it.peek() != Some(&'\n') {
                    out.push('\n');
                }
            }
            c if c.is_control() && c != '\n' => {}
            c => out.push(c),
        }
    }
    out
}

/// What a text carries beyond the core redaction: the credentials of an address (`user:pass@host`), the
/// value of a `key=` style query parameter, and the key formats of the providers.
fn extra_rules() -> &'static [(regex::Regex, &'static str)] {
    use std::sync::OnceLock;
    static RULES: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    RULES.get_or_init(|| {
        let table: &[(&str, &str)] = &[
            // scheme://user:pass@host
            (r"(?i)\b([a-z][a-z0-9+.-]*://)[^\s/?#]*@", "${1}[redacted]@"),
            // ?key=..., &api_key=..., &access_token=...
            (
                r#"(?i)([?&;]\s*[a-z0-9_.-]*(?:key|token|secret|password|passwd|signature|sig|auth)=)[^&\s)"'<>#,]+"#,
                "${1}[redacted]",
            ),
            // Google and Groq keys
            (r"\bAIza[0-9A-Za-z_-]{20,}", crate::redact::MASK),
            (r"\bgsk_[0-9A-Za-z]{16,}", crate::redact::MASK),
        ];
        table.iter().map(|(p, w)| (regex::Regex::new(p).expect("a fixed pattern"), *w)).collect()
    })
}

/// `s` for a card: [`tidy`], then the secrets taken out. Done when the card is built, so nothing downstream
/// (the transcript, a saved session, headless output, a log) ever holds them.
fn safe(s: &str) -> String {
    let mut out = crate::redact::redact(&tidy(s)).into_owned();
    for (re, with) in extra_rules() {
        if let std::borrow::Cow::Owned(t) = re.replace_all(&out, *with) {
            out = t;
        }
    }
    out
}

/// `m` without the `for url (...)` part reqwest puts in its message: the address is not a cause.
fn without_url(m: &str) -> String {
    match m.find("for url (") {
        Some(i) => {
            let rest = &m[i..];
            match rest.find("): ").or_else(|| rest.find(')')) {
                Some(j) if rest[j..].starts_with("): ") => format!("{}{}", &m[..i], &rest[j + 3..]),
                Some(j) => format!("{}{}", &m[..i], &rest[j + 1..]),
                None => m[..i].to_string(),
            }
        }
        None => m.to_string(),
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

    /// The card for a provider's error, with what the session knows of the call. The title, the cause and
    /// the first step are short (the screen shows only those); the other steps and the raw text open on demand.
    pub fn of(e: &ProviderError, ctx: &ProblemCtx) -> Problem {
        let m = e.message.to_ascii_lowercase();
        let status = e.status;
        let who = ctx.who();
        let host = ctx.host();
        let url = ctx.shown_url();
        let switch = "Or pick another model or provider with `/model`.";
        let mut p = match e.kind {
            ErrorKind::Cancelled => return Problem::new("Cancelled", ""),
            ErrorKind::Network => network(&m, ctx, &who, &host, &url),
            _ if status == Some(402) || no_credit(e, &m) => {
                Problem::new(format!("{who} has no credit left"), "The key works, but the credit is used up.")
                    .step(format!("Add credit on {who}'s billing page."))
                    .step(switch)
            }
            ErrorKind::Auth if status.is_none() && m.contains("no key found") => no_key(ctx, &who),
            ErrorKind::Auth if status.is_none() => {
                Problem::new(format!("Signing in to {who} failed"), "The login is missing, expired or refused.")
                    .step(format!("Run `nexo auth login {}`.", ctx.provider))
                    .step(switch)
            }
            ErrorKind::Auth if status == Some(403) && has(&m, &["country", "region", "territory", "location"]) => {
                Problem::new(format!("{who} is not available where you are"), "It refuses requests from your country.")
                    .step("Pick another provider with `/model`.")
                    .step(format!("Check where {who} is offered."))
            }
            ErrorKind::Auth => key_rejected(e, ctx, &who),
            ErrorKind::RateLimit => {
                let (why, first) = match e.retry_after {
                    Some(d) => (
                        format!("It asks you to wait {}.", wait_words(d)),
                        format!("Wait {}, then send again.", wait_words(d)),
                    ),
                    None => (
                        "Too many requests in a short time.".to_string(),
                        "Wait a minute, then send again.".to_string(),
                    ),
                };
                Problem::new(format!("{who} is limiting requests"), why)
                    .step(first)
                    .step("If it keeps happening, check your plan's limits on the provider's site.")
                    .step(switch)
            }
            ErrorKind::Overloaded => Problem::new(format!("{who} is overloaded"), "Its servers are busy.")
                .step("Wait a minute, then send again.")
                .step(switch),
            ErrorKind::ContextLength => {
                let why = match ctx.window {
                    Some(w) => format!("The model takes up to {} tokens at once.", thousands(w)),
                    None => "This request is bigger than the model can read at once.".to_string(),
                };
                Problem::new(format!("The conversation is too long for {}", model_or(ctx)), why)
                    .step("Run `/compact`.")
                    .step("Or start fresh with `/clear`.")
                    .step("Or pick a model with a larger window: `/model`.")
            }
            ErrorKind::Parse => unreadable(&host, &url),
            _ if m.contains("web page") => unreadable(&host, &url),
            _ if status == Some(404) && !m.contains("model") => {
                Problem::new(format!("{host} has nothing at that address"), "The server answered 404.")
                    .step(format!("Check the address ({url}); it usually ends in `/v1`."))
                    .step("`nexo config` shows where it is set.")
            }
            ErrorKind::BadRequest | ErrorKind::Other if model_missing(&m, status) => {
                let mut p = Problem::new(
                    format!("{who} has no model {} for you", model_or(ctx)),
                    "It may not exist, or your account may have no access to it.",
                );
                if ctx.provider == "ollama" || ctx.base_url.contains(":11434") {
                    p = p.step(format!("Download it with `ollama pull {}`.", ctx.model));
                }
                p.step("Pick another with `/model`.")
            }
            ErrorKind::BadRequest
                if has(
                    &m,
                    &[
                        "content_filter",
                        "content filter",
                        "content_policy",
                        "content management policy",
                        "safety system",
                        "safety settings",
                        "blocked",
                        "moderation",
                        "usage policy",
                        "prohibited",
                        "violat",
                    ],
                ) =>
            {
                Problem::new(format!("{who} refused this request"), "A safety or content filter blocked it.")
                    .step("Reword your last message, then send it again.")
                    .step(switch)
            }
            ErrorKind::BadRequest => Problem::new(
                format!("{who} refused the request"),
                match status {
                    Some(s) => format!("It answered {s}."),
                    None => String::new(),
                },
            )
            .step("Check the model and its settings with `/model`.")
            .step("If it keeps failing, send the details to the provider's support."),
            ErrorKind::Server => Problem::new(
                format!("{who} had an internal error"),
                match status {
                    Some(s) => format!("It answered {s}; the fault is on its side."),
                    None => "The fault is on its side.".to_string(),
                },
            )
            .step("Wait a moment, then send again.")
            .step(switch),
            // the cause is not known: none is guessed
            ErrorKind::Other => {
                Problem::new(format!("The request to {who} failed"), "").step("Send your message again.")
            }
        };
        p.detail = detail_of(e, &host, p.title.as_str());
        p
    }
}

/// The words of an empty balance. The soft ones ("exceeded your current quota") are also how a per-minute
/// limit is worded, so on a rate limit they count only when nothing says the wait will end it.
fn no_credit(e: &ProviderError, m: &str) -> bool {
    let hard = &[
        "insufficient_quota",
        "insufficient balance",
        "insufficient credit",
        "credit balance",
        "out of credits",
        "any credits",
        "payment required",
        "no credits",
    ];
    let soft = &["exceeded your current quota", "plan and billing", "billing details"];
    let comes_back = e.retry_after.is_some()
        || has(
            m,
            &[
                "retry in",
                "retry after",
                "retrydelay",
                "retry_delay",
                "try again in",
                "per minute",
                "perminute",
                "per second",
                "per min",
                "requests per",
                "tokens per",
            ],
        );
    has(m, hard) || (has(m, soft) && !((e.kind == ErrorKind::RateLimit || e.status == Some(429)) && comes_back))
}

fn model_or(ctx: &ProblemCtx) -> String {
    if ctx.model.is_empty() {
        "the model".to_string()
    } else {
        format!("`{}`", ctx.model)
    }
}

fn model_missing(m: &str, status: Option<u16>) -> bool {
    // a request setting the model refuses ("Unsupported parameter: 'temperature' is not supported with
    // this model") is not a missing model: the weak phrases need the model itself to be named as absent
    let setting = has(m, &["parameter", "unsupported value", "unsupported_value"]);
    let named = has(m, &["model not", "model is not", "model `", "model '", "model \"", "models/"]);
    m.contains("model_not_found")
        || (m.contains("model")
            && (has(
                m,
                &[
                    "not found",
                    "does not exist",
                    "not exist",
                    "unknown model",
                    "invalid model",
                    "no such model",
                    "unsupported model",
                    "doesn't exist",
                    "do not have access",
                    "no access",
                ],
            ) || (!setting && named && has(m, &["is not supported", "not available", "could not find"]))))
        || (status == Some(404) && m.contains("model"))
}

fn no_key(ctx: &ProblemCtx, who: &str) -> Problem {
    let p = Problem::new(format!("No key for {who}"), "nexo found no API key, so nothing was sent.");
    let name = &ctx.provider;
    let p = if ctx.login {
        p.step(format!("Run `nexo auth login {name}`."))
    } else {
        p.step(format!("Run `nexo auth login {name} --key`."))
    };
    let p = match &ctx.key_env {
        Some(var) => p.step(format!("Or set `{var}` in your shell and restart nexo.")),
        None => p,
    };
    p.step("Or pick a provider you already use with `/model`.")
}

fn key_rejected(e: &ProviderError, ctx: &ProblemCtx, who: &str) -> Problem {
    let why = match e.status {
        Some(403) => "The key may be invalid or have no access to this model.",
        _ => "The key was not accepted; it may be expired or revoked.",
    };
    let p = Problem::new(format!("{who} rejected the key"), why);
    let mut p = if ctx.login {
        p.step(format!("Run `nexo auth login {}`.", ctx.provider))
    } else {
        p.step(format!("Run `nexo auth login {} --key`.", ctx.provider))
    };
    if let Some(var) = &ctx.key_env {
        p = p.step(format!("If the key comes from `{var}`, check what that variable holds."));
    }
    p.step("Or pick another provider with `/model`.")
}

fn unreadable(host: &str, url: &str) -> Problem {
    Problem::new(format!("Could not read the answer from {host}"), "It does not look like a reply from an AI service.")
        .step(format!("Check the address ({url}); it usually ends in `/v1`."))
        .step("`nexo config` shows where it is set.")
}

fn network(m: &str, ctx: &ProblemCtx, who: &str, host: &str, url: &str) -> Problem {
    // the request address reqwest puts in its message is not a cause: a host named `ssl.example.com` or
    // `api.openai-proxy.com` must not read as a certificate or a proxy fault
    let m = &without_url(m);
    if has(
        m,
        &[
            "dns error",
            "failed to lookup",
            "name or service not known",
            "no such host",
            "nodename nor servname",
            "name resolution",
            "could not resolve",
        ],
    ) {
        return Problem::new(format!("Could not find {host}"), "The address could not be found.")
            .step("Check your internet connection.")
            .step(format!("Check the address ({url}) for typos. `nexo config` shows where it is set."))
            .step("On a VPN or proxy, check that it can look up addresses.");
    }
    if has(m, &["connection refused", "os error 111", "os error 10061", "actively refused"]) {
        if ctx.is_local() {
            let name = if ctx.provider == "ollama" || url.contains(":11434") {
                "Ollama"
            } else if ctx.provider.contains("lmstudio") || ctx.provider.contains("lm-studio") || url.contains(":1234") {
                "LM Studio"
            } else {
                "The local server"
            };
            let start = match name {
                "Ollama" => "Start it with `ollama serve`.".to_string(),
                "LM Studio" => "Start its server in LM Studio, or run `lms server start`.".to_string(),
                _ => "Start your model server.".to_string(),
            };
            let why = if name == "The local server" {
                "The local server does not seem to be running.".to_string()
            } else {
                format!("{name} does not seem to be running.")
            };
            return Problem::new(format!("Nothing answers at {url}"), why)
                .step(start)
                .step("Check that the port in the address is the one it uses.")
                .step("Then send your message again.");
        }
        return Problem::new(format!("{host} refused the connection"), "The server is down, or the address is wrong.")
            .step("Wait a moment, then send again.")
            .step(format!("Check the address ({url})."));
    }
    if has(
        m,
        &["certificate", "handshake", "invalid peer", "unknownissuer", "self-signed", "self signed", "tls ", "ssl"],
    ) {
        return Problem::new(
            format!("The secure connection to {host} failed"),
            "Its security certificate could not be verified.",
        )
        .step("Check the date and time of this computer.")
        .step("On a company network, ask your IT team to trust its certificate.")
        .step("For a server of your own, use an `http://` address.");
    }
    if m.contains("proxy") {
        return Problem::new("The proxy did not let nexo through", "The proxy set in your environment failed.")
            .step("Check `HTTPS_PROXY` and `HTTP_PROXY`.")
            .step("Remove them to connect directly.");
    }
    if has(m, &["timed out", "timeout", "deadline"]) {
        let p = Problem::new(format!("{who} took too long to answer"), "No answer came in time.")
            .step("Send your message again.");
        return if ctx.is_local() {
            p.step("A big local model can take a while to load: wait for it, or try a smaller one with `/model`.")
        } else {
            p.step("Check your connection.")
        };
    }
    if has(m, &["stream cut", "stream ended", "truncated", "connection reset", "broken pipe", "closed"]) {
        return Problem::new("The reply was cut off", format!("The connection to {who} dropped mid-answer."))
            .step("Send your message again. What was already shown is kept.")
            .step("If it keeps happening, check your connection or pick another provider with `/model`.");
    }
    if ctx.is_local() {
        // the internet is not in the way of a server on this machine or its network
        return Problem::new(format!("Could not reach {who}"), "The local server did not answer.")
            .step("Check that the server is running.")
            .step(format!("Check the address ({url})."));
    }
    Problem::new(format!("Could not reach {who}"), "The connection failed before an answer came.")
        .step("Check your internet connection.")
        .step(format!("Check the address ({url})."))
}

/// The raw words: status and address first, then what the provider said. Secrets are taken out.
fn detail_of(e: &ProviderError, host: &str, title: &str) -> Option<String> {
    if e.kind == ErrorKind::Auth && e.status.is_none() && e.message.to_ascii_lowercase().contains("no key found") {
        return None;
    }
    if e.kind == ErrorKind::Cancelled {
        return None;
    }
    let msg = if e.kind == ErrorKind::Parse || title.starts_with("Could not read the answer") {
        shorten(&safe(&e.message), 200)
    } else {
        shorten(&safe(&e.message), 600)
    };
    let head = match (e.status, host.is_empty()) {
        (Some(s), false) => format!("HTTP {s} from {host}"),
        (Some(s), true) => format!("HTTP {s}"),
        (None, _) => String::new(),
    };
    let text = match (head.is_empty(), msg.is_empty() || msg == format!("HTTP {}", e.status.unwrap_or(0))) {
        (true, _) => msg,
        (false, true) => head,
        (false, false) => format!("{head}: {msg}"),
    };
    Some(safe(&text)).filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(provider: &str, model: &str, base: &str) -> ProblemCtx {
        ProblemCtx {
            provider: provider.into(),
            model: model.into(),
            base_url: base.into(),
            key_env: Some(format!("{}_API_KEY", provider.to_ascii_uppercase())),
            login: false,
            window: None,
        }
    }

    fn of(status: u16, body: &str, c: &ProblemCtx) -> Problem {
        Problem::of(&ProviderError::from_http(status, body), c)
    }

    fn net(msg: &str, c: &ProblemCtx) -> Problem {
        Problem::of(&ProviderError::new(ErrorKind::Network, msg), c)
    }

    /// Every card: a title, a cause, 1 to 3 steps, no em dash, none of the banned words.
    fn sane(p: &Problem) {
        assert!(!p.title.is_empty(), "{p:?}");
        assert!(!p.why.is_empty(), "{p:?}");
        assert!((1..=3).contains(&p.steps.len()), "{p:?}");
        // the three rows of the card fit 71 columns, with room on the title row for a count and a chevron
        let n = |s: &str| s.replace('`', "").chars().count();
        assert!(n(&p.title) <= 63 && n(&p.why) <= 69 && n(&p.steps[0]) <= 69, "{p:?}");
        let all = p.plain();
        assert!(!all.contains('—'), "{all}");
        assert!(!format!("{} {} {}", p.title, p.why, p.steps.join(" ")).contains('!'), "{all}");
        for w in ["simply", "just "] {
            assert!(!all.to_ascii_lowercase().contains(w), "{w}: {all}");
        }
    }

    #[test]
    fn no_key_names_the_variable_and_the_login() {
        let mut c = ctx("openai", "gpt-5", "https://api.openai.com/v1");
        let e = ProviderError::new(ErrorKind::Auth, "no key (no key found: set OPENAI_API_KEY)");
        let p = Problem::of(&e, &c);
        sane(&p);
        assert_eq!(p.title, "No key for openai");
        assert!(p.steps[0].contains("`nexo auth login openai --key`") && p.steps[1].contains("`OPENAI_API_KEY`"));
        assert_eq!(p.detail, None);
        c.login = true;
        let p = Problem::of(&e, &c);
        assert!(p.steps[0].contains("`nexo auth login openai`") && !p.steps[0].contains("--key"), "{p:?}");
    }

    #[test]
    fn a_rejected_key_is_not_a_missing_one() {
        let c = ctx("openai", "gpt-5", "https://api.openai.com/v1");
        let body = r#"{"error":{"message":"Incorrect API key provided: sk-proj-abc. You can find your API key at https://platform.openai.com/account/api-keys.","type":"invalid_request_error","param":null,"code":"invalid_api_key"}}"#;
        let p = of(401, body, &c);
        sane(&p);
        assert_eq!(p.title, "openai rejected the key");
        assert!(p.steps.iter().any(|s| s.contains("`OPENAI_API_KEY`")));
        let d = p.detail.unwrap();
        assert!(d.starts_with("HTTP 401 from api.openai.com: ") && d.contains("Incorrect API key"), "{d}");
        let p = of(
            401,
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
            &ctx("anthropic", "claude", "https://api.anthropic.com"),
        );
        assert_eq!(p.title, "anthropic rejected the key");
        let p = of(
            403,
            r#"{"error":{"code":403,"message":"Method doesn't allow unregistered callers","status":"PERMISSION_DENIED"}}"#,
            &ctx("gemini", "g", "https://generativelanguage.googleapis.com"),
        );
        sane(&p);
        assert!(p.why.contains("no access"), "{p:?}");
        // a login-capable provider points at the login
        let mut c = ctx("copilot", "m", "https://api.githubcopilot.com");
        c.login = true;
        assert!(of(401, "bad", &c).steps[0].contains("`nexo auth login copilot`"));
    }

    #[test]
    fn a_login_that_fails_is_told_apart() {
        let c = ctx("codex", "m", "https://chatgpt.com");
        let p = Problem::of(&ProviderError::new(ErrorKind::Auth, "the refresh token was rejected"), &c);
        sane(&p);
        assert_eq!(p.title, "Signing in to codex failed");
        assert!(p.steps[0].contains("`nexo auth login codex`"));
    }

    #[test]
    fn no_credit_in_every_vendors_words() {
        let cases = [
            (
                402,
                r#"{"error":{"message":"Insufficient Balance","type":"unknown_error","param":null,"code":"invalid_request_error"}}"#,
                "deepseek",
            ),
            (
                429,
                r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details.","type":"insufficient_quota","param":null,"code":"insufficient_quota"}}"#,
                "openai",
            ),
            (
                400,
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing to upgrade or purchase credits."}}"#,
                "anthropic",
            ),
            (
                402,
                r#"{"error":{"code":402,"message":"Insufficient credits. Add more using https://openrouter.ai/settings/credits"}}"#,
                "openrouter",
            ),
            (
                429,
                r#"{"error":{"code":429,"message":"You exceeded your current quota, please check your plan and billing details. For more information on this error, head to: https://ai.google.dev/gemini-api/docs/rate-limits.","status":"RESOURCE_EXHAUSTED"}}"#,
                "gemini",
            ),
            (
                403,
                r#"{"code":"The caller does not have permission","error":"Your newly created team doesn't have any credits yet. Please purchase credits on the console."}"#,
                "xai",
            ),
            (402, "", "mistral"),
        ];
        for (status, body, name) in cases {
            let p = of(status, body, &ctx(name, "m", "https://api.example.com/v1"));
            sane(&p);
            assert_eq!(p.title, format!("{name} has no credit left"), "{status} {body}");
            assert!(p.steps[0].contains("billing page"));
            assert!(p.detail.as_deref().unwrap().starts_with(&format!("HTTP {status}")), "{p:?}");
        }
    }

    #[test]
    fn a_rate_limit_says_the_wait_when_it_is_known() {
        let c = ctx("groq", "llama", "https://api.groq.com/openai/v1");
        let body = r#"{"error":{"message":"Rate limit reached for model `llama` in organization `org` on tokens per minute (TPM): Limit 6000, Used 5900, Requested 500. Please try again in 6s.","type":"tokens","code":"rate_limit_exceeded"}}"#;
        let mut e = ProviderError::from_http(429, body);
        let p = Problem::of(&e, &c);
        sane(&p);
        assert_eq!(p.title, "groq is limiting requests");
        assert!(p.steps[0].starts_with("Wait a minute"), "{p:?}");
        e.retry_after = Some(Duration::from_secs(45));
        let p = Problem::of(&e, &c);
        assert!(p.why.contains("wait 45 s") && p.steps[0].starts_with("Wait 45 s"), "{p:?}");
        e.retry_after = Some(Duration::from_secs(600));
        assert!(Problem::of(&e, &c).steps[0].starts_with("Wait 10 min"));
    }

    #[test]
    fn overloaded_and_server_errors() {
        let c = ctx("anthropic", "claude", "https://api.anthropic.com");
        let p = of(529, r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#, &c);
        sane(&p);
        assert_eq!(p.title, "anthropic is overloaded");
        assert!(p.detail.unwrap().starts_with("HTTP 529"));
        let p = of(503, "Service Unavailable", &c);
        assert_eq!(p.title, "anthropic is overloaded");
        let p = of(
            500,
            r#"{"error":{"message":"The server had an error while processing your request. Sorry about that!","type":"server_error"}}"#,
            &ctx("openai", "m", "https://api.openai.com/v1"),
        );
        sane(&p);
        assert_eq!(p.title, "openai had an internal error");
        assert!(p.why.contains("500"));
        assert_eq!(of(502, "", &c).title, "anthropic had an internal error");
    }

    #[test]
    fn too_long_names_the_window_and_the_way_out() {
        let mut c = ctx("openai", "gpt-4o", "https://api.openai.com/v1");
        let body = r#"{"error":{"message":"This model's maximum context length is 128000 tokens. However, your messages resulted in 140000 tokens. Please reduce the length of the messages.","type":"invalid_request_error","param":"messages","code":"context_length_exceeded"}}"#;
        let p = of(400, body, &c);
        sane(&p);
        assert_eq!(p.title, "The conversation is too long for `gpt-4o`");
        assert!(p.why.starts_with("This request is bigger"), "{p:?}");
        c.window = Some(128000);
        let p = of(400, body, &c);
        assert!(p.why.contains("128,000 tokens"), "{p:?}");
        assert!(p.steps[0].contains("`/compact`") && p.steps[2].contains("`/model`"));
        let p = of(
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 213000 tokens > 200000 maximum"}}"#,
            &c,
        );
        assert!(p.title.starts_with("The conversation is too long"));
    }

    #[test]
    fn a_missing_model_points_at_the_picker() {
        let c = ctx("openai", "gpt-5-turbo", "https://api.openai.com/v1");
        let p = of(
            404,
            r#"{"error":{"message":"The model `gpt-5-turbo` does not exist or you do not have access to it.","type":"invalid_request_error","param":null,"code":"model_not_found"}}"#,
            &c,
        );
        sane(&p);
        assert_eq!(p.title, "openai has no model `gpt-5-turbo` for you");
        assert!(p.steps.last().unwrap().contains("`/model`"));
        let p = of(
            400,
            r#"{"error":{"message":"Model Not Exist","type":"invalid_request_error"}}"#,
            &ctx("deepseek", "deepseek-chta", "https://api.deepseek.com"),
        );
        assert!(p.title.contains("no model `deepseek-chta`"), "{p:?}");
        let p = of(
            404,
            r#"{"error":{"message":"models/gemini-9 is not found for API version v1beta, or is not supported for generateContent.","status":"NOT_FOUND"}}"#,
            &ctx("gemini", "gemini-9", "https://generativelanguage.googleapis.com"),
        );
        assert!(p.title.contains("no model `gemini-9`"), "{p:?}");
        // Ollama: the model has to be pulled first
        let p = of(
            404,
            r#"{"error":{"message":"model 'llama9' not found","type":"api_error"}}"#,
            &ctx("ollama", "llama9", "http://localhost:11434/v1"),
        );
        assert_eq!(p.steps[0], "Download it with `ollama pull llama9`.");
        // a 404 that says nothing of a model is the address
        let p = of(404, "404 page not found", &ctx("custom", "m", "https://example.org/api"));
        sane(&p);
        assert!(p.title.starts_with("example.org has nothing"), "{p:?}");
    }

    #[test]
    fn a_filter_refusal_asks_for_other_words() {
        let c = ctx("openai", "gpt-5", "https://api.openai.com/v1");
        let p = of(
            400,
            r#"{"error":{"message":"Your request was rejected as a result of our safety system. Your prompt may contain text that is not allowed by our safety system.","type":"invalid_request_error","code":"content_filter"}}"#,
            &c,
        );
        sane(&p);
        assert_eq!(p.title, "openai refused this request");
        let p = of(
            400,
            r#"{"error":{"code":400,"message":"Content has been blocked because it violates the safety settings.","status":"INVALID_ARGUMENT"}}"#,
            &ctx("gemini", "g", "https://generativelanguage.googleapis.com"),
        );
        assert_eq!(p.title, "gemini refused this request");
        // another 400 is the generic card with the provider's words in the detail
        let p = of(
            400,
            r#"{"error":{"message":"messages: text content blocks must be non-empty","type":"invalid_request_error"}}"#,
            &ctx("anthropic", "c", "https://api.anthropic.com"),
        );
        sane(&p);
        assert_eq!(p.title, "anthropic refused the request");
        assert!(p.detail.unwrap().contains("non-empty"));
        let p = of(403, r#"{"error":{"message":"Country, region, or territory not supported"}}"#, &c);
        assert_eq!(p.title, "openai is not available where you are");
    }

    #[test]
    fn a_local_server_that_is_off_says_how_to_start_it() {
        let msg = "error sending request for url (http://localhost:11434/v1/chat/completions): client error (Connect): tcp connect error: Connection refused (os error 111)";
        let p = net(msg, &ctx("ollama", "llama3", "http://localhost:11434/v1"));
        sane(&p);
        assert_eq!(p.title, "Nothing answers at http://localhost:11434/v1");
        assert!(p.why.starts_with("Ollama does not seem to be running"));
        assert_eq!(p.steps[0], "Start it with `ollama serve`.");
        assert!(p.detail.unwrap().contains("os error 111"));
        let p = net(msg, &ctx("lmstudio", "m", "http://127.0.0.1:1234/v1"));
        assert!(p.steps[0].contains("`lms server start`"), "{p:?}");
        let p = net(msg, &ctx("llamacpp", "m", "http://127.0.0.1:8080/v1"));
        assert!(p.why.starts_with("The local server"), "{p:?}");
        // a remote host that refuses is not "is it running?"
        let p = net(msg, &ctx("custom", "m", "https://llm.example.com/v1"));
        assert_eq!(p.title, "llm.example.com refused the connection");
    }

    #[test]
    fn network_causes_are_told_apart() {
        let c = ctx("openai", "gpt-5", "https://api.openai.com/v1");
        let dns = net("error sending request for url (https://api.openai.com/v1/chat/completions): client error (Connect): dns error: failed to lookup address information: Name or service not known", &c);
        sane(&dns);
        assert_eq!(dns.title, "Could not find api.openai.com");
        let tls = net("error sending request: client error (Connect): invalid peer certificate: UnknownIssuer", &c);
        sane(&tls);
        assert_eq!(tls.title, "The secure connection to api.openai.com failed");
        let proxy = net(
            "error sending request: client error (Connect): proxy connect error: 407 Proxy Authentication Required",
            &c,
        );
        sane(&proxy);
        assert_eq!(proxy.title, "The proxy did not let nexo through");
        let slow = net("error sending request: operation timed out", &c);
        sane(&slow);
        assert_eq!(slow.title, "openai took too long to answer");
        let cut = net("stream cut: error decoding response body: connection reset", &c);
        sane(&cut);
        assert_eq!(cut.title, "The reply was cut off");
        let cut = net("the stream ended before the reply began", &c);
        assert_eq!(cut.title, "The reply was cut off");
        let other = net("something odd", &c);
        sane(&other);
        assert_eq!(other.title, "Could not reach openai");
        // every one keeps the raw text
        for p in [dns, tls, proxy, slow, cut, other] {
            assert!(p.detail.is_some(), "{p:?}");
        }
    }

    #[test]
    fn an_answer_that_is_not_an_api_shows_its_first_200_chars() {
        let c = ctx("custom", "m", "http://localhost:8000");
        let junk = format!("not JSON: expected value at line 1 column 1 {}", "x".repeat(400));
        let p = Problem::of(&ProviderError::new(ErrorKind::Parse, junk), &c);
        sane(&p);
        assert_eq!(p.title, "Could not read the answer from localhost");
        let d = p.detail.unwrap();
        assert!(d.chars().count() <= 200, "{}", d.chars().count());
        assert!(p.steps[0].contains("http://localhost:8000") && p.steps[0].contains("/v1"));
        // a web page where the API should be
        let p = of(404, "the server answered with a web page, not an API reply (check the base URL)", &c);
        assert!(p.title.starts_with("Could not read the answer"), "{p:?}");
    }

    /// A synthetic key: joined at run time so no file holds a literal one.
    fn fake(prefix: &str, body: &str) -> String {
        [prefix, body].concat()
    }

    #[test]
    fn the_raw_text_never_carries_a_secret_or_the_address_credentials() {
        // a reqwest-shaped message that holds the credentialed address, as the local-server test's does
        let c = ctx("custom", "m", "https://user:hunter2@llm.example.com/v1?key=zzz9");
        let msg = "error sending request for url (https://user:hunter2@llm.example.com/v1/chat/completions?key=zzz9&api_key=yyy8&model=x): client error (Connect): tcp connect error: Connection refused (os error 111)";
        let p = net(msg, &c);
        let all = p.plain();
        for secret in ["hunter2", "zzz9", "yyy8"] {
            assert!(!all.contains(secret), "{secret} in {all}");
        }
        assert!(
            p.detail.as_deref().unwrap().contains("llm.example.com/v1/chat/completions"),
            "the address stays: {all}"
        );
        assert!(all.contains("model=x"), "only the secret parameters go: {all}");
        // a JSON body that echoes a key, in each provider's format
        let keys = [
            fake("s", "k-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789"),
            fake("AI", "zaSyA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q"),
            fake("gs", "k_abcdefghijklmnopqrstuvwxyz0123456789"),
            "Bearer abcdef1234567890abcdef".to_string(),
        ];
        for k in &keys {
            let body = format!(r#"{{"error":{{"message":"Incorrect key {k} given","type":"invalid_request_error"}}}}"#);
            let p = of(401, &body, &c);
            let needle = k.strip_prefix("Bearer ").unwrap_or(k);
            assert!(!p.plain().contains(needle), "{needle} in {}", p.plain());
            assert!(!serde_json::to_string(&p).unwrap().contains(needle));
        }
        let echoed = r#"{"api_key":"qq11ww22ee33","error":{"message":"bad header x-api-key: rr44tt55yy66 and ?apikey=uu77ii88"}}"#;
        let all = of(401, echoed, &c).plain();
        for secret in ["qq11ww22ee33", "rr44tt55yy66", "uu77ii88"] {
            assert!(!all.contains(secret), "{secret} in {all}");
        }
        // the address a step shows has neither
        assert!(!c.shown_url().contains("hunter2") && !c.shown_url().contains("zzz9"), "{}", c.shown_url());
        assert_eq!(c.host(), "llm.example.com");
        // a text made by hand is cleaned the same way
        let t =
            Problem::from_text(&format!("failed at https://bob:pw123@host.example/x?token=abc999 with {}", keys[0]));
        assert!(
            !t.plain().contains("pw123") && !t.plain().contains("abc999") && !t.plain().contains(&keys[0]),
            "{t:?}"
        );
    }

    #[test]
    fn an_unsupported_setting_is_not_a_missing_model() {
        let c = ctx("openai", "o3", "https://api.openai.com/v1");
        let p = of(
            400,
            r#"{"error":{"message":"Unsupported parameter: 'temperature' is not supported with this model.","type":"invalid_request_error","param":"temperature","code":"unsupported_parameter"}}"#,
            &c,
        );
        sane(&p);
        assert_eq!(p.title, "openai refused the request", "{p:?}");
        assert!(p.detail.unwrap().contains("'temperature' is not supported"));
        // a model that is really absent, in the wording of "not available", is still told apart
        let p = of(400, r#"{"error":{"message":"The model `o9` is not available for your account."}}"#, &c);
        assert!(p.title.contains("no model `o3`"), "{p:?}");
    }

    #[test]
    fn a_text_made_of_many_words_keeps_them_all_in_the_detail() {
        let text = "mcp server `files` failed: could not start\nspawn error: No such file or directory\nPATH was /usr/bin:/bin";
        let p = Problem::from_text(text);
        assert_eq!(p.title, "mcp server `files` failed: could not start");
        assert_eq!(p.detail.as_deref(), Some(text), "the whole text opens from the card");
        assert_eq!(p.plain(), text, "and the plain form is the text, once");
        let long = format!("cannot read the file: {}", "word ".repeat(40));
        let p = Problem::from_text(&long);
        assert_eq!(p.detail.as_deref(), Some(long.trim()));
        assert_eq!(p.plain(), long.trim());
        // a short line is only a title
        let p = Problem::from_text("cannot read session abc");
        assert_eq!(p.detail, None);
        assert_eq!(p.one_line(), "cannot read session abc");
    }

    #[test]
    fn the_address_in_the_message_is_not_a_cause() {
        // reqwest puts the request address in its message; a host with "proxy" or "ssl" in it is not a fault
        for (base, host) in [
            ("https://api.openai-proxy.com/v1", "api.openai-proxy.com"),
            ("https://ssl.example.com/v1", "ssl.example.com"),
        ] {
            let c = ctx("custom", "m", base);
            let msg = format!("error sending request for url ({base}/chat/completions): operation timed out");
            let p = net(&msg, &c);
            sane(&p);
            assert_eq!(p.title, "custom took too long to answer", "{host}: {p:?}");
            let p = net(&format!("error sending request for url ({base}/chat/completions)"), &c);
            assert_eq!(p.title, "Could not reach custom", "{host}: {p:?}");
        }
        // the cause itself still counts
        let c = ctx("custom", "m", "https://api.openai-proxy.com/v1");
        let p = net("error sending request for url (https://api.openai-proxy.com/v1/x): proxy connect error: 407", &c);
        assert_eq!(p.title, "The proxy did not let nexo through");
    }

    #[test]
    fn a_rate_limit_that_says_it_comes_back_is_not_an_empty_balance() {
        let c = ctx("gemini", "gemini-2.5-pro", "https://generativelanguage.googleapis.com");
        let body = r#"{"error":{"code":429,"message":"You exceeded your current quota, please check your plan and billing details. For more information on this error, head to: https://ai.google.dev/gemini-api/docs/rate-limits.","status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.QuotaFailure","violations":[{"quotaMetric":"generativelanguage.googleapis.com/generate_content_free_tier_requests","quotaId":"GenerateRequestsPerMinutePerProjectPerModel-FreeTier"}]},{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"37s"}]}}"#;
        let p = of(429, body, &c);
        sane(&p);
        assert_eq!(p.title, "gemini is limiting requests", "{p:?}");
        // the header's wait, with the quota wording alone
        let mut e = ProviderError::from_http(
            429,
            r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details."}}"#,
        );
        assert_eq!(Problem::of(&e, &c).title, "gemini has no credit left", "nothing says it comes back");
        e.retry_after = Some(Duration::from_secs(30));
        let p = Problem::of(&e, &c);
        assert_eq!(p.title, "gemini is limiting requests");
        assert!(p.steps[0].starts_with("Wait 30 s"), "{p:?}");
        // a plain "insufficient_quota" stays an empty balance even with a wait
        e.message = r#"{"error":{"code":"insufficient_quota","message":"You exceeded your current quota"}}"#.into();
        assert_eq!(Problem::of(&e, &c).title, "gemini has no credit left");
    }

    #[test]
    fn a_request_too_long_after_compacting_says_it_short_and_starts_with_a_step() {
        let mut c = ctx("openai", "gpt-4o", "https://api.openai.com/v1");
        c.window = Some(128000);
        let body = r#"{"error":{"message":"This model's maximum context length is 128000 tokens.","code":"context_length_exceeded"}}"#;
        let p = of(400, body, &c).after_compaction_failed();
        sane(&p);
        assert_eq!(p.why, "Compacting did not help.");
        assert!(!p.steps.iter().any(|s| s.contains("/compact")), "{p:?}");
        assert!(!p.steps[0].starts_with("Or "), "{p:?}");
        assert_eq!(p.steps[0], "Start fresh with `/clear`.");
        assert!(p.steps[1].starts_with("Or pick a model") && p.steps[1].contains("`/model`"), "{p:?}");
    }

    #[test]
    fn a_server_on_this_machine_that_fails_in_an_unknown_way_does_not_blame_the_internet() {
        let p = net("something odd", &ctx("ollama", "llama3", "http://localhost:11434/v1"));
        sane(&p);
        assert!(!p.plain().to_ascii_lowercase().contains("internet"), "{p:?}");
        assert!(p.steps[0].contains("running"), "{p:?}");
        let p = net("something odd", &ctx("openai", "m", "https://api.openai.com/v1"));
        assert!(p.steps[0].contains("internet"), "{p:?}");
    }

    #[test]
    fn what_a_server_said_has_no_escape_or_control_bytes() {
        assert_eq!(tidy("\x1b[31mred\x1b[0m ok\x07\x08"), "red ok");
        assert_eq!(tidy("a\x1b]0;owned\x07b\x1b]8;;http://x\x1b\\link"), "ablink");
        assert_eq!(tidy("one\r\ntwo\rthree\tfour\x00"), "one\ntwo\nthree four");
        assert_eq!(tidy("\u{9b}31mx\u{85}y"), "xy");
        let body = "{\"error\":{\"message\":\"\\u001b[31mbad\\u001b]0;pwned\\u0007 key\"}}";
        let e = ProviderError::from_http(400, body);
        let p = Problem::of(&e, &ctx("openai", "m", "https://api.openai.com/v1"));
        assert!(!p.plain().contains('\x1b') && !p.plain().chars().any(|c| c.is_control() && c != '\n'), "{p:?}");
        let raw = ProviderError::new(ErrorKind::Other, "x \x1b[2J\x1b[Hscreen wiped");
        let p = Problem::of(&raw, &ctx("openai", "m", "https://api.openai.com/v1"));
        assert!(!p.plain().contains('\x1b'), "{:?}", p.plain());
        assert!(!Problem::from_text("\x1b[2Jboom").plain().contains('\x1b'));
    }

    #[test]
    fn gemini_and_anthropic_failures_in_their_own_words() {
        let g = ctx("gemini", "gemini-2.5-pro", "https://generativelanguage.googleapis.com");
        let a = ctx("anthropic", "claude-sonnet-4-5", "https://api.anthropic.com");
        let p = of(
            503,
            r#"{"error":{"code":503,"message":"The model is overloaded. Please try again later.","status":"UNAVAILABLE"}}"#,
            &g,
        );
        sane(&p);
        assert_eq!(p.title, "gemini is overloaded");
        let p = of(
            400,
            r#"{"error":{"code":400,"message":"The input token count (1196265) exceeds the maximum number of tokens allowed (1048575).","status":"INVALID_ARGUMENT"}}"#,
            &g,
        );
        sane(&p);
        assert!(p.title.starts_with("The conversation is too long"), "{p:?}");
        let p = of(
            429,
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed your organization's rate limit of 30,000 input tokens per minute (org: 1234, model: claude-sonnet-4-5). For details, refer to: https://docs.claude.com/en/api/rate-limits."}}"#,
            &a,
        );
        sane(&p);
        assert_eq!(p.title, "anthropic is limiting requests");
        let p = of(
            413,
            r#"{"type":"error","error":{"type":"request_too_large","message":"Request exceeds the maximum allowed number of bytes."}}"#,
            &a,
        );
        sane(&p);
        assert!(p.title.starts_with("The conversation is too long"), "{p:?}");
        let p = of(
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 213000 tokens > 200000 maximum"}}"#,
            &a,
        );
        assert!(p.title.contains("too long"), "{p:?}");
    }

    #[test]
    fn one_line_is_the_title_and_the_cause_on_a_single_line() {
        let p = Problem::new("It broke", "A thing\nfailed.").step("Do it.").detail("raw\ntext");
        assert_eq!(p.one_line(), "It broke: A thing failed.");
        assert_eq!(Problem::new("Only a title", "").one_line(), "Only a title");
    }

    #[test]
    fn plain_lists_title_why_numbered_steps_then_the_detail() {
        let p =
            Problem::new("It broke", "A thing failed.").step("Do `this`.").step("Then that.").detail("HTTP 500: boom");
        assert_eq!(p.plain(), "It broke\nA thing failed.\n1. Do `this`.\n2. Then that.\nHTTP 500: boom");
        assert_eq!(Problem::from_text("only a line").plain(), "only a line");
    }

    #[test]
    fn the_notice_round_trips_with_and_without_a_problem() {
        use crate::agent::{AgentEvent, Level};
        let p = Problem::new("t", "w").step("s");
        let with = AgentEvent::Notice { level: Level::Error, text: p.plain(), problem: Some(Box::new(p)) };
        let json = serde_json::to_string(&with).unwrap();
        assert!(json.contains("\"problem\""), "{json}");
        assert_eq!(serde_json::from_str::<AgentEvent>(&json).unwrap(), with);
        let without = AgentEvent::Notice { level: Level::Info, text: "hi".into(), problem: None };
        let json = serde_json::to_string(&without).unwrap();
        assert_eq!(json, r#"{"event":"notice","level":"info","text":"hi"}"#, "the shape did not change");
        assert_eq!(serde_json::from_str::<AgentEvent>(&json).unwrap(), without);
    }

    #[test]
    fn a_problem_round_trips_and_old_json_still_reads() {
        let p = Problem::new("t", "w").step("s").detail("d");
        let back: Problem = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
        let bare: Problem = serde_json::from_str(r#"{"title":"t"}"#).unwrap();
        assert_eq!(bare, Problem::new("t", ""));
        assert!(!serde_json::to_string(&Problem::new("t", "")).unwrap().contains("detail"));
    }
}
