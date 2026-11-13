use super::*;

pub(super) fn process_env() -> Env {
    Arc::new(|k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
}

/// A redirect may not lead from a public address to a local or private one (this machine, the
/// local network, a cloud's metadata service): a page could otherwise read them for the model.
pub(super) fn redirect_ok(previous: &[Url], next: &Url) -> bool {
    previous.first().is_some_and(private_host) || !private_host(next)
}

/// `localhost`, or an address of loopback, private, link-local, shared or unique-local ranges.
pub(super) fn private_host(url: &Url) -> bool {
    let Some(h) = url.host_str() else { return false };
    let h = h.trim_start_matches('[').trim_end_matches(']').trim_end_matches('.').to_ascii_lowercase();
    match h.parse::<std::net::IpAddr>() {
        Ok(ip) => private_ip(ip),
        Err(_) => h == "localhost" || h.ends_with(".localhost"),
    }
}

pub(super) fn private_ip(ip: std::net::IpAddr) -> bool {
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
pub(super) fn public_only(host: &str, addrs: Vec<std::net::SocketAddr>) -> Result<Vec<std::net::SocketAddr>, String> {
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
pub(super) struct PublicResolver;

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

pub(super) fn builder(cfg: &WebConfig) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(cfg.user_agent.clone().unwrap_or_else(|| DEFAULT_UA.to_string()))
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(10))
}

pub(super) fn client(cfg: &WebConfig, redirects: usize) -> Result<reqwest::Client, String> {
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
pub(super) const MAX_REDIRECTS: usize = 5;

/// Where a response sends the reader next, if it is a redirect.
pub(super) fn redirect_target(resp: &reqwest::Response) -> Option<Url> {
    if !matches!(resp.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    let loc = resp.headers().get(reqwest::header::LOCATION)?.to_str().ok()?;
    resp.url().join(loc.trim()).ok().filter(|u| matches!(u.scheme(), "http" | "https"))
}

/// What to do with one redirect of `web_fetch`.
#[derive(Debug, PartialEq)]
pub(super) enum Hop {
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
pub(super) fn hop(chain: &[Url], next: &Url, check: Option<&nexo_core::tool::PolicyCheck>) -> Hop {
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
