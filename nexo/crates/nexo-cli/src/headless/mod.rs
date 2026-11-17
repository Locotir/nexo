//! `nexo -p`: one request, or a stream of them, without the interface. The contract for scripts
//! and CI (output formats, input stream, limits, schema, exit codes) is in docs/HEADLESS.md.

use std::collections::VecDeque;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::{Args, ValueEnum};
use nexo_core::agent::{channels, AgentCommand, AgentEvent, Level, TurnEnd};
use nexo_core::config::OnLimit;
use serde_json::{json, Value};

use crate::setup::{self, Picked};

mod input;
mod output;
mod reduce;

pub use input::*;
pub use output::*;
use reduce::*;

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
    /// With --print: text | json | stream-json (see https://github.com/Locotir/nexo/blob/main/nexo/docs/HEADLESS.md).
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
    /// Attach an image to the first request; repeat for more (`nexo -i shot.png "why is this red?"`).
    #[arg(short = 'i', long, value_name = "FILE")]
    pub image: Vec<PathBuf>,
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

/// What the run sends in the person's place when the answer did not fit `--json-schema`. It says who writes it:
/// taken as the person's words, a model apologises or explains before the value, and the value is all that counts.
fn schema_retry(errors: &str) -> String {
    format!(
        "Automatic check by nexo, not a message from the person: your answer did not validate against the schema: \
         {errors}. Reply again with only the corrected JSON value."
    )
}

/// Tools the agent registers after the setup's tool set is built, so `--allowed-tools` and `--disallowed-tools`
/// can name them although the set they are matched against does not hold them yet.
const LATE_TOOLS: &[&str] =
    &["task", "workflow", "shared_memory_read", "shared_memory_write", "exit_plan", "goal_brief", "ultracode_execute"];

/// The warning for entries of a tool flag that name no tool, so a typo or another program's tool name does not
/// silently empty the tool set (an allow list) or change nothing (a deny list).
fn unmatched_tools(flag: &str, entries: &[String], names: &[String]) -> Option<String> {
    let known = |e: &str| names.iter().any(|n| n.to_lowercase() == e) || LATE_TOOLS.contains(&e);
    let missing: Vec<String> = entries.iter().filter(|e| !known(e.as_str())).map(|e| format!("'{e}'")).collect();
    if missing.is_empty() {
        return None;
    }
    let mut all: Vec<&str> = names.iter().map(String::as_str).chain(LATE_TOOLS.iter().copied()).collect();
    all.sort_unstable();
    all.dedup();
    Some(format!("nexo: {flag}: no tool matches {} (tools: {})", missing.join(", "), all.join(", ")))
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
    let fmt = format_asked(&flags, legacy_json);
    let stream_in = flags.input_format == Some(InputFormat::StreamJson);
    let schema = match flags.json_schema.as_deref().map(crate::schema::load).transpose() {
        Ok(s) => s,
        Err(e) => return usage(&e),
    };
    if stream_in && schema.is_some() {
        return usage("--json-schema cannot be used with --input-format stream-json");
    }
    let mut first_images: Vec<nexo_core::agent::Attachment> = match images(&cwd, &flags.image) {
        Ok(v) => v.into_iter().map(|path| nexo_core::agent::Attachment { path }).collect(),
        Err(e) => return usage(&e),
    };
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
    // a warning, not an error: a script that already passes a name this build lacks keeps running
    let allowed = keep.clone().unwrap_or_default();
    for w in [
        unmatched_tools("--allowed-tools", &allowed, &names),
        unmatched_tools("--disallowed-tools", &drop_tools, &names),
    ]
    .into_iter()
    .flatten()
    {
        eprintln!("{w}");
    }
    let tools: Vec<String> = agent_setup.tools.names().map(String::from).collect();
    let agent = tokio::spawn(nexo_agent::run(ends, agent_setup));

    let reach = config.providers.get(&picked.provider).map(|p| unreachable_note(&picked.provider, &p.base_url));
    let mut run = Run::new(fmt, tools, nexo_agent::context::from_config(&config.context).recall(), reach);
    run.quiet_text = schema.is_some();
    let mut stdin_rx = stream_in.then(stdin_lines);
    let mut busy = false;
    let mut sent_any = true;
    let mut attempt = 0u8;
    let mut exit = 0;
    let mut last: Option<Value> = None;
    let mut interrupts = 0;
    let mut agent_gone = false;
    // One wait for the whole run: a request that lands while the loop handles an event is kept for the next poll, and the
    // teardown below listens on the same one. A fresh wait each time would hear only what comes after it exists, and
    // the handlers outlive it, so a `kill` in between would do nothing at all.
    let mut stop = Box::pin(nexo_core::signals::stop());
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
                sent_any = handle
                    .commands
                    .send(AgentCommand::Submit { text: msg, attachments: std::mem::take(&mut first_images) })
                    .is_ok();
                busy = true;
            } else if stdin_rx.is_none() {
                break;
            }
        }
        tokio::select! {
            biased;
            _ = &mut stop => {
                stop = Box::pin(nexo_core::signals::stop());
                interrupts += 1;
                if interrupts > 1 {
                    nexo_agent::audit::close_all();
                    std::process::exit(EXIT_INTERRUPTED);
                }
                if busy {
                    let _ = handle.commands.send(AgentCommand::StopBy(nexo_core::agent::Stopper::CtrlC));
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
                if code == EXIT_DENIED {
                    // the refusal names the tool and what would allow it; a fixed text would hide both
                    problem = run.r.denied.clone().filter(|d| !d.is_empty());
                }
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
                            inbox.push_back(schema_retry(&e));
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
                                _ => problem.clone().unwrap_or_else(|| {
                                    "a tool call was refused: nobody can be asked in a non-interactive run".to_string()
                                }),
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
    let stopped = tokio::select! {
        r = tokio::time::timeout(Duration::from_secs(10), agent) => Some(r),
        _ = &mut stop => None,
    };
    let Some(stopped) = stopped else {
        // another request to stop while the agent winds down: the same as the second Ctrl+C, no more waiting
        nexo_agent::audit::close_all();
        std::process::exit(EXIT_INTERRUPTED);
    };
    let failed = match stopped {
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
    #[test]
    fn the_schema_retry_says_it_is_not_the_person_and_asks_for_the_value_alone() {
        let t = super::schema_retry("/name: missing");
        assert!(t.starts_with("Automatic check by nexo, not a message from the person:"), "{t}");
        assert!(t.contains("/name: missing") && t.ends_with("Reply again with only the corrected JSON value."), "{t}");
        assert!(!t.contains('\n'), "{t}");
    }

    #[test]
    fn a_tool_flag_entry_is_unmatched_unless_it_names_a_tool_of_the_set_or_a_late_one() {
        let names = vec!["bash".to_string(), "read".to_string()];
        let w = super::unmatched_tools("--allowed-tools", &["shell".to_string()], &names).unwrap();
        assert!(w.contains("no tool matches 'shell'") && w.contains("bash, exit_plan, goal_brief, read"), "{w}");
        assert!(super::unmatched_tools("--allowed-tools", &["bash".into(), "task".into()], &names).is_none());
    }
}
