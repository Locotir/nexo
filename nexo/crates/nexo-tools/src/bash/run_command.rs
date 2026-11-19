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
    let (end, held) = run_command_logged(command, cwd, sandbox, timeout, cancel, tmp, on_line, None).await;
    if let Some(held) = held {
        // this caller wants the call to end with the shell, and nothing left reading
        for r in held.readers {
            r.abort();
        }
    }
    end
}

/// What a command left running behind it: the shell exited, but a process it started holds the output
/// pipes. The readers are kept, so the process keeps its output and is not killed by SIGPIPE on its
/// next write.
pub(super) struct Held {
    /// The group id of the shell's session: the shell's pid, as it made the session itself.
    pub(super) pgid: u32,
    /// The lines the readers send, until every holder of the pipes has closed them.
    pub(super) rx: mpsc::UnboundedReceiver<String>,
    pub(super) readers: Vec<tokio::task::JoinHandle<()>>,
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
) -> (End, Option<Held>) {
    let mut cmd = match build_command(command, cwd, sandbox, false) {
        Ok(c) => c,
        Err(e) => return (End::Failed(e), None),
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
        Err(e) => return (End::Failed(e.to_string()), None),
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
                    None => End::Signalled(signal_of(&s)),
                },
                Err(e) => End::Lost(e.to_string()),
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
    // the group is gone, so nothing holds the pipes: the readers are only behind on a big output.
    // Keep reading until they close, fall silent for DRAIN, or the call is cancelled or times out
    // (a process that left the group with its own setsid may still write to the pipes).
    #[cfg(unix)]
    if open && matches!(end, End::Exited(_)) {
        if let Some(pid) = pid {
            // SAFETY: signal 0 only checks that the process group exists; ESRCH means it does not.
            let group_gone = unsafe { libc::kill(-(pid as i32), 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if group_gone {
                while open {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        _ = &mut deadline => break,
                        line = tokio::time::timeout(DRAIN, rx.recv()) => match line {
                            Ok(Some(l)) => on_line(&l),
                            Ok(None) => open = false,
                            Err(_) => break,
                        },
                    }
                }
            }
        }
    }
    // the shell exited and a process it started still holds the pipes: the readers go on, for a job
    // (process groups are a unix thing: elsewhere the pid is only the shell's, which has exited and may be reused)
    let pgid = pid.filter(|_| cfg!(unix) && matches!(end, End::Exited(_)) && open);
    if pgid.is_none() {
        for r in &readers {
            r.abort();
        }
    }
    // lines already read when the readers stopped (for a held command: the ones queued now)
    while let Ok(l) = rx.try_recv() {
        on_line(&l);
    }
    match pgid {
        Some(pgid) => (end, Some(Held { pgid, rx, readers })),
        None => (end, None),
    }
}

/// The signal that ended a process, 0 where the system does not say.
fn signal_of(status: &std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        std::os::unix::process::ExitStatusExt::signal(status).unwrap_or(0)
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        0
    }
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
            text = match no_cwd(cwd) {
                Some(missing) => format!("{missing}; the command did not run"),
                None => format!("cannot run the command: {e}"),
            };
            (None, false)
        }
        End::Lost(e) => {
            text =
                format!("{text}\n[the command started, but waiting for it failed: {e}; its exit status is not known]");
            (None, false)
        }
        End::Signalled(_) | End::Cancelled => (None, false),
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
