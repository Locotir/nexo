//! `nexo -p`: one request, or a stream of them, without the interface. The contract for scripts
//! and CI (output formats, input stream, limits, schema, exit codes) is in docs/HEADLESS.md.

use std::collections::VecDeque;
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::{Args, ValueEnum};
use nexo_core::agent::{channels, AgentCommand, AgentEvent, Level, TurnEnd};
use nexo_core::config::OnLimit;
use serde_json::{json, Value};

use crate::setup::{self, Picked};

/// The run failed or the model hit an error.
pub const EXIT_FAILED: i32 = 1;
/// The command line is wrong (clap uses the same code).
pub const EXIT_USAGE: i32 = 2;
/// The final answer did not validate against `--json-schema`, even after one retry.
pub const EXIT_SCHEMA: i32 = 3;
/// `--max-turns` (or `agent.max_turns`) was reached.
pub const EXIT_MAX_TURNS: i32 = 4;
/// A tool call was refused by the permission policy and nobody could be asked.
pub const EXIT_DENIED: i32 = 5;
/// Interrupted (Ctrl-C).
pub const EXIT_INTERRUPTED: i32 = 130;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// The reply as plain text (default).
    Text,
    /// One JSON object at the end.
    Json,
    /// One JSON event per line as it happens, ending with the result object.
    StreamJson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InputFormat {
    /// The prompt (and plain stdin, added to it).
    Text,
    /// User messages as JSON lines on stdin.
    StreamJson,
}

/// The flags that only mean something with `--print`.
#[derive(Args, Debug, Default)]
pub struct Flags {
    /// With --print: text | json | stream-json (see docs/HEADLESS.md).
    #[arg(long, value_enum, value_name = "FORMAT", conflicts_with = "json")]
    pub output_format: Option<OutputFormat>,
    /// With --print: text | stream-json (user messages as JSON lines on stdin).
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub input_format: Option<InputFormat>,
    /// With --print: stop after N tool rounds and exit 4.
    #[arg(long, value_name = "N")]
    pub max_turns: Option<u32>,
    /// With --print: only these tools, comma separated (`read,grep,bash(git *)`; a rule with a pattern also allows it).
    #[arg(long, alias = "allowedTools", value_delimiter = ',', value_name = "TOOLS")]
    pub allowed_tools: Vec<String>,
    /// With --print: never these tools, comma separated (`bash`, or a deny rule such as `bash(rm *)`).
    #[arg(long, alias = "disallowedTools", value_delimiter = ',', value_name = "TOOLS")]
    pub disallowed_tools: Vec<String>,
    /// With --print: text added to the end of the system prompt.
    #[arg(long, value_name = "TEXT")]
    pub append_system_prompt: Option<String>,
    /// One more folder the tools may read and write (repeatable).
    #[arg(long, value_name = "PATH")]
    pub add_dir: Vec<PathBuf>,
    /// With --print: the final answer must validate against this JSON Schema (a file, or inline JSON); exit 3 if it does not.
    #[arg(long, value_name = "FILE|JSON")]
    pub json_schema: Option<String>,
}

impl Flags {
    /// Whether any flag was given (they are refused without --print).
    pub fn any(&self) -> bool {
        self.output_format.is_some()
            || self.input_format.is_some()
            || self.max_turns.is_some()
            || !self.allowed_tools.is_empty()
            || !self.disallowed_tools.is_empty()
            || self.append_system_prompt.is_some()
            || self.json_schema.is_some()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fmt {
    Text,
    Json,
    StreamJson,
    /// `--json`: every internal agent event as it is, kept for the scripts that read it.
    Legacy,
}

/// The stdin of a run: what was piped in.
fn read_stdin() -> String {
    let mut s = String::new();
    if !std::io::stdin().is_terminal() {
        let _ = std::io::stdin().read_to_string(&mut s);
    }
    s
}

/// The text of one user message of the input stream. `Ok(None)`: a line that is not a user message.
fn user_text(line: &str) -> Result<Option<String>, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("not JSON: {e}"))?;
    if v.get("type").and_then(Value::as_str).is_some_and(|t| t != "user") {
        return Ok(None);
    }
    let content = v
        .pointer("/message/content")
        .or_else(|| v.get("content"))
        .or_else(|| v.get("text"))
        .or_else(|| v.get("prompt"))
        .ok_or("a user message needs message.content (or content, text)")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.as_str().or_else(|| p.get("text").and_then(Value::as_str)))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return Err("the message content must be a string or a list of text blocks".into()),
    };
    Ok(Some(text))
}

/// Where the rules of a folder start: the path with forward slashes, which the rule globs read on every system.
/// Lets the tools read and write in each of `dirs` (relative to `cwd`) besides the project, and
/// lets a sandboxed command write there; the folders as resolved, or why one is not a folder.
pub fn add_dirs(
    config: &mut nexo_core::Config,
    cwd: &std::path::Path,
    dirs: &[PathBuf],
) -> Result<Vec<String>, String> {
    let mut roots = Vec::new();
    for d in dirs {
        let abs = if d.is_absolute() { d.clone() } else { cwd.join(d) };
        let Ok(real) = abs.canonicalize() else { return Err(format!("--add-dir {}: not a folder", d.display())) };
        if !real.is_dir() {
            return Err(format!("--add-dir {}: not a folder", d.display()));
        }
        // the folder as given and as resolved: on Windows the two can differ (8.3 short names, `\\?\`)
        let mut spellings = vec![rule_path(&abs), rule_path(&real)];
        spellings.dedup();
        for p in &spellings {
            for tool in ["read", "grep", "glob", "ls", "write", "edit"] {
                config.permissions.allow.push(format!("{tool}({p}/**)"));
            }
        }
        // a bash command in the sandbox may write there too (a managed policy can still shut that door)
        if !config.permissions.sandbox_write_dirs.contains(&real) {
            config.permissions.sandbox_write_dirs.push(real.clone());
        }
        roots.push(real.display().to_string());
    }
    Ok(roots)
}

fn rule_path(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    s.strip_prefix(r"\\?\").unwrap_or(&s).replace('\\', "/").trim_end_matches('/').to_string()
}

#[derive(Default)]
struct Totals {
    session_id: String,
    model: String,
    turns: u32,
    usage: nexo_core::Usage,
    cost: f64,
}

/// What one request has done so far.
#[derive(Default)]
struct Req {
    /// The last assistant message.
    text: String,
    /// The first turn end that was not a normal one.
    end: Option<TurnEnd>,
    denied: bool,
    error: Option<String>,
}

impl Req {
    fn code(&self) -> i32 {
        match self.end {
            Some(TurnEnd::Error) => EXIT_FAILED,
            Some(TurnEnd::MaxTurns) => EXIT_MAX_TURNS,
            Some(TurnEnd::Interrupted) => EXIT_INTERRUPTED,
            _ if self.denied => EXIT_DENIED,
            _ => 0,
        }
    }
}

struct Run {
    fmt: Fmt,
    tools: Vec<String>,
    started: Instant,
    t: Totals,
    r: Req,
    /// Text printed so far ends in the middle of a line.
    printed: bool,
    /// Text mode with a schema prints the validated answer at the end, not the stream.
    quiet_text: bool,
}

fn line(v: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

impl Run {
    /// Handles one agent event; true when the request is over.
    fn on_event(&mut self, ev: &AgentEvent) -> bool {
        if self.fmt == Fmt::Legacy {
            if let Ok(s) = serde_json::to_string(ev) {
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{s}");
            }
        }
        let stream = self.fmt == Fmt::StreamJson;
        match ev {
            AgentEvent::Ready { session_id, provider, model, cwd, permission_mode } => {
                self.t.session_id = session_id.clone();
                self.t.model = model.clone();
                if stream {
                    line(&json!({
                        "type": "init", "session_id": session_id, "provider": provider, "model": model,
                        "tools": self.tools, "cwd": cwd, "permission_mode": permission_mode,
                    }));
                }
            }
            AgentEvent::SessionChanged { session_id, .. } => self.t.session_id = session_id.clone(),
            AgentEvent::TurnStart { .. } => self.t.turns += 1,
            AgentEvent::AssistantStart => {
                self.r.text.clear();
                if self.fmt == Fmt::Text && self.printed {
                    println!();
                    self.printed = false;
                }
            }
            AgentEvent::TextDelta { text } => {
                self.r.text.push_str(text);
                if stream {
                    line(&json!({"type": "assistant_delta", "text": text}));
                } else if self.fmt == Fmt::Text && !self.quiet_text {
                    let mut out = std::io::stdout().lock();
                    let _ = out.write_all(text.as_bytes());
                    let _ = out.flush();
                    self.printed = !text.ends_with('\n');
                }
            }
            AgentEvent::AssistantDone { message } if stream => {
                line(&json!({"type": "assistant", "text": message.text()}));
            }
            AgentEvent::ToolCall { id, name, args, summary } => {
                if stream {
                    line(&json!({"type": "tool_use", "id": id, "name": name, "input": args, "summary": summary}));
                } else if self.fmt != Fmt::Legacy {
                    eprintln!("· {summary}");
                }
            }
            AgentEvent::ToolDone { id, output, elapsed_ms } => {
                if output.is_error && output.content.starts_with("Permission denied") {
                    self.r.denied = true;
                }
                if stream {
                    line(&json!({
                        "type": "tool_result", "id": id, "is_error": output.is_error,
                        "content": output.content, "elapsed_ms": elapsed_ms,
                    }));
                }
            }
            AgentEvent::Usage { session, cost_usd, .. } => {
                self.t.usage = *session;
                self.t.cost = *cost_usd;
            }
            AgentEvent::Notice { level, text } => {
                if *level == Level::Error {
                    self.r.error = Some(text.clone());
                }
                if stream {
                    line(&json!({"type": "notice", "level": format!("{level:?}").to_lowercase(), "text": text}));
                } else if self.fmt != Fmt::Legacy {
                    eprintln!("[{level:?}] {text}");
                }
            }
            AgentEvent::TurnEnd { reason } => {
                if self.fmt == Fmt::Text && self.printed {
                    println!();
                    self.printed = false;
                }
                if !matches!(reason, TurnEnd::Done) && self.r.end.is_none() {
                    self.r.end = Some(*reason);
                }
            }
            AgentEvent::Idle { .. } | AgentEvent::Shutdown => return true,
            _ => {}
        }
        false
    }

    /// The result object, for a request that ended with `code`.
    fn result(&self, code: i32, text: &str, structured: Option<Value>, error: Option<String>) -> Value {
        let (subtype, stop) = match code {
            0 => ("success", "end_turn"),
            EXIT_SCHEMA => ("error_schema", "end_turn"),
            EXIT_MAX_TURNS => ("error_max_turns", "max_turns"),
            EXIT_DENIED => ("error_permission_denied", "end_turn"),
            EXIT_INTERRUPTED => ("interrupted", "interrupted"),
            _ => ("error", "error"),
        };
        let mut v = json!({
            "type": "result", "subtype": subtype, "is_error": code != 0, "exit_code": code,
            "result": text, "session_id": self.t.session_id, "model": self.t.model,
            "num_turns": self.t.turns,
            "usage": {
                "input_tokens": self.t.usage.input, "output_tokens": self.t.usage.output,
                "cache_read_tokens": self.t.usage.cache_read, "cache_write_tokens": self.t.usage.cache_write,
            },
            "cost_usd": self.t.cost, "duration_ms": self.started.elapsed().as_millis() as u64,
            "stop_reason": stop,
        });
        // the subscription plan the request ran on, when its backend said how much of it is left
        let now = nexo_core::auth::now() as i64;
        let plans: Vec<Value> = [nexo_providers::usage::CODEX, nexo_providers::usage::COPILOT]
            .into_iter()
            .filter_map(nexo_providers::usage::get)
            .map(|u| u.to_json(now))
            .collect();
        if !plans.is_empty() {
            v["plan_usage"] = Value::Array(plans);
        }
        if let Some(s) = structured {
            v["structured_output"] = s;
        }
        if let Some(e) = error {
            v["error"] = json!(e);
        }
        v
    }
}

/// Reads stdin lines on a thread, so the run can wait on them and on the agent at once.
fn stdin_lines() -> tokio::sync::mpsc::UnboundedReceiver<String> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for l in std::io::stdin().lock().lines() {
            match l {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

fn usage(msg: &str) -> anyhow::Result<i32> {
    eprintln!("nexo: {msg}");
    Ok(EXIT_USAGE)
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    mut config: nexo_core::Config,
    cwd: PathBuf,
    picked: Picked,
    resume: Option<String>,
    prompt: String,
    legacy_json: bool,
    flags: Flags,
) -> anyhow::Result<i32> {
    use std::io::Write as _;
    let fmt = match (flags.output_format, legacy_json) {
        (Some(OutputFormat::Json), _) => Fmt::Json,
        (Some(OutputFormat::StreamJson), _) => Fmt::StreamJson,
        (Some(OutputFormat::Text), _) => Fmt::Text,
        (None, true) => Fmt::Legacy,
        (None, false) => Fmt::Text,
    };
    let stream_in = flags.input_format == Some(InputFormat::StreamJson);
    let schema = match flags.json_schema.as_deref().map(crate::schema::load).transpose() {
        Ok(s) => s,
        Err(e) => return usage(&e),
    };
    if stream_in && schema.is_some() {
        return usage("--json-schema cannot be used with --input-format stream-json");
    }
    // plain stdin is part of the request
    let mut inbox: VecDeque<String> = VecDeque::new();
    if !stream_in {
        let piped = read_stdin();
        let text = match (prompt.trim().is_empty(), piped.trim().is_empty()) {
            (_, true) => prompt,
            (true, false) => piped,
            (false, false) => format!("{}\n\n{}", prompt.trim_end(), piped.trim_end()),
        };
        if text.trim().is_empty() {
            return usage("nothing to do: give a prompt with -p or pipe one in");
        }
        inbox.push_back(text);
    } else if !prompt.trim().is_empty() {
        inbox.push_back(prompt);
    }

    // the limits and rules of this run, over the configuration
    if let Some(n) = flags.max_turns {
        if n == 0 {
            return usage("--max-turns must be at least 1");
        }
        config.agent.max_turns = n;
        // at the limit the run stops (exit 4); it does not ask the model to go on
        config.agent.on_limit = OnLimit::Ask;
    }
    let mut extra = config.agent.system_prompt_extra.take().unwrap_or_default();
    let mut add = |text: &str| {
        if !extra.trim().is_empty() {
            extra.push_str("\n\n");
        }
        extra.push_str(text.trim());
    };
    if let Some(t) = &flags.append_system_prompt {
        add(t);
    }
    let roots = match add_dirs(&mut config, &cwd, &flags.add_dir) {
        Ok(r) => r,
        Err(m) => return usage(&m),
    };
    config.enforce_managed();
    if !roots.is_empty() {
        add(&format!("You may also read and write in these folders: {}.", roots.join(", ")));
    }
    if !extra.trim().is_empty() {
        config.agent.system_prompt_extra = Some(extra);
    }
    let mut keep: Option<Vec<String>> = None;
    for a in &flags.allowed_tools {
        let a = a.trim();
        if a.is_empty() {
            continue;
        }
        if a.contains('(') {
            config.permissions.allow.push(a.to_string());
        }
        let name = a.split('(').next().unwrap_or(a).trim().to_lowercase();
        keep.get_or_insert_with(Vec::new).push(name);
    }
    let mut drop_tools = Vec::new();
    for d in &flags.disallowed_tools {
        let d = d.trim();
        if d.contains('(') {
            config.permissions.deny.push(d.to_string());
        } else if !d.is_empty() {
            drop_tools.push(d.to_lowercase());
        }
    }

    // what a script or another program runs is not one of the person's conversations
    nexo_agent::session::mark_headless();
    let (mut handle, ends) = channels();
    let gate: std::sync::Arc<dyn nexo_core::PermissionGate> = std::sync::Arc::new(setup::HeadlessGate);
    let mut agent_setup = setup::agent_setup(&config, &cwd, &picked, resume, Some(gate), None).await?;
    let names: Vec<String> = agent_setup.tools.names().map(String::from).collect();
    for n in &names {
        let l = n.to_lowercase();
        if keep.as_ref().is_some_and(|k| !k.contains(&l)) || drop_tools.contains(&l) {
            agent_setup.tools.remove(n);
        }
    }
    let tools: Vec<String> = agent_setup.tools.names().map(String::from).collect();
    let agent = tokio::spawn(nexo_agent::run(ends, agent_setup));

    let mut run = Run {
        fmt,
        tools,
        started: Instant::now(),
        t: Totals::default(),
        r: Req::default(),
        printed: false,
        quiet_text: schema.is_some(),
    };
    let mut stdin_rx = stream_in.then(stdin_lines);
    let mut busy = false;
    let mut sent_any = true;
    let mut attempt = 0u8;
    let mut exit = 0;
    let mut last: Option<Value> = None;
    let mut interrupts = 0;
    let mut agent_gone = false;
    'main: loop {
        if !busy {
            if let Some(mut msg) = inbox.pop_front() {
                if attempt == 0 {
                    if let Some(s) = &schema {
                        msg = format!(
                            "{msg}\n\nReply with only a JSON value that validates against this JSON Schema, with no other text and no code fence:\n{s}"
                        );
                    }
                }
                run.r = Req::default();
                sent_any = handle.commands.send(AgentCommand::Submit { text: msg, attachments: vec![] }).is_ok();
                busy = true;
            } else if stdin_rx.is_none() {
                break;
            }
        }
        tokio::select! {
            biased;
            _ = tokio::signal::ctrl_c() => {
                interrupts += 1;
                if interrupts > 1 {
                    nexo_agent::audit::close_all();
                    std::process::exit(EXIT_INTERRUPTED);
                }
                if busy {
                    let _ = handle.commands.send(AgentCommand::Interrupt);
                } else {
                    exit = EXIT_INTERRUPTED;
                    break 'main;
                }
            }
            ev = handle.events.recv() => {
                let Some(ev) = ev else { agent_gone = true; break 'main };
                if !run.on_event(&ev) {
                    continue;
                }
                if matches!(ev, AgentEvent::Shutdown) {
                    agent_gone = true;
                    break 'main;
                }
                if !busy {
                    continue;
                }
                busy = false;
                let mut code = run.r.code();
                let text = run.r.text.clone();
                let mut structured = None;
                let mut problem = run.r.error.clone();
                if let (0, Some(s)) = (code, &schema) {
                    let checked = crate::schema::extract(&text).and_then(|v| {
                        let errs = crate::schema::validate(s, &v);
                        if errs.is_empty() { Ok(v) } else { Err(errs.join("; ")) }
                    });
                    match checked {
                        Ok(v) => structured = Some(v),
                        Err(e) if attempt == 0 => {
                            // one more try, in the same session, told what was wrong
                            attempt = 1;
                            inbox.push_back(format!(
                                "Your answer did not validate against the schema: {e}. Reply again with only the corrected JSON value."
                            ));
                            continue;
                        }
                        Err(e) => {
                            code = EXIT_SCHEMA;
                            problem = Some(format!("the answer does not match the schema: {e}"));
                        }
                    }
                }
                if exit == 0 {
                    exit = code;
                }
                if fmt == Fmt::Text {
                    match (&structured, code) {
                        (Some(v), _) => println!("{v}"),
                        (None, EXIT_SCHEMA | EXIT_MAX_TURNS | EXIT_DENIED) => {
                            let why = match code {
                                EXIT_SCHEMA => problem.clone().unwrap_or_default(),
                                EXIT_MAX_TURNS => "stopped at the turn limit".to_string(),
                                _ => "a tool call was refused: nobody can be asked in a non-interactive run".to_string(),
                            };
                            eprintln!("nexo: {why}");
                        }
                        _ => {}
                    }
                }
                let result = run.result(code, &text, structured, problem);
                if fmt == Fmt::StreamJson {
                    line(&result);
                }
                last = Some(result);
                attempt = 0;
                if code == EXIT_INTERRUPTED {
                    break 'main;
                }
            }
            msg = async { stdin_rx.as_mut().expect("guarded").recv().await }, if !busy && stdin_rx.is_some() => {
                match msg {
                    None => stdin_rx = None,
                    Some(l) if l.trim().is_empty() => {}
                    Some(l) => match user_text(&l) {
                        Ok(Some(t)) => inbox.push_back(t),
                        Ok(None) => {}
                        Err(e) => {
                            eprintln!("nexo: stdin: {e}: {l}");
                            exit = EXIT_USAGE;
                            break 'main;
                        }
                    },
                }
            }
        }
    }
    let _ = handle.commands.send(AgentCommand::Shutdown);
    // the agent can stop before any turn (a session that cannot be resumed): that is a failure
    // the wait ends as soon as the agent stops; a slow machine still gets to let its session lock go
    let failed = match tokio::time::timeout(Duration::from_secs(10), agent).await {
        Ok(Ok(Err(e))) => Some(format!("{e:#}")),
        Ok(Err(e)) => Some(format!("the agent stopped: {e}")),
        _ if !sent_any => Some("the agent stopped before the request".to_string()),
        _ if agent_gone && last.is_none() => Some("the agent stopped before it answered".to_string()),
        _ => None,
    };
    let mut out = std::io::stdout().lock();
    if let Some(e) = failed {
        match fmt {
            Fmt::Legacy => {
                let _ = writeln!(out, "{}", json!({"event": "error", "message": e}));
            }
            Fmt::Json | Fmt::StreamJson => {
                let _ = writeln!(out, "{}", run.result(EXIT_FAILED, "", None, Some(e.clone())));
            }
            Fmt::Text => {}
        }
        let _ = out.flush();
        eprintln!("nexo: {e}");
        return Ok(EXIT_FAILED);
    }
    if fmt == Fmt::Json {
        let v = last.unwrap_or_else(|| run.result(if exit == 0 { EXIT_INTERRUPTED } else { exit }, "", None, None));
        let _ = writeln!(out, "{v}");
    } else if fmt == Fmt::StreamJson && last.is_none() {
        let _ = writeln!(out, "{}", run.result(exit, "", None, None));
    }
    let _ = out.flush();
    Ok(exit)
}

#[cfg(test)]
mod tests {
    use super::user_text;

    #[test]
    fn reads_the_shapes_of_a_user_message() {
        let t = |s| user_text(s).unwrap();
        assert_eq!(t(r#"{"type":"user","message":{"role":"user","content":"hi"}}"#), Some("hi".into()));
        assert_eq!(
            t(r#"{"message":{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}}"#),
            Some("a\nb".into())
        );
        assert_eq!(t(r#"{"content":"x"}"#), Some("x".into()));
        assert_eq!(t(r#"{"type":"control","x":1}"#), None);
        assert!(user_text("nope").is_err());
        assert!(user_text(r#"{"type":"user"}"#).is_err());
    }
}

#[cfg(test)]
mod add_dir_tests {
    use super::*;

    #[test]
    fn a_folder_added_gets_the_file_tools_and_the_sandbox() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("other")).unwrap();
        let mut config = nexo_core::Config::default();
        let roots = add_dirs(&mut config, d.path(), &[PathBuf::from("other")]).unwrap();
        let real = d.path().join("other").canonicalize().unwrap();
        assert_eq!(roots, vec![real.display().to_string()]);
        let p = rule_path(&real);
        for tool in ["read", "write", "edit"] {
            assert!(
                config.permissions.allow.contains(&format!("{tool}({p}/**)")),
                "{tool}: {:?}",
                config.permissions.allow
            );
        }
        assert_eq!(config.permissions.sandbox_write_dirs, vec![real]);
        let e = add_dirs(&mut config, d.path(), &[PathBuf::from("missing")]).unwrap_err();
        assert_eq!(e, "--add-dir missing: not a folder");
    }
}
