//! `bash`: a shell command in the working directory.

use std::collections::VecDeque;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use nexo_core::{Compact, Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use nexo_core::jobs::JobKind;
use tokio_util::sync::CancellationToken;

use crate::util::{bool_arg, cap_text, int_arg, one_line, str_arg};

pub const DEFAULT_TIMEOUT_S: u64 = 120;
pub const MAX_TIMEOUT_S: u64 = 600;
/// Characters of output kept (head and tail).
pub const MAX_OUTPUT_CHARS: usize = 30_000;

pub struct Bash;

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &str {
        "bash"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".into(),
            description: format!(
                "Run a shell command ({}) in the working directory; returns its output \
                 (stdout and stderr interleaved) and exit code. Killed after `timeout` \
                 seconds (default 120, max 600); long output keeps its head \
                 and tail. `background: true` \
                 runs it as a job while you work: the call returns its job id, you are told \
                 when it ends, and `jobs` reads its output or stops it. Background jobs end with the session; \
                 `detach: true` keeps a service running after it. \
                 Session scratch: $NEXO_TMP.",
                match nexo_core::shell::shell().kind {
                    "powershell" => "Windows PowerShell: write PowerShell, not sh",
                    "bash" => "Git Bash on Windows: POSIX shell, C:/... paths work",
                    _ => "`sh -c`",
                }
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "timeout": {"type": "integer", "minimum": 1, "maximum": 600},
                    "description": {"type": "string", "description": "A few words on what it does, for the person."},
                    "wait_until": {"type": "string", "description": "Shell condition polled up to `timeout`; then `command` runs."},
                    "background": {"type": "boolean"},
                    "detach": {"type": "boolean", "description": "With `background`: outlives nexo, logs to a file; stop it by pid."},
                    "unsandboxed": {"type": "boolean", "description": "Only after the OS sandbox blocked a command: run outside it; the person is asked."}
                },
                "required": ["command"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: "Run a shell command in the working directory; returns output and exit code. `background: true` makes it a job (see `jobs`).",
            params: &[
                ("timeout", "Seconds, default 120, max 600"),
                                ("wait_until", "Shell condition polled before running"),
                ("detach", "With `background`: outlives nexo"),
                ("unsandboxed", "Rerun outside the sandbox"),
            ],
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Exec
    }

    fn call_kind(&self, args: &Value) -> ToolKind {
        let plain = !bool_arg(args, "background") && !bool_arg(args, "detach") && !bool_arg(args, "unsandboxed");
        match str_arg(args, "command") {
            Some(c) if plain && only_looks(c) => ToolKind::Read,
            _ => ToolKind::Exec,
        }
    }

    fn sandbox_holds(&self, args: &Value, sandbox: &nexo_core::sandbox::SandboxSettings) -> bool {
        // a detached command outlives nexo even in the sandbox: it is asked about
        !bool_arg(args, "unsandboxed") && !bool_arg(args, "detach") && crate::sandbox::auto_allows(sandbox)
    }

    fn sandbox_status(&self, sandbox: &nexo_core::sandbox::SandboxSettings) -> Option<(String, bool)> {
        let works = !sandbox.mode.is_on() || crate::sandbox::auto_allows(sandbox);
        Some((crate::sandbox::describe(sandbox), works))
    }

    fn summary(&self, args: &Value) -> String {
        match str_arg(args, "command") {
            Some(c) => format!("bash: {}", one_line(c, 2000)),
            None => "bash (no command)".into(),
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(command) = str_arg(&args, "command") else {
            return ToolOutput::error("bash: `command` is required");
        };
        let sandbox = if bool_arg(&args, "unsandboxed") { Default::default() } else { ctx.sandbox.clone() };
        if bool_arg(&args, "detach") {
            if !bool_arg(&args, "background") {
                return ToolOutput::error("bash: `detach` needs `background: true`");
            }
            return detached(command, ctx, &sandbox);
        }
        if bool_arg(&args, "background") {
            return background(command, ctx, sandbox);
        }
        if let Some(refusal) = blind_sleep(command) {
            return ToolOutput::error(refusal);
        }
        let timeout =
            Duration::from_secs(int_arg(&args, "timeout").unwrap_or(DEFAULT_TIMEOUT_S).clamp(1, MAX_TIMEOUT_S));
        if let Some(cond) = str_arg(&args, "wait_until") {
            if let Some(stop) = wait_until(cond, timeout, ctx, &sandbox).await {
                return stop;
            }
        }
        let mut output = Capture::new();
        let tmp = tmp_dir(&ctx.session_id);
        let guard = crate::guarded::before(&sandbox, &ctx.cwd);
        let end = run_command(command, &ctx.cwd, &sandbox, Some(timeout), &ctx.cancel, Some(&tmp), |l| {
            ctx.report(l);
            output.push(l);
        })
        .await;
        let missing = if matches!(end, End::Exited(0)) { output.not_found() } else { Vec::new() };
        let outcome = Outcome { output: output.text(), end };
        let mut content = if outcome.output.trim().is_empty() {
            "(no output)".to_string()
        } else {
            cap_text(&outcome.output, MAX_OUTPUT_CHARS)
        };
        let is_error = match outcome.end {
            End::Exited(0) => false,
            // a command that ran to the end and answered with a code (`cmp`, `grep`, `diff`, `test`, a test
            // runner) did not fail: the code is shown, and the model judges by it and the output. Not found
            // (127), not runnable (126) and a signal's code are failures.
            End::Exited(code) => {
                content.push_str(&format!("\n\n[exit code {code}]"));
                nexo_core::shell::exit_is_failure(code)
            }
            End::Signalled => {
                content.push_str("\n\n[killed by a signal]");
                true
            }
            End::TimedOut => {
                content.push_str(&format!("\n\n[killed: timed out after {} s]", timeout.as_secs()));
                true
            }
            End::Cancelled => {
                content.push_str("\n\n[cancelled]");
                true
            }
            End::Failed(ref e) => {
                content = format!("{}: cannot run the command: {e}", nexo_core::shell::shell().label());
                true
            }
        };
        if !missing.is_empty() {
            // the status is the last command's: say that an earlier one was not found
            content.push_str(&format!("\n[note: a command in this script was not found: {}]", missing.join(", ")));
        }
        if matches!(outcome.end, End::Exited(c) if c != 0) {
            if let Some(note) = skipped_by_probe(command, &ctx.cwd) {
                content.push('\n');
                content.push_str(&note);
            }
        }
        // a denial by the sandbox is usually a plain exit 1: the note is for any non-zero end
        if is_error || matches!(outcome.end, End::Exited(c) if c != 0) {
            if let Some(note) = crate::sandbox::note_for(&sandbox, &outcome.output) {
                content.push_str("\n\n");
                content.push_str(&note);
            }
        }
        if let Some(note) = guard.and_then(|g| crate::guarded::after(g, &sandbox, &ctx.cwd)) {
            content.push_str("\n\n");
            content.push_str(&note);
        }
        let exit_code = match outcome.end {
            End::Exited(c) => Some(c),
            _ => None,
        };
        let display = ToolDisplay::Shell {
            command: command.to_string(),
            stdout: outcome.output,
            stderr: String::new(),
            exit_code,
        };
        ToolOutput { content, is_error, display, images: Vec::new() }
    }
}

mod tmp_files;
pub use tmp_files::*;
mod capture;
use capture::*;
mod command_hints;
use command_hints::*;
mod background_jobs;
use background_jobs::*;
mod output_lines;
pub use output_lines::*;
mod process_group;
pub use process_group::*;
mod run_command;
pub(crate) use run_command::*;
mod read_only_words;
pub use read_only_words::*;

#[cfg(test)]
mod tests;

// every test here probes pids, so the module is Unix only
#[cfg(all(test, unix))]
mod sweep_tests;
