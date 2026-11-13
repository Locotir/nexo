use super::*;

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
pub(super) enum Kind {
    Html,
    Text,
    Refused(String),
}

pub(super) fn kind_of(content_type: &str, body: &[u8]) -> Kind {
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
pub(super) fn page(text: &str, start: usize, max: usize) -> (String, Option<String>) {
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

pub(super) fn shown_url(u: &Url) -> String {
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
                    "url": {"type": "string"},
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
