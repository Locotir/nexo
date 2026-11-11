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

/// The scratch folder of a session, in the OS temp folder, outside the project: `$NEXO_TMP` in its commands.
pub fn tmp_dir(session: &str) -> std::path::PathBuf {
    let id: String = session.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(40).collect();
    let id = if id.is_empty() { "main".to_string() } else { id };
    std::env::temp_dir().join(format!("nexo-tmp-{}-{id}", std::process::id()))
}

/// Removes a session's scratch folder; the agent calls it when the session ends or is replaced.
pub fn wipe_tmp(session: &str) {
    let _ = std::fs::remove_dir_all(tmp_dir(session));
}

/// Most directory entries a sweep looks at.
const SWEEP_LOOK: usize = 5_000;
/// Most folders a sweep removes.
const SWEEP_REMOVE: usize = 200;

/// A folder touched more recently than this stays even when its pid is gone: a process in another
/// container that shares the temp folder has a pid that cannot be probed from here.
const SWEEP_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// Removes the scratch folders that processes now gone left in the OS temp folder (a killed or crashed
/// nexo never wiped its own). Bounded, silent, errors ignored; the agent runs it once at start.
pub fn sweep_tmp() {
    sweep_tmp_in(&std::env::temp_dir(), &pid_alive, SWEEP_MIN_AGE);
}

/// Whether process `pid` exists (a process of another user counts as alive).
#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(not(unix))]
fn pid_alive(_pid: i32) -> bool {
    true
}

/// The pid in a `nexo-tmp-<digits>-<rest>` folder name.
fn tmp_name_pid(name: &str) -> Option<i32> {
    let (digits, rest) = name.strip_prefix("nexo-tmp-")?.split_once('-')?;
    if rest.is_empty() || digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<i32>().ok().filter(|p| *p > 0)
}

/// `sweep_tmp` on a given temp folder, with the test for "this pid is alive" and the least age given.
/// Windows and other systems do not probe pids: a folder goes only when untouched for 7 days.
pub fn sweep_tmp_in(tmp: &std::path::Path, alive: &dyn Fn(i32) -> bool, min_age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(tmp) else { return };
    let me = std::process::id();
    let mut removed = 0;
    for entry in entries.take(SWEEP_LOOK) {
        if removed >= SWEEP_REMOVE {
            break;
        }
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(tmp_name_pid) else { continue };
        if pid as u32 == me {
            continue;
        }
        let path = entry.path();
        // never follow a link: a symlink is not a directory here
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if !meta.is_dir() {
            continue;
        }
        let age = meta.modified().ok().and_then(|m| m.elapsed().ok());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // SAFETY: geteuid has no preconditions.
            if meta.uid() != unsafe { libc::geteuid() } || alive(pid) || !age.is_some_and(|a| a >= min_age) {
                continue;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = alive;
            let least = min_age.max(std::time::Duration::from_secs(7 * 24 * 3600));
            if !age.is_some_and(|a| a > least) {
                continue;
            }
        }
        removed += 1;
        let _ = std::fs::remove_dir_all(&path);
    }
}

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
                    "command": {"type": "string", "description": "The command line to run."},
                    "timeout": {"type": "integer", "minimum": 1, "maximum": 600, "description": "Seconds, default 120."},
                    "description": {"type": "string", "description": "A few words on what it does, for the person."},
                    "wait_until": {"type": "string", "description": "Shell condition polled up to `timeout`; then `command` runs."},
                    "background": {"type": "boolean", "description": "Run as a job, return at once."},
                    "detach": {"type": "boolean", "description": "With `background`: outlives nexo, logs to a file; stop it by pid."},
                    "unsandboxed": {"type": "boolean", "description": "Only after the OS sandbox blocked a command: run outside it; the person is asked."}
                },
                "required": ["command"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact> {
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
            End::Exited(code) => {
                content.push_str(&format!("\n\n[exit code {code}]"));
                true
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
        if is_error {
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

enum End {
    Exited(i32),
    Signalled,
    TimedOut,
    Cancelled,
    Failed(String),
}

struct Outcome {
    output: String,
    end: End,
}

/// The output as it comes, bounded: the first half of `MAX_OUTPUT_CHARS`
/// is kept whole, then the last half, whatever the command prints in
/// between (a build log of a gigabyte costs no memory).
struct Capture {
    head: String,
    tail: VecDeque<String>,
    tail_chars: usize,
    omitted: usize,
}

impl Capture {
    const HALF: usize = MAX_OUTPUT_CHARS / 2;

    fn new() -> Capture {
        Capture { head: String::new(), tail: VecDeque::new(), tail_chars: 0, omitted: 0 }
    }

    fn push(&mut self, line: &str) {
        let n = line.chars().count() + 1;
        if self.head.len() < Self::HALF {
            self.head.push_str(line);
            self.head.push('\n');
            return;
        }
        self.tail.push_back(line.to_string());
        self.tail_chars += n;
        while self.tail_chars > Self::HALF {
            if let Some(dropped) = self.tail.pop_front() {
                let k = dropped.chars().count() + 1;
                self.tail_chars -= k;
                self.omitted += k;
            }
        }
    }

    /// Names of commands the shell reported as not found, in the lines kept (head and tail,
    /// never the omitted middle): at most 3, each once.
    fn not_found(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for line in self.head.lines().chain(self.tail.iter().map(|l| l.as_str())) {
            if let Some(n) = not_found_name(line) {
                if !names.contains(&n) {
                    names.push(n);
                    if names.len() == 3 {
                        break;
                    }
                }
            }
        }
        names
    }

    fn text(mut self) -> String {
        if self.omitted > 0 {
            self.head.push_str(&format!("\n… [{} characters omitted] …\n\n", self.omitted));
        }
        for l in self.tail {
            self.head.push_str(&l);
            self.head.push('\n');
        }
        self.head
    }
}

/// The command a shell says it did not find on `line`, if it does: `sh: 1: X: not found`,
/// `bash: [line N: ]X: command not found`, `zsh: command not found: X`, `X: command not found`
/// the Windows `'X' is not recognized as an internal or external command`, and PowerShell's
/// `The term 'X' is not recognized as the name of a cmdlet` (5.1) or `as a name of a cmdlet` (7).
fn not_found_name(line: &str) -> Option<String> {
    let line = line.trim_end();
    let name = if let Some((_, rest)) = line.split_once("The term '") {
        // the line may wrap after "recognized as" in a narrow host, so the rest is not required
        rest.split_once("' is not recognized as")?.0
    } else if let Some(rest) = line.strip_prefix('\'') {
        rest.split_once("' is not recognized as an internal or external command")?.0
    } else if let Some((_, name)) = line.rsplit_once("command not found: ") {
        name
    } else if let Some(before) = line.strip_suffix(": command not found") {
        before.rsplit(": ").next()?
    } else {
        // dash and ash: `sh: 1: X: not found`; other tools' "not found" are not the shell's
        let before = line.strip_suffix(": not found")?;
        if !before.split(": ").next()?.ends_with("sh") {
            return None;
        }
        before.rsplit(": ").next()?
    };
    (!name.is_empty() && !name.contains(char::is_whitespace)).then(|| name.to_string())
}

/// For a failed `a && b && c`: the note to give when a link before the last is `ls`, `cat` or
/// `stat` of a plain path that does not exist (resolved after any `cd` links). `&&` stops there,
/// so what follows never ran, which the model takes for an answer about the rest. Anything
/// not plainly that (quotes, variables, globs, `;`, `||`, pipes) gets no note.
fn skipped_by_probe(command: &str, cwd: &std::path::Path) -> Option<String> {
    let links: Vec<&str> = command.split("&&").collect();
    let mut dir = cwd.to_path_buf();
    for link in &links[..links.len().checked_sub(1)?] {
        if link.contains(|c: char| "|;&<(){}$`'\"*?[~\n".contains(c)) {
            return None;
        }
        let words: Vec<&str> = link.split_whitespace().filter(|w| !w.contains('>')).collect();
        match words.split_first() {
            Some((&"cd", [to])) => dir = dir.join(to),
            Some((&"cd", _)) => return None,
            Some((&("ls" | "cat" | "stat"), args)) => {
                if let Some(missing) = args.iter().find(|a| !a.starts_with('-') && !dir.join(a).exists()) {
                    return Some(format!(
                        "[note: `{missing}` does not exist, so && skipped the commands after it; join with ; (or `|| true`) for paths that may not exist]"
                    ));
                }
            }
            _ => {}
        }
    }
    None
}

/// The refusal for a foreground command that starts with a fixed wait of 2 seconds or more
/// (`sleep 45; ...`, `sleep 25 && ...`, `sleep 0.5m` on a line of its own): it wastes the time when the
/// thing is ready and is too short when it is not. Only a leading `sleep` followed by `;`, `&&`, a
/// newline or the end counts. Left alone: `sleep 1` or less; a `sleep` in a loop body, a quoted
/// `sh -c '...'`, a heredoc or a background job (`sleep 20 &`), because none of those is first at
/// top level; and a `sleep` after other commands (`cmd; sleep 5; cmd2`), because there the wait may
/// be a pause between steps that the model cannot poll for, and refusing it would be a false positive.
fn blind_sleep(command: &str) -> Option<String> {
    if nexo_core::shell::shell().kind == "powershell" {
        return None;
    }
    let after = command.trim_start().strip_prefix("sleep")?;
    if !after.starts_with([' ', '\t']) {
        return None;
    }
    let (mut rest, mut secs, mut args) = (after, 0.0_f64, 0);
    loop {
        rest = rest.trim_start_matches([' ', '\t']);
        let end = rest.find(|c: char| c.is_whitespace() || ";&|<>()`$'\"\\".contains(c)).unwrap_or(rest.len());
        if end == 0 {
            break;
        }
        secs += sleep_seconds(&rest[..end])?;
        args += 1;
        rest = &rest[end..];
    }
    let waited = format!("sleep{}", after[..after.len() - rest.len()].trim_end());
    let rest = match rest {
        "" => "",
        r if r.starts_with("&&") => &r[2..],
        r if r.starts_with([';', '\n']) => &r[1..],
        _ => return None,
    };
    if args == 0 || secs < 2.0 {
        return None;
    }
    let rest = rest.trim();
    let cap = ((secs * 2.0).ceil() as u64).clamp(2, MAX_TIMEOUT_S);
    let cond = poll_condition(rest).unwrap_or_else(|| "<what shows it is ready>".into());
    let (tail, rest_json) = match rest {
        "" => (String::new(), "\"true\"".to_string()),
        r if !r.contains('\n') && r.len() <= 200 => (format!("; {r}"), serde_json::to_string(r).unwrap_or_default()),
        _ => ("; <the rest of your command>".into(), "\"<the rest of your command>\"".into()),
    };
    let cond_json = serde_json::to_string(&cond).unwrap_or_default();
    Some(format!(
        "bash: not run. A fixed wait (`{}`) wastes time when the thing is already ready and is too short when it is not.\n\
         Wait for the condition instead: {{\"wait_until\": {cond_json}, \"command\": {rest_json}, \"timeout\": {cap}}}. \
         It is a command that succeeds once ready: `[ -e FILE ]`, `curl -sf URL`, `grep -q READY LOG`. \
         In one command: timeout {cap} sh -c 'until {cond}; do sleep 1; done'{tail}\n\
         To wait for a command you started with `background: true`, use jobs {{\"action\": \"output\", \"id\": N, \"wait\": 60}}.",
        one_line(&waited, 60)
    ))
}

/// Polls the shell condition `cond` about every 0.5 s for up to `timeout`. None once it succeeds
/// (the command may run); otherwise the result to return at once: the condition never held, the
/// call was cancelled, or the shell could not start. Plain polling, not inotify: it works the same
/// for any condition and on every system, and half a second is no wait to speak of.
async fn wait_until(
    cond: &str,
    timeout: Duration,
    ctx: &ToolContext,
    sandbox: &nexo_core::sandbox::SandboxSettings,
) -> Option<ToolOutput> {
    const EVERY: Duration = Duration::from_millis(500);
    let start = std::time::Instant::now();
    loop {
        let mut last = Capture::new();
        let left = timeout.saturating_sub(start.elapsed()).max(Duration::from_secs(1));
        match run_command(cond, &ctx.cwd, sandbox, Some(left), &ctx.cancel, Some(&tmp_dir(&ctx.session_id)), |l| {
            last.push(l)
        })
        .await
        {
            End::Exited(0) => return None,
            End::Cancelled => return Some(ToolOutput::error("[cancelled while waiting]")),
            End::Failed(e) => {
                let label = nexo_core::shell::shell().label();
                return Some(ToolOutput::error(format!("{label}: cannot run the condition: {e}")));
            }
            _ => {}
        }
        let waited = start.elapsed();
        if waited >= timeout {
            let mut content = format!("[waited {} s: condition never held]", waited.as_secs());
            let out = last.text();
            if !out.trim().is_empty() {
                content.push_str("\nlast output of the condition:\n");
                content.push_str(&cap_text(out.trim_end(), 2000));
            }
            return Some(ToolOutput::error(content));
        }
        tokio::select! {
            _ = tokio::time::sleep(EVERY.min(timeout - waited)) => {}
            _ = ctx.cancel.cancelled() => return Some(ToolOutput::error("[cancelled while waiting]")),
        }
    }
}

/// Seconds in a `sleep` argument: a number with an optional `s`, `m`, `h` or `d`.
fn sleep_seconds(word: &str) -> Option<f64> {
    if !word.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let (num, unit) = match word.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
        Some((i, _)) => word.split_at(i),
        None => (word, "s"),
    };
    let mult = match unit {
        "s" => 1.0,
        "m" => 60.0,
        "h" => 3600.0,
        "d" => 86400.0,
        _ => return None,
    };
    num.parse::<f64>().ok().filter(|n| n.is_finite()).map(|n| n * mult)
}

/// A condition for the poll when the command after the wait plainly shows what it waits for:
/// a `curl` of a URL, or a plain absolute path on a single line. None for anything less sure.
fn poll_condition(rest: &str) -> Option<String> {
    if rest.contains('\n') {
        return None;
    }
    let plain = |t: &str| t.chars().all(|c| c.is_ascii_alphanumeric() || "/._-:%@=+,".contains(c));
    let tokens = || rest.split_whitespace().map(|t| t.trim_matches(['"', '\'', ';', '&']));
    if rest.starts_with("curl ") {
        let url = tokens().find(|t| (t.starts_with("http://") || t.starts_with("https://")) && plain(t))?;
        return Some(format!("curl -sf {url} >/dev/null"));
    }
    let path = tokens().find_map(|t| {
        let t = t.split_once('=').map_or(t, |(name, v)| {
            if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                v
            } else {
                t
            }
        });
        (t.starts_with('/') && t.len() > 1 && plain(t)).then_some(t)
    })?;
    Some(format!("[ -e {path} ]"))
}

/// Starts `command` as a job of the session and returns its number at once.
fn background(command: &str, ctx: &ToolContext, sandbox: nexo_core::sandbox::SandboxSettings) -> ToolOutput {
    let job = ctx.jobs.start(JobKind::Shell, command);
    let id = job.id();
    let (line, cwd) = (command.to_string(), ctx.cwd.clone());
    let tmp = tmp_dir(&ctx.session_id);
    let guard = crate::guarded::before(&sandbox, &cwd);
    tokio::spawn(async move {
        let cancel = job.cancel();
        let end = run_command(&line, &cwd, &sandbox, None, &cancel, Some(&tmp), |l| job.line(l)).await;
        if let Some(note) = guard.and_then(|g| crate::guarded::after(g, &sandbox, &cwd)) {
            job.line(&note);
        }
        match end {
            End::Exited(c) => job.finish(c == 0, format!("exit {c}")),
            End::Signalled => job.finish(false, "killed by a signal"),
            End::Failed(e) => {
                job.line(&format!("cannot run the command: {e}"));
                job.finish(false, "failed to start")
            }
            // killed: the registry already says so
            End::TimedOut | End::Cancelled => drop(job),
        }
    });
    let content = format!(
        "Started background job {id}: `{}`. It runs on while you work; you will be told \
         when it ends. Read its output with jobs {{\"action\": \"output\", \"id\": {id}}} (add \"wait\" \
         to wait for it), stop it with {{\"action\": \"kill\", \"id\": {id}}}.",
        one_line(command, 100)
    );
    ToolOutput::ok(content)
}

/// Starts `command` on its own, in a session of its own, with its output in a log file, and leaves it
/// running when this process exits. It is not a job of the session (the registry kills those at the
/// end), so nothing here waits for it or stops it; the sandbox is the one of any other command.
fn detached(command: &str, ctx: &ToolContext, sandbox: &nexo_core::sandbox::SandboxSettings) -> ToolOutput {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join("nexo-detached");
    let made = {
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        b.create(&dir)
    };
    if let Err(e) = made {
        return ToolOutput::error(format!("bash: cannot make {}: {e}", dir.display()));
    }
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let log =
        dir.join(format!("{ms}-{}-{}.log", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let file = {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        o.open(&log)
    };
    let (out, err) = match file.and_then(|f| f.try_clone().map(|c| (f, c))) {
        Ok(p) => p,
        Err(e) => return ToolOutput::error(format!("bash: cannot open {}: {e}", log.display())),
    };
    let mut cmd = match build_command(command, &ctx.cwd, sandbox, true) {
        Ok(c) => c,
        Err(e) => {
            return ToolOutput::error(format!("{}: cannot run the command: {e}", nexo_core::shell::shell().label()))
        }
    };
    // no kill_on_drop: the child is left to itself
    cmd.current_dir(&ctx.cwd).stdin(Stdio::null()).stdout(out).stderr(err);
    #[cfg(unix)]
    // SAFETY: runs in the forked child before exec and only calls `setsid`, which is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| match libc::setsid() {
            -1 => Err(std::io::Error::last_os_error()),
            _ => Ok(()),
        });
    }
    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ToolOutput::error(format!("{}: cannot run the command: {e}", nexo_core::shell::shell().label()))
        }
    };
    let Some(pid) = child.id() else {
        return ToolOutput::error("bash: the detached command ended before it could be followed");
    };
    drop(child);
    let stop = if cfg!(windows) { format!("taskkill /T /F /PID {pid}") } else { format!("kill -- -{pid}") };
    ToolOutput::ok(format!(
        "Started detached process {pid}: `{}`. It outlives nexo and is not a job (the `jobs` tool does not \
         list it). Its output is in {}. Stop it by pid when it is no longer needed: `{stop}`.",
        one_line(command, 100),
        log.display()
    ))
}

/// Bytes kept of one line of output; the rest of a longer line is dropped as it comes.
pub const LINE_BYTES: usize = 16 * 1024;
/// How long output still in the pipes is read after the shell exits: a process it left running
/// may hold them open for good.
const DRAIN: Duration = Duration::from_millis(300);

/// Sends each line `r` gives to `tx`, decoded lossily (a byte that is not UTF-8 does not stop the
/// reading) and cut to `LINE_BYTES` (a line with no end costs no more memory than that).
async fn read_lines<R: tokio::io::AsyncRead + Unpin>(r: R, tx: mpsc::UnboundedSender<String>) {
    let mut r = BufReader::new(r);
    let mut line: Vec<u8> = Vec::new();
    let mut cut = 0usize;
    loop {
        let (used, ended) = match r.fill_buf().await {
            Ok([]) | Err(_) => break,
            Ok(buf) => {
                let (chunk, ended) = match buf.iter().position(|b| *b == b'\n') {
                    Some(i) => (&buf[..i], true),
                    None => (buf, false),
                };
                let room = LINE_BYTES.saturating_sub(line.len());
                line.extend_from_slice(&chunk[..chunk.len().min(room)]);
                cut += chunk.len().saturating_sub(room);
                (chunk.len() + usize::from(ended), ended)
            }
        };
        r.consume(used);
        if ended && tx.send(finish_line(&mut line, &mut cut)).is_err() {
            return;
        }
    }
    if !line.is_empty() || cut > 0 {
        let _ = tx.send(finish_line(&mut line, &mut cut));
    }
}

/// The line read so far as text, said to be cut when it was; both are reset.
fn finish_line(line: &mut Vec<u8>, cut: &mut usize) -> String {
    if *cut > 0 {
        // a character split by the cut goes whole
        if let Err(e) = std::str::from_utf8(line) {
            if e.error_len().is_none() {
                *cut += line.len() - e.valid_up_to();
                line.truncate(e.valid_up_to());
            }
        }
    } else if line.last() == Some(&b'\r') {
        line.pop();
    }
    let mut s = String::from_utf8_lossy(line).into_owned();
    if *cut > 0 {
        s.push_str(&format!(" … [{} bytes of this line cut]", *cut));
    }
    line.clear();
    *cut = 0;
    s
}

/// Kills the command's process group when the call is dropped before the command ended (the
/// person interrupted the turn): `kill_on_drop` alone ends the shell, not what it started.
struct GroupGuard {
    pid: Option<u32>,
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        // on Windows this is `taskkill /T`: `kill_on_drop` alone would leave the tree running
        kill_group(self.pid.take());
    }
}

/// The shell command for `command`, inside the sandbox when it is on (`detach`: one that is not
/// tied to this process's life).
fn build_command(
    command: &str,
    cwd: &std::path::Path,
    sandbox: &nexo_core::sandbox::SandboxSettings,
    detach: bool,
) -> Result<tokio::process::Command, String> {
    let shell = nexo_core::shell::shell();
    let args = shell.args_for(command);
    let wrapped = if detach {
        crate::sandbox::wrap_detached(sandbox, cwd, &shell.program, &args)
    } else {
        crate::sandbox::wrap(sandbox, cwd, &shell.program, &args)
    };
    let mut cmd = match wrapped? {
        Some(c) => c,
        None => {
            let mut c = tokio::process::Command::new(&shell.program);
            c.args(&args);
            c
        }
    };
    // `nexo bin` is part of nexo: with no `nexo` on the PATH, the folder of the running one is added at its end
    add_nexo_path(&mut cmd, std::env::var_os("PATH").as_deref(), std::env::current_exe().ok().as_deref());
    Ok(cmd)
}

fn add_nexo_path(cmd: &mut tokio::process::Command, path: Option<&std::ffi::OsStr>, exe: Option<&std::path::Path>) {
    if let Some(path) = nexo_on_path(path, exe) {
        cmd.env("PATH", path);
    }
}

/// The PATH for a command so that `nexo` is found: `path` with the folder of the running nexo (`exe`) added at the
/// END, never in front of anything. `None` (leave the PATH alone) when a `nexo` is already found on it, when the
/// running program is not called `nexo`, when there is no PATH to add to (the shell then uses its own default), or
/// when others can write in that folder (a nexo run from `/tmp`): a program someone else drops there must not be found.
pub fn nexo_on_path(path: Option<&std::ffi::OsStr>, exe: Option<&std::path::Path>) -> Option<std::ffi::OsString> {
    let name = if cfg!(windows) { "nexo.exe" } else { "nexo" };
    let exe = exe?;
    if exe.file_name()? != std::ffi::OsStr::new(name) {
        return None;
    }
    let (path, dir) = (path.filter(|p| !p.is_empty())?, exe.parent()?);
    if std::env::split_paths(path).any(|d| d.join(name).is_file()) || others_write(dir) {
        return None;
    }
    std::env::join_paths(std::env::split_paths(path).chain(std::iter::once(dir.to_path_buf()))).ok()
}

/// Whether the group or everybody may write in `dir`; a folder that cannot be asked counts as such.
#[cfg(unix)]
fn others_write(dir: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(dir).map_or(true, |m| m.permissions().mode() & 0o022 != 0)
}

/// Windows keeps who may write in a list this does not read: the folder is taken as the person's own.
#[cfg(not(unix))]
fn others_write(_dir: &std::path::Path) -> bool {
    false
}

/// Runs `command` in `cwd`, each line of its output (stdout and stderr) to
/// `on_line`, until it exits, `timeout` passes or `cancel` fires. The call ends
/// when the shell exits, even if something it left running still holds the pipes.
async fn run_command(
    command: &str,
    cwd: &std::path::Path,
    sandbox: &nexo_core::sandbox::SandboxSettings,
    timeout: Option<Duration>,
    cancel: &CancellationToken,
    tmp: Option<&std::path::Path>,
    mut on_line: impl FnMut(&str),
) -> End {
    let mut cmd = match build_command(command, cwd, sandbox, false) {
        Ok(c) => c,
        Err(e) => return End::Failed(e),
    };
    cmd.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    if let Some(tmp) = tmp {
        let _ = std::fs::create_dir_all(tmp);
        cmd.env("NEXO_TMP", tmp);
    }
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return End::Failed(e.to_string()),
    };
    let pid = child.id();
    // declared after `child`: dropped before it, while the group's leader is still there
    let mut guard = GroupGuard { pid };
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        readers.push(tokio::spawn(read_lines(out, tx.clone())));
    }
    if let Some(err) = child.stderr.take() {
        readers.push(tokio::spawn(read_lines(err, tx.clone())));
    }
    drop(tx);
    // no timeout: a year is as good as forever, and keeps one code path
    let timeout = timeout.unwrap_or(Duration::from_secs(86_400 * 365));
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut open = true;
    let end = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break End::Cancelled,
            _ = &mut deadline => break End::TimedOut,
            status = child.wait() => break match status {
                Ok(s) => match s.code() {
                    Some(c) => End::Exited(c),
                    None => End::Signalled,
                },
                Err(e) => End::Failed(e.to_string()),
            },
            line = rx.recv(), if open => match line {
                Some(l) => on_line(&l),
                None => open = false,
            },
        }
    };
    if matches!(end, End::Cancelled | End::TimedOut) {
        kill_group(pid);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    // the shell is gone (and reaped): its pid may be another process's soon
    guard.pid = None;
    // what is still in the pipes, for a short while: they close at once unless something the
    // command left running holds them
    let drain = tokio::time::sleep(DRAIN);
    tokio::pin!(drain);
    while open {
        tokio::select! {
            biased;
            _ = &mut drain => break,
            line = rx.recv() => match line {
                Some(l) => on_line(&l),
                None => open = false,
            },
        }
    }
    for r in readers {
        r.abort();
    }
    // lines already read when the readers stopped
    while let Ok(l) = rx.try_recv() {
        on_line(&l);
    }
    end
}

/// How a command that proves a change ended.
pub(crate) struct Proof {
    /// Its exit code; None when it was killed (time, signal, cancel) or did not start.
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub secs: f64,
    /// Its output (stdout and stderr), head and tail kept.
    pub output: String,
}

/// Runs `command` in `cwd` until it ends or `timeout` passes.
pub(crate) async fn prove(
    command: &str,
    cwd: &std::path::Path,
    sandbox: &nexo_core::sandbox::SandboxSettings,
    timeout: Duration,
    cancel: &CancellationToken,
    tmp: Option<&std::path::Path>,
) -> Proof {
    let t0 = std::time::Instant::now();
    let mut output = Capture::new();
    let end = run_command(command, cwd, sandbox, Some(timeout), cancel, tmp, |l| output.push(l)).await;
    let mut text = output.text();
    let (exit, timed_out) = match end {
        End::Exited(c) => (Some(c), false),
        End::TimedOut => (None, true),
        End::Failed(e) => {
            text = format!("cannot run the command: {e}");
            (None, false)
        }
        End::Signalled | End::Cancelled => (None, false),
    };
    Proof { exit, timed_out, secs: t0.elapsed().as_secs_f64(), output: text }
}

/// Kills the whole process group the command started (its children too). Windows has no groups:
/// there the tree under the process goes (`taskkill /T`), as ending the shell alone leaves what it
/// started running. Called while the process is still alive, so the tree can still be found.
pub(crate) fn kill_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // SAFETY: a plain signal to a process group this process created.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    if let Some(pid) = pid {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(any(unix, windows)))]
    let _ = pid;
}

/// Whether `command` only looks at things: every part of the pipeline or list is one of the
/// reading programs, and nothing writes (no redirection to a file, no `sed -i`, no `find -delete`,
/// no substitution that could run something else). Such a command may run beside reads.
fn only_looks(command: &str) -> bool {
    command_words(command).is_some_and(|segs| !segs.is_empty() && segs.iter().all(|s| reads_only(s)))
}

/// The words of each part of the pipeline or list of `command`, split the way a shell would for plain
/// text, and nothing run or expanded. `None` for a command that redirects to a file, groups, or
/// substitutes (`$(...)`, backticks), or leaves a quote open: its words say too little to judge.
pub fn command_words(command: &str) -> Option<Vec<Vec<String>>> {
    let cleaned = command.replace("2>&1", " ").replace("&>/dev/null", " ").replace(">/dev/null", " ");
    let mut segments: Vec<Vec<String>> = vec![Vec::new()];
    let mut word = String::new();
    let mut has_word = false;
    let mut quote: Option<char> = None;
    let mut chars = cleaned.chars().peekable();
    macro_rules! end_word {
        () => {
            if has_word {
                segments.last_mut().unwrap().push(std::mem::take(&mut word));
                has_word = false;
            }
        };
    }
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if q == '"' && (c == '`' || (c == '$' && chars.peek() == Some(&'('))) {
                    return None;
                } else if q == '"' && c == '\\' {
                    word.extend(chars.next());
                } else {
                    word.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    has_word = true;
                }
                '\\' => {
                    has_word = true;
                    word.extend(chars.next());
                }
                '>' | '<' | '`' | '(' | ')' | '{' | '}' => return None,
                '$' if chars.peek() == Some(&'(') => return None,
                '|' | ';' | '\n' => {
                    end_word!();
                    if c == '|' && chars.peek() == Some(&'|') {
                        chars.next();
                    }
                    segments.push(Vec::new());
                }
                '&' => {
                    if chars.next() != Some('&') {
                        return None;
                    }
                    end_word!();
                    segments.push(Vec::new());
                }
                c if c.is_whitespace() => end_word!(),
                c => {
                    word.push(c);
                    has_word = true;
                }
            },
        }
    }
    if quote.is_some() {
        return None;
    }
    if has_word {
        segments.last_mut().unwrap().push(word);
    }
    Some(segments.into_iter().filter(|s| !s.is_empty()).collect())
}

/// One command (its words) of the reading kind.
fn reads_only(words: &[String]) -> bool {
    let flags = || words[1..].iter().filter(|w| w.starts_with('-'));
    match words[0].as_str() {
        "cat" | "head" | "tail" | "grep" | "egrep" | "fgrep" | "rg" | "ls" | "wc" | "stat" | "file" => true,
        // `nexo bin` reads a file as bytes and prints. `name` and `note` also save into nexo's data folder, so they ask
        "nexo" => {
            words.get(1).map(String::as_str) == Some("bin")
                && matches!(
                    words.get(2).map(String::as_str),
                    Some("info" | "funcs" | "fn" | "xref" | "calls" | "strings" | "find" | "names" | "bytes")
                )
        }
        "sed" => {
            flags().any(|w| w == "-n" || w == "--quiet" || w == "--silent")
                && !flags().any(|w| w.starts_with("--in-place") || (!w.starts_with("--") && w.contains('i')))
                && words[1..].iter().find(|w| !w.starts_with('-')).is_some_and(|s| prints_lines(s))
        }
        "find" => !words[1..].iter().any(|w| {
            matches!(
                w.as_str(),
                "-exec" | "-execdir" | "-ok" | "-okdir" | "-delete" | "-fprint" | "-fprint0" | "-fprintf" | "-fls"
            )
        }),
        "git" => {
            matches!(words.get(1).map(String::as_str), Some("log" | "diff" | "status" | "show"))
                && !words.iter().any(|w| w.starts_with("--output") || w == "--ext-diff")
        }
        _ => false,
    }
}

/// A `sed -n` script that only prints lines: `5p`, `10,20p`, `$p`, `/text/p`.
fn prints_lines(script: &str) -> bool {
    let Some(addr) = script.strip_suffix('p') else { return false };
    addr.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '$')
        || (addr.len() >= 2 && addr.starts_with('/') && addr.ends_with('/') && !addr.contains(';'))
}
#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[tokio::test]
    async fn nexo_tmp_is_kept_between_commands_until_it_is_wiped() {
        let d = tempfile::tempdir().unwrap();
        let mut c = ToolContext::new(d.path().to_path_buf());
        c.session_id = "tmp-test-session".into();
        let out = Bash.run(json!({"command": "echo probe > \"$NEXO_TMP/probe.txt\" && echo \"$NEXO_TMP\""}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        let dir = std::path::PathBuf::from(out.content.trim());
        assert!(dir.join("probe.txt").is_file(), "{dir:?}");
        assert!(!dir.starts_with(d.path()), "the folder is not in the project");
        // a later command (the next turn) still finds the file
        let out = Bash.run(json!({"command": "cat \"$NEXO_TMP/probe.txt\"; echo again > \"$NEXO_TMP/p2\""}), &c).await;
        assert_eq!(out.content.trim(), "probe");
        wipe_tmp("tmp-test-session");
        assert!(!dir.exists(), "{dir:?} is gone once the session ends");
        // a later command gets a fresh one
        let out =
            Bash.run(json!({"command": "ls \"$NEXO_TMP\"; echo x > \"$NEXO_TMP/p3\"; ls \"$NEXO_TMP\""}), &c).await;
        assert_eq!(out.content.trim(), "p3");
        wipe_tmp("tmp-test-session");
    }

    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn own_checks_see_nexo_tmp() {
        let d = tempfile::tempdir().unwrap();
        let mut c = ToolContext::new(d.path().to_path_buf());
        c.session_id = "tmp-own-session".into();
        let own = crate::postcheck::run_own("echo \"[$NEXO_TMP]\"", &c).await;
        assert!(own.contains("nexo-tmp-"), "{own}");
        wipe_tmp("tmp-own-session");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn background_jobs_see_nexo_tmp() {
        let d = tempfile::tempdir().unwrap();
        let mut c = ToolContext::new(d.path().to_path_buf());
        c.session_id = "tmp-bg2-session".into();
        let marker = d.path().join("seen.txt");
        let cmd = format!("printf %s \"$NEXO_TMP\" > '{}'", marker.display());
        let out = Bash.run(json!({"command": cmd, "background": true}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        for _ in 0..100 {
            if marker.is_file() && std::fs::metadata(&marker).map(|m| m.len() > 0).unwrap_or(false) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let seen = std::fs::read_to_string(&marker).unwrap_or_default();
        assert!(seen.contains("nexo-tmp-"), "NEXO_TMP in a background job: {seen:?}");
        wipe_tmp("tmp-bg2-session");
    }

    #[test]
    fn read_only_commands_are_read_kind() {
        let kind = |c: &str| Bash.call_kind(&json!({"command": c}));
        for (c, want) in [
            ("cat x", ToolKind::Read),
            ("sed -n '1,20p' src/a.rs", ToolKind::Read),
            ("sed -i 's/a/b/' x", ToolKind::Exec),
            ("sed -n '1w out' x", ToolKind::Exec),
            ("cargo test", ToolKind::Exec),
            ("cat a | grep b", ToolKind::Read),
            ("grep -rn 'a|b' src 2>&1 | head -5", ToolKind::Read),
            ("cat a > b", ToolKind::Exec),
            ("cat a >> b", ToolKind::Exec),
            ("ls -la && git log --oneline -5", ToolKind::Read),
            ("git status", ToolKind::Read),
            ("git checkout main", ToolKind::Exec),
            ("find . -name '*.rs' -delete", ToolKind::Exec),
            ("find . -name '*.rs'", ToolKind::Read),
            ("cat a | tee b", ToolKind::Exec),
            ("rm x", ToolKind::Exec),
            ("cat $(echo x)", ToolKind::Exec),
            ("tail -f log &", ToolKind::Exec),
            ("", ToolKind::Exec),
        ] {
            assert_eq!(kind(c), want, "{c}");
        }
        assert_eq!(Bash.call_kind(&json!({"command": "cat x", "background": true})), ToolKind::Exec);
    }

    #[test]
    fn nexo_bin_reads_are_read_kind_and_what_saves_or_follows_is_judged_as_before() {
        let kind = |c: &str| Bash.call_kind(&json!({"command": c}));
        for c in [
            "nexo bin info a.out",
            "nexo bin funcs a.out --sort size --lines 20",
            "nexo bin fn a.out main --bytes",
            "nexo bin xref a.out 0x401a30 --json",
            "nexo bin calls a.out main",
            "nexo bin strings a.out --grep 'http' --sort refs",
            "nexo bin find a.out 48 8d 05",
            "nexo bin names a.out",
            "nexo bin bytes a.out 0x402000 64 --as u32",
            "nexo bin fn a.out main 2>&1 | head -40",
            "cd src && nexo bin info a.out",
        ] {
            let want = if c.starts_with("cd ") { ToolKind::Exec } else { ToolKind::Read };
            assert_eq!(kind(c), want, "{c}");
        }
        for c in [
            // saves into nexo's data folder: asks, as a command that writes does
            "nexo bin name a.out main entry",
            "nexo bin note a.out main starts here",
            "nexo bin name a.out main --clear",
            // not a read, or not a form that is known
            "nexo bin",
            "nexo bin --help",
            "nexo bin nope a.out",
            "nexo info a.out",
            "nexo bin fn a.out main | tee out.txt",
            "nexo bin fn a.out main > out.txt",
            "nexo bin fn a.out main; rm x",
            "nexo bin fn a.out main && touch x",
            "nexo bin fn $(touch x) main",
            "nexo bin fn `touch x` main",
            "nexo bin fn a.out main &",
        ] {
            assert_eq!(kind(c), ToolKind::Exec, "{c}");
        }
        // a name with a quote or a dash is only text: the judgment reads it and runs nothing
        assert_eq!(kind("nexo bin info '$(touch x)'"), ToolKind::Read);
        assert_eq!(kind("nexo bin info -- -weird"), ToolKind::Read);
        assert_eq!(kind("nexo bin info \"$(touch x)\""), ToolKind::Exec);
        assert_eq!(kind("nexo bin info a.out\ntouch x"), ToolKind::Exec);
        assert_eq!(Bash.call_kind(&json!({"command": "nexo bin info a", "background": true})), ToolKind::Exec);
    }

    #[test]
    fn command_words_splits_without_running_or_expanding() {
        let w = |c: &str| command_words(c);
        assert_eq!(
            w("objdump -d 'my file' | head -5; strings x"),
            Some(vec![
                vec!["objdump".into(), "-d".into(), "my file".into()],
                vec!["head".into(), "-5".into()],
                vec!["strings".into(), "x".into()],
            ])
        );
        assert_eq!(
            w("objdump -d a 2>&1 | head"),
            Some(vec![vec!["objdump".into(), "-d".into(), "a".into()], vec!["head".into()]])
        );
        assert_eq!(w("'$(x)' a"), Some(vec![vec!["$(x)".into(), "a".into()]]));
        for c in ["objdump -d $(x)", "objdump -d `x`", "objdump -d a > b", "objdump -d 'a", "objdump (a)", "x &"] {
            assert_eq!(w(c), None, "{c}");
        }
        assert_eq!(w(""), Some(vec![]));
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_others_can_write_in_is_never_added_to_the_path() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let (bin, other) = (home.path().join("shared"), home.path().join("other"));
        for d in [&bin, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(bin.join("nexo"), "x").unwrap();
        let path = std::env::join_paths([&other]).unwrap();
        for mode in [0o777, 0o1777, 0o775, 0o757] {
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(nexo_on_path(Some(&path), Some(&bin.join("nexo"))), None, "{mode:o}");
        }
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(nexo_on_path(Some(&path), Some(&bin.join("nexo"))).is_some());
        // a folder that is gone cannot be asked, so it is not added either
        assert_eq!(nexo_on_path(Some(&path), Some(&home.path().join("gone").join("nexo"))), None);
    }

    #[test]
    fn nexo_is_added_at_the_end_of_the_path_only_when_none_is_found() {
        use std::ffi::OsString;
        let name = if cfg!(windows) { "nexo.exe" } else { "nexo" };
        let home = tempfile::tempdir().unwrap();
        let (bin, other, with) = (home.path().join("app"), home.path().join("other"), home.path().join("with"));
        for d in [&bin, &other, &with] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(bin.join(name), "x").unwrap();
        std::fs::write(with.join(name), "x").unwrap();
        // whatever the umask of the machine: this folder is the person's alone
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let exe = bin.join(name);
        let join = |ds: &[&std::path::Path]| std::env::join_paths(ds).unwrap();
        // absent: the folder of the running nexo comes last, what was there stays in order
        let path = join(&[home.path(), &other]);
        let got = nexo_on_path(Some(&path), Some(&exe)).expect("added");
        let parts: Vec<_> = std::env::split_paths(&got).collect();
        assert_eq!(parts, [home.path().to_path_buf(), other.clone(), bin.clone()]);
        // present anywhere on it: untouched
        for ds in [vec![&with as &std::path::Path, &other], vec![&other, &with]] {
            assert_eq!(nexo_on_path(Some(&join(&ds)), Some(&exe)), None);
        }
        // the folder of the running one is already there: untouched
        assert_eq!(nexo_on_path(Some(&join(&[&bin, &other])), Some(&exe)), None);
        // a program not called nexo, no path to add to, no known program: untouched
        assert_eq!(nexo_on_path(Some(&path), Some(&bin.join("nexo-test-binary"))), None);
        assert_eq!(nexo_on_path(None, Some(&exe)), None);
        assert_eq!(nexo_on_path(Some(&OsString::new()), Some(&exe)), None);
        assert_eq!(nexo_on_path(Some(&path), None), None);
        // and the command that is built carries it, or carries nothing of its own
        let env_of = |p: Option<&std::ffi::OsStr>| {
            let mut c = tokio::process::Command::new("x");
            add_nexo_path(&mut c, p, Some(&exe));
            c.as_std().get_envs().find(|(k, _)| *k == "PATH").map(|(_, v)| v.map(OsString::from))
        };
        assert_eq!(env_of(Some(&path)), Some(Some(got)));
        assert_eq!(env_of(Some(&join(&[&with]))), None);
    }

    #[tokio::test]
    async fn runs_and_reports_output_and_exit_code() {
        if nexo_core::shell::shell().kind == "powershell" {
            eprintln!("the test writes POSIX shell; this system runs PowerShell: skipped");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let out = Bash.run(json!({"command": "echo hi; echo err >&2; exit 3"}), &c).await;
        assert!(out.is_error);
        assert!(out.content.contains("hi\n") && out.content.contains("err\n"), "{}", out.content);
        assert!(out.content.ends_with("[exit code 3]"));
        match out.display {
            ToolDisplay::Shell { exit_code, .. } => assert_eq!(exit_code, Some(3)),
            _ => panic!(),
        }
        let out = Bash.run(json!({"command": "pwd"}), &c).await;
        assert!(!out.is_error);
        // Git Bash prints its own form of the path (/tmp/x, not C:\...\Temp\x): compare the folder
        let name = d.path().file_name().unwrap().to_string_lossy().into_owned();
        assert!(out.content.trim().ends_with(&format!("/{name}")), "{}", out.content);
        let out = Bash.run(json!({"command": "true"}), &c).await;
        assert_eq!(out.content, "(no output)");
        assert!(Bash.run(json!({}), &c).await.is_error);
    }

    #[tokio::test]
    async fn streams_lines_and_kills_on_timeout_and_cancel() {
        if nexo_core::shell::shell().kind == "powershell" {
            eprintln!("the test writes POSIX shell; this system runs PowerShell: skipped");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut c = ToolContext::new(d.path().to_path_buf());
        c.progress = Some(tx);
        let t = std::time::Instant::now();
        let out = Bash.run(json!({"command": "echo start; sleep 30; echo never", "timeout": 1}), &c).await;
        assert!(t.elapsed() < Duration::from_secs(5));
        assert!(out.is_error && out.content.contains("timed out"), "{}", out.content);
        assert!(out.content.contains("start"));
        assert_eq!(rx.recv().await.unwrap().line, "start");

        let c2 = ToolContext::new(d.path().to_path_buf());
        let cancel = c2.cancel.clone();
        let t = std::time::Instant::now();
        let run = Bash.run(json!({"command": "echo go; sleep 30", "timeout": 60}), &c2);
        let out = tokio::join!(run, async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel.cancel();
        })
        .0;
        assert!(t.elapsed() < Duration::from_secs(5));
        assert!(out.content.contains("[cancelled]"), "{}", out.content);
    }

    fn posix() -> bool {
        if nexo_core::shell::shell().kind == "powershell" {
            eprintln!("the test writes POSIX shell; this system runs PowerShell: skipped");
            return false;
        }
        true
    }

    #[tokio::test]
    async fn a_leading_fixed_sleep_is_refused_with_a_poll_rewrite() {
        if !posix() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        for (cmd, has) in [
            ("sleep 45; R=/tmp/x/out; ls -la $R; find $R -type f | head", "[ -e /tmp/x/out ]"),
            ("sleep 25 && python3 - <<'PY'\nprint(1)\nPY", "wait_until"),
            ("sleep 3 && curl -s http://127.0.0.1:8080/health", "curl -sf http://127.0.0.1:8080/health >/dev/null"),
            ("sleep 0.5m", "timeout 60 "),
            ("  sleep 5s\necho hi", "timeout 10 "),
            ("sleep 45", "timeout 90 "),
        ] {
            let t = std::time::Instant::now();
            let out = Bash.run(json!({"command": cmd}), &c).await;
            assert!(t.elapsed() < Duration::from_secs(1), "{cmd} ran");
            assert!(
                out.is_error && out.content.contains("until") && out.content.contains("wait_until"),
                "{cmd}: {}",
                out.content
            );
            assert!(out.content.contains(has), "{cmd}: {}", out.content);
        }
    }

    #[tokio::test]
    async fn short_sleeps_and_sleeps_that_are_not_first_still_run() {
        if !posix() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        for cmd in [
            "sleep 1; echo ok",
            "sleep 0.5 && echo ok",
            "sleep 1.9; echo ok",
            "echo a; sleep 2; echo ok",
            "timeout 5 sh -c 'until [ -e nothing ] || true; do sleep 2; done'; echo ok",
            "i=0; while [ $i -lt 1 ]; do sleep 2; i=1; done; echo ok",
            "sh -c 'sleep 2; echo ok'",
            "cat <<'EOF' >/dev/null\nsleep 5\nEOF\necho ok",
            "sleep 20 & echo ok",
        ] {
            let out = Bash.run(json!({"command": cmd, "timeout": 10}), &c).await;
            assert!(!out.is_error && out.content.contains("ok"), "{cmd}: {}", out.content);
        }
        let out = Bash.run(json!({"command": "sleep 2; echo bg", "background": true}), &c).await;
        assert!(out.content.contains("Started background job"), "{}", out.content);
    }

    #[tokio::test]
    async fn wait_until_returns_when_a_file_appears() {
        if !posix() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        Bash.run(json!({"command": "sleep 1; echo done > flag.txt", "background": true}), &c).await;
        let t = std::time::Instant::now();
        let out = Bash
            .run(
                json!({"command": "cat flag.txt", "wait_until": "[ -e flag.txt ] && [ -s flag.txt ]", "timeout": 30}),
                &c,
            )
            .await;
        assert!(!out.is_error && out.content.contains("done"), "{}", out.content);
        assert!(t.elapsed() >= Duration::from_millis(900) && t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    }

    #[tokio::test]
    async fn a_condition_that_never_holds_is_reported_and_the_command_does_not_run() {
        if !posix() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let t = std::time::Instant::now();
        let out =
            Bash.run(json!({"command": "touch ran.txt", "wait_until": "echo not yet; false", "timeout": 1}), &c).await;
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        assert!(out.is_error && out.content.contains("condition never held"), "{}", out.content);
        assert!(out.content.starts_with("[waited 1 s") && out.content.contains("not yet"), "{}", out.content);
        assert!(!d.path().join("ran.txt").exists());
    }

    #[tokio::test]
    async fn cancel_stops_the_wait() {
        if !posix() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let cancel = c.cancel.clone();
        let t = std::time::Instant::now();
        let run = Bash.run(json!({"command": "touch ran.txt", "wait_until": "false", "timeout": 60}), &c);
        let out = tokio::join!(run, async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            cancel.cancel();
        })
        .0;
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
        assert!(out.is_error && out.content.contains("cancelled"), "{}", out.content);
        assert!(!d.path().join("ran.txt").exists());
    }

    #[test]
    fn reads_the_shells_not_found_forms() {
        for (line, want) in [
            ("sh: 1: python3: not found", Some("python3")),
            ("/bin/sh: 1: foo: not found", Some("foo")),
            ("bash: foo: command not found", Some("foo")),
            ("bash: line 3: foo: command not found", Some("foo")),
            ("zsh: command not found: foo", Some("foo")),
            ("foo: command not found", Some("foo")),
            ("'foo' is not recognized as an internal or external command,", Some("foo")),
            ("foo : The term 'foo' is not recognized as the name of a cmdlet, function, script file,", Some("foo")),
            ("The term 'foo' is not recognized as a name of a cmdlet, function,", Some("foo")),
            ("Error: file: not found", None),
            ("all fine", None),
        ] {
            assert_eq!(not_found_name(line).as_deref(), want, "{line}");
        }
    }

    #[tokio::test]
    async fn a_command_not_found_in_a_script_that_exits_0_is_noted() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        if nexo_core::shell::shell().kind == "powershell" {
            eprintln!("PowerShell words a missing command its own way: skipped");
            return;
        }
        let (missing, ok) = ("false_cmd_xyz; true", "echo fine; true");
        let out = Bash.run(json!({"command": missing}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.ends_with("\n[note: a command in this script was not found: false_cmd_xyz]"),
            "{}",
            out.content
        );
        let out = Bash.run(json!({"command": ok}), &c).await;
        assert!(!out.is_error && !out.content.contains("[note:"), "{}", out.content);
        // a non-zero exit shows the error itself: no note
        let out = Bash.run(json!({"command": "false_cmd_xyz"}), &c).await;
        assert!(out.is_error && !out.content.contains("[note:"), "{}", out.content);
        let out = Bash.run(json!({"command": "a1x; b2x; a1x; c3x; d4x; true"}), &c).await;
        assert!(out.content.ends_with("a1x, b2x, c3x]"), "{}", out.content);
    }

    #[test]
    fn a_missing_path_probe_in_an_and_chain_is_named() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("Cargo.toml"), "").unwrap();
        let p = d.path();
        let note = |c: &str| skipped_by_probe(c, p);
        let n = note("ls tests 2>/dev/null && cat src/lib.rs").expect("probe of a missing path");
        assert!(n.contains("tests") && n.contains(';'), "{n}");
        assert!(note("cat Cargo.toml && ls -R src tests 2>/dev/null && echo done").unwrap().contains("tests"));
        // the probe sits after a cd: the path is looked for there
        assert!(note("cd src && ls nope && echo x").unwrap().contains("nope"));
        assert!(note("cd src && ls ../src && echo x").is_none());
        // the path exists, is the last link, is a glob or a variable, or the chain uses ; or ||
        assert!(note("ls src && cat missing.rs").is_none());
        assert!(note("ls tests*/ && echo x").is_none());
        assert!(note("ls $HOME/zz && echo x").is_none());
        assert!(note("ls tests || echo none && echo x").is_none());
        assert!(note("ls tests; cat src/lib.rs").is_none());
        assert!(note("grep -n x tests && echo x").is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_probe_in_a_chain_adds_a_note_to_the_result() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let out = Bash.run(json!({"command": "ls tests 2>/dev/null && echo after"}), &c).await;
        assert!(out.is_error && out.content.contains("[exit code"), "{}", out.content);
        assert!(
            out.content.contains("skipped the commands after it") && out.content.contains("`tests`"),
            "{}",
            out.content
        );
        let out = Bash.run(json!({"command": "ls tests 2>/dev/null; echo after"}), &c).await;
        assert!(!out.is_error && !out.content.contains("skipped the commands"), "{}", out.content);
    }

    #[tokio::test]
    async fn caps_long_output() {
        if nexo_core::shell::shell().kind == "powershell" {
            eprintln!("the test writes POSIX shell; this system runs PowerShell: skipped");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let out = Bash.run(json!({"command": "seq 1 20000"}), &c).await;
        assert!(!out.is_error);
        assert!(out.content.chars().count() < MAX_OUTPUT_CHARS + 200);
        assert!(out.content.starts_with("1\n2\n"));
        assert!(out.content.trim_end().ends_with("20000"));
        assert!(out.content.contains("characters omitted"));
        // a million lines: bounded while it runs, head and tail kept, quick
        let t = std::time::Instant::now();
        // awk, not seq: macOS's seq writes 1000000 as 1e+06
        let out = Bash.run(json!({"command": "awk 'BEGIN { for (i = 1; i <= 1000000; i++) print i }'"}), &c).await;
        assert!(t.elapsed() < Duration::from_secs(20));
        assert!(
            out.content.starts_with("1\n2\n") && out.content.trim_end().ends_with("1000000"),
            "{}",
            &out.content[out.content.len() - 40..]
        );
        assert!(out.content.chars().count() < MAX_OUTPUT_CHARS + 200);
        match &out.display {
            ToolDisplay::Shell { stdout, .. } => {
                assert!(stdout.chars().count() < MAX_OUTPUT_CHARS + 200, "the display is bounded too")
            }
            _ => panic!(),
        }
        assert_eq!(Bash.summary(&json!({"command": "cargo test\n# second"})), "bash: cargo test …");
    }

    #[tokio::test]
    async fn lines_are_decoded_lossily_and_cut_to_a_bound() {
        let mut input = b"caf\xe9\nok\r\n".to_vec();
        input.extend(std::iter::repeat_n(b'y', LINE_BYTES * 4));
        input.extend_from_slice(b"\nlast");
        let (tx, mut rx) = mpsc::unbounded_channel();
        read_lines(&input[..], tx).await;
        assert_eq!(rx.recv().await.unwrap(), "caf\u{fffd}");
        assert_eq!(rx.recv().await.unwrap(), "ok");
        let long = rx.recv().await.unwrap();
        assert!(long.starts_with(&"y".repeat(LINE_BYTES)), "the head is kept");
        assert!(long.ends_with(&format!(" … [{} bytes of this line cut]", LINE_BYTES * 3)), "{}", &long[LINE_BYTES..]);
        assert_eq!(rx.recv().await.unwrap(), "last");
        assert!(rx.recv().await.is_none());
        // a character split by the cut is not half kept
        let mut s = "a".repeat(LINE_BYTES - 1).into_bytes();
        s.extend_from_slice("é tail\n".as_bytes());
        let (tx, mut rx) = mpsc::unbounded_channel();
        read_lines(&s[..], tx).await;
        let l = rx.recv().await.unwrap();
        assert!(!l.contains('\u{fffd}') && l.contains("bytes of this line cut"), "{}", &l[LINE_BYTES - 4..]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn output_that_is_not_utf8_or_one_huge_line_is_kept_bounded() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let out = Bash.run(json!({"command": "printf 'caf\\351\\n'; echo after"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("caf\u{fffd}\nafter"), "{}", out.content);
        // 8 MB with no line end
        let out = Bash.run(json!({"command": "head -c 8000000 /dev/zero | tr '\\0' y; echo; echo end"}), &c).await;
        assert!(!out.is_error, "{}", &out.content[out.content.len().saturating_sub(200)..]);
        assert!(out.content.contains("bytes of this line cut") && out.content.trim_end().ends_with("end"));
        assert!(out.content.chars().count() < MAX_OUTPUT_CHARS + 200);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_call_ends_when_the_shell_exits_even_if_a_child_holds_the_pipes() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let t = std::time::Instant::now();
        let out = Bash.run(json!({"command": "sleep 20 & echo $!", "timeout": 30}), &c).await;
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        assert!(!out.is_error, "{}", out.content);
        let pid: i32 = out.content.trim().parse().unwrap();
        // SAFETY: a signal to the child this test started
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }

    /// Whether process `pid` still runs (a zombie counts as gone).
    #[cfg(target_os = "linux")]
    fn alive(pid: i32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(s) => !s.rsplit(')').next().unwrap_or("").trim_start().starts_with('Z'),
            Err(_) => false,
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn dropping_the_call_kills_what_the_command_started() {
        let d = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut c = ToolContext::new(d.path().to_path_buf());
        c.progress = Some(tx);
        let run = Bash.run(json!({"command": "sleep 300 & echo $!; wait", "timeout": 600}), &c);
        // the turn's select drops the call when the person interrupts it
        let pid: i32 = tokio::select! {
            _ = run => panic!("the command ended by itself"),
            p = rx.recv() => p.unwrap().line.trim().parse().unwrap(),
        };
        let t = std::time::Instant::now();
        while alive(pid) && t.elapsed() < Duration::from_secs(2) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!alive(pid), "the background child of the dropped command still runs");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_detached_command_names_its_log_and_pid_and_outlives_the_registry() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let out = Bash.run(json!({"command": "echo up; exec sleep 30", "background": true, "detach": true}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        let pid: i32 =
            out.content.split("detached process ").nth(1).unwrap().split(':').next().unwrap().parse().unwrap();
        let log = out.content.split("output is in ").nth(1).unwrap().split(". Stop").next().unwrap().to_string();
        assert!(out.content.contains("outlives nexo"), "{}", out.content);
        assert!(c.jobs.list().is_empty(), "a detached command must not be a job of the session");
        let t = std::time::Instant::now();
        while std::fs::read_to_string(&log).unwrap_or_default().trim() != "up" && t.elapsed() < Duration::from_secs(2) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "up");
        // its own session: it leads its group, and the session's end does not reach it
        assert_eq!(unsafe { libc::getsid(pid) }, pid);
        c.jobs.kill_all();
        drop(c);
        assert!(unsafe { libc::kill(pid, 0) } == 0 && alive(pid), "the detached command died with the registry");
        // SAFETY: a signal to the process this test started
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        let _ = std::fs::remove_file(&log);
    }

    #[tokio::test]
    async fn detach_needs_background() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let out = Bash.run(json!({"command": "true", "detach": true}), &c).await;
        assert!(out.is_error && out.content.contains("background"), "{}", out.content);
    }
}

// every test here probes pids, so the module is Unix only
#[cfg(all(test, unix))]
mod sweep_tests {
    use super::*;
    use std::fs;

    fn mk(root: &std::path::Path, name: &str) -> std::path::PathBuf {
        let d = root.join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("f.txt"), "x").unwrap();
        d
    }

    /// No least age: the tests make their folders just before the sweep.
    const NOW: std::time::Duration = std::time::Duration::ZERO;

    #[test]
    fn a_folder_touched_lately_stays_though_its_pid_is_gone() {
        let t = tempfile::tempdir().unwrap();
        let fresh = mk(t.path(), "nexo-tmp-4000005-main");
        sweep_tmp_in(t.path(), &|_| false, std::time::Duration::from_secs(3600));
        assert!(fresh.join("f.txt").is_file());
        sweep_tmp_in(t.path(), &|_| false, NOW);
        assert!(!fresh.exists());
    }

    // the sweep is by pid on Unix; elsewhere by age, so these run on Unix only
    #[test]
    fn dead_goes_live_and_own_stay() {
        let t = tempfile::tempdir().unwrap();
        let dead = mk(t.path(), "nexo-tmp-4000001-main");
        let live = mk(t.path(), "nexo-tmp-4000002-main");
        let own = mk(t.path(), &format!("nexo-tmp-{}-main", std::process::id()));
        sweep_tmp_in(t.path(), &|p| p == 4000002 || p == std::process::id() as i32, NOW);
        assert!(!dead.exists());
        assert!(live.join("f.txt").is_file());
        assert!(own.join("f.txt").is_file());
        // even when the probe says the own pid is dead
        sweep_tmp_in(t.path(), &|_| false, NOW);
        assert!(own.join("f.txt").is_file());
        assert!(!live.exists());
    }

    #[test]
    fn other_names_links_and_files_stay() {
        let t = tempfile::tempdir().unwrap();
        let keep: Vec<_> =
            ["nexo-tmp-abc-x", "nexo-tmpfoo", "nexo-build-123", "nexo-detached", "nexo-tmp-5-", "nexo-tmp-0-x"]
                .iter()
                .map(|n| mk(t.path(), n))
                .collect();
        let target = mk(t.path(), "target-dir");
        std::os::unix::fs::symlink(&target, t.path().join("nexo-tmp-4000003-x")).unwrap();
        fs::write(t.path().join("nexo-tmp-4000004-x"), "file").unwrap();
        sweep_tmp_in(t.path(), &|_| false, NOW);
        for k in keep {
            assert!(k.join("f.txt").is_file(), "{k:?}");
        }
        assert!(target.join("f.txt").is_file());
        assert!(fs::symlink_metadata(t.path().join("nexo-tmp-4000003-x")).unwrap().is_symlink());
        assert!(t.path().join("nexo-tmp-4000004-x").is_file());
    }

    #[test]
    fn at_most_200_folders_are_removed() {
        let t = tempfile::tempdir().unwrap();
        for i in 0..230 {
            mk(t.path(), &format!("nexo-tmp-{}-x", 4100000 + i));
        }
        sweep_tmp_in(t.path(), &|_| false, NOW);
        let left = fs::read_dir(t.path()).unwrap().count();
        assert_eq!(left, 30);
    }

    #[test]
    fn real_probe_sees_this_process() {
        assert!(pid_alive(std::process::id() as i32));
    }
}
