use super::*;

/// Kills the command's process group when the call is dropped before the command ended (the
/// person interrupted the turn): `kill_on_drop` alone ends the shell, not what it started.
pub(super) struct GroupGuard {
    pub(super) pid: Option<u32>,
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        // on Windows this is `taskkill /T`: `kill_on_drop` alone would leave the tree running
        kill_group(self.pid.take());
    }
}

/// The shell command for `command`, inside the sandbox when it is on (`detach`: one that is not
/// tied to this process's life).
pub(super) fn build_command(
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

pub(super) fn add_nexo_path(
    cmd: &mut tokio::process::Command,
    path: Option<&std::ffi::OsStr>,
    exe: Option<&std::path::Path>,
) {
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
pub(super) fn others_write(dir: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(dir).map_or(true, |m| m.permissions().mode() & 0o022 != 0)
}

/// Windows keeps who may write in a list this does not read: the folder is taken as the person's own.
#[cfg(not(unix))]
pub(super) fn others_write(_dir: &std::path::Path) -> bool {
    false
}
