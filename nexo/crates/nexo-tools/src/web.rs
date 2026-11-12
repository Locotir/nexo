//! `web_search` and `web_fetch`: the web, for what the repository does not hold.
//!
//! `web_search` asks one of several backends and answers with a numbered list
//! of titles, URLs and snippets. With `search = "auto"` (the default) it takes
//! the first backend whose key is present (Tavily, Brave, Exa), else a SearXNG
//! instance when `url` is set, else DuckDuckGo, which needs no key; when a
//! keyed backend fails it falls back to DuckDuckGo and says so.
//!
//! `web_fetch` reads one URL: HTML becomes readable Markdown (title first,
//! scripts, styles, navigation and forms dropped, links made absolute); text,
//! Markdown, JSON and XML come back as they are; anything else is refused. Long
//! pages come in pieces: the output ends with the `start` of the next one.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use nexo_core::config::WebConfig;
use nexo_core::tool::Check;
use nexo_core::{Compact, Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use regex::Regex;
use reqwest::Url;
use serde_json::{json, Value};

use crate::util::{int_arg, str_arg};

const DEFAULT_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) nexo/0.1";
const DDG_BASE: &str = "https://html.duckduckgo.com";
const MAX_BODY: usize = 5 * 1024 * 1024;
const DEFAULT_RESULTS: u64 = 8;
const MAX_RESULTS: u64 = 20;
const DEFAULT_CHARS: u64 = 20_000;
const MAX_CHARS: u64 = 100_000;

/// Where keys are read from: the process environment, or a fixed map in tests.
type Env = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Why a search backend failed. `refused` is set when the backend itself said no (a block, a rate
/// limit, a bad key, a server error, no connection, a timeout): asking again will not help, and the
/// query is not at fault.
struct Fail {
    text: String,
    refused: bool,
}

impl Fail {
    fn refused(text: String) -> Fail {
        Fail { text, refused: true }
    }
}

impl From<String> for Fail {
    fn from(text: String) -> Fail {
        Fail { text, refused: false }
    }
}

/// Refusals in a row that open the breaker, and how long it stays open after the last one.
const BREAKER_AFTER: u32 = 2;
const BREAKER_OPEN: Duration = Duration::from_secs(120);
const NO_RETRY: &str = "Web search is not available right now: do not retry it; go on with what you have, or open a URL you already know with web_fetch.";
const BREAKER_SAID: &str = "web_search: not available in this session (the backend refused 2 requests in a row). Do not call it again; go on with what you have, or open a URL you already know with web_fetch.";

/// Counts refused searches in a row. Open while `BREAKER_AFTER` refusals have come in a row and the
/// last one is younger than `open_for`. The first call after it closes is a probe: one more refusal
/// opens it again.
struct Breaker {
    /// (refusals in a row, clock reading at the last one)
    state: Mutex<(u32, Duration)>,
    /// Time since an arbitrary start; a test replaces it.
    now: Arc<dyn Fn() -> Duration + Send + Sync>,
    open_for: Duration,
}

impl Breaker {
    fn new() -> Breaker {
        let start = std::time::Instant::now();
        Breaker {
            state: Mutex::new((0, Duration::ZERO)),
            now: Arc::new(move || start.elapsed()),
            open_for: BREAKER_OPEN,
        }
    }

    fn is_open(&self) -> bool {
        let (n, at) = *self.state.lock().unwrap_or_else(|e| e.into_inner());
        n >= BREAKER_AFTER && (self.now)().saturating_sub(at) < self.open_for
    }

    fn refused(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.0 = s.0.saturating_add(1);
        s.1 = (self.now)();
    }

    fn reset(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).0 = 0;
    }
}

fn process_env() -> Env {
    Arc::new(|k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
}

/// A redirect may not lead from a public address to a local or private one (this machine, the
/// local network, a cloud's metadata service): a page could otherwise read them for the model.
fn redirect_ok(previous: &[Url], next: &Url) -> bool {
    previous.first().is_some_and(private_host) || !private_host(next)
}

/// `localhost`, or an address of loopback, private, link-local, shared or unique-local ranges.
fn private_host(url: &Url) -> bool {
    let Some(h) = url.host_str() else { return false };
    let h = h.trim_start_matches('[').trim_end_matches(']').trim_end_matches('.').to_ascii_lowercase();
    match h.parse::<std::net::IpAddr>() {
        Ok(ip) => private_ip(ip),
        Err(_) => h == "localhost" || h.ends_with(".localhost"),
    }
}

fn private_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v) => {
            let o = v.octets();
            v.is_loopback()
                || v.is_private()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_broadcast()
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || o[0] == 0
        }
        std::net::IpAddr::V6(v) => {
            let g = v.segments();
            let s = g[0];
            let v4 = |hi: u16, lo: u16| {
                let [a, b] = hi.to_be_bytes();
                let [c, d] = lo.to_be_bytes();
                private_ip(std::net::IpAddr::V4(std::net::Ipv4Addr::new(a, b, c, d)))
            };
            v.is_loopback()
                || v.is_unspecified()
                || (s & 0xfe00) == 0xfc00
                || (s & 0xffc0) == 0xfe80
                || (s & 0xffc0) == 0xfec0
                || v.to_ipv4_mapped().is_some_and(|m| private_ip(std::net::IpAddr::V4(m)))
                // an IPv4 address inside: `::a.b.c.d`, NAT64 `64:ff9b::/96`, 6to4 `2002::/16`, Teredo
                || (g[..6].iter().all(|&x| x == 0) && v4(g[6], g[7]))
                || (s == 0x64 && g[1] == 0xff9b && g[2..6].iter().all(|&x| x == 0) && v4(g[6], g[7]))
                || (s == 0x64 && g[1] == 0xff9b && g[2] == 1)
                || (s == 0x2002 && v4(g[1], g[2]))
                || (s == 0x2001 && g[1] == 0 && v4(!g[6], !g[7]))
        }
    }
}

/// The addresses a name resolved to, when every one is public; `localhost` and `*.localhost`,
/// local by their name (the person saw it when asked), keep theirs.
fn public_only(host: &str, addrs: Vec<std::net::SocketAddr>) -> Result<Vec<std::net::SocketAddr>, String> {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    if h == "localhost" || h.ends_with(".localhost") {
        return Ok(addrs);
    }
    match addrs.iter().find(|a| private_ip(a.ip())) {
        Some(a) => Err(format!("{host} resolves to {}, a local or private address", a.ip())),
        None => Ok(addrs),
    }
}

/// The resolver of `web_fetch`: a name is checked before anything connects to it, on the first
/// request and every redirect, so a public-looking name cannot lead to this machine or the local
/// network. An address written as such (`http://127.0.0.1/`) is not resolved and stays allowed.
struct PublicResolver;

impl reqwest::dns::Resolve for PublicResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let addrs = public_only(&host, addrs)?;
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

fn builder(cfg: &WebConfig) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(cfg.user_agent.clone().unwrap_or_else(|| DEFAULT_UA.to_string()))
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(10))
}

fn client(cfg: &WebConfig, redirects: usize) -> Result<reqwest::Client, String> {
    nexo_providers::build_client(|| {
        builder(cfg).redirect(reqwest::redirect::Policy::custom(move |att| {
            if att.previous().len() > redirects {
                att.error("too many redirects")
            } else if !redirect_ok(att.previous(), att.url()) {
                att.error("a redirect to a local or private address")
            } else {
                att.follow()
            }
        }))
    })
}

/// The redirects `web_fetch` follows, at most.
const MAX_REDIRECTS: usize = 5;

/// Where a response sends the reader next, if it is a redirect.
fn redirect_target(resp: &reqwest::Response) -> Option<Url> {
    if !matches!(resp.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    let loc = resp.headers().get(reqwest::header::LOCATION)?.to_str().ok()?;
    resp.url().join(loc.trim()).ok().filter(|u| matches!(u.scheme(), "http" | "https"))
}

/// What to do with one redirect of `web_fetch`.
#[derive(Debug, PartialEq)]
enum Hop {
    Follow,
    /// Not followed, said to the model as the page's text (another site the rules do not cover).
    Stop(String),
    Refuse(String),
}

/// A redirect from `chain` (the pages read so far, the asked one first) to `next`: the address rule
/// first, then the permission rules, as if `next` had been asked for. A site the rules do not cover
/// (they would ask the person) is not read on the way: the model is told the address, and asking for
/// it is asked about like any other fetch. Within the asked site the rules say the same, so it is
/// followed.
fn hop(chain: &[Url], next: &Url, check: Option<&nexo_core::tool::PolicyCheck>) -> Hop {
    let asked = &chain[0];
    if chain.len() > MAX_REDIRECTS {
        return Hop::Refuse(format!("{asked} redirects more than {MAX_REDIRECTS} times"));
    }
    if !redirect_ok(chain, next) {
        return Hop::Refuse(format!("{asked} redirects to {next}, a local or private address"));
    }
    let Some(check) = check else { return Hop::Follow };
    match check("web_fetch", ToolKind::Network, &json!({"url": next.as_str()})) {
        Check::Allow => Hop::Follow,
        Check::Deny => Hop::Refuse(format!("{asked} redirects to {next}, which a permission rule denies")),
        Check::Ask if next.host_str() == asked.host_str() => Hop::Follow,
        Check::Ask => Hop::Stop(format!(
            "{asked} redirects to {next}, another site; it was not read. To read it, call web_fetch with that URL."
        )),
    }
}

// ------------------------------------------------------------------ search

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    DuckDuckGo,
    Brave,
    Tavily,
    Exa,
    Searxng,
}

impl Backend {
    pub fn parse(s: &str) -> Option<Backend> {
        match s.trim().to_ascii_lowercase().as_str() {
            "duckduckgo" | "ddg" => Some(Backend::DuckDuckGo),
            "brave" => Some(Backend::Brave),
            "tavily" => Some(Backend::Tavily),
            "exa" => Some(Backend::Exa),
            "searxng" | "searx" => Some(Backend::Searxng),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Backend::DuckDuckGo => "duckduckgo",
            Backend::Brave => "brave",
            Backend::Tavily => "tavily",
            Backend::Exa => "exa",
            Backend::Searxng => "searxng",
        }
    }

    /// The environment variables that may hold its key, in order.
    fn key_vars(self) -> &'static [&'static str] {
        match self {
            Backend::Brave => &["BRAVE_API_KEY", "BRAVE_SEARCH_API_KEY"],
            Backend::Tavily => &["TAVILY_API_KEY"],
            Backend::Exa => &["EXA_API_KEY"],
            Backend::DuckDuckGo | Backend::Searxng => &[],
        }
    }

    fn base(self) -> &'static str {
        match self {
            Backend::DuckDuckGo => DDG_BASE,
            Backend::Brave => "https://api.search.brave.com",
            Backend::Tavily => "https://api.tavily.com",
            Backend::Exa => "https://api.exa.ai",
            Backend::Searxng => "",
        }
    }
}

/// One hit.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

pub struct WebSearch {
    cfg: WebConfig,
    http: Result<reqwest::Client, String>,
    env: Env,
    ddg_base: String,
    breaker: Breaker,
}

impl WebSearch {
    pub fn new(cfg: &WebConfig) -> WebSearch {
        WebSearch::with_env(cfg, process_env())
    }

    fn with_env(cfg: &WebConfig, env: Env) -> WebSearch {
        WebSearch {
            cfg: cfg.clone(),
            http: client(cfg, 5),
            env,
            ddg_base: DDG_BASE.to_string(),
            breaker: Breaker::new(),
        }
    }

    fn http(&self) -> Result<&reqwest::Client, String> {
        self.http.as_ref().map_err(Clone::clone)
    }

    fn key(&self, b: Backend) -> Option<String> {
        if let Some(var) = &self.cfg.api_key_env {
            return (self.env)(var);
        }
        b.key_vars().iter().find_map(|v| (self.env)(v))
    }

    fn is_auto(&self) -> bool {
        let s = self.cfg.search.trim();
        s.is_empty() || s.eq_ignore_ascii_case("auto")
    }

    /// The backend to ask, and whether DuckDuckGo stands behind it.
    fn choose(&self) -> Result<(Backend, bool), String> {
        let wanted = self.cfg.search.trim();
        if self.is_auto() {
            for b in [Backend::Tavily, Backend::Brave, Backend::Exa] {
                // a key named in the config belongs to no backend in particular: auto cannot use it
                if self.cfg.api_key_env.is_none() && self.key(b).is_some() {
                    return Ok((b, true));
                }
            }
            if self.cfg.url.is_some() {
                return Ok((Backend::Searxng, true));
            }
            return Ok((Backend::DuckDuckGo, false));
        }
        let b = Backend::parse(wanted).ok_or_else(|| {
            format!("unknown backend `{wanted}` ([web] search: auto, duckduckgo, brave, tavily, exa, searxng)")
        })?;
        Ok((b, false))
    }

    fn base(&self, b: Backend) -> String {
        let base = match (b, &self.cfg.url) {
            // in auto mode `url` belongs to the backend chosen first, never to the fallback
            (Backend::DuckDuckGo, _) if self.is_auto() => self.ddg_base.clone(),
            (_, Some(u)) => u.clone(),
            (Backend::DuckDuckGo, None) => self.ddg_base.clone(),
            (b, None) => b.base().to_string(),
        };
        base.trim_end_matches('/').to_string()
    }

    async fn ask(&self, b: Backend, query: &str, n: u64, domains: &[String]) -> Result<Vec<Hit>, Fail> {
        let base = self.base(b);
        let key = || self.key(b).ok_or_else(|| format!("{} needs a key in {}", b.name(), self.key_hint(b)));
        let site_query = with_sites(query, domains);
        match b {
            Backend::Tavily => {
                let mut body = json!({"query": query, "max_results": n});
                if !domains.is_empty() {
                    body["include_domains"] = json!(domains);
                }
                let v =
                    self.json(self.http()?.post(format!("{base}/search")).bearer_auth(key()?).json(&body), b).await?;
                Ok(hits(&v["results"], "title", "url", &["content"]))
            }
            Backend::Brave => {
                let count = n.to_string();
                let req = self
                    .http()?
                    .get(with_query(&format!("{base}/res/v1/web/search"), &[("q", &site_query), ("count", &count)])?)
                    .header("Accept", "application/json")
                    .header("X-Subscription-Token", key()?);
                let v = self.json(req, b).await?;
                Ok(hits(&v["web"]["results"], "title", "url", &["description"]))
            }
            Backend::Exa => {
                let mut body =
                    json!({"query": query, "numResults": n, "type": "auto", "contents": {"highlights": true}});
                if !domains.is_empty() {
                    body["includeDomains"] = json!(domains);
                }
                let v = self
                    .json(self.http()?.post(format!("{base}/search")).header("x-api-key", key()?).json(&body), b)
                    .await?;
                Ok(hits(&v["results"], "title", "url", &["highlights", "summary", "text"]))
            }
            Backend::Searxng => {
                if base.is_empty() {
                    return Err("searxng needs its address: [web] url = \"https://searx.example.org\""
                        .to_string()
                        .into());
                }
                let req =
                    self.http()?.get(with_query(&format!("{base}/search"), &[("q", &site_query), ("format", "json")])?);
                let v = self.json(req, b).await?;
                Ok(hits(&v["results"], "title", "url", &["content"]))
            }
            Backend::DuckDuckGo => {
                let resp = self
                    .http()?
                    .post(format!("{base}/html/"))
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(form_encode(&[("q", &site_query), ("kl", "wt-wt")]))
                    .send()
                    .await
                    .map_err(|e| Fail::refused(format!("duckduckgo: {}", net_error(&e))))?;
                let status = resp.status();
                let text = resp.text().await.map_err(|e| Fail::refused(format!("duckduckgo: {}", net_error(&e))))?;
                if !status.is_success() || text.contains("anomaly-modal") || text.contains("challenge-form") {
                    return Err(Fail::refused(format!(
                        "duckduckgo refused the search (HTTP {}; usually a rate limit or a bot check); \
                         a key for Tavily or Brave, or a SearXNG instance, avoids it",
                        status.as_u16()
                    )));
                }
                let mut out = parse_ddg(&text);
                out.truncate(n as usize);
                Ok(out)
            }
        }
    }

    fn key_hint(&self, b: Backend) -> String {
        match &self.cfg.api_key_env {
            Some(v) => format!("${v}"),
            None => b.key_vars().iter().map(|v| format!("${v}")).collect::<Vec<_>>().join(" or "),
        }
    }

    async fn json(&self, req: reqwest::RequestBuilder, b: Backend) -> Result<Value, Fail> {
        let resp = req.send().await.map_err(|e| Fail::refused(format!("{}: {}", b.name(), net_error(&e))))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| Fail::refused(format!("{}: {}", b.name(), net_error(&e))))?;
        if !status.is_success() {
            let text = format!("{}: HTTP {} {}", b.name(), status.as_u16(), crate::util::one_line(&text, 200));
            // 400, 404 and 422 blame the request; any other refusal is the backend's
            return Err(Fail { text, refused: !matches!(status.as_u16(), 400 | 404 | 422) });
        }
        Ok(serde_json::from_str(&text).map_err(|e| format!("{}: the answer is not JSON ({e})", b.name()))?)
    }
}

/// `base?k=v&…`, encoded.
fn with_query(base: &str, pairs: &[(&str, &str)]) -> Result<Url, String> {
    let mut u = Url::parse(base).map_err(|e| format!("bad address {base}: {e}"))?;
    u.query_pairs_mut().extend_pairs(pairs.iter().copied());
    Ok(u)
}

/// `k=v&…` as an HTML form sends it.
fn form_encode(pairs: &[(&str, &str)]) -> String {
    let mut u = Url::parse("http://form.invalid/").expect("a fixed URL");
    u.query_pairs_mut().extend_pairs(pairs.iter().copied());
    u.query().unwrap_or("").to_string()
}

fn net_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".into()
    } else if e.is_connect() {
        format!("cannot connect ({e})")
    } else {
        e.to_string()
    }
}

fn with_sites(query: &str, domains: &[String]) -> String {
    match domains.len() {
        0 => query.to_string(),
        1 => format!("{query} site:{}", domains[0]),
        _ => format!("{query} ({})", domains.iter().map(|d| format!("site:{d}")).collect::<Vec<_>>().join(" OR ")),
    }
}

/// Hits out of a JSON list: the first non-empty snippet field wins (a list of strings is joined).
fn hits(list: &Value, title: &str, url: &str, snippet: &[&str]) -> Vec<Hit> {
    let Some(items) = list.as_array() else { return Vec::new() };
    items
        .iter()
        .filter_map(|it| {
            let url = it.get(url)?.as_str()?.trim().to_string();
            if url.is_empty() {
                return None;
            }
            let snippet = snippet
                .iter()
                .filter_map(|k| match it.get(*k) {
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(Value::Array(a)) => Some(a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" … ")),
                    _ => None,
                })
                .find(|s| !s.trim().is_empty())
                .unwrap_or_default();
            Some(Hit {
                title: clean(it.get(title).and_then(Value::as_str).unwrap_or(&url)),
                url,
                snippet: clean(&snippet),
            })
        })
        .collect()
}

/// Text out of an HTML fragment: tags out, entities decoded, spaces collapsed.
fn clean(s: &str) -> String {
    static TAG: OnceLock<Regex> = OnceLock::new();
    let tag = TAG.get_or_init(|| Regex::new(r"<[^>]*>").unwrap());
    let text = decode_entities(&tag.replace_all(s, ""));
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    static ENT: OnceLock<Regex> = OnceLock::new();
    let ent = ENT.get_or_init(|| Regex::new(r"&(#[0-9]{1,7}|#[xX][0-9a-fA-F]{1,6}|[a-zA-Z]{2,8});").unwrap());
    ent.replace_all(s, |c: &regex::Captures| {
        let e = &c[1];
        let ch = if let Some(hex) = e.strip_prefix("#x").or_else(|| e.strip_prefix("#X")) {
            u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
        } else if let Some(dec) = e.strip_prefix('#') {
            dec.parse::<u32>().ok().and_then(char::from_u32)
        } else {
            match e {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                "ndash" => Some('–'),
                "mdash" => Some('—'),
                "hellip" => Some('…'),
                "laquo" => Some('«'),
                "raquo" => Some('»'),
                "copy" => Some('©'),
                _ => None,
            }
        };
        ch.map(String::from).unwrap_or_else(|| c[0].to_string())
    })
    .into_owned()
}

/// The results of DuckDuckGo's HTML page, ads left out.
pub fn parse_ddg(html: &str) -> Vec<Hit> {
    static TITLE: OnceLock<Regex> = OnceLock::new();
    static HREF: OnceLock<Regex> = OnceLock::new();
    static SNIPPET: OnceLock<Regex> = OnceLock::new();
    let title =
        TITLE.get_or_init(|| Regex::new(r#"(?s)<a\s([^>]*class="[^"]*\bresult__a\b[^"]*"[^>]*)>(.*?)</a>"#).unwrap());
    let href = HREF.get_or_init(|| Regex::new(r#"\bhref="([^"]*)""#).unwrap());
    let snippet = SNIPPET.get_or_init(|| {
        Regex::new(r#"(?s)class="[^"]*\bresult__snippet\b[^"]*"[^>]*>(.*?)</(?:a|td|div|span)>"#).unwrap()
    });
    let found: Vec<_> = title.captures_iter(html).collect();
    let mut out = Vec::new();
    for (i, c) in found.iter().enumerate() {
        let whole = c.get(0).unwrap();
        let end = found.get(i + 1).map(|n| n.get(0).unwrap().start()).unwrap_or(html.len());
        // the block that holds this result, to tell the ads apart
        let block_start = html[..whole.start()].rfind("<div class=\"result").unwrap_or(0);
        let block_head = &html[block_start..whole.start()];
        let Some(raw) = href.captures(&c[1]).map(|h| decode_entities(&h[1])) else { continue };
        if block_head.contains("result--ad") || raw.contains("duckduckgo.com/y.js") || raw.contains("ad_domain") {
            continue;
        }
        let Some(url) = ddg_target(&raw) else { continue };
        let snip = snippet.captures(&html[whole.end()..end]).map(|s| clean(&s[1])).unwrap_or_default();
        out.push(Hit { title: clean(&c[2]), url, snippet: snip });
    }
    out
}

/// The real address behind DuckDuckGo's redirect (`//duckduckgo.com/l/?uddg=…`).
fn ddg_target(raw: &str) -> Option<String> {
    let abs = if raw.starts_with("//") { format!("https:{raw}") } else { raw.to_string() };
    let u = Url::parse(&abs).ok()?;
    if u.host_str().is_some_and(|h| h.ends_with("duckduckgo.com")) {
        if let Some((_, v)) = u.query_pairs().find(|(k, _)| k == "uddg") {
            return Some(v.into_owned());
        }
        return None;
    }
    matches!(u.scheme(), "http" | "https").then(|| u.to_string())
}

fn render_hits(query: &str, backend: &str, hits: &[Hit], note: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(n) = note {
        out.push_str(n);
        out.push('\n');
    }
    if hits.is_empty() {
        out.push_str(&format!("no results for \"{query}\" ({backend})"));
        return out;
    }
    out.push_str(&format!("{} results for \"{query}\" ({backend}):\n", hits.len()));
    for (i, h) in hits.iter().enumerate() {
        out.push_str(&format!("\n{}. {}\n   {}\n", i + 1, h.title, h.url));
        if !h.snippet.is_empty() {
            out.push_str(&format!("   {}\n", crate::util::one_line(&h.snippet, 400)));
        }
    }
    out.trim_end().to_string()
}

fn domains_arg(args: &Value) -> Vec<String> {
    match args.get("domains") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(|d| d.trim().trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/'))
            .filter(|d| !d.is_empty())
            .map(String::from)
            .collect(),
        Some(Value::String(s)) => {
            s.split([',', ' ']).map(str::trim).filter(|d| !d.is_empty()).map(String::from).collect()
        }
        _ => Vec::new(),
    }
}

#[async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &str {
        "web_search"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_search".into(),
            description: "Search the web. Use it for anything current or outside the repository: library docs and \
                          versions, error messages, APIs, news. Answers with titles, URLs and snippets; read the \
                          pages that matter with `web_fetch` before relying on them, and cite their URLs."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What to search for, as you would type it in a search engine."},
                    "max_results": {"type": "integer", "description": "How many results (default 8, at most 20)."},
                    "domains": {"type": "array", "items": {"type": "string"}, "description": "Only results from these sites, e.g. [\"docs.rs\"]."}
                },
                "required": ["query"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: "Search the web for anything current or outside the repository; returns titles, URLs, snippets. Read pages with `web_fetch`.",
            params: &[
                ("max_results", "Default 8"),
                ("domains", "Only these sites"),
            ],
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Search
    }

    fn summary(&self, args: &Value) -> String {
        let q = str_arg(args, "query").unwrap_or("?");
        let d = domains_arg(args);
        if d.is_empty() {
            format!("web_search: {}", crate::util::one_line(q, 100))
        } else {
            format!("web_search: {} (on {})", crate::util::one_line(q, 100), d.join(", "))
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(query) = str_arg(&args, "query").map(str::trim).filter(|q| !q.is_empty()) else {
            return ToolOutput::error("web_search: `query` is required");
        };
        let n = int_arg(&args, "max_results").unwrap_or(DEFAULT_RESULTS).clamp(1, MAX_RESULTS);
        let domains = domains_arg(&args);
        let (backend, fallback) = match self.choose() {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("web_search: {e}")),
        };
        if self.breaker.is_open() {
            return ToolOutput::error(BREAKER_SAID);
        }
        // Err carries the text and whether every backend tried for this call refused
        let work = async {
            match self.ask(backend, query, n, &domains).await {
                Ok(h) => Ok(render_hits(query, backend.name(), &h, None)),
                Err(e) if fallback && backend != Backend::DuckDuckGo => {
                    match self.ask(Backend::DuckDuckGo, query, n, &domains).await {
                        Ok(h) => Ok(render_hits(
                            query,
                            "duckduckgo",
                            &h,
                            Some(&format!("({}; fell back to duckduckgo)", e.text)),
                        )),
                        Err(e2) => {
                            Err((format!("{}; the fallback failed too: {}", e.text, e2.text), e.refused && e2.refused))
                        }
                    }
                }
                Err(e) => Err((e.text, e.refused)),
            }
        };
        tokio::select! {
            r = work => match r {
                Ok(text) => {
                    self.breaker.reset();
                    ToolOutput::ok(text)
                }
                Err((e, true)) => {
                    self.breaker.refused();
                    ToolOutput::error(format!("web_search: {}. {NO_RETRY}", e.trim_end_matches('.')))
                }
                Err((e, false)) => {
                    // the query's own trouble: not a refusal, and it ends the run of them
                    self.breaker.reset();
                    ToolOutput::error(format!("web_search: {e}"))
                }
            },
            _ = ctx.cancel.cancelled() => ToolOutput::error("web_search: cancelled"),
        }
    }
}

// ------------------------------------------------------------------ fetch

pub struct WebFetch {
    http: Result<reqwest::Client, String>,
}

impl WebFetch {
    pub fn new(cfg: &WebConfig) -> WebFetch {
        // redirects are followed by hand, each one checked (`hop`)
        WebFetch {
            http: nexo_providers::build_client(|| {
                builder(cfg).redirect(reqwest::redirect::Policy::none()).dns_resolver(Arc::new(PublicResolver))
            }),
        }
    }

    fn http(&self) -> Result<&reqwest::Client, String> {
        self.http.as_ref().map_err(Clone::clone)
    }
}

/// What a page is, by its Content-Type (and its first bytes when it has none).
#[derive(Debug, PartialEq)]
enum Kind {
    Html,
    Text,
    Refused(String),
}

fn kind_of(content_type: &str, body: &[u8]) -> Kind {
    let ct = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if ct.contains("html") {
        return Kind::Html;
    }
    if ct.starts_with("text/")
        || ct.contains("json")
        || ct.contains("xml")
        || ct.contains("javascript")
        || ct.contains("yaml")
        || ct.contains("toml")
        || ct == "application/x-sh"
    {
        return Kind::Text;
    }
    if ct.is_empty() || ct == "application/octet-stream" {
        let head = String::from_utf8_lossy(&body[..body.len().min(512)]).trim_start().to_ascii_lowercase();
        if head.starts_with("<!doctype html") || head.starts_with("<html") {
            return Kind::Html;
        }
        if !crate::util::is_binary(&body[..body.len().min(8192)]) {
            return Kind::Text;
        }
    }
    Kind::Refused(if ct.is_empty() { "binary content".into() } else { ct })
}

/// HTML as Markdown: title first, the noise out, links absolute.
pub fn html_to_markdown(html: &str, base: Option<&Url>) -> String {
    static TITLE: OnceLock<Regex> = OnceLock::new();
    static LINK: OnceLock<Regex> = OnceLock::new();
    static BLANKS: OnceLock<Regex> = OnceLock::new();
    let title_re = TITLE.get_or_init(|| Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap());
    let title = title_re.captures(html).map(|c| clean(&c[1])).filter(|t| !t.is_empty());
    let html = match base {
        Some(base) => {
            let link = LINK.get_or_init(|| Regex::new(r#"(?i)\b(href|src)\s*=\s*"([^"]*)""#).unwrap());
            link.replace_all(html, |c: &regex::Captures| {
                let v = decode_entities(&c[2]);
                let abs = if v.starts_with('#') || v.starts_with("javascript:") || v.starts_with("mailto:") {
                    v
                } else {
                    base.join(&v).map(|u| u.to_string()).unwrap_or(v)
                };
                format!("{}=\"{}\"", &c[1], abs.replace('"', "%22"))
            })
            .into_owned()
        }
        None => html.to_string(),
    };
    let conv = htmd::HtmlToMarkdown::builder()
        .skip_tags(vec![
            "head", "script", "style", "nav", "svg", "noscript", "iframe", "form", "button", "select", "template",
            "canvas", "dialog",
        ])
        .build();
    let md = conv.convert(&html).unwrap_or_else(|_| clean(&html));
    let blanks = BLANKS.get_or_init(|| Regex::new(r"\n[ \t]*(\n[ \t]*){2,}").unwrap());
    let md: String = md.lines().map(str::trim_end).collect::<Vec<_>>().join("\n");
    let md = blanks.replace_all(md.trim(), "\n\n").into_owned();
    match title {
        Some(t) if !md.starts_with(&format!("# {t}")) => format!("# {t}\n\n{md}"),
        _ => md,
    }
}

/// `text` from char `start`, at most `max` chars, with the note that says where the rest starts.
fn page(text: &str, start: usize, max: usize) -> (String, Option<String>) {
    let total = text.chars().count();
    if start >= total && total > 0 {
        return (String::new(), Some(format!("start={start} is past the end ({total} characters)")));
    }
    let from = text.char_indices().nth(start).map(|(i, _)| i).unwrap_or(text.len());
    let rest = &text[from..];
    let to = rest.char_indices().nth(max).map(|(i, _)| i).unwrap_or(rest.len());
    let piece = rest[..to].to_string();
    let next = start + piece.chars().count();
    let note = (next < total).then(|| format!("… {} more characters; call again with start={next}", total - next));
    (piece, note)
}

fn shown_url(u: &Url) -> String {
    let host = u.host_str().unwrap_or("");
    let path = if u.path() == "/" { "" } else { u.path() };
    format!("{host}{path}")
}

#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_fetch".into(),
            description: "Read a web page (http or https). HTML comes back as Markdown with absolute links; text, \
                          Markdown and JSON as they are. Long pages come in pieces: the output ends with the \
                          `start` of the next piece."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "The full URL, http:// or https://."},
                    "start": {"type": "integer", "description": "Character to start from (default 0), to read on."},
                    "max_chars": {"type": "integer", "description": "Characters to return (default 20000, at most 100000)."}
                },
                "required": ["url"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: "Read a web page as Markdown; a long page ends with the next `start`.",
            params: &[("start", "Character to start from"), ("max_chars", "Default 20000")],
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Network
    }

    fn summary(&self, args: &Value) -> String {
        let raw = str_arg(args, "url").unwrap_or("?");
        let shown = Url::parse(raw.trim()).map(|u| shown_url(&u)).unwrap_or_else(|_| raw.to_string());
        match int_arg(args, "start").filter(|s| *s > 0) {
            Some(s) => format!("web_fetch: {shown} (from {s})"),
            None => format!("web_fetch: {shown}"),
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(raw) = str_arg(&args, "url").map(str::trim).filter(|u| !u.is_empty()) else {
            return ToolOutput::error("web_fetch: `url` is required");
        };
        let url = match Url::parse(raw) {
            Ok(u) if matches!(u.scheme(), "http" | "https") => u,
            Ok(u) => return ToolOutput::error(format!("web_fetch: only http and https URLs, not `{}:`", u.scheme())),
            Err(e) => return ToolOutput::error(format!("web_fetch: `{raw}` is not a URL ({e})")),
        };
        let start = int_arg(&args, "start").unwrap_or(0) as usize;
        let max = int_arg(&args, "max_chars").unwrap_or(DEFAULT_CHARS).clamp(1, MAX_CHARS) as usize;
        let work = async {
            let mut chain: Vec<Url> = Vec::new();
            let mut at = url.clone();
            let mut resp = loop {
                let resp = self
                    .http()?
                    .get(at.clone())
                    .header(
                        "Accept",
                        "text/html,application/xhtml+xml,text/markdown;q=0.9,text/plain;q=0.9,application/json;q=0.8,*/*;q=0.5",
                    )
                    .send()
                    .await
                    .map_err(|e| net_error(&e))?;
                // a public name that led, by a redirect, to a local address (its DNS answer) is not read either
                if !chain.is_empty() && !private_host(&url) && resp.remote_addr().is_some_and(|a| private_ip(a.ip())) {
                    return Err(format!("{at} is a local or private address, reached by a redirect from {url}"));
                }
                let Some(next) = redirect_target(&resp) else { break resp };
                chain.push(at);
                match hop(&chain, &next, ctx.check.as_ref()) {
                    Hop::Follow => at = next,
                    Hop::Stop(say) => return Ok(Err(say)),
                    Hop::Refuse(why) => return Err(why),
                }
            };
            let status = resp.status();
            let final_url = resp.url().clone();
            if !status.is_success() {
                let mut say =
                    format!("HTTP {} {} for {final_url}", status.as_u16(), status.canonical_reason().unwrap_or(""));
                if matches!(status.as_u16(), 404 | 410) {
                    say.push_str(
                        " The page does not exist: do not guess URLs; use one that a search result or a page gave you.",
                    );
                }
                return Err(say);
            }
            let ct = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let mut body: Vec<u8> = Vec::new();
            let mut cut = false;
            while let Some(chunk) = resp.chunk().await.map_err(|e| net_error(&e))? {
                body.extend_from_slice(&chunk);
                if body.len() > MAX_BODY {
                    body.truncate(MAX_BODY);
                    cut = true;
                    break;
                }
            }
            Ok::<_, String>(Ok((final_url, ct, body, cut)))
        };
        let (final_url, ct, body, cut) = tokio::select! {
            r = work => match r {
                Ok(Ok(v)) => v,
                Ok(Err(say)) => return ToolOutput::ok(format!("URL: {url}\n\n{say}")),
                Err(e) => return ToolOutput::error(format!("web_fetch: {e}")),
            },
            _ = ctx.cancel.cancelled() => return ToolOutput::error("web_fetch: cancelled"),
        };
        let text = match kind_of(&ct, &body) {
            Kind::Html => html_to_markdown(&String::from_utf8_lossy(&body), Some(&final_url)),
            Kind::Text => String::from_utf8_lossy(&body).into_owned(),
            Kind::Refused(what) => {
                return ToolOutput::error(format!(
                    "web_fetch: {final_url} is {what}, not text; web_fetch reads HTML, text, Markdown and JSON"
                ))
            }
        };
        let (piece, note) = page(&text, start, max);
        let mut out = format!("URL: {final_url}\n");
        if final_url != url {
            out.push_str(&format!("(redirected from {url})\n"));
        }
        if cut {
            out.push_str("(the page is over 5 MB; only its start was read)\n");
        }
        out.push('\n');
        out.push_str(&piece);
        if let Some(n) = note {
            out.push_str(&format!("\n\n{n}"));
        }
        ToolOutput::ok(out)
    }
}

/// The two web tools, or none when `[web] enabled = false`.
pub fn tools(cfg: &WebConfig) -> Vec<Arc<dyn Tool>> {
    if !cfg.enabled {
        return Vec::new();
    }
    vec![Arc::new(WebSearch::new(cfg)), Arc::new(WebFetch::new(cfg))]
}

/// Keys in a map, for tests.
#[cfg(test)]
fn env_of(pairs: &[(&str, &str)]) -> Env {
    let m: std::collections::BTreeMap<String, String> =
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    Arc::new(move |k: &str| m.get(k).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn ctx() -> ToolContext {
        ToolContext::new(std::env::temp_dir())
    }

    fn cfg(search: &str, url: Option<String>) -> WebConfig {
        WebConfig { search: search.into(), url, ..Default::default() }
    }

    const DDG_PAGE: &str = r#"<!DOCTYPE html><html><body>
<div class="serp__results"><div id="links" class="results">
  <div class="result results_links results_links_deep result--ad ">
    <div class="links_main links_deep result__body">
      <h2 class="result__title"><a rel="nofollow" class="result__a" href="https://duckduckgo.com/y.js?ad_domain=shop.example&amp;u3=x">Buy Rust now</a></h2>
      <a class="result__snippet" href="https://duckduckgo.com/y.js?ad_domain=shop.example">An ad.</a>
    </div>
  </div>
  <div class="result results_links results_links_deep web-result ">
    <div class="links_main links_deep result__body">
      <h2 class="result__title">
        <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fratatui.rs%2Fconcepts%2F%3Fa%3D1%26b%3D2&amp;rut=abc123">Concepts | <b>Ratatui</b></a>
      </h2>
      <div class="result__extras"><div class="result__extras__url"><a class="result__url" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fratatui.rs%2F">ratatui.rs</a></div></div>
      <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fratatui.rs%2F"><b>Ratatui</b> is a Rust crate for cooking up terminal user interfaces &amp; more&#x27;s.</a>
      <div class="clear"></div>
    </div>
  </div>
  <div class="result results_links results_links_deep web-result ">
    <div class="links_main links_deep result__body">
      <h2 class="result__title"><a class="result__a" rel="nofollow" href="https://docs.rs/ratatui/latest/ratatui/">ratatui - Rust</a></h2>
      <a class="result__snippet" href="https://docs.rs/ratatui/">API documentation for the Rust `ratatui` crate.</a>
    </div>
  </div>
</div></div></body></html>"#;

    #[test]
    fn duckduckgo_html_is_parsed_without_ads() {
        let h = parse_ddg(DDG_PAGE);
        assert_eq!(h.len(), 2, "{h:?}");
        assert_eq!(h[0].title, "Concepts | Ratatui");
        assert_eq!(h[0].url, "https://ratatui.rs/concepts/?a=1&b=2");
        assert_eq!(h[0].snippet, "Ratatui is a Rust crate for cooking up terminal user interfaces & more's.");
        assert_eq!(h[1].url, "https://docs.rs/ratatui/latest/ratatui/");
        assert_eq!(h[1].snippet, "API documentation for the Rust `ratatui` crate.");
    }

    #[test]
    fn entities_and_sites() {
        assert_eq!(decode_entities("a &amp; b &lt;c&gt; &#39;d&#39; &#x41; &bogus;"), "a & b <c> 'd' A &bogus;");
        assert_eq!(with_sites("q", &[]), "q");
        assert_eq!(with_sites("q", &["docs.rs".into()]), "q site:docs.rs");
        assert_eq!(with_sites("q", &["a.io".into(), "b.io".into()]), "q (site:a.io OR site:b.io)");
        assert_eq!(domains_arg(&json!({"domains": ["https://docs.rs/", "  "]})), vec!["docs.rs"]);
    }

    #[tokio::test]
    async fn duckduckgo_backend_posts_the_form() {
        let s = MockServer::start_async().await;
        let m = s
            .mock_async(|when, then| {
                when.method(POST).path("/html/").body_includes("q=ratatui+site%3Adocs.rs");
                then.status(200).header("content-type", "text/html").body(DDG_PAGE);
            })
            .await;
        let t = WebSearch::with_env(&cfg("duckduckgo", Some(s.base_url())), env_of(&[]));
        let out = t.run(json!({"query": "ratatui", "domains": ["docs.rs"], "max_results": 1}), &ctx()).await;
        m.assert_async().await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.starts_with("1 results for \"ratatui\" (duckduckgo):"), "{}", out.content);
        assert!(out.content.contains("1. Concepts | Ratatui\n   https://ratatui.rs/concepts/?a=1&b=2"));
        assert!(!out.content.contains("docs.rs/ratatui/latest"), "max_results holds");
    }

    #[tokio::test]
    async fn duckduckgo_bot_check_is_explained() {
        let s = MockServer::start_async().await;
        s.mock_async(|when, then| {
            when.method(POST).path("/html/");
            then.status(202).body("<div class=\"anomaly-modal\">bots</div>");
        })
        .await;
        let t = WebSearch::with_env(&cfg("duckduckgo", Some(s.base_url())), env_of(&[]));
        let out = t.run(json!({"query": "x"}), &ctx()).await;
        assert!(out.is_error);
        assert!(out.content.contains("rate limit or a bot check"), "{}", out.content);
    }

    const SEARCH_DOWN: &str = "Web search is not available right now: do not retry it; go on with what you have, or open a URL you already know with web_fetch.";

    /// A search tool whose clock is a number of seconds the test moves.
    fn timed(t: &mut WebSearch) -> Arc<AtomicU64> {
        let secs = Arc::new(AtomicU64::new(0));
        let c = secs.clone();
        t.breaker.now = Arc::new(move || Duration::from_secs(c.load(Ordering::SeqCst)));
        secs
    }

    #[tokio::test]
    async fn a_refused_search_says_not_to_retry() {
        let s = MockServer::start_async().await;
        s.mock_async(|when, then| {
            when.method(POST).path("/html/");
            then.status(429).body("");
        })
        .await;
        let t = WebSearch::with_env(&cfg("duckduckgo", Some(s.base_url())), env_of(&[]));
        let out = t.run(json!({"query": "x"}), &ctx()).await;
        assert!(out.is_error);
        assert!(out.content.contains("duckduckgo refused the search (HTTP 429"), "{}", out.content);
        assert!(out.content.ends_with(SEARCH_DOWN), "{}", out.content);
        // an unreachable backend counts too
        let t = WebSearch::with_env(&cfg("searxng", Some("http://127.0.0.1:1".into())), env_of(&[]));
        let out = t.run(json!({"query": "x"}), &ctx()).await;
        assert!(out.is_error && out.content.ends_with(SEARCH_DOWN), "{}", out.content);
        // a bad request is the query's trouble, not a refusal
        let s = MockServer::start_async().await;
        s.mock_async(|when, then| {
            when.method(GET).path("/search");
            then.status(400).body("bad query");
        })
        .await;
        let t = WebSearch::with_env(&cfg("searxng", Some(s.base_url())), env_of(&[]));
        let out = t.run(json!({"query": "x"}), &ctx()).await;
        assert!(out.is_error && !out.content.contains(SEARCH_DOWN), "{}", out.content);
    }

    #[tokio::test]
    async fn two_refusals_in_a_row_open_the_breaker_and_time_closes_it() {
        let s = MockServer::start_async().await;
        let m = s
            .mock_async(|when, then| {
                when.method(POST).path("/html/");
                then.status(429).body("");
            })
            .await;
        let mut t = WebSearch::with_env(&cfg("duckduckgo", Some(s.base_url())), env_of(&[]));
        let secs = timed(&mut t);
        for _ in 0..2 {
            let out = t.run(json!({"query": "x"}), &ctx()).await;
            assert!(out.content.ends_with(SEARCH_DOWN), "{}", out.content);
        }
        assert_eq!(m.calls_async().await, 2);
        let out = t.run(json!({"query": "x"}), &ctx()).await;
        assert!(out.is_error);
        assert_eq!(out.content, BREAKER_SAID);
        assert!(out.content.contains("refused 2 requests in a row"));
        assert_eq!(m.calls_async().await, 2, "an open breaker makes no request");
        secs.store(119, Ordering::SeqCst);
        t.run(json!({"query": "x"}), &ctx()).await;
        assert_eq!(m.calls_async().await, 2, "still open at 119 s");
        secs.store(121, Ordering::SeqCst);
        let out = t.run(json!({"query": "x"}), &ctx()).await;
        assert_eq!(m.calls_async().await, 3, "closed after 120 s");
        assert!(out.content.ends_with(SEARCH_DOWN), "{}", out.content);
    }

    #[tokio::test]
    async fn a_good_search_resets_the_count() {
        let s = MockServer::start_async().await;
        let t = WebSearch::with_env(&cfg("duckduckgo", Some(s.base_url())), env_of(&[]));
        for (code, body) in [(429, ""), (200, DDG_PAGE), (429, ""), (200, DDG_PAGE)] {
            let m = s
                .mock_async(move |when, then| {
                    when.method(POST).path("/html/");
                    then.status(code).body(body);
                })
                .await;
            let out = t.run(json!({"query": "x"}), &ctx()).await;
            // a refusal, a success, a refusal, a success: never two refusals in a row
            assert_eq!(out.is_error, code == 429, "{}", out.content);
            assert_ne!(out.content, BREAKER_SAID);
            m.delete_async().await;
        }
    }

    #[tokio::test]
    async fn with_two_backends_the_breaker_counts_only_when_both_refused() {
        let s = MockServer::start_async().await;
        s.mock_async(|when, then| {
            when.method(POST).path("/search");
            then.status(401).body("no");
        })
        .await;
        let ok = s
            .mock_async(|when, then| {
                when.method(POST).path("/html/");
                then.status(200).body(DDG_PAGE);
            })
            .await;
        let mut t = WebSearch::with_env(&cfg("auto", Some(s.base_url())), env_of(&[("TAVILY_API_KEY", "bad")]));
        t.ddg_base = s.base_url();
        // the keyed backend refuses, DuckDuckGo answers: three calls, no breaker
        for _ in 0..3 {
            assert!(!t.run(json!({"query": "x"}), &ctx()).await.is_error);
        }
        assert_eq!(ok.calls_async().await, 3);
        ok.delete_async().await;
        let both = s
            .mock_async(|when, then| {
                when.method(POST).path("/html/");
                then.status(429).body("");
            })
            .await;
        for _ in 0..2 {
            let out = t.run(json!({"query": "x"}), &ctx()).await;
            assert!(
                out.content.contains("the fallback failed too") && out.content.ends_with(SEARCH_DOWN),
                "{}",
                out.content
            );
        }
        let out = t.run(json!({"query": "x"}), &ctx()).await;
        assert_eq!(out.content, BREAKER_SAID);
        assert_eq!(both.calls_async().await, 2);
    }

    #[tokio::test]
    async fn a_404_fetch_says_not_to_guess_urls_and_other_errors_stay_as_they_were() {
        let s = MockServer::start_async().await;
        for (path, code) in [("/gone", 410), ("/missing", 404), ("/boom", 500), ("/nope", 403)] {
            s.mock_async(move |when, then| {
                when.method(GET).path(path);
                then.status(code).body("x");
            })
            .await;
        }
        let t = WebFetch::new(&WebConfig::default());
        let sentence = " The page does not exist: do not guess URLs; use one that a search result or a page gave you.";
        let out = t.run(json!({"url": s.url("/missing")}), &ctx()).await;
        assert!(
            out.is_error && out.content.ends_with(&format!("HTTP 404 Not Found for {}{sentence}", s.url("/missing"))),
            "{}",
            out.content
        );
        let out = t.run(json!({"url": s.url("/gone")}), &ctx()).await;
        assert!(out.is_error && out.content.ends_with(sentence), "{}", out.content);
        let out = t.run(json!({"url": s.url("/boom")}), &ctx()).await;
        assert_eq!(out.content, format!("web_fetch: HTTP 500 Internal Server Error for {}", s.url("/boom")));
        let out = t.run(json!({"url": s.url("/nope")}), &ctx()).await;
        assert_eq!(out.content, format!("web_fetch: HTTP 403 Forbidden for {}", s.url("/nope")));
    }
    #[tokio::test]
    async fn tavily_backend() {
        let s = MockServer::start_async().await;
        let m = s
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/search")
                    .header("authorization", "Bearer tv-key")
                    .json_body_includes(r#"{"query": "rust 2024", "max_results": 3, "include_domains": ["rust-lang.org"]}"#);
                then.status(200).json_body(json!({
                    "query": "rust 2024",
                    "results": [
                        {"title": "Rust 2024 | Edition Guide", "url": "https://doc.rust-lang.org/edition-guide/rust-2024/", "content": "The Rust 2024 Edition was stabilised in 1.85.", "score": 0.9},
                        {"title": "No url", "content": "dropped"}
                    ]
                }));
            })
            .await;
        let t = WebSearch::with_env(&cfg("tavily", Some(s.base_url())), env_of(&[("TAVILY_API_KEY", "tv-key")]));
        let out = t.run(json!({"query": "rust 2024", "max_results": 3, "domains": ["rust-lang.org"]}), &ctx()).await;
        m.assert_async().await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("(tavily)"));
        assert!(out.content.contains("1. Rust 2024 | Edition Guide\n   https://doc.rust-lang.org/edition-guide/rust-2024/\n   The Rust 2024 Edition"));
        assert!(!out.content.contains("No url"));
    }

    #[tokio::test]
    async fn brave_backend() {
        let s = MockServer::start_async().await;
        let m = s
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/res/v1/web/search")
                    .query_param("q", "tokio select")
                    .query_param("count", "8")
                    .header("x-subscription-token", "br-key");
                then.status(200).json_body(json!({
                    "type": "search",
                    "web": {"type": "search", "results": [
                        {"title": "select! in <strong>tokio</strong>", "url": "https://docs.rs/tokio/latest/tokio/macro.select.html", "description": "Waits on multiple concurrent branches, returning when the <strong>first</strong> branch completes."}
                    ]}
                }));
            })
            .await;
        let t = WebSearch::with_env(&cfg("brave", Some(s.base_url())), env_of(&[("BRAVE_SEARCH_API_KEY", "br-key")]));
        let out = t.run(json!({"query": "tokio select"}), &ctx()).await;
        m.assert_async().await;
        assert!(out.content.contains("1. select! in tokio\n   https://docs.rs/tokio/latest/tokio/macro.select.html\n   Waits on multiple concurrent branches, returning when the first branch completes."), "{}", out.content);
    }

    #[tokio::test]
    async fn exa_backend() {
        let s = MockServer::start_async().await;
        let m = s
            .mock_async(|when, then| {
                when.method(POST).path("/search").header("x-api-key", "exa-key").json_body_includes(r#"{"numResults": 2}"#);
                then.status(200).json_body(json!({
                    "requestId": "r1",
                    "results": [
                        {"id": "1", "title": "Serde", "url": "https://serde.rs/", "highlights": ["Serde is a framework", "for serializing"], "publishedDate": null}
                    ]
                }));
            })
            .await;
        let t = WebSearch::with_env(&cfg("exa", Some(s.base_url())), env_of(&[("EXA_API_KEY", "exa-key")]));
        let out = t.run(json!({"query": "serde", "max_results": 2}), &ctx()).await;
        m.assert_async().await;
        assert!(
            out.content.contains("1. Serde\n   https://serde.rs/\n   Serde is a framework … for serializing"),
            "{}",
            out.content
        );
    }

    #[tokio::test]
    async fn searxng_backend_and_its_address() {
        let s = MockServer::start_async().await;
        let m = s
            .mock_async(|when, then| {
                when.method(GET).path("/search").query_param("format", "json").query_param("q", "nexo");
                then.status(200).json_body(json!({
                    "query": "nexo",
                    "results": [{"title": "Nexo", "url": "https://example.org/nexo", "content": "A harness.", "engine": "bing"}]
                }));
            })
            .await;
        let t = WebSearch::with_env(&cfg("searxng", Some(format!("{}/", s.base_url()))), env_of(&[]));
        let out = t.run(json!({"query": "nexo"}), &ctx()).await;
        m.assert_async().await;
        assert!(out.content.contains("(searxng)") && out.content.contains("A harness."), "{}", out.content);
        let none = WebSearch::with_env(&cfg("searxng", None), env_of(&[]));
        let out = none.run(json!({"query": "nexo"}), &ctx()).await;
        assert!(out.is_error && out.content.contains("[web] url"), "{}", out.content);
    }

    #[tokio::test]
    async fn auto_takes_the_keyed_backend_and_falls_back_to_duckduckgo() {
        let s = MockServer::start_async().await;
        let tav = s
            .mock_async(|when, then| {
                when.method(POST).path("/search");
                then.status(401).body(r#"{"detail": {"error": "Unauthorized: missing or invalid API key."}}"#);
            })
            .await;
        let ddg = s
            .mock_async(|when, then| {
                when.method(POST).path("/html/");
                then.status(200).body(DDG_PAGE);
            })
            .await;
        let mut t = WebSearch::with_env(&cfg("auto", Some(s.base_url())), env_of(&[("TAVILY_API_KEY", "bad")]));
        t.ddg_base = s.base_url();
        assert_eq!(t.choose().unwrap(), (Backend::Tavily, true));
        let out = t.run(json!({"query": "ratatui"}), &ctx()).await;
        tav.assert_async().await;
        ddg.assert_async().await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.starts_with("(tavily: HTTP 401"), "{}", out.content);
        assert!(out.content.contains("fell back to duckduckgo"));
        assert!(out.content.contains("Concepts | Ratatui"));
        // no key, no address: DuckDuckGo; an address alone: SearXNG
        assert_eq!(
            WebSearch::with_env(&cfg("auto", None), env_of(&[])).choose().unwrap(),
            (Backend::DuckDuckGo, false)
        );
        assert_eq!(
            WebSearch::with_env(&cfg("auto", Some("http://s".into())), env_of(&[])).choose().unwrap(),
            (Backend::Searxng, true)
        );
        assert!(WebSearch::with_env(&cfg("google", None), env_of(&[])).choose().is_err());
    }

    #[tokio::test]
    async fn a_named_backend_without_its_key_says_which_variable() {
        let t = WebSearch::with_env(&cfg("brave", None), env_of(&[]));
        let out = t.run(json!({"query": "x"}), &ctx()).await;
        assert!(out.is_error);
        assert!(out.content.contains("$BRAVE_API_KEY or $BRAVE_SEARCH_API_KEY"), "{}", out.content);
        let mut c = cfg("tavily", None);
        c.api_key_env = Some("MY_TAVILY".into());
        let out = WebSearch::with_env(&c, env_of(&[])).run(json!({"query": "x"}), &ctx()).await;
        assert!(out.content.contains("$MY_TAVILY"), "{}", out.content);
        let out = WebSearch::with_env(&c, env_of(&[])).run(json!({}), &ctx()).await;
        assert!(out.is_error && out.content.contains("`query` is required"));
    }

    const ARTICLE: &str = r#"<!doctype html><html><head><title>Async &amp; Await</title>
<style>body{color:red}</style><script>alert("x")</script></head>
<body><nav><a href="/">Home</a> | <a href="/blog">Blog</a></nav>
<main><h1>Async &amp; Await</h1>
<p>Read <a href="../guide/intro.html">the guide</a> first.</p>
<h2>Example</h2>
<pre><code>async fn main() {}</code></pre>
<ul><li>one</li><li>two</li></ul>
<form><input name="q"><button>Go</button></form>
</main><noscript>Enable JS</noscript></body></html>"#;

    #[test]
    fn html_becomes_markdown_with_absolute_links() {
        let base = Url::parse("https://example.org/book/ch1/page.html").unwrap();
        let md = html_to_markdown(ARTICLE, Some(&base));
        assert!(md.starts_with("# Async & Await"), "{md}");
        assert_eq!(md.matches("Async & Await").count(), 1, "the title is not repeated: {md}");
        assert!(md.contains("[the guide](https://example.org/book/guide/intro.html)"), "{md}");
        assert!(md.contains("## Example"), "{md}");
        assert!(md.contains("async fn main() {}"), "{md}");
        assert!(md.contains("one") && md.contains("two"));
        for gone in ["alert", "color:red", "Blog", "Enable JS", "Go"] {
            assert!(!md.contains(gone), "{gone} left in: {md}");
        }
    }

    #[test]
    fn pages_are_cut_in_characters() {
        let (p, n) = page("héllo wörld", 0, 5);
        assert_eq!(p, "héllo");
        assert_eq!(n.as_deref(), Some("… 6 more characters; call again with start=5"));
        let (p, n) = page("héllo wörld", 5, 100);
        assert_eq!(p, " wörld");
        assert!(n.is_none());
        let (_, n) = page("abc", 9, 5);
        assert!(n.unwrap().contains("past the end"));
    }

    #[test]
    fn redirect_blocks_private_hosts() {
        let u = |s: &str| Url::parse(s).unwrap();
        for s in [
            "http://127.0.0.1:1/",
            "http://169.254.169.254/",
            "http://[::1]/",
            "http://10.0.0.1/",
            "http://localhost/",
            "http://[::ffff:192.168.1.1]/",
            "http://[fe80::1]/",
            "http://0.1.2.3/",
            "http://[::127.0.0.1]/",
            "http://[::169.254.169.254]/",
            "http://[64:ff9b::7f00:1]/",
            "http://[64:ff9b::10.0.0.1]/",
            "http://[64:ff9b:1::8.8.8.8]/",
            "http://[2002:7f00:1::]/",
            "http://[2001:0:4136:e378:8000:63bf:80ff:fffe]/",
            "http://[fec0::1]/",
        ] {
            assert!(private_host(&u(s)), "{s}");
            assert!(!redirect_ok(&[u("https://docs.rs/")], &u(s)), "{s}");
        }
        assert!(!private_host(&u("https://docs.rs/")));
        assert!(!private_host(&u("http://8.8.8.8/")));
        for s in ["http://[2606:4700::1111]/", "http://[64:ff9b::808:808]/", "http://[2002:808:808::]/"] {
            assert!(!private_host(&u(s)), "{s}");
        }
        assert!(redirect_ok(&[u("https://docs.rs/")], &u("https://crates.io/")));
        assert!(
            redirect_ok(&[u("http://127.0.0.1:8080/")], &u("http://127.0.0.1:8080/b")),
            "a local start may stay local"
        );
    }

    #[test]
    fn a_name_is_refused_before_connecting_when_it_resolves_to_a_private_address() {
        let a = |s: &str| -> std::net::SocketAddr { s.parse().unwrap() };
        let public = vec![a("93.184.216.34:0"), a("[2606:2800:220:1::1]:0")];
        assert_eq!(public_only("example.com", public.clone()), Ok(public));
        for addrs in [
            vec![a("127.0.0.1:0")],
            vec![a("93.184.216.34:0"), a("10.0.0.5:0")],
            vec![a("[::1]:0")],
            vec![a("[::ffff:169.254.169.254]:0")],
            vec![a("[64:ff9b::c0a8:101]:0")],
        ] {
            let e = public_only("rebind.example", addrs).unwrap_err();
            assert!(e.contains("local or private"), "{e}");
        }
        let local = vec![a("127.0.0.1:0")];
        assert_eq!(public_only("localhost", local.clone()), Ok(local.clone()));
        assert_eq!(public_only("app.localhost.", local.clone()), Ok(local));
    }

    #[test]
    fn kinds_by_type_and_bytes() {
        assert_eq!(kind_of("text/html; charset=utf-8", b""), Kind::Html);
        assert_eq!(kind_of("application/json", b"{}"), Kind::Text);
        assert_eq!(kind_of("text/markdown", b"# a"), Kind::Text);
        assert_eq!(kind_of("", b"<!DOCTYPE html><html>"), Kind::Html);
        assert_eq!(kind_of("application/pdf", b"%PDF-1.7"), Kind::Refused("application/pdf".into()));
        assert!(matches!(kind_of("", &[0u8, 159, 146, 150, 0, 0]), Kind::Refused(_)));
    }

    #[tokio::test]
    async fn fetch_reads_html_text_and_pages() {
        let s = MockServer::start_async().await;
        s.mock_async(|when, then| {
            when.method(GET).path("/book/ch1/page.html");
            then.status(200).header("content-type", "text/html; charset=utf-8").body(ARTICLE);
        })
        .await;
        s.mock_async(|when, then| {
            when.method(GET).path("/data.json");
            then.status(200).header("content-type", "application/json").body(r#"{"a": 1}"#);
        })
        .await;
        s.mock_async(|when, then| {
            when.method(GET).path("/long.txt");
            then.status(200).header("content-type", "text/plain").body("0123456789".repeat(10));
        })
        .await;
        let t = WebFetch::new(&WebConfig::default());
        let out = t.run(json!({"url": s.url("/book/ch1/page.html")}), &ctx()).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.starts_with(&format!("URL: {}\n\n# Async & Await", s.url("/book/ch1/page.html"))),
            "{}",
            out.content
        );
        assert!(out.content.contains(&format!("[the guide]({})", s.url("/book/guide/intro.html"))), "{}", out.content);
        let out = t.run(json!({"url": s.url("/data.json")}), &ctx()).await;
        assert!(out.content.ends_with(r#"{"a": 1}"#), "{}", out.content);
        let out = t.run(json!({"url": s.url("/long.txt"), "max_chars": 30}), &ctx()).await;
        assert!(out.content.ends_with("… 70 more characters; call again with start=30"), "{}", out.content);
        let out = t.run(json!({"url": s.url("/long.txt"), "start": 95}), &ctx()).await;
        assert!(out.content.ends_with("\n\n56789"), "{}", out.content);
    }

    #[tokio::test]
    async fn fetch_refuses_what_it_cannot_read() {
        let s = MockServer::start_async().await;
        s.mock_async(|when, then| {
            when.method(GET).path("/missing");
            then.status(404).body("not here");
        })
        .await;
        s.mock_async(|when, then| {
            when.method(GET).path("/paper.pdf");
            then.status(200).header("content-type", "application/pdf").body("%PDF-1.7 ...");
        })
        .await;
        let t = WebFetch::new(&WebConfig::default());
        let out = t.run(json!({"url": s.url("/missing")}), &ctx()).await;
        assert!(out.is_error && out.content.contains("HTTP 404 Not Found"), "{}", out.content);
        let out = t.run(json!({"url": s.url("/paper.pdf")}), &ctx()).await;
        assert!(out.is_error && out.content.contains("application/pdf, not text"), "{}", out.content);
        let out = t.run(json!({"url": "file:///etc/passwd"}), &ctx()).await;
        assert!(out.is_error && out.content.contains("only http and https"), "{}", out.content);
        let out = t.run(json!({"url": "not a url"}), &ctx()).await;
        assert!(out.is_error && out.content.contains("is not a URL"), "{}", out.content);
    }

    /// Rules that deny `deny`, allow `allow`, and ask about any other host.
    fn rules(deny: &'static str, allow: &'static str) -> nexo_core::tool::PolicyCheck {
        Arc::new(move |tool: &str, kind, args: &Value| {
            assert_eq!((tool, kind), ("web_fetch", ToolKind::Network));
            let host = Url::parse(args["url"].as_str().unwrap()).unwrap().host_str().unwrap().to_string();
            if host == deny {
                Check::Deny
            } else if host == allow {
                Check::Allow
            } else {
                Check::Ask
            }
        })
    }

    #[test]
    fn a_redirect_is_checked_by_the_rules_as_if_its_target_were_asked_for() {
        let u = |s: &str| Url::parse(s).unwrap();
        let docs = [u("https://docs.rs/tokio")];
        let r = rules("evil.example", "crates.io");
        assert_eq!(hop(&docs, &u("https://docs.rs/tokio/latest/"), Some(&r)), Hop::Follow, "same site");
        assert_eq!(hop(&docs, &u("https://crates.io/x"), Some(&r)), Hop::Follow, "allowed by a rule");
        assert!(
            matches!(hop(&docs, &u("https://evil.example/?k=1"), Some(&r)), Hop::Refuse(w) if w.contains("denies"))
        );
        assert!(
            matches!(hop(&docs, &u("https://other.example/"), Some(&r)), Hop::Stop(s) if s.contains("call web_fetch with that URL"))
        );
        assert_eq!(hop(&docs, &u("https://other.example/"), None), Hop::Follow, "no rules, only the address rule");
        // the address rule comes first, whatever the rules allow
        let all = rules("", "169.254.169.254");
        assert!(
            matches!(hop(&docs, &u("http://169.254.169.254/latest/"), Some(&all)), Hop::Refuse(w) if w.contains("private"))
        );
        let six: Vec<Url> = (0..6).map(|i| u(&format!("https://docs.rs/{i}"))).collect();
        assert!(matches!(hop(&six, &u("https://docs.rs/7"), None), Hop::Refuse(w) if w.contains("more than 5")));
    }

    #[tokio::test]
    async fn fetch_follows_a_redirect_only_where_the_rules_would_let_it_go() {
        let s = MockServer::start_async().await;
        let other = format!("http://localhost:{}/far", s.port());
        s.mock_async(|when, then| {
            when.method(GET).path("/old");
            then.status(301).header("location", "/new");
        })
        .await;
        s.mock_async(|when, then| {
            when.method(GET).path("/new");
            then.status(200).header("content-type", "text/plain").body("the new page");
        })
        .await;
        s.mock_async(|when, then| {
            when.method(GET).path("/away");
            then.status(302).header("location", other.as_str());
        })
        .await;
        let far = s
            .mock_async(|when, then| {
                when.method(GET).path("/far");
                then.status(200).header("content-type", "text/plain").body("the far page");
            })
            .await;
        let t = WebFetch::new(&WebConfig::default());
        let asks = ToolContext { check: Some(rules("", "")), ..ctx() };
        let out = t.run(json!({"url": s.url("/old")}), &asks).await;
        assert!(!out.is_error && out.content.ends_with("the new page"), "{}", out.content);
        assert!(out.content.contains(&format!("(redirected from {})", s.url("/old"))), "{}", out.content);
        // another site the rules do not cover: not read, its address said
        let out = t.run(json!({"url": s.url("/away")}), &asks).await;
        assert!(
            !out.is_error && out.content.contains(&other) && !out.content.contains("the far page"),
            "{}",
            out.content
        );
        far.assert_calls_async(0).await;
        let denies = ToolContext { check: Some(rules("localhost", "")), ..ctx() };
        let out = t.run(json!({"url": s.url("/away")}), &denies).await;
        assert!(out.is_error && out.content.contains("a permission rule denies"), "{}", out.content);
        far.assert_calls_async(0).await;
        let allows = ToolContext { check: Some(rules("", "localhost")), ..ctx() };
        let out = t.run(json!({"url": s.url("/away")}), &allows).await;
        assert!(!out.is_error && out.content.ends_with("the far page"), "{}", out.content);
    }

    #[test]
    fn summaries_and_registration() {
        let f = WebFetch::new(&WebConfig::default());
        assert_eq!(
            f.summary(&json!({"url": "https://docs.rs/tokio/latest/tokio/?x=1"})),
            "web_fetch: docs.rs/tokio/latest/tokio/"
        );
        assert_eq!(
            f.summary(&json!({"url": "https://ratatui.rs/", "start": 2000})),
            "web_fetch: ratatui.rs (from 2000)"
        );
        let s = WebSearch::new(&WebConfig::default());
        assert_eq!(
            s.summary(&json!({"query": "tokio select", "domains": ["docs.rs"]})),
            "web_search: tokio select (on docs.rs)"
        );
        assert_eq!(s.kind(), ToolKind::Search);
        assert_eq!(f.kind(), ToolKind::Network);
        assert_eq!(tools(&WebConfig::default()).len(), 2);
        assert!(tools(&WebConfig { enabled: false, ..Default::default() }).is_empty());
    }
}
