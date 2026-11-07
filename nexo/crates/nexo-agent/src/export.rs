//! Sharing a session: one self-contained HTML file, markdown or JSON, with secrets taken out first.
//!
//! The HTML has inline CSS only, no script, no font and no address that is loaded, and it follows the
//! reader's light or dark setting. Text is escaped everywhere and markdown is rendered without raw
//! HTML passing through: a message holding `<script>` shows as the text it is.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;

use nexo_core::message::{Message, Part, Role};
use nexo_core::stream::Usage;
use nexo_core::tool::{ToolDisplay, ToolOutput};
use pulldown_cmark::{html, Event, Options, Parser, Tag, TagEnd};
use serde_json::{json, Value};

use crate::scrub::{scrub, scrub_value, touches_env_file};
use crate::session::Session;

/// What an export is written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Html,
    Markdown,
    Json,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        match s.to_ascii_lowercase().as_str() {
            "html" => Some(Format::Html),
            "md" | "markdown" => Some(Format::Markdown),
            "json" => Some(Format::Json),
            _ => None,
        }
    }

    /// The format a file name asks for (`.html`, `.json`), markdown otherwise.
    pub fn of_path(p: &std::path::Path) -> Format {
        p.extension().and_then(|e| e.to_str()).and_then(Format::parse).unwrap_or(Format::Markdown)
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Html => "html",
            Format::Markdown => "md",
            Format::Json => "json",
        }
    }
}

/// The session as it is shared, redacted or not.
struct Doc {
    id: String,
    title: String,
    created: String,
    provider: String,
    model: String,
    cwd: String,
    messages: Vec<Message>,
    outputs: BTreeMap<String, ToolOutput>,
    usage: Usage,
    cost: f64,
}

const MASK: &str = nexo_core::redact::MASK;

impl Doc {
    fn from_session(s: &Session, redact: bool) -> Doc {
        let meta = s.meta();
        let mut messages = s.messages().to_vec();
        let mut outputs = s.outputs().clone();
        let mut title = meta.title.clone();
        let mut cwd = meta.cwd.display().to_string();
        if redact {
            // the calls that touch a .env file: their arguments and results are dropped whole
            let mut env_ids: HashSet<String> = HashSet::new();
            for m in &messages {
                for p in &m.content {
                    if let Part::ToolCall { id, args, .. } = p {
                        if touches_env_file(args) {
                            env_ids.insert(id.clone());
                        }
                    }
                }
            }
            for m in &mut messages {
                for p in &mut m.content {
                    scrub_part(p, &env_ids);
                }
            }
            for (id, o) in outputs.iter_mut() {
                *o = if env_ids.contains(id) { ToolOutput::ok(MASK) } else { scrub_output(o) };
            }
            title = scrub(&title);
            cwd = scrub(&cwd);
        }
        Doc {
            id: meta.id.clone(),
            title,
            created: meta.created.clone(),
            provider: meta.provider.clone(),
            model: meta.model.clone(),
            cwd,
            messages,
            outputs,
            usage: s.usage(),
            cost: s.cost(),
        }
    }

    /// The turns: requests the person typed (tool results are not).
    fn turns(&self) -> usize {
        self.messages.iter().filter(|m| m.role == Role::User && !m.is_tool_results()).count()
    }

    /// What a call answered: the recorded output, else the result in the next message.
    fn result_of(&self, call_id: &str) -> Option<(String, bool)> {
        if let Some(o) = self.outputs.get(call_id) {
            return Some((o.content.clone(), o.is_error));
        }
        self.messages.iter().flat_map(|m| m.content.iter()).find_map(|p| match p {
            Part::ToolResult { call_id: c, content, is_error, .. } if c == call_id => {
                Some((Session::result_text(content), *is_error))
            }
            _ => None,
        })
    }
}

fn scrub_part(p: &mut Part, env_ids: &HashSet<String>) {
    match p {
        Part::Text { text } => *text = scrub(text),
        Part::Reasoning { text, signature, encrypted, .. } => {
            *text = scrub(text);
            *signature = None;
            *encrypted = None;
        }
        Part::RedactedReasoning { data, .. } => data.clear(),
        Part::ToolCall { id, args, .. } => {
            if env_ids.contains(id.as_str()) {
                hide_contents(args);
            }
            scrub_value(args);
        }
        Part::ToolResult { call_id, content, .. } => {
            if env_ids.contains(call_id.as_str()) {
                *content = vec![Part::text(MASK)];
            } else {
                content.iter_mut().for_each(|c| scrub_part(c, env_ids));
            }
        }
        Part::Image { .. } => {}
    }
}

/// Every string of the arguments but the path and the command, which say which file it was.
fn hide_contents(args: &mut Value) {
    if let Value::Object(o) = args {
        for (k, v) in o.iter_mut() {
            if !matches!(k.as_str(), "path" | "file_path" | "command") {
                if let Value::String(s) = v {
                    *s = MASK.to_string();
                }
            }
        }
    }
}

fn scrub_output(o: &ToolOutput) -> ToolOutput {
    let mut v = serde_json::to_value(&o.display).unwrap_or(Value::Null);
    scrub_value(&mut v);
    let display = serde_json::from_value(v).unwrap_or(ToolDisplay::Plain);
    ToolOutput { content: scrub(&o.content), is_error: o.is_error, display }
}

/// The session in `format`. `summary` names what a call does (`read src/a.rs`) from the tool and its
/// arguments; `redact` takes secrets out first (always on unless the person turned it off).
pub fn render(session: &Session, format: Format, redact: bool, summary: &dyn Fn(&str, &Value) -> String) -> String {
    match format {
        Format::Markdown => {
            let text = session.export_markdown(summary);
            if redact {
                scrub(&text)
            } else {
                text
            }
        }
        Format::Json => json_of(&Doc::from_session(session, redact)),
        Format::Html => html_of(&Doc::from_session(session, redact), summary),
    }
}

fn json_of(d: &Doc) -> String {
    let v = json!({
        "id": d.id,
        "title": d.title,
        "created": d.created,
        "provider": d.provider,
        "model": d.model,
        "cwd": d.cwd,
        "turns": d.turns(),
        "usage": {
            "input": d.usage.input,
            "output": d.usage.output,
            "cache_read": d.usage.cache_read,
            "cache_write": d.usage.cache_write,
        },
        "cost_usd": d.cost,
        "messages": d.messages,
        "tool_outputs": d.outputs,
    });
    serde_json::to_string_pretty(&v).unwrap_or_default() + "\n"
}

/// HTML-escaped text, also safe inside a quoted attribute.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Whether a link may be a link: web and mail addresses, and places in the same page.
fn safe_url(u: &str) -> bool {
    let u = u.trim().to_ascii_lowercase();
    u.starts_with("https://") || u.starts_with("http://") || u.starts_with("mailto:") || u.starts_with('#')
}

/// Markdown as safe HTML: raw HTML in the text is shown as text, images as their alt text, and only
/// safe links stay links.
pub fn markdown_html(text: &str) -> String {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut links: Vec<bool> = Vec::new();
    let events = Parser::new_ext(text, opts).filter_map(|ev| match ev {
        Event::Html(s) | Event::InlineHtml(s) => Some(Event::Text(s)),
        Event::Start(Tag::Image { .. }) | Event::End(TagEnd::Image) => None,
        Event::Start(Tag::Link { ref dest_url, .. }) => {
            let ok = safe_url(dest_url);
            links.push(ok);
            ok.then_some(ev)
        }
        Event::End(TagEnd::Link) => links.pop().unwrap_or(false).then_some(ev),
        ev => Some(ev),
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

/// The lines of a text, at most `max`, with a note when more were left out.
fn clipped(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max {
        return text.to_string();
    }
    format!("{}\n... {} more lines", lines[..max].join("\n"), lines.len() - max)
}

const OUTPUT_LINES: usize = 400;

fn diff_html(old: &str, new: &str, path: &str) -> String {
    let mut out = format!("<pre class=\"diff\"><span class=\"hunk\">{}</span>\n", esc(path));
    for l in clipped(old, OUTPUT_LINES).lines() {
        let _ = writeln!(out, "<span class=\"del\">- {}</span>", esc(l));
    }
    for l in clipped(new, OUTPUT_LINES).lines() {
        let _ = writeln!(out, "<span class=\"add\">+ {}</span>", esc(l));
    }
    out.push_str("</pre>");
    out
}

fn tool_html(d: &Doc, id: &str, name: &str, args: &Value, summary: &dyn Fn(&str, &Value) -> String) -> String {
    let result = d.result_of(id);
    let (state, label) = match &result {
        None => ("none", "no result"),
        Some((_, true)) => ("err", "error"),
        Some(_) => ("ok", "ok"),
    };
    let mut out = format!(
        "<details class=\"tool\"><summary><b>{}</b> <span class=\"sum\">{}</span> <span class=\"st {}\">{}</span></summary>\n",
        esc(name),
        esc(&summary(name, args)),
        state,
        label
    );
    let shown = args.to_string();
    let shown = serde_json::to_string_pretty(args).unwrap_or(shown);
    let _ = writeln!(out, "<div class=\"lbl\">arguments</div><pre>{}</pre>", esc(&clipped(&shown, OUTPUT_LINES)));
    if let Some(o) = d.outputs.get(id) {
        if let ToolDisplay::Diff { path, old, new, .. } = &o.display {
            let _ = writeln!(out, "<div class=\"lbl\">change</div>{}", diff_html(old, new, path));
        }
    }
    if let Some((body, _)) = &result {
        let _ = writeln!(out, "<div class=\"lbl\">result</div><pre>{}</pre>", esc(&clipped(body, OUTPUT_LINES)));
    }
    out.push_str("</details>\n");
    out
}

fn html_of(d: &Doc, summary: &dyn Fn(&str, &Value) -> String) -> String {
    let title = if d.title.is_empty() { "nexo session".to_string() } else { d.title.clone() };
    let mut out = String::with_capacity(16 * 1024);
    let _ = write!(
        out,
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"color-scheme\" content=\"light dark\">\n<title>{}</title>\n<style>{}</style>\n</head>\n<body>\n",
        esc(&title),
        CSS
    );
    let u = d.usage;
    let _ = write!(
        out,
        "<header>\n<h1>{}</h1>\n<dl>\n<div><dt>model</dt><dd>{}/{}</dd></div>\n<div><dt>date</dt><dd>{}</dd></div>\n\
         <div><dt>turns</dt><dd>{}</dd></div>\n\
         <div><dt>tokens</dt><dd>{} in, {} out, {} cached</dd></div>\n<div><dt>cost</dt><dd>${:.4}</dd></div>\n</dl>\n\
         </header>\n<main>\n",
        esc(&title),
        esc(&d.provider),
        esc(&d.model),
        esc(&d.created),
        d.turns(),
        u.input + u.cache_write,
        u.output,
        u.cache_read,
        d.cost
    );
    for m in &d.messages {
        if m.is_tool_results() {
            continue; // shown under the calls that made them
        }
        let (class, who) = if m.role == Role::User { ("user", "You") } else { ("assistant", "nexo") };
        let _ = writeln!(out, "<section class=\"msg {class}\"><h2>{who}</h2>");
        for p in &m.content {
            match p {
                Part::Text { text } if !text.trim().is_empty() => {
                    let _ = writeln!(out, "<div class=\"md\">{}</div>", markdown_html(text));
                }
                Part::Reasoning { text, .. } if !text.trim().is_empty() => {
                    let _ = writeln!(
                        out,
                        "<details class=\"thinking\"><summary>reasoning</summary><div class=\"md\">{}</div></details>",
                        markdown_html(text)
                    );
                }
                Part::ToolCall { id, name, args } => out.push_str(&tool_html(d, id, name, args, summary)),
                Part::Image { .. } => out.push_str("<p class=\"note\">[image left out]</p>\n"),
                _ => {}
            }
        }
        out.push_str("</section>\n");
    }
    out.push_str("</main>\n<footer>Exported from nexo. Self-contained file: no scripts, no network.</footer>\n</body>\n</html>\n");
    out
}

const CSS: &str = "\
:root{color-scheme:light dark;--bg:#fafafa;--fg:#1c2128;--mut:#667085;--card:#fff;--line:#d9dee5;--user:#eef4ff;\
--code:#f2f4f7;--add:#e6f6ea;--addfg:#116329;--del:#fdeaea;--delfg:#a40e26;--ok:#116329;--err:#a40e26;--link:#0b5fd8}\
@media (prefers-color-scheme:dark){:root{--bg:#0f1318;--fg:#e6e9ee;--mut:#98a2b3;--card:#171c23;--line:#2a313b;\
--user:#16233a;--code:#1f252d;--add:#12301c;--addfg:#7ee2a0;--del:#3a1620;--delfg:#ff9aa8;--ok:#7ee2a0;--err:#ff9aa8;--link:#7fb2ff}}\
*{box-sizing:border-box}\
body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.55 system-ui,-apple-system,Segoe UI,Roboto,sans-serif}\
header,main,footer{max-width:60rem;margin:0 auto;padding:1rem 1.25rem}\
h1{font-size:1.5rem;margin:.5rem 0}\
dl{display:flex;flex-wrap:wrap;gap:.5rem 1.5rem;margin:0;color:var(--mut)}\
dl div{display:flex;gap:.4rem}dt{font-weight:600}dd{margin:0}\
.msg{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:.25rem 1rem .75rem;margin:1rem 0}\
.msg.user{background:var(--user)}\
.msg h2{font-size:.8rem;text-transform:uppercase;letter-spacing:.06em;color:var(--mut);margin:.75rem 0 .25rem}\
.md p{margin:.5rem 0}.md a{color:var(--link)}\
pre,code{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:.88rem}\
pre{background:var(--code);border-radius:6px;padding:.6rem .8rem;overflow:auto;white-space:pre-wrap;word-break:break-word;margin:.4rem 0}\
:not(pre)>code{background:var(--code);padding:.1rem .3rem;border-radius:4px}\
table{border-collapse:collapse}td,th{border:1px solid var(--line);padding:.25rem .5rem}\
blockquote{margin:.5rem 0;padding-left:1rem;border-left:3px solid var(--line);color:var(--mut)}\
details{margin:.5rem 0}summary{cursor:pointer}\
details.tool{border:1px solid var(--line);border-radius:8px;padding:.4rem .7rem}\
details.thinking{color:var(--mut)}\
.sum{color:var(--mut);font-family:ui-monospace,Menlo,Consolas,monospace;font-size:.85rem}\
.st{font-size:.75rem;border:1px solid currentColor;border-radius:99px;padding:0 .45rem;margin-left:.3rem}\
.st.ok{color:var(--ok)}.st.err{color:var(--err)}.st.none{color:var(--mut)}\
.lbl{font-size:.75rem;color:var(--mut);margin-top:.5rem;text-transform:uppercase;letter-spacing:.05em}\
.diff .add{display:block;background:var(--add);color:var(--addfg)}\
.diff .del{display:block;background:var(--del);color:var(--delfg)}\
.diff .hunk{color:var(--mut)}\
.note,footer{color:var(--mut);font-size:.85rem}\
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_html_and_script_links_are_inert() {
        let h = markdown_html("hi <script>alert(1)</script> [x](javascript:alert(1)) ![i](http://e.com/a.png)");
        assert!(!h.contains("<script"), "{h}");
        assert!(!h.contains("href=\"javascript"), "{h}");
        assert!(!h.contains("<img"), "{h}");
        assert!(h.contains("&lt;script&gt;"), "{h}");
        let ok = markdown_html("[site](https://example.com)");
        assert!(ok.contains("href=\"https://example.com\""), "{ok}");
    }

    #[test]
    fn an_edit_shows_as_a_diff_and_env_calls_are_hidden() {
        let mut s = Session::ephemeral(std::path::Path::new("."), "p", "m");
        s.append(&Message::user("fix it")).unwrap();
        let calls = vec![
            Part::tool_call("c1", "edit", json!({"path": "a.rs", "old_string": "x", "new_string": "y"})),
            Part::tool_call("c2", "read", json!({"path": ".env"})),
        ];
        s.append(&Message::new(Role::Assistant, calls)).unwrap();
        let res = vec![Part::tool_result("c1", "edited", false), Part::tool_result("c2", "SECRET_THING=abc", false)];
        s.append(&Message::tool_results(res)).unwrap();
        let diff = ToolOutput {
            content: "edited".into(),
            is_error: false,
            display: ToolDisplay::Diff { path: "a.rs".into(), old: "x".into(), new: "y".into(), start_line: 1 },
        };
        s.record_output("c1", &diff).unwrap();
        s.record_output("c2", &ToolOutput::ok("SECRET_THING=abc")).unwrap();
        let h = render(&s, Format::Html, true, &|n, _| n.to_string());
        assert!(h.contains("class=\"del\">- x") && h.contains("class=\"add\">+ y"), "{h}");
        assert!(!h.contains("SECRET_THING"), "{h}");
        assert_eq!(h.matches("<details class=\"tool\"").count(), 2);
        let raw = render(&s, Format::Html, false, &|n, _| n.to_string());
        assert!(raw.contains("SECRET_THING"));
    }

    #[test]
    fn esc_covers_the_attribute_characters() {
        assert_eq!(esc("<a href=\"x\">&'"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;");
    }
}
