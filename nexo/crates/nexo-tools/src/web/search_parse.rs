use super::*;

/// `base?k=v&…`, encoded.
pub(super) fn with_query(base: &str, pairs: &[(&str, &str)]) -> Result<Url, String> {
    let mut u = Url::parse(base).map_err(|e| format!("bad address {base}: {e}"))?;
    u.query_pairs_mut().extend_pairs(pairs.iter().copied());
    Ok(u)
}

/// `k=v&…` as an HTML form sends it.
pub(super) fn form_encode(pairs: &[(&str, &str)]) -> String {
    let mut u = Url::parse("http://form.invalid/").expect("a fixed URL");
    u.query_pairs_mut().extend_pairs(pairs.iter().copied());
    u.query().unwrap_or("").to_string()
}

pub(super) fn net_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".into()
    } else if e.is_connect() {
        format!("cannot connect ({e})")
    } else {
        e.to_string()
    }
}

pub(super) fn with_sites(query: &str, domains: &[String]) -> String {
    match domains.len() {
        0 => query.to_string(),
        1 => format!("{query} site:{}", domains[0]),
        _ => format!("{query} ({})", domains.iter().map(|d| format!("site:{d}")).collect::<Vec<_>>().join(" OR ")),
    }
}

/// Hits out of a JSON list: the first non-empty snippet field wins (a list of strings is joined).
pub(super) fn hits(list: &Value, title: &str, url: &str, snippet: &[&str]) -> Vec<Hit> {
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
pub(super) fn clean(s: &str) -> String {
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
pub(super) fn ddg_target(raw: &str) -> Option<String> {
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

pub(super) fn render_hits(query: &str, backend: &str, hits: &[Hit], note: Option<&str>) -> String {
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

pub(super) fn domains_arg(args: &Value) -> Vec<String> {
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
