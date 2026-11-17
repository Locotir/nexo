//! Drops what is not the site's own API from a capture: other schemes, other hosts, preflights, trackers, static
//! files, pages, requests that got no answer, redirects. Every drop is counted with its reason, so nothing vanishes
//! without a trace.

use crate::exchange::Exchange;
use indexmap::IndexMap;

/// Hosts (and their sub-domains) that only measure, advertise or log.
const TRACKERS: [&str; 36] = [
    "google-analytics.com",
    "analytics.google.com",
    "googletagmanager.com",
    "doubleclick.net",
    "googlesyndication.com",
    "googleadservices.com",
    "connect.facebook.net",
    "hotjar.com",
    "hotjar.io",
    "segment.io",
    "segment.com",
    "sentry.io",
    "datadoghq.com",
    "nr-data.net",
    "newrelic.com",
    "mixpanel.com",
    "amplitude.com",
    "clarity.ms",
    "bat.bing.com",
    "fullstory.com",
    "heapanalytics.com",
    "launchdarkly.com",
    "optimizely.com",
    "criteo.com",
    "criteo.net",
    "taboola.com",
    "outbrain.com",
    "adnxs.com",
    "scorecardresearch.com",
    "quantserve.com",
    "cloudflareinsights.com",
    "plausible.io",
    "posthog.com",
    "analytics.tiktok.com",
    "px.ads.linkedin.com",
    "snap.licdn.com",
];

/// What the browser calls a request for a static resource.
const STATIC_KINDS: [&str; 7] = ["image", "stylesheet", "script", "font", "media", "manifest", "texttrack"];

/// File extensions of static resources.
const STATIC_EXTS: [&str; 21] = [
    "js", "mjs", "css", "map", "png", "jpg", "jpeg", "gif", "webp", "avif", "svg", "ico", "woff", "woff2", "ttf",
    "otf", "eot", "mp4", "webm", "mp3", "wav",
];

/// Whether `host` is `domain` or one of its sub-domains.
fn under(host: &str, domain: &str) -> bool {
    host == domain
        || (host.len() > domain.len()
            && host.ends_with(domain)
            && host.as_bytes()[host.len() - domain.len() - 1] == b'.')
}

/// The response media type, lower case: from the body, else from the `content-type` header.
fn response_mime(x: &Exchange) -> String {
    let raw = match &x.resp_body {
        Some(b) if !b.mime.is_empty() => b.mime.clone(),
        _ => x.resp_header("content-type").unwrap_or("").to_string(),
    };
    raw.split(';').next().unwrap_or("").trim().to_ascii_lowercase()
}

fn is_tracker(x: &Exchange, host: &str) -> bool {
    TRACKERS.iter().any(|t| under(host, t))
        || (under(host, "facebook.com") && x.url.path().starts_with("/tr"))
        || x.kind == "ping"
        || x.kind == "beacon"
}

fn is_static(x: &Exchange) -> bool {
    if STATIC_KINDS.contains(&x.kind.as_str()) {
        return true;
    }
    let last = x.url.path().rsplit('/').next().unwrap_or("");
    if let Some((_, ext)) = last.rsplit_once('.') {
        let ext = ext.to_ascii_lowercase();
        if STATIC_EXTS.contains(&ext.as_str()) {
            return true;
        }
    }
    let mime = response_mime(x);
    ["image/", "font/", "video/", "audio/"].iter().any(|p| mime.starts_with(p))
        || mime == "text/css"
        || mime == "text/javascript"
        || mime == "application/javascript"
}

fn is_page(x: &Exchange) -> bool {
    x.kind == "document" || (x.kind.is_empty() && x.method == "GET" && response_mime(x) == "text/html")
}

/// The first reason to drop an exchange, in the order of the module doc.
fn reason(x: &Exchange, hosts: &[String]) -> Option<&'static str> {
    if x.url.scheme() != "http" && x.url.scheme() != "https" {
        return Some("not http");
    }
    let host = x.url.host_str().unwrap_or("").to_ascii_lowercase();
    if !hosts.is_empty() && !hosts.iter().any(|h| under(&host, &h.trim().to_ascii_lowercase())) {
        return Some("other host");
    }
    if x.method == "OPTIONS" {
        return Some("preflight");
    }
    if is_tracker(x, &host) {
        return Some("tracker");
    }
    if is_static(x) {
        return Some("static file");
    }
    if is_page(x) {
        return Some("page");
    }
    if x.status == 0 {
        return Some("no response");
    }
    if (300..400).contains(&x.status) && x.resp_header("set-cookie").is_none() {
        return Some("redirect");
    }
    None
}

/// Which exchanges are worth looking at: the indexes kept (in order) and how many were dropped for each reason.
/// Reasons are checked in this order: `not http`, `other host` (only when `hosts` is not empty: a host equal to one
/// of them or under it is kept), `preflight`, `tracker`, `static file`, `page`, `no response`, `redirect` (a 3xx
/// without a `set-cookie` is only a hop; one that sets a cookie is kept, it is how many logins end).
pub fn sift(all: &[Exchange], hosts: &[String]) -> (Vec<usize>, IndexMap<&'static str, usize>) {
    let mut kept = Vec::new();
    let mut dropped: IndexMap<&'static str, usize> = IndexMap::new();
    for (i, x) in all.iter().enumerate() {
        match reason(x, hosts) {
            Some(why) => *dropped.entry(why).or_insert(0) += 1,
            None => kept.push(i),
        }
    }
    (kept, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::Raw;

    fn ex(method: &str, url: &str, kind: &str, status: u16, mime: &str) -> Exchange {
        Raw {
            method: method.into(),
            url: url.into(),
            status,
            kind: kind.into(),
            resp_body: if mime.is_empty() { None } else { Some((mime.into(), "x".into())) },
            ..Raw::default()
        }
        .into_exchange(0)
        .unwrap()
    }

    fn why(x: Exchange, hosts: &[&str]) -> Option<&'static str> {
        let hosts: Vec<String> = hosts.iter().map(|s| s.to_string()).collect();
        reason(&x, &hosts)
    }

    #[test]
    fn a_json_call_is_kept() {
        let x = ex("GET", "https://api.test/v1/items", "xhr", 200, "application/json");
        assert_eq!(why(x, &[]), None);
    }

    #[test]
    fn other_schemes_are_dropped() {
        assert_eq!(why(ex("GET", "data:text/plain,hi", "", 200, ""), &[]), Some("not http"));
        assert_eq!(why(ex("GET", "wss://a.test/socket", "websocket", 101, ""), &[]), Some("not http"));
    }

    #[test]
    fn other_hosts_are_dropped_only_when_hosts_are_given() {
        let url = "https://cdn.other.test/api/x";
        assert_eq!(why(ex("GET", url, "xhr", 200, "application/json"), &[]), None);
        assert_eq!(why(ex("GET", url, "xhr", 200, "application/json"), &["api.test"]), Some("other host"));
        assert_eq!(why(ex("GET", url, "xhr", 200, "application/json"), &["other.test"]), None);
        assert_eq!(why(ex("GET", "https://other.test/x", "xhr", 200, "application/json"), &["other.test"]), None);
        assert_eq!(
            why(ex("GET", "https://notother.test/x", "xhr", 200, "application/json"), &["other.test"]),
            Some("other host")
        );
    }

    #[test]
    fn preflights_are_dropped() {
        assert_eq!(why(ex("OPTIONS", "https://api.test/v1/items", "preflight", 204, ""), &[]), Some("preflight"));
    }

    #[test]
    fn trackers_are_dropped_by_host_kind_and_facebook_path() {
        assert_eq!(why(ex("POST", "https://www.google-analytics.com/g/collect", "xhr", 204, ""), &[]), Some("tracker"));
        assert_eq!(
            why(ex("POST", "https://o1.ingest.sentry.io/api/1/envelope/", "fetch", 200, ""), &[]),
            Some("tracker")
        );
        assert_eq!(why(ex("GET", "https://www.facebook.com/tr?id=1", "image", 200, ""), &[]), Some("tracker"));
        assert_eq!(why(ex("POST", "https://api.test/log", "ping", 200, ""), &[]), Some("tracker"));
        assert_eq!(why(ex("POST", "https://api.test/log", "beacon", 200, ""), &[]), Some("tracker"));
        assert_eq!(why(ex("GET", "https://www.facebook.com/api/graphql", "xhr", 200, "application/json"), &[]), None);
        assert_eq!(why(ex("GET", "https://notsentry.io.test/x", "xhr", 200, "application/json"), &[]), None);
    }

    #[test]
    fn static_files_are_dropped_by_kind_extension_and_mime() {
        assert_eq!(why(ex("GET", "https://api.test/a", "image", 200, ""), &[]), Some("static file"));
        assert_eq!(why(ex("GET", "https://api.test/app.js?v=3", "fetch", 200, ""), &[]), Some("static file"));
        assert_eq!(why(ex("GET", "https://api.test/logo.PNG", "", 200, ""), &[]), Some("static file"));
        assert_eq!(why(ex("GET", "https://api.test/a/b", "xhr", 200, "image/png"), &[]), Some("static file"));
        assert_eq!(
            why(ex("GET", "https://api.test/a/b", "xhr", 200, "text/css; charset=utf-8"), &[]),
            Some("static file")
        );
        assert_eq!(
            why(ex("GET", "https://api.test/a/b", "xhr", 200, "application/javascript"), &[]),
            Some("static file")
        );
        assert_eq!(why(ex("GET", "https://api.test/s.wav", "xhr", 200, ""), &[]), Some("static file"));
    }

    #[test]
    fn pages_are_dropped_but_an_xhr_returning_html_is_kept() {
        assert_eq!(why(ex("GET", "https://api.test/", "document", 200, "text/html"), &[]), Some("page"));
        assert_eq!(why(ex("GET", "https://api.test/home", "", 200, "text/html"), &[]), Some("page"));
        assert_eq!(why(ex("GET", "https://api.test/fragment", "xhr", 200, "text/html"), &[]), None);
        assert_eq!(why(ex("POST", "https://api.test/form", "", 200, "text/html"), &[]), None);
    }

    #[test]
    fn requests_without_a_response_are_dropped() {
        assert_eq!(why(ex("GET", "https://api.test/v1/items", "xhr", 0, ""), &[]), Some("no response"));
    }

    #[test]
    fn redirects_are_dropped_unless_they_set_a_cookie() {
        assert_eq!(why(ex("GET", "https://api.test/go", "xhr", 302, ""), &[]), Some("redirect"));
        let kept = Raw {
            method: "POST".into(),
            url: "https://api.test/login".into(),
            status: 302,
            kind: "xhr".into(),
            resp_headers: vec![("Set-Cookie".into(), "sid=abc12345; Path=/".into())],
            ..Raw::default()
        }
        .into_exchange(0)
        .unwrap();
        assert_eq!(why(kept, &[]), None);
    }

    #[test]
    fn sift_keeps_order_and_counts_every_drop() {
        let all = vec![
            ex("GET", "https://api.test/a", "xhr", 200, "application/json"),
            ex("GET", "https://api.test/app.css", "stylesheet", 200, "text/css"),
            ex("OPTIONS", "https://api.test/a", "preflight", 204, ""),
            ex("GET", "https://api.test/b", "fetch", 200, "application/json"),
            ex("GET", "https://api.test/c.js", "script", 200, "text/javascript"),
            ex("POST", "https://api.test/c", "fetch", 0, ""),
        ];
        let (kept, dropped) = sift(&all, &[]);
        assert_eq!(kept, vec![0, 3]);
        assert_eq!(dropped.get("static file"), Some(&2));
        assert_eq!(dropped.get("preflight"), Some(&1));
        assert_eq!(dropped.get("no response"), Some(&1));
        assert_eq!(dropped.values().sum::<usize>() + kept.len(), all.len());
    }

    #[test]
    fn the_first_matching_reason_wins() {
        // A tracker's image is a tracker; an OPTIONS to another host is "other host".
        assert_eq!(
            why(ex("GET", "https://www.google-analytics.com/collect.gif", "image", 200, "image/gif"), &[]),
            Some("tracker")
        );
        assert_eq!(why(ex("OPTIONS", "https://x.test/a", "preflight", 204, ""), &["api.test"]), Some("other host"));
    }
}
