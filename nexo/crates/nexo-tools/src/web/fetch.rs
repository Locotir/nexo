use super::*;
use encoding_rs::Encoding;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::time::Instant;

// ------------------------------------------------------------------ fetch

/// How many converted pages `web_fetch` keeps for continuation calls.
const PAGE_COPIES: usize = 4;
/// How long a kept copy answers a continuation call.
const PAGE_COPY_TTL: Duration = Duration::from_secs(60);
/// How much of an error response body is read, enough for a JSON error or the head of a challenge page.
const ERROR_BODY_READ: usize = 8 * 1024;
/// How many characters of that body the model is shown.
const ERROR_BODY_SHOWN: usize = 500;

/// A page as `web_fetch` converted it, kept so that reading on does not download it again.
#[derive(Clone)]
pub(super) struct PageCopy {
    /// The URL as requested (redirects are resolved inside one fetch, so the final URL is not the key).
    pub(super) key: String,
    pub(super) at: Instant,
    pub(super) final_url: Url,
    pub(super) text: String,
    /// Why only the start of the page was read, when that is so.
    pub(super) cut: Option<String>,
    pub(super) charset: Option<&'static str>,
}

pub struct WebFetch {
    http: Result<reqwest::Client, String>,
    pub(super) copies: Mutex<VecDeque<PageCopy>>,
}

impl WebFetch {
    pub fn new(cfg: &WebConfig) -> WebFetch {
        // redirects are followed by hand, each one checked (`hop`)
        WebFetch {
            copies: Mutex::new(VecDeque::new()),
            http: nexo_http::build_client(|| {
                builder(cfg).redirect(reqwest::redirect::Policy::none()).dns_resolver(Arc::new(PublicResolver))
            }),
        }
    }

    fn http(&self) -> Result<&reqwest::Client, String> {
        self.http.as_ref().map_err(Clone::clone)
    }

    /// The kept copy of `key` while it is under a minute old.
    pub(super) fn copy_of(&self, key: &str) -> Option<PageCopy> {
        let copies = self.copies.lock().unwrap_or_else(|e| e.into_inner());
        copies.iter().find(|c| c.key == key && c.at.elapsed() < PAGE_COPY_TTL).cloned()
    }

    /// Keeps the converted page of `copy.key`, replacing an older copy of it; the oldest copy goes when full.
    pub(super) fn keep(&self, copy: PageCopy) {
        let mut copies = self.copies.lock().unwrap_or_else(|e| e.into_inner());
        copies.retain(|c| c.key != copy.key);
        if copies.len() >= PAGE_COPIES {
            copies.pop_front();
        }
        copies.push_back(copy);
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

/// The page text in the charset the page declares: a byte order mark first, then the `charset` of the Content-Type,
/// then (for HTML) a `<meta>` in the first KiB, as browsers do. When none names a charset this knows, UTF-8 if the
/// bytes are UTF-8, else windows-1252, as browsers read an unlabeled legacy page.
/// `cut` says the body was cut short (the end of the page is missing): only then may it end inside a UTF-8 character.
/// The second value is the charset name (and why) when it was not UTF-8, so the model can be told.
pub(super) fn decode<'a>(
    content_type: &str,
    body: &'a [u8],
    html: bool,
    cut: bool,
) -> (Cow<'a, str>, Option<&'static str>) {
    static HEADER: OnceLock<regex::bytes::Regex> = OnceLock::new();
    static META: OnceLock<regex::bytes::Regex> = OnceLock::new();
    let (found, text) = match Encoding::for_bom(body) {
        Some((enc, skip)) => (Some(enc), &body[skip..]),
        None => {
            let header =
                HEADER.get_or_init(|| regex::bytes::Regex::new(r#"(?i)charset\s*=\s*["']?\s*([^"'>;\s/]+)"#).unwrap());
            let meta = META.get_or_init(|| {
                regex::bytes::Regex::new(r#"(?i)<meta[^>]*?charset\s*=\s*["']?\s*([^"'>;\s/]+)"#).unwrap()
            });
            // the replacement encoding would turn a whole page into one U+FFFD, so a label that maps to it is read as no label
            let known = |label: &[u8]| Encoding::for_label(label).filter(|e| *e != encoding_rs::REPLACEMENT);
            let by_header = header.captures(content_type.as_bytes()).and_then(|c| known(&c[1]));
            let by_meta = || {
                let head = &body[..body.len().min(1024)];
                meta.captures(head).and_then(|c| known(&c[1])).map(|enc| {
                    // a page that is readable as ASCII here cannot really be UTF-16, so the label is wrong
                    if enc == encoding_rs::UTF_16LE || enc == encoding_rs::UTF_16BE {
                        encoding_rs::UTF_8
                    } else {
                        enc
                    }
                })
            };
            (by_header.or_else(|| if html { by_meta() } else { None }), body)
        }
    };
    match found {
        Some(enc) if enc != encoding_rs::UTF_8 => (enc.decode_without_bom_handling(text).0, Some(enc.name())),
        Some(_) => (String::from_utf8_lossy(text), None),
        None => match std::str::from_utf8(text) {
            Ok(_) => (String::from_utf8_lossy(text), None),
            // the bytes end inside a character because the body was cut: what is whole is UTF-8
            Err(e) if cut && e.error_len().is_none() => (String::from_utf8_lossy(&text[..e.valid_up_to()]), None),
            Err(_) => (
                encoding_rs::WINDOWS_1252.decode_without_bom_handling(text).0,
                Some("windows-1252, as the page names no charset and is not UTF-8"),
            ),
        },
    }
}

/// What an error response said, for the model: the start of its body when it is text, JSON, XML or HTML.
/// A response without a Content-Type shows nothing, as before.
fn error_body(content_type: &str, raw: &[u8], base: &Url) -> Option<String> {
    if content_type.trim().is_empty() || raw.is_empty() {
        return None;
    }
    let (text, _) = decode(content_type, raw, true, raw.len() >= ERROR_BODY_READ);
    let text = match kind_of(content_type, raw) {
        Kind::Html => html_to_markdown(&text, Some(base)),
        Kind::Text => text.into_owned(),
        Kind::Refused(_) => return None,
    };
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    Some(match text.char_indices().nth(ERROR_BODY_SHOWN) {
        Some((cut, _)) => format!("Response body (first {ERROR_BODY_SHOWN} characters): {}", &text[..cut]),
        None => format!("Response body: {text}"),
    })
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
            let ct = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            if !status.is_success() {
                let mut say =
                    format!("HTTP {} {} for {final_url}", status.as_u16(), status.canonical_reason().unwrap_or(""));
                // when to try again is a fact the server gives only here
                if let Some(wait) = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                {
                    say.push_str(&format!(". Retry-After: {wait}."));
                }
                if matches!(status.as_u16(), 404 | 410) {
                    say.push_str(
                        " The page does not exist: do not guess URLs; use one that a search result or a page gave you.",
                    );
                }
                // why it failed (an API's JSON error, a challenge page) is in the body; a broken read just shows none
                let mut raw: Vec<u8> = Vec::new();
                while raw.len() < ERROR_BODY_READ {
                    match resp.chunk().await {
                        Ok(Some(chunk)) => raw.extend_from_slice(&chunk),
                        _ => break,
                    }
                }
                raw.truncate(ERROR_BODY_READ);
                if let Some(shown) = error_body(&ct, &raw, &final_url) {
                    say.push_str(&format!("\n\n{shown}"));
                }
                return Err(say);
            }
            // a type that cannot be read is known from the headers: its body is not downloaded to be refused
            if let Kind::Refused(what) = kind_of(&ct, &[]) {
                let size = resp
                    .headers()
                    .get(reqwest::header::CONTENT_LENGTH)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok());
                return Err(refused_text(&final_url, &what, size));
            }
            let mut body: Vec<u8> = Vec::new();
            let mut cut: Option<String> = None;
            loop {
                match resp.chunk().await {
                    Ok(Some(chunk)) => body.extend_from_slice(&chunk),
                    Ok(None) => break,
                    // what arrived before the connection broke is kept; with nothing received it is the error
                    Err(e) if body.is_empty() => return Err(net_error(&e)),
                    Err(e) => {
                        let got = match body.len() {
                            n if n < 1024 => format!("{n} bytes"),
                            n => format!("{} KB", (n + 512) / 1024),
                        };
                        cut = Some(format!(
                            "the connection ended after {got} of the body ({}); only its start was read",
                            net_error(&e)
                        ));
                        break;
                    }
                }
                if body.len() > MAX_BODY {
                    body.truncate(MAX_BODY);
                    cut = Some("the page is over 5 MB; only its start was read".into());
                    break;
                }
            }
            Ok::<_, String>(Ok((final_url, ct, body, cut)))
        };
        // reading on from `start` uses the copy of the last read of this URL; a read from the start always fetches
        let kept = if start > 0 { self.copy_of(url.as_str()) } else { None };
        let (final_url, text, cut, charset, age, fresh) = match kept {
            Some(c) => (c.final_url, c.text, c.cut, c.charset, Some(c.at.elapsed()), false),
            None => {
                let (final_url, ct, body, cut) = tokio::select! {
                    r = work => match r {
                        Ok(Ok(v)) => v,
                        Ok(Err(say)) => return ToolOutput::ok(format!("URL: {url}\n\n{say}")),
                        Err(e) => return ToolOutput::error(format!("web_fetch: {e}")),
                    },
                    _ = ctx.cancel.cancelled() => return ToolOutput::error("web_fetch: cancelled"),
                };
                let (text, charset) = match kind_of(&ct, &body) {
                    Kind::Html => {
                        let (html, charset) = decode(&ct, &body, true, cut.is_some());
                        (html_to_markdown(&html, Some(&final_url)), charset)
                    }
                    Kind::Text => {
                        let (text, charset) = decode(&ct, &body, false, cut.is_some());
                        (text.into_owned(), charset)
                    }
                    Kind::Refused(what) => {
                        return ToolOutput::error(format!("web_fetch: {}", refused_text(&final_url, &what, None)));
                    }
                };
                (final_url, text, cut, charset, None, true)
            }
        };
        let (piece, note) = page(&text, start, max);
        let mut out = format!("URL: {final_url}\n");
        if final_url != url {
            out.push_str(&format!("(redirected from {url})\n"));
        }
        if let Some(why) = &cut {
            out.push_str(&format!("({why})\n"));
        }
        if let Some(name) = charset {
            out.push_str(&format!("(decoded as {name})\n"));
        }
        if let Some(age) = age {
            out.push_str(&format!("(read from the copy fetched {} s ago)\n", age.as_secs()));
        }
        out.push('\n');
        out.push_str(&piece);
        if let Some(n) = note {
            out.push_str(&format!("\n\n{n}"));
        }
        if fresh {
            self.keep(PageCopy { key: url.to_string(), at: Instant::now(), final_url, text, cut, charset });
        }
        ToolOutput::ok(out)
    }
}

/// Why a type `web_fetch` cannot read is refused, without the tool prefix its callers add. A PDF also gets what is known of the file.
fn refused_text(final_url: &Url, what: &str, size: Option<u64>) -> String {
    let mut say = format!("{final_url} is {what}, not text; web_fetch reads HTML, text, Markdown and JSON");
    if what == "application/pdf" {
        match size {
            Some(n) => say.push_str(&format!(". The file is a PDF ({n} bytes)")),
            None => say.push_str(". The file is a PDF"),
        }
        say.push('.');
    }
    say
}
