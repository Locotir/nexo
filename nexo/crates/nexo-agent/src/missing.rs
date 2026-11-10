//! Which common commands are not installed where commands run, as one line for the Project block.
//! Probed once per working directory for the life of the process, then reused byte for byte, so
//! the cached prompt prefix does not change from one request to the next.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The commands a model reaches for. Order here is the order of the line.
pub const COMMANDS: [&str; 18] = [
    "git", "curl", "wget", "python3", "python", "pip", "node", "npm", "make", "gcc", "file", "xxd", "jq", "sudo", "ps",
    "pgrep", "unzip", "time",
];

/// The names of `list` that `probe_output` (one missing name per line) reports, in list order.
pub fn missing<'a>(list: &[&'a str], probe_output: &str) -> Vec<&'a str> {
    let gone: Vec<&str> = probe_output.lines().map(str::trim).collect();
    list.iter().copied().filter(|c| gone.contains(c)).collect()
}

/// `Not installed here: curl, file, xxd.`, or an empty string when nothing is missing.
pub fn line(missing: &[&str]) -> String {
    if missing.is_empty() {
        return String::new();
    }
    format!("Not installed here: {}.", missing.join(", "))
}

/// Runs one `sh -c` loop in `cwd` that prints each name of `list` that `command -v` does not
/// find. `None` on any error or after 2 seconds, and on systems without a Unix shell.
#[cfg(unix)]
pub fn probe(list: &[&str], cwd: &Path) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let script = r#"for c in "$@"; do command -v "$c" >/dev/null 2>&1 || printf '%s\n' "$c"; done"#;
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(script)
        .arg("sh")
        .args(list)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) if st.success() => return reader.join().ok(),
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if start.elapsed() >= Duration::from_secs(2) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

#[cfg(not(unix))]
pub fn probe(_list: &[&str], _cwd: &Path) -> Option<String> {
    None
}

/// The line for `cwd`: probed on first use, the same text on every later call.
pub fn session_line(cwd: &Path) -> String {
    static KEPT: Mutex<Option<HashMap<PathBuf, String>>> = Mutex::new(None);
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    kept.get_or_insert_with(HashMap::new)
        .entry(cwd.to_path_buf())
        .or_insert_with(|| probe(&COMMANDS, cwd).map(|o| line(&missing(&COMMANDS, &o))).unwrap_or_default())
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_keeps_list_order_and_ignores_unknown_names() {
        let got = missing(&COMMANDS, "xxd\ncurl\n\nnot-in-list\nfile\n");
        assert_eq!(got, ["curl", "file", "xxd"]);
        assert!(missing(&COMMANDS, "").is_empty());
    }

    #[test]
    fn the_line_text_and_order() {
        assert_eq!(line(&["curl", "file", "xxd"]), "Not installed here: curl, file, xxd.");
        assert_eq!(line(&["git"]), "Not installed here: git.");
        assert_eq!(line(&[]), "");
    }

    #[test]
    fn the_list_is_the_agreed_one() {
        assert_eq!(
            COMMANDS.join(","),
            "git,curl,wget,python3,python,pip,node,npm,make,gcc,file,xxd,jq,sudo,ps,pgrep,unzip,time"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_real_probe_finds_the_absent_and_not_the_present() {
        let d = tempfile::tempdir().unwrap();
        let out = probe(&["nexo-no-such-cmd-xyz", "sh"], d.path()).expect("probe ran");
        assert_eq!(missing(&["nexo-no-such-cmd-xyz", "sh"], &out), ["nexo-no-such-cmd-xyz"]);
    }

    #[test]
    fn the_session_keeps_one_answer() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(session_line(d.path()), session_line(d.path()));
    }
}
