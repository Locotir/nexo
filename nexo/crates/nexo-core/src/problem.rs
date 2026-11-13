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
mod tests;
