//! The description of an API as short text an agent can read in one go: a header with what was kept and dropped, the
//! hosts, the auth schemes, then one block per endpoint, the busiest first, until the budget is used. Only names,
//! types and shapes are printed, never a value that came from a header, a cookie or an opaque string.

use crate::auth::{Auth, Scheme};
use crate::spec::{Api, Endpoint, Payload};

/// How many hosts, auth lines, query parameters, headers and statuses are listed before `…+N`.
const MAX_HOSTS: usize = 6;
const MAX_AUTH: usize = 8;
const MAX_QUERY: usize = 10;
const MAX_HEADERS: usize = 6;
const MAX_STATUS: usize = 4;

fn auth_line(i: usize, a: &Auth) -> String {
    let what = match &a.scheme {
        Scheme::Bearer { header } => format!("bearer in {header}"),
        Scheme::Basic { header } => format!("basic in {header}"),
        Scheme::ApiKeyHeader { name } => format!("api key in header {name}"),
        Scheme::ApiKeyQuery { name } => format!("api key in ?{name}"),
        Scheme::Cookie { names } => {
            let shown: Vec<&str> = names.iter().take(4).map(String::as_str).collect();
            let more = if names.len() > 4 { format!(", …+{}", names.len() - 4) } else { String::new() };
            format!("cookie {}{}", shown.join(", "), more)
        }
        Scheme::Csrf { header, from } => format!("csrf in {header}, from {from}"),
    };
    match &a.source {
        Some(s) => format!("[a{}] {what}, {s}", i + 1),
        None => format!("[a{}] {what}", i + 1),
    }
}

/// `json {…}`, `text/html`, or `empty` for what one status returned.
fn response_line(status: u16, p: &Payload, budget: usize) -> String {
    if p.shape.seen > 0 {
        let extra = if p.non_json > 0 { format!(" (+{} not json)", p.non_json) } else { String::new() };
        format!("-> {status} json {}{extra}", p.shape.sketch(budget))
    } else if !p.mime.is_empty() {
        format!("-> {status} {}", p.mime)
    } else {
        format!("-> {status} empty")
    }
}

fn block(e: &Endpoint, multi: bool) -> String {
    let host = if multi { e.origin.split("://").nth(1).unwrap_or(&e.origin) } else { "" };
    let refs: String = e.auth.iter().map(|i| format!(" a{}", i + 1)).collect();
    let gql = e.gql.as_ref().map(|g| format!(" {g}")).unwrap_or_default();
    let mut lines = vec![format!("[{}] {} {}{}{} x{}{}", e.id, e.method, host, e.path, gql, e.calls, refs)];
    if !e.query.is_empty() {
        let mut items: Vec<String> = e
            .query
            .iter()
            .take(MAX_QUERY)
            .map(|p| format!("{}{} {}", p.name, if p.seen < e.calls { "?" } else { "" }, p.shape.sketch(40)))
            .collect();
        if e.query.len() > MAX_QUERY {
            items.push(format!("…+{}", e.query.len() - MAX_QUERY));
        }
        lines.push(format!("  query: {}", items.join(", ")));
    }
    if !e.headers.is_empty() {
        let mut items: Vec<String> = e.headers.iter().take(MAX_HEADERS).cloned().collect();
        if e.headers.len() > MAX_HEADERS {
            items.push(format!("…+{}", e.headers.len() - MAX_HEADERS));
        }
        lines.push(format!("  headers: {}", items.join(", ")));
    }
    if let Some(b) = &e.body {
        let label = if e.gql.is_some() { "vars" } else { "body" };
        if b.shape.seen > 0 {
            let extra = if b.non_json > 0 { format!(" (+{} not json)", b.non_json) } else { String::new() };
            lines.push(format!("  {label}: {}{extra}", b.shape.sketch(200)));
        } else {
            let mime = if b.mime.is_empty() { "not json" } else { b.mime.as_str() };
            lines.push(format!("  {label}: {mime}"));
        }
    }
    for (n, (status, p)) in e.responses.iter().enumerate() {
        if n == MAX_STATUS {
            lines.push(format!("  -> …+{} more statuses", e.responses.len() - MAX_STATUS));
            break;
        }
        lines.push(format!("  {}", response_line(*status, p, if n == 0 { 160 } else { 80 })));
    }
    if let Some(p) = &e.paging {
        lines.push(format!("  paging: {}", p.describe()));
    }
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

/// The summary. The header, hosts and auth lines always print; endpoint blocks are added while the whole stays within
/// `budget` characters, and the ones left out are counted in a last line.
pub fn render(api: &Api, budget: usize) -> String {
    let dropped_total: usize = api.dropped.values().sum();
    let kept = api.total.saturating_sub(dropped_total);
    let mut reasons: Vec<(&&str, &usize)> = api.dropped.iter().collect();
    reasons.sort_by(|a, b| b.1.cmp(a.1));
    let dropped = if reasons.is_empty() {
        "none".to_string()
    } else {
        reasons.iter().map(|(r, n)| format!("{n} {r}")).collect::<Vec<_>>().join(", ")
    };
    let mut out =
        format!("{} calls, {} kept, {} endpoints; dropped: {}\n", api.total, kept, api.endpoints.len(), dropped);

    if !api.hosts.is_empty() {
        let mut hosts: Vec<String> = api.hosts.iter().take(MAX_HOSTS).cloned().collect();
        if api.hosts.len() > MAX_HOSTS {
            hosts.push(format!("…+{}", api.hosts.len() - MAX_HOSTS));
        }
        out.push_str(&format!("hosts: {}\n", hosts.join(", ")));
    }
    if api.auth.is_empty() {
        out.push_str("auth: none seen\n");
    } else {
        out.push_str("auth:\n");
        for (i, a) in api.auth.iter().enumerate().take(MAX_AUTH) {
            out.push_str(&format!("  {}\n", auth_line(i, a)));
        }
        if api.auth.len() > MAX_AUTH {
            out.push_str(&format!("  …+{} more\n", api.auth.len() - MAX_AUTH));
        }
    }

    let multi = api.hosts.len() > 1;
    let total = api.endpoints.len();
    let mut shown = 0;
    for (i, e) in api.endpoints.iter().enumerate() {
        let b = block(e, multi);
        let tail = if i + 1 < total { tail_line(total - i - 1).chars().count() } else { 0 };
        if out.chars().count() + b.chars().count() + tail > budget {
            break;
        }
        out.push_str(&b);
        shown += 1;
    }
    if shown < total {
        out.push_str(&tail_line(total - shown));
    }
    out
}

fn tail_line(n: usize) -> String {
    format!("… {n} more endpoints (--json for all)\n")
}
