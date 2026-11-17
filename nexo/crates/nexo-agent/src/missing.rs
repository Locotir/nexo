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

/// Runs one loop in `cwd`, in the shell the model's commands run in (`sh`, or Git Bash on Windows), that prints
/// each name of `list` that `command -v` does not find. Asking that shell and not the Windows PATH matters:
/// Git Bash finds `file`, `xxd` and `ps` in its own `usr/bin`, which the PATH does not list. `None` on any
/// error, after 2 seconds, and under PowerShell (its aliases make `curl` and `ps` mean something else).
pub fn probe(list: &[&str], cwd: &Path) -> Option<String> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let shell = nexo_core::shell::shell();
    if shell.kind == "powershell" {
        return None;
    }
    // The names go into the script itself (`args_for` takes one string), so only plain program names are allowed.
    if list.iter().any(|c| c.is_empty() || !c.chars().all(|ch| ch.is_ascii_alphanumeric() || "._+-".contains(ch))) {
        return None;
    }
    // On a Mac the programs of the Command Line Tools are shims in /usr/bin that find nothing without the tools:
    // they are named `shim:<name>` and `settle_shims` decides what they are.
    let script = format!(
        r#"mac=0; [ "$(uname)" = Darwin ] && mac=1
for c in {}; do
  p=$(command -v "$c" 2>/dev/null) || { printf '%s\n' "$c"; continue; }
  [ $mac = 1 ] && [ "$p" = "/usr/bin/$c" ] && printf 'shim:%s\n' "$c"
done; true"#,
        list.join(" ")
    );
    let mut child = shell
        .command(&script)
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
            Ok(Some(st)) if st.success() => {
                return reader.join().ok().map(|o| settle_shims(&o, cfg!(target_os = "macos") && !developer_tools()))
            }
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

/// The programs a Mac ships as shims for the Command Line Tools.
const SHIMS: [&str; 5] = ["git", "python3", "make", "gcc", "clang"];

/// Whether `xcode-select -p` finds a developer folder (the real programs behind the shims).
fn developer_tools() -> bool {
    std::process::Command::new("xcode-select")
        .arg("-p")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true)
}

/// The probe output with each `shim:<name>` line turned into `<name>` (missing) when the shims have no tools behind
/// them, and dropped otherwise.
fn settle_shims(out: &str, shims_empty: bool) -> String {
    let mut text = String::new();
    for l in out.lines() {
        match l.strip_prefix("shim:") {
            Some(n) if shims_empty && SHIMS.contains(&n) => text.push_str(&format!("{n}\n")),
            Some(_) => {}
            None => text.push_str(&format!("{l}\n")),
        }
    }
    text
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
    fn shims_are_missing_only_when_the_developer_tools_are_not_there() {
        let out = "curl\nshim:git\nshim:make\nshim:ls\n";
        assert_eq!(missing(&COMMANDS, &settle_shims(out, true)), ["git", "curl", "make"]);
        assert_eq!(missing(&COMMANDS, &settle_shims(out, false)), ["curl"]);
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

    #[test]
    fn the_real_probe_finds_the_absent_and_not_the_present() {
        // PowerShell is the one shell the probe leaves alone, so there is nothing to ask there.
        if nexo_core::shell::shell().kind == "powershell" {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let out = probe(&["nexo-no-such-cmd-xyz", "sh"], d.path()).expect("probe ran");
        assert_eq!(missing(&["nexo-no-such-cmd-xyz", "sh"], &out), ["nexo-no-such-cmd-xyz"]);
    }

    #[test]
    fn a_name_that_is_not_a_plain_program_name_is_not_put_in_the_script() {
        // The names are pasted into the shell script, so one with shell syntax must never reach it.
        let d = tempfile::tempdir().unwrap();
        assert_eq!(probe(&["git; echo gone"], d.path()), None);
        assert_eq!(probe(&[""], d.path()), None);
    }

    #[test]
    fn the_session_keeps_one_answer() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(session_line(d.path()), session_line(d.path()));
    }
}
