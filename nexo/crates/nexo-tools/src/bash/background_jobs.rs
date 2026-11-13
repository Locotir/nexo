use super::*;

/// Starts `command` as a job of the session and returns its number at once.
pub(super) fn background(command: &str, ctx: &ToolContext, sandbox: nexo_core::sandbox::SandboxSettings) -> ToolOutput {
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
pub(super) fn detached(command: &str, ctx: &ToolContext, sandbox: &nexo_core::sandbox::SandboxSettings) -> ToolOutput {
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
