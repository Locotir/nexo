//! What the map asks of the system: the packages the person installed by hand, and the package that owns a program.
//! Each call has a time limit and gives nothing when the tool is missing, slow or fails.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// The package tools the map knows, by the program that is looked for in `PATH`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pacman,
    Dpkg,
    Rpm,
}

/// How long the list of installed packages may take, once, when the index is built.
const LIST_LIMIT: Duration = Duration::from_secs(2);
/// How long the owner of one program may take.
const OWNER_LIMIT: Duration = Duration::from_millis(300);

/// `program args`, its output when it ends in `limit` with success. A command past the limit is killed.
fn run(program: &str, args: &[&str], limit: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = out.read_to_end(&mut bytes);
        let _ = tx.send(bytes);
    });
    match rx.recv_timeout(limit) {
        Ok(bytes) => child.wait().ok()?.success().then(|| String::from_utf8_lossy(&bytes).into_owned()),
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

/// Whether `name` is a file in one of `dirs`.
fn found_in(dirs: &[PathBuf], name: &str) -> bool {
    dirs.iter().any(|d| d.join(name).is_file())
}

/// The package tool of this system, when one is there (Linux only: macOS and Windows have none the map uses).
pub fn tool(path_dirs: &[PathBuf]) -> Option<Tool> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    [("pacman", Tool::Pacman), ("dpkg", Tool::Dpkg), ("rpm", Tool::Rpm)]
        .into_iter()
        .find(|(name, _)| found_in(path_dirs, name))
        .map(|(_, t)| t)
}

/// The names of the packages the person installed by hand (`pacman -Qqe`, `apt-mark showmanual`,
/// `dnf repoquery --userinstalled`), lowercase, or None where no such tool answers in time.
pub fn installed(path_dirs: &[PathBuf]) -> Option<Vec<String>> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let text = if found_in(path_dirs, "pacman") {
        run("pacman", &["-Qqe"], LIST_LIMIT)?
    } else if found_in(path_dirs, "apt-mark") {
        run("apt-mark", &["showmanual"], LIST_LIMIT)?
    } else if found_in(path_dirs, "dnf") {
        run("dnf", &["repoquery", "--userinstalled", "--qf", "%{name}"], LIST_LIMIT)?
    } else {
        return None;
    };
    Some(text.lines().map(|l| l.trim().to_lowercase()).filter(|l| !l.is_empty()).collect())
}

/// The package that owns `program` with its version, as the tool says it (`quickshell 0.3.2-1.1`).
pub fn owner(tool: Tool, program: &Path) -> Option<String> {
    let path = program.to_str()?;
    let text = match tool {
        Tool::Pacman => {
            let out = run("pacman", &["-Qo", path], OWNER_LIMIT)?;
            out.split_once(" is owned by ")?.1.trim().to_string()
        }
        Tool::Dpkg => {
            let out = run("dpkg", &["-S", path], OWNER_LIMIT)?;
            let name = out.lines().next()?.split_once(':')?.0.trim().to_string();
            let version = run("dpkg-query", &["-W", "-f=${Version}", &name], OWNER_LIMIT).unwrap_or_default();
            format!("{name} {}", version.trim()).trim().to_string()
        }
        Tool::Rpm => {
            run("rpm", &["-qf", "--qf", "%{NAME} %{VERSION}-%{RELEASE}", path], OWNER_LIMIT)?.trim().to_string()
        }
    };
    (!text.is_empty()).then_some(text)
}
