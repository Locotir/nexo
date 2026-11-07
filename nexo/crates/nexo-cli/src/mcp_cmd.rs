//! `nexo mcp add | list | remove | login`: manage the `[mcp.<name>]` tables of the configuration
//! files nexo reads (the user's, or the project's `.nexo/config.toml`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use clap::Subcommand;
use nexo_core::config::McpServerConfig;
use nexo_core::Config;
use toml_edit::{value, Array, DocumentMut, Item, Table};

#[derive(Subcommand, Debug)]
pub enum McpCmd {
    /// Add a server: `nexo mcp add <name> <command> [args...]`, or
    /// `nexo mcp add --transport http <name> <url> [--header K:V]` for a remote one.
    Add {
        /// A short name; its tools are called `mcp__<name>__<tool>`.
        name: String,
        /// `stdio` (a command, the default), `http` (streamable HTTP, with the old SSE transport as a
        /// fallback) or `sse` (the old transport only).
        #[arg(long, value_parser = ["stdio", "http", "sse"], default_value = "stdio")]
        transport: String,
        /// `user` (~/.nexo/config.toml, the default) or `project` (.nexo/config.toml of this project).
        #[arg(long, value_parser = ["user", "project"], default_value = "user")]
        scope: String,
        /// A header for a remote server, `Name:value` (the value may use `${ENV_VAR}`). Repeatable.
        #[arg(long = "header", short = 'H', value_name = "K:V")]
        header: Vec<String>,
        /// An environment variable for a local server, `NAME=value`. Repeatable.
        #[arg(long = "env", short = 'e', value_name = "K=V")]
        env: Vec<String>,
        /// The command and its arguments, or the url.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true, value_name = "COMMAND_OR_URL")]
        rest: Vec<String>,
    },
    /// List the servers and whether each one connects, needs a login or fails.
    List,
    /// Remove a server from the configuration (and its stored login).
    Remove {
        name: String,
        /// Only from this scope (default: the project's file if it has it, else the user's).
        #[arg(long, value_parser = ["user", "project"])]
        scope: Option<String>,
    },
    /// Sign in to a remote server that asks for it (OAuth in the browser).
    Login { name: String },
}

pub async fn run(cwd: &Path, config: &Config, what: &McpCmd) -> anyhow::Result<i32> {
    match what {
        McpCmd::Add { name, transport, scope, header, env, rest } => {
            let (rest, header, scope) = if transport == "stdio" {
                (rest.clone(), header.clone(), scope.clone())
            } else {
                take_trailing_flags(rest, header, scope)
            };
            let cfg = build(transport, &rest, &header, env)?;
            let file = scope_file(cwd, &scope);
            add_to_file(&file, name, &cfg)?;
            println!("added {name} to {}", file.display());
            if scope == "project"
                && !nexo_core::config::is_trusted(
                    &nexo_core::config::project_root(&file),
                    &nexo_core::paths::trusted_projects_file(),
                )
            {
                println!("this project is not trusted yet, so the server is ignored until `nexo config trust`");
            }
            if cfg.url.is_some() {
                println!("if it asks for a login, run `nexo mcp login {name}`");
            }
            Ok(0)
        }
        McpCmd::List => list(config).await,
        McpCmd::Remove { name, scope } => remove(cwd, config, name, scope.as_deref()),
        McpCmd::Login { name } => login(config, name).await,
    }
}

/// With a remote server the flags may come after the url (`add --transport http n https://x --header K:V`),
/// where the trailing arguments swallowed them.
fn take_trailing_flags(rest: &[String], header: &[String], scope: &str) -> (Vec<String>, Vec<String>, String) {
    let (mut keep, mut headers, mut scope) = (Vec::new(), header.to_vec(), scope.to_string());
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--header" | "-H" => headers.extend(it.next().cloned()),
            "--scope" => scope = it.next().cloned().unwrap_or(scope),
            s if s.starts_with("--header=") => headers.push(s["--header=".len()..].to_string()),
            s if s.starts_with("--scope=") => scope = s["--scope=".len()..].to_string(),
            _ => keep.push(a.clone()),
        }
    }
    (keep, headers, scope)
}

/// The server a command line describes.
fn build(transport: &str, rest: &[String], headers: &[String], env: &[String]) -> anyhow::Result<McpServerConfig> {
    let mut cfg = McpServerConfig::default();
    if transport == "stdio" {
        let (cmd, args) = rest.split_first().ok_or_else(|| anyhow!("no command"))?;
        cfg.command = cmd.clone();
        cfg.args = args.to_vec();
        for e in env {
            let (k, v) = e.split_once('=').ok_or_else(|| anyhow!("--env wants NAME=value, got {e:?}"))?;
            cfg.env.insert(k.trim().to_string(), v.to_string());
        }
        if !headers.is_empty() {
            bail!("--header is for a remote server (--transport http)");
        }
    } else {
        if rest.len() != 1 {
            bail!("a remote server takes one url, got {} arguments", rest.len());
        }
        let u = url::Url::parse(&rest[0]).map_err(|e| anyhow!("bad url {:?}: {e}", rest[0]))?;
        if !matches!(u.scheme(), "http" | "https") {
            bail!("the url must start with http:// or https://");
        }
        cfg.url = Some(rest[0].clone());
        cfg.transport = Some(transport.to_string());
        for h in headers {
            let (k, v) = h.split_once(':').ok_or_else(|| anyhow!("--header wants Name:value, got {h:?}"))?;
            cfg.headers.insert(k.trim().to_string(), v.trim().to_string());
        }
        if !env.is_empty() {
            bail!("--env is for a local server");
        }
    }
    Ok(cfg)
}

fn scope_file(cwd: &Path, scope: &str) -> PathBuf {
    if scope == "project" {
        nexo_core::paths::project_config_file(cwd).unwrap_or_else(|| cwd.join(".nexo").join("config.toml"))
    } else {
        nexo_core::paths::user_config_file()
    }
}

fn read_doc(file: &Path) -> anyhow::Result<DocumentMut> {
    match std::fs::read_to_string(file) {
        Ok(t) => t.parse::<DocumentMut>().with_context(|| format!("cannot parse {}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", file.display())),
    }
}

fn write_doc(file: &Path, doc: &DocumentMut) -> anyhow::Result<()> {
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(file, doc.to_string()).with_context(|| format!("cannot write {}", file.display()))
}

fn strings(items: &[String]) -> Array {
    items.iter().map(String::as_str).collect()
}

fn string_table(m: &BTreeMap<String, String>) -> Table {
    let mut t = Table::new();
    for (k, v) in m {
        t.insert(k, value(v.as_str()));
    }
    t
}

/// Writes `[mcp.<name>]` into the config file at `file`, keeping the rest of it as it is.
pub fn add_to_file(file: &Path, name: &str, cfg: &McpServerConfig) -> anyhow::Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        bail!("the name {name:?} may only hold letters, digits, - and _");
    }
    let mut doc = read_doc(file)?;
    if doc.get("mcp").and_then(Item::as_table).is_some_and(|t| t.contains_key(name)) {
        bail!("{name} is already in {}; remove it first (`nexo mcp remove {name}`)", file.display());
    }
    let mcp = doc.entry("mcp").or_insert_with(|| {
        let mut t = Table::new();
        t.set_implicit(true);
        Item::Table(t)
    });
    let mcp = mcp.as_table_mut().ok_or_else(|| anyhow!("`mcp` in {} is not a table", file.display()))?;
    let mut t = Table::new();
    if let Some(u) = &cfg.url {
        t.insert("url", value(u.as_str()));
        if let Some(tr) = &cfg.transport {
            t.insert("transport", value(tr.as_str()));
        }
        if !cfg.headers.is_empty() {
            t.insert("headers", Item::Table(string_table(&cfg.headers)));
        }
    } else {
        t.insert("command", value(cfg.command.as_str()));
        if !cfg.args.is_empty() {
            t.insert("args", value(strings(&cfg.args)));
        }
        if !cfg.env.is_empty() {
            t.insert("env", Item::Table(string_table(&cfg.env)));
        }
    }
    mcp.insert(name, Item::Table(t));
    write_doc(file, &doc)
}

/// Takes `[mcp.<name>]` out of the file; true when it was there.
pub fn remove_from_file(file: &Path, name: &str) -> anyhow::Result<bool> {
    if !file.is_file() {
        return Ok(false);
    }
    let mut doc = read_doc(file)?;
    let had = doc.get_mut("mcp").and_then(Item::as_table_mut).is_some_and(|t| t.remove(name).is_some());
    if had {
        write_doc(file, &doc)?;
    }
    Ok(had)
}

fn remove(cwd: &Path, config: &Config, name: &str, scope: Option<&str>) -> anyhow::Result<i32> {
    let scopes: Vec<&str> = match scope {
        Some(s) => vec![s],
        None => vec!["project", "user"],
    };
    for s in scopes {
        let file = scope_file(cwd, s);
        if remove_from_file(&file, name)? {
            let _ = nexo_core::auth::remove(&nexo_tools::mcp_oauth::key(name));
            println!("removed {name} from {}", file.display());
            return Ok(0);
        }
    }
    if config.mcp.contains_key(name) {
        println!("{name} comes from the project's .mcp.json, not from a nexo file: edit that file");
    } else {
        println!("no server named {name}");
    }
    Ok(1)
}

/// The address of a remote server without a query, user info or fragment (they may hold a key).
fn shown_url(u: &str) -> String {
    match url::Url::parse(u) {
        Ok(mut p) => {
            p.set_query(None);
            p.set_fragment(None);
            let _ = p.set_username("");
            let _ = p.set_password(None);
            p.to_string()
        }
        Err(_) => "(bad url)".into(),
    }
}

enum Status {
    Connected(usize),
    NeedsLogin,
    Failed(String),
    Disabled,
}

async fn probe(name: String, cfg: McpServerConfig) -> Status {
    if cfg.enabled == Some(false) {
        return Status::Disabled;
    }
    match tokio::time::timeout(Duration::from_secs(20), nexo_tools::mcp::McpClient::connect(&name, &cfg)).await {
        Ok(Ok(c)) => {
            let n = c.defs().len();
            c.close().await;
            Status::Connected(n)
        }
        Ok(Err(e)) if e.downcast_ref::<nexo_tools::mcp_http::NeedsLogin>().is_some() => Status::NeedsLogin,
        Ok(Err(e)) => {
            let text = format!("{e:#}");
            let text = nexo_core::redact::redact(&text).replace('\n', " ");
            Status::Failed(text.chars().take(100).collect())
        }
        Err(_) => Status::Failed("timed out".into()),
    }
}

async fn list(config: &Config) -> anyhow::Result<i32> {
    if config.mcp.is_empty() {
        println!("no MCP servers; add one with `nexo mcp add <name> <command...>` or `nexo mcp add --transport http <name> <url>`");
        return Ok(0);
    }
    let tasks: Vec<_> =
        config.mcp.iter().map(|(n, c)| (n.clone(), c.clone(), tokio::spawn(probe(n.clone(), c.clone())))).collect();
    for (name, cfg, task) in tasks {
        let status = task.await.unwrap_or(Status::Failed("check crashed".into()));
        let target = match &cfg.url {
            Some(u) => format!("{} ({})", shown_url(u), cfg.transport.as_deref().unwrap_or("http")),
            None => cfg.command.clone(),
        };
        let state = match status {
            Status::Connected(n) => format!("connected, {n} tools"),
            Status::NeedsLogin => format!("needs login (nexo mcp login {name})"),
            Status::Failed(why) => format!("failed: {why}"),
            Status::Disabled => "disabled".into(),
        };
        println!("{name:<16} {state:<34} {target}");
    }
    for n in &config.notices {
        if n.contains(".mcp.json") {
            println!("note: {n}");
        }
    }
    Ok(0)
}

async fn login(config: &Config, name: &str) -> anyhow::Result<i32> {
    let Some(cfg) = config.mcp.get(name) else {
        println!("no server named {name}");
        return Ok(1);
    };
    if cfg.url.is_none() {
        println!("{name} is a local command; only remote servers have a login");
        return Ok(1);
    }
    let open = |url: &str| {
        if nexo_providers::login::open_url(url) {
            println!("Opened your browser to sign in. If nothing opened, go to:\n{url}");
        } else {
            println!("Open this address in a browser to sign in:\n{url}");
        }
    };
    let host = nexo_tools::mcp_oauth::login(name, cfg, &nexo_core::auth::file(), &open).await?;
    println!("signed in to {host}; the login is kept in nexo's credential store");
    match nexo_tools::mcp::McpClient::connect(name, cfg).await {
        Ok(c) => {
            println!("connected, {} tools", c.defs().len());
            c.close().await;
        }
        Err(e) => println!("signed in, but the server did not connect: {e:#}"),
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_stdio_server_is_written_and_read_back() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("config.toml");
        std::fs::write(&f, "# mine\nmodel = \"x\"\n").unwrap();
        let cfg = build("stdio", &s(&["npx", "-y", "srv"]), &[], &s(&["K=v=w"])).unwrap();
        add_to_file(&f, "files", &cfg).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.starts_with("# mine\n"), "{text}");
        let back = Config::parse(&text).unwrap();
        assert_eq!(back.mcp["files"], cfg);
        assert_eq!(back.mcp["files"].env["K"], "v=w");
        assert!(add_to_file(&f, "files", &cfg).unwrap_err().to_string().contains("already"));
        assert!(remove_from_file(&f, "files").unwrap());
        assert!(!remove_from_file(&f, "files").unwrap());
        assert!(Config::parse(&std::fs::read_to_string(&f).unwrap()).unwrap().mcp.is_empty());
    }

    #[test]
    fn a_remote_server_keeps_its_headers_unexpanded() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("nested").join("config.toml");
        let (rest, headers, scope) = take_trailing_flags(
            &s(&["https://x.test/mcp", "--header", "Authorization: Bearer ${TOK}", "--scope", "project"]),
            &[],
            "user",
        );
        assert_eq!((rest.clone(), scope.as_str()), (s(&["https://x.test/mcp"]), "project"));
        let cfg = build("http", &rest, &headers, &[]).unwrap();
        add_to_file(&f, "remote", &cfg).unwrap();
        let back = Config::parse(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(back.mcp["remote"].url.as_deref(), Some("https://x.test/mcp"));
        assert_eq!(back.mcp["remote"].headers["Authorization"], "Bearer ${TOK}");
        assert_eq!(back.mcp["remote"].transport.as_deref(), Some("http"));
    }

    #[test]
    fn bad_input_is_refused() {
        assert!(build("http", &s(&["ftp://x"]), &[], &[]).is_err());
        assert!(build("http", &s(&["https://x", "extra"]), &[], &[]).is_err());
        assert!(build("http", &s(&["https://x"]), &s(&["nocolon"]), &[]).is_err());
        assert!(build("stdio", &s(&["cmd"]), &[], &s(&["noequals"])).is_err());
        let d = tempfile::tempdir().unwrap();
        assert!(add_to_file(&d.path().join("c.toml"), "bad name", &McpServerConfig::default()).is_err());
    }

    #[test]
    fn a_shown_url_loses_its_secrets() {
        assert_eq!(shown_url("https://u:p@x.test/mcp?key=abc#f"), "https://x.test/mcp");
    }
}
