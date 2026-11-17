//! The causes: what a provider's error means and what to do about it.

use crate::error::{ErrorKind, ProviderError};

use super::*;

impl Problem {
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

/// The words that can only mean an empty balance, never a per-minute limit. They are what the HTTP layer may
/// use on a 429 to stop retrying (waiting never refills a balance); the soft words below are not safe there,
/// because "exceeded your current quota" is also how Gemini words a per-minute limit.
pub(crate) fn says_no_credit(m: &str) -> bool {
    has(
        &m.to_ascii_lowercase(),
        &[
            "insufficient_quota",
            "insufficient balance",
            "insufficient credit",
            "credit balance",
            "out of credits",
            "any credits",
            "payment required",
            "no credits",
        ],
    )
}

/// The words of an empty balance. The soft ones ("exceeded your current quota") are also how a per-minute
/// limit is worded, so on a rate limit they count only when nothing says the wait will end it.
pub(super) fn no_credit(e: &ProviderError, m: &str) -> bool {
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
    says_no_credit(m) || (has(m, soft) && !((e.kind == ErrorKind::RateLimit || e.status == Some(429)) && comes_back))
}

pub(super) fn model_or(ctx: &ProblemCtx) -> String {
    if ctx.model.is_empty() {
        "the model".to_string()
    } else {
        format!("`{}`", ctx.model)
    }
}

pub(super) fn model_missing(m: &str, status: Option<u16>) -> bool {
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

pub(super) fn no_key(ctx: &ProblemCtx, who: &str) -> Problem {
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

pub(super) fn key_rejected(e: &ProviderError, ctx: &ProblemCtx, who: &str) -> Problem {
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

pub(super) fn unreadable(host: &str, url: &str) -> Problem {
    Problem::new(format!("Could not read the answer from {host}"), "It does not look like a reply from an AI service.")
        .step(format!("Check the address ({url}); it usually ends in `/v1`."))
        .step("`nexo config` shows where it is set.")
}

pub(super) fn network(m: &str, ctx: &ProblemCtx, who: &str, host: &str, url: &str) -> Problem {
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
pub(super) fn detail_of(e: &ProviderError, host: &str, title: &str) -> Option<String> {
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
