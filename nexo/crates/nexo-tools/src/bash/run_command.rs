use super::*;

/// Runs `command` in `cwd`, each line of its output (stdout and stderr) to
/// `on_line`, until it exits, `timeout` passes or `cancel` fires. The call ends
/// when the shell exits, even if something it left running still holds the pipes.
pub(super) async fn run_command(
    command: &str,
    cwd: &std::path::Path,
    sandbox: &nexo_core::sandbox::SandboxSettings,
    timeout: Option<Duration>,
    cancel: &CancellationToken,
    tmp: Option<&std::path::Path>,
    on_line: impl FnMut(&str),
) -> End {
    run_command_logged(command, cwd, sandbox, timeout, cancel, tmp, on_line, None).await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_command_logged(
    command: &str,
    cwd: &std::path::Path,
    sandbox: &nexo_core::sandbox::SandboxSettings,
    timeout: Option<Duration>,
    cancel: &CancellationToken,
    tmp: Option<&std::path::Path>,
    mut on_line: impl FnMut(&str),
    log: Option<RawLog>,
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
    // A session of its own (which is also a group of its own, so `kill_group` still reaches every
    // descendant): the command has no controlling terminal, so it cannot open /dev/tty and inject
    // keystrokes (TIOCSTI) into the one the user types in. `process_group` must not be set as well:
    // std runs setpgid first, and setsid then fails with EPERM for a group leader.
    #[cfg(unix)]
    // SAFETY: runs in the forked child before exec and only calls `setsid`, which is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| match libc::setsid() {
            -1 => Err(std::io::Error::last_os_error()),
            _ => Ok(()),
        });
    }
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
        readers.push(tokio::spawn(read_lines_logged(out, tx.clone(), log.clone())));
    }
    if let Some(err) = child.stderr.take() {
        readers.push(tokio::spawn(read_lines_logged(err, tx.clone(), log.clone())));
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
pub fn kill_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // SAFETY: a plain signal to a process group this process created.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    if let Some(pid) = pid {
        use std::os::windows::process::CommandExt;
        // no window for the helper: a nexo started without a console would flash one. It is waited
        // for on purpose, as the tree must be walked before the shell under it is killed.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    #[cfg(not(any(unix, windows)))]
    let _ = pid;
}
