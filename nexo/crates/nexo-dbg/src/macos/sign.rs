//! The signed copy `run` debugs: a program that allows a debugger, made from the user's own file.

use super::*;

/// The program that runs under the debugger: a signed copy of a file the user owns, or the file itself.
pub(super) struct Prepared {
    pub(super) run: PathBuf,
    pub(super) copy: Option<PathBuf>,
    pub(super) note: String,
}

const ENTITLEMENTS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>com.apple.security.get-task-allow</key><true/></dict></plist>\n";

/// Copy `exe` beside itself (so `@executable_path` libraries still load), or into the session folder when its folder
/// is not writable, and sign the copy ad hoc with get-task-allow alone. A file of another user is not copied.
pub(super) fn prepare(exe: &Path) -> Result<Prepared> {
    let meta = std::fs::metadata(exe).map_err(|e| os_err(&format!("cannot read {}", exe.display()), e))?;
    // SAFETY: no arguments, no failure.
    if meta.uid() != unsafe { libc::geteuid() } {
        return Ok(Prepared {
            run: exe.to_path_buf(),
            copy: None,
            note: format!(
                "{} belongs to another user, so nexo runs it as it is, unsigned: macOS lets nexo debug it only if its own signature allows debugging (get-task-allow)",
                exe.display()
            ),
        });
    }
    let data = std::fs::read(exe).map_err(|e| os_err(&format!("cannot read {}", exe.display()), e))?;
    let name = exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "program".into());
    // one name per session: several sessions in one process may run the same program at once. The numbers have a
    // fixed width because the kernel copies the path the program is started from onto the top of its stack: a name
    // one digit longer would move the stack, and with randomisation off a run must find it where the last one did.
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let copy_name = format!(".{name}.nexo-dbg-{:010}-{n:010}", std::process::id());
    let beside = exe.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let signed = nexo_bin::adhoc_sign(&data, &name, &[nexo_bin::GET_TASK_ALLOW]);
    let bytes = signed.as_deref().unwrap_or(&data);
    let mut copy = None;
    for dir in [beside.to_path_buf(), crate::state_dir()] {
        let _ = std::fs::create_dir_all(&dir);
        sweep(&dir, &name);
        let at = dir.join(&copy_name);
        let _ = std::fs::remove_file(&at);
        let made = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&at)
            .and_then(|mut f| f.write_all(bytes));
        match made {
            Ok(()) => {
                copy = Some(at);
                break;
            }
            Err(_) => {
                let _ = std::fs::remove_file(&at);
            }
        }
    }
    let Some(copy) = copy else {
        return fail(format!(
            "cannot write a signed copy of {} beside it or in {}: macOS lets nexo debug only programs signed to allow it",
            exe.display(),
            crate::state_dir().display()
        ));
    };
    if let Err(e) = signed {
        // the system's own signer is the fallback: it ships with every macOS
        let plist = copy.with_file_name(format!("{copy_name}.plist"));
        let ran = std::fs::write(&plist, ENTITLEMENTS).and_then(|()| {
            std::process::Command::new("/usr/bin/codesign")
                .args(["-s", "-", "-f", "--entitlements"])
                .arg(&plist)
                .arg(&copy)
                .output()
        });
        let _ = std::fs::remove_file(&plist);
        let ok = ran.as_ref().is_ok_and(|o| o.status.success());
        if !ok {
            let _ = std::fs::remove_file(&copy);
            let why = match ran {
                Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                Err(e) => e.to_string(),
            };
            return fail(format!(
                "cannot sign a copy of {} to allow debugging ({e}; /usr/bin/codesign: {why}); macOS lets nexo debug only programs signed to allow it",
                exe.display()
            ));
        }
    }
    Ok(Prepared {
        run: copy.clone(),
        note: format!(
            "macOS: this runs {}, a copy of {} signed ad hoc with get-task-allow so macOS lets nexo debug it; nexo signs copies of your own files only, leaves the original as it is, and removes the copy when the session ends",
            copy.display(),
            exe.display()
        ),
        copy: Some(copy),
    })
}

/// Remove copies of `name` that sessions which no longer run left in `dir` (a debugger that was killed).
fn sweep(dir: &Path, name: &str) {
    let lead = format!(".{name}.nexo-dbg-");
    let Ok(list) = std::fs::read_dir(dir) else { return };
    for e in list.flatten() {
        let file = e.file_name();
        let pid = file.to_str().and_then(|f| f.strip_prefix(&lead)).and_then(|p| p.split('-').next());
        let Some(pid) = pid.and_then(|p| p.parse::<i32>().ok()) else {
            continue;
        };
        if !alive(pid) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}
