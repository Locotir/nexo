//! `bash`: a shell command in the working directory.

use std::collections::VecDeque;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use nexo_core::{Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSpec};
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
                "Run a shell command ({}) in the working directory and return its output \
                 (stdout and stderr interleaved) and exit code. Use it to build, test, run git, \
                 and for anything the other tools do not cover; use `read`, `grep`, `glob` and \
                 `ls` instead of cat, grep, find and ls. The command is killed after `timeout` \
                 seconds (default 120, at most 600); long output is cut in the middle, keeping \
                 the head and the tail. Avoid interactive commands. With `background: true` the \
                 command runs as a job while you go on working (servers, watchers, long builds and \
                 test suites): the call returns its job id at once, there is no timeout, you are told \
                 when it ends, and the `jobs` tool reads its output or stops it.",
                match nexo_core::shell::shell().kind {
                    "powershell" => "Windows PowerShell: write PowerShell, not sh",
                    "bash" => "Git Bash on Windows: POSIX shell, Windows paths work as C:/...",
                    _ => "`sh -c`",
                }
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The command line to run."},
                    "timeout": {"type": "integer", "minimum": 1, "maximum": 600, "description": "Seconds before the command is killed. Default 120."},
                    "description": {"type": "string", "description": "A few words on what the command does (shown to the person)."},
                    "background": {"type": "boolean", "description": "Run it as a background job and return at once. Default false."}
                },
                "required": ["command"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Exec
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
        if bool_arg(&args, "background") {
            return background(command, ctx);
        }
        let timeout =
            Duration::from_secs(int_arg(&args, "timeout").unwrap_or(DEFAULT_TIMEOUT_S).clamp(1, MAX_TIMEOUT_S));
        let mut output = Capture::new();
        let end = run_command(command, &ctx.cwd, Some(timeout), &ctx.cancel, |l| {
            ctx.report(l);
            output.push(l);
        })
        .await;
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
        ToolOutput { content, is_error, display }
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

/// Starts `command` as a job of the session and returns its number at once.
fn background(command: &str, ctx: &ToolContext) -> ToolOutput {
    let job = ctx.jobs.start(JobKind::Shell, command);
    let id = job.id();
    let (line, cwd) = (command.to_string(), ctx.cwd.clone());
    tokio::spawn(async move {
        let cancel = job.cancel();
        let end = run_command(&line, &cwd, None, &cancel, |l| job.line(l)).await;
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
        "Started background job {id}: `{}`. It runs on while you work, with no timeout; you will be told \
         when it ends. Read its output with jobs {{\"action\": \"output\", \"id\": {id}}} (add \"wait\" \
         to wait for it), stop it with {{\"action\": \"kill\", \"id\": {id}}}.",
        one_line(command, 100)
    );
    ToolOutput::ok(content)
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

/// Runs `command` in `cwd`, each line of its output (stdout and stderr) to
/// `on_line`, until it exits, `timeout` passes or `cancel` fires. The call ends
/// when the shell exits, even if something it left running still holds the pipes.
async fn run_command(
    command: &str,
    cwd: &std::path::Path,
    timeout: Option<Duration>,
    cancel: &CancellationToken,
    mut on_line: impl FnMut(&str),
) -> End {
    let shell = nexo_core::shell::shell();
    let mut cmd = tokio::process::Command::new(&shell.program);
    cmd.args(shell.args_for(command))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let run = Bash.run(json!({"command": "sleep 30", "timeout": 60}), &c2);
        let out = tokio::join!(run, async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel.cancel();
        })
        .0;
        assert!(t.elapsed() < Duration::from_secs(5));
        assert!(out.content.contains("[cancelled]"), "{}", out.content);
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
}
