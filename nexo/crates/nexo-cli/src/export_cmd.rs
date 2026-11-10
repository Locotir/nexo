//! `nexo export`: write a session as a shareable file (HTML, markdown or JSON), secrets taken out.

use std::path::PathBuf;

use clap::Args;
use nexo_agent::export::{render, Format};
use nexo_agent::session::{self, Session};
use serde_json::Value;

#[derive(Args, Debug)]
pub struct ExportArgs {
    /// The session id (`nexo sessions` lists them). Default: the latest session of this directory, else the newest one.
    pub session: Option<String>,
    /// Use the latest session (the default when no id is given).
    #[arg(long, conflicts_with = "session")]
    pub last: bool,
    /// One self-contained HTML file: inline styles, no scripts, light and dark (the default format).
    #[arg(long, group = "format")]
    pub html: bool,
    /// Markdown.
    #[arg(long, group = "format")]
    pub markdown: bool,
    /// JSON: the messages and the tool outputs.
    #[arg(long, group = "format")]
    pub json: bool,
    /// Write here (default: <session id>.html, .md or .json in the current directory; `-` prints).
    #[arg(short = 'o', long, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Keep keys, tokens and .env contents in the file (a warning is printed).
    #[arg(long)]
    pub no_redact: bool,
}

pub fn run(a: &ExportArgs, cwd: &std::path::Path) -> anyhow::Result<i32> {
    let format = if a.json {
        Format::Json
    } else if a.markdown {
        Format::Markdown
    } else if a.html {
        Format::Html
    } else {
        a.output
            .as_deref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .and_then(Format::parse)
            .unwrap_or(Format::Html)
    };
    let session = match &a.session {
        Some(id) => Session::open(id).map_err(|_| anyhow::anyhow!("no session {id:?}; `nexo sessions` lists them"))?,
        None => {
            // a session nothing was said in has nothing to export
            let Some(meta) = session::latest_for(cwd).or_else(|| session::list().into_iter().find(|m| m.messages > 0))
            else {
                println!("no sessions yet");
                return Ok(0);
            };
            Session::open(&meta.id)?
        }
    };
    if a.no_redact {
        eprintln!("nexo: warning: --no-redact keeps keys, tokens and .env contents in the file; do not share it");
    }
    let tools = nexo_tools::builtin();
    let summary = |name: &str, args: &Value| match tools.get(name) {
        Some(t) => t.summary(args),
        None => format!("{name} {}", args.to_string().chars().take(80).collect::<String>()),
    };
    let text = render(&session, format, !a.no_redact, &summary);
    if a.output.as_deref().is_some_and(|p| p.as_os_str() == "-") {
        print!("{text}");
        return Ok(0);
    }
    let path = a.output.clone().unwrap_or_else(|| PathBuf::from(format!("{}.{}", session.id(), format.extension())));
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, text)?;
    println!("exported {} to {}", session.id(), path.display());
    Ok(0)
}
