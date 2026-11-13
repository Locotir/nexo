//! The shell commands run in: `sh -c` on Unix; on Windows, Git Bash when it is
//! installed (the model writes POSIX shell, and Git for Windows ships `bash`),
//! else PowerShell. Every command nexo runs (the `bash` tool, hooks,
//! `api_key_cmd`) goes through here, so they all agree.

use std::path::PathBuf;
use std::sync::OnceLock;

/// The shell: its program, the arguments before the command, and the name the model is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// `sh`, `bash`, `powershell`: what the `bash` tool's description says it runs in.
    pub kind: &'static str,
}

/// Put before a PowerShell command: piped, PowerShell and the programs it runs write in the console's
/// code page (850, 437, ...), while nexo reads UTF-8, so `José` would come back as `Jos\u{fffd}`.
/// `try` keeps it quiet where there is no console to set.
pub const UTF8_OUTPUT: &str = "try { [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false) } catch {}; \
                               $OutputEncoding = [Text.UTF8Encoding]::new($false); ";

impl Shell {
    /// A `std` command that runs `command` in this shell.
    pub fn command(&self, command: &str) -> std::process::Command {
        let mut c = std::process::Command::new(&self.program);
        c.args(self.args_for(command));
        c
    }

    /// The name people know it by, from its program: `sh`, `bash`, `pwsh`, `powershell`.
    pub fn label(&self) -> String {
        // both separators, so a Windows path reads the same wherever it is looked at
        let path = self.program.to_string_lossy().to_ascii_lowercase();
        let file = path.rsplit(['/', '\\']).next().unwrap_or_default();
        let stem = file.strip_suffix(".exe").unwrap_or(file);
        if stem.is_empty() {
            self.kind.to_string()
        } else {
            stem.to_string()
        }
    }

    /// The arguments for `command` (for a `tokio::process::Command` on `program`).
    pub fn args_for(&self, command: &str) -> Vec<String> {
        let mut a = self.args.clone();
        if self.kind == "powershell" {
            a.push(format!("{UTF8_OUTPUT}{command}"));
        } else {
            a.push(command.to_string());
        }
        a
    }
}

/// Whether a command that ended with the exit `code` did not run to completion in the sense that matters:
/// the shell could not find it (127) or run it (126), a signal killed it (the shell reports 128 + the
/// signal, 129 to 192), or, on Windows, it crashed (an NTSTATUS such as 0xC0000005 reads as a negative
/// `i32`). Any other non-zero code is the program's own answer by design (`cmp`, `diff`, `grep`, `test`,
/// `false`, a test runner): the command ran, and the code is information, not a failure.
pub fn exit_is_failure(code: i32) -> bool {
    matches!(code, 126 | 127 | 129..=192) || code < 0
}

/// The exit code a shell result ends with (`[exit code N]`, written by the `bash` tool), if it has one.
pub fn exit_code_in(text: &str) -> Option<i32> {
    text.lines().rev().map(str::trim).find_map(|l| l.strip_prefix("[exit code ")?.strip_suffix(']')?.parse().ok())
}

/// `name` on the PATH, with the extensions Windows runs (`.exe`, `.cmd`, `.bat`).
pub fn find_program(name: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) { &["", ".exe", ".cmd", ".bat"] } else { &[""] };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|d| exts.iter().map(|e| d.join(format!("{name}{e}"))).find(|p| p.is_file()))
}

/// The program to start for `name`: on Windows a bare name is looked up on the PATH with the
/// extensions Windows runs, since a process started by a bare name only finds an `.exe` there and
/// `npx`, `uvx` or `code` are `.cmd` files; elsewhere, and for a path, `name` itself.
pub fn program(name: &str) -> PathBuf {
    if cfg!(windows) && !name.contains(['/', '\\']) {
        find_program(name).unwrap_or_else(|| name.into())
    } else {
        name.into()
    }
}

/// The editors to open a file in, each as its program and arguments, tried in order: `$VISUAL`,
/// else `$EDITOR`, else Notepad on Windows and `nano` then `vi` elsewhere.
pub fn editors() -> Vec<Vec<String>> {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    editors_from(get("VISUAL").or_else(|| get("EDITOR")), cfg!(windows), |p| p.is_file())
}

/// The choice itself, with the variable and the file check given (tests).
pub fn editors_from(
    chosen: Option<String>,
    windows: bool,
    is_file: impl Fn(&std::path::Path) -> bool,
) -> Vec<Vec<String>> {
    match chosen.as_deref().map(str::trim) {
        // a path with spaces and no quotes (`C:\Program Files\...\editor.exe`) is the program whole
        Some(c) if is_file(std::path::Path::new(c)) => vec![vec![c.to_string()]],
        Some(c) => {
            vec![split_words(c, windows)].into_iter().filter(|w| w.first().is_some_and(|p| !p.is_empty())).collect()
        }
        None if windows => vec![vec!["notepad".into()]],
        None => vec![vec!["nano".into()], vec!["vi".into()]],
    }
}

/// A command line split into words: quotes keep spaces in a word (double ones, and single ones off
/// Windows). Off Windows a backslash keeps the next character as it is (in double quotes only before
/// `"` or `\`); on Windows it is the separator of paths and stays.
pub fn split_words(s: &str, windows: bool) -> Vec<String> {
    let (mut words, mut word, mut open) = (Vec::new(), String::new(), false);
    let mut quote: Option<char> = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('\''), c) => word.push(c),
            (_, '\\') if !windows => match chars.peek() {
                Some(&n) if quote.is_none() || n == '"' || n == '\\' => {
                    word.push(n);
                    chars.next();
                }
                _ => word.push('\\'),
            },
            (None, '"') => quote = Some('"'),
            (None, '\'') if !windows => quote = Some('\''),
            (None, c) if c.is_whitespace() => {
                if open {
                    words.push(std::mem::take(&mut word));
                }
                open = false;
                continue;
            }
            (_, c) => word.push(c),
        }
        open = true;
    }
    if open {
        words.push(word);
    }
    words
}

/// A path as nexo shows it (and as rules and globs are written): `/` between the parts,
/// on Windows too.
pub fn slashed(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s.into_owned()
    }
}

/// The shell for this system, found once.
pub fn shell() -> &'static Shell {
    static S: OnceLock<Shell> = OnceLock::new();
    S.get_or_init(|| pick(cfg!(windows), find_program, |p| p.is_file()))
}

/// The choice itself, with the PATH lookup and the file check given (tests).
pub fn pick(
    windows: bool,
    find: impl Fn(&str) -> Option<PathBuf>,
    is_file: impl Fn(&std::path::Path) -> bool,
) -> Shell {
    if !windows {
        return Shell { program: "sh".into(), args: vec!["-c".into()], kind: "sh" };
    }
    // Git for Windows: `bash` on the PATH, or at its usual place, for all users (the program folders,
    // on whatever drive) or for one (under the user's local app data)
    let usual = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|d| PathBuf::from(d).join("Git\\bin\\bash.exe"))
        .chain(std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs\\Git\\bin\\bash.exe")))
        .chain(
            ["C:\\Program Files\\Git\\bin\\bash.exe", "C:\\Program Files (x86)\\Git\\bin\\bash.exe"].map(PathBuf::from),
        );
    let git_bash = find("bash")
        .filter(|p| !p.to_string_lossy().to_ascii_lowercase().contains("system32"))
        .or_else(|| usual.into_iter().find(|p| is_file(p)));
    if let Some(b) = git_bash {
        return Shell { program: b, args: vec!["-c".into()], kind: "bash" };
    }
    let ps = find("pwsh").unwrap_or_else(|| PathBuf::from("powershell"));
    Shell {
        program: ps,
        args: vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into()],
        kind: "powershell",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_runs_sh_and_windows_prefers_git_bash_then_powershell() {
        let none = |_: &std::path::Path| false;
        assert_eq!(pick(false, |_| None, none).kind, "sh");
        let git =
            pick(true, |n| (n == "bash").then(|| PathBuf::from("C:\\Program Files\\Git\\usr\\bin\\bash.exe")), none);
        assert_eq!((git.kind, git.args.as_slice()), ("bash", &["-c".to_string()][..]));
        // the WSL launcher in System32 is not Git Bash
        let wsl = pick(true, |n| (n == "bash").then(|| PathBuf::from("C:\\Windows\\System32\\bash.exe")), none);
        assert_eq!(wsl.kind, "powershell");
        // Git Bash not on the PATH but at its usual place
        let usual = pick(true, |_| None, |p| p.to_string_lossy().ends_with("Git\\bin\\bash.exe"));
        assert_eq!(usual.kind, "bash");
        let pwsh = pick(true, |n| (n == "pwsh").then(|| PathBuf::from("C:\\pwsh\\pwsh.exe")), none);
        assert_eq!(pwsh.program, PathBuf::from("C:\\pwsh\\pwsh.exe"));
        // the name shown is the program's own, not the tool's
        assert_eq!(pwsh.label(), "pwsh");
        assert_eq!(pick(true, |_| None, none).label(), "powershell");
        assert_eq!(git.label(), "bash");
        assert_eq!(pick(false, |_| None, none).label(), "sh");
        // PowerShell writes to a pipe in the console's code page unless told to write UTF-8
        let last = pwsh.args_for("Get-Date").pop().unwrap();
        assert!(last.starts_with(UTF8_OUTPUT) && last.ends_with("Get-Date"), "{last}");
        let cmd = pwsh.command("Get-Date");
        assert_eq!(cmd.get_args().last().unwrap().to_string_lossy(), last);
        assert_eq!(git.args_for("ls").last().unwrap(), "ls", "bash is left alone");
    }

    #[test]
    fn an_editor_command_keeps_windows_paths_and_quoted_words() {
        let w = |s: &str, win: bool| split_words(s, win);
        assert_eq!(w("code --wait", false), ["code", "--wait"]);
        assert_eq!(w("  emacs   -nw  ", false), ["emacs", "-nw"]);
        // on Windows a backslash is the separator of paths and quotes keep the spaces
        assert_eq!(
            w("\"C:\\Program Files\\Notepad++\\notepad++.exe\" -multiInst", true),
            ["C:\\Program Files\\Notepad++\\notepad++.exe", "-multiInst"]
        );
        assert_eq!(w("C:\\tools\\hx.exe", true), ["C:\\tools\\hx.exe"]);
        // elsewhere the shell's rules: a backslash keeps a space, single quotes keep everything
        assert_eq!(w("my\\ editor 'a b' \"c \\\" d\" \"e\\f\"", false), ["my editor", "a b", "c \" d", "e\\f"]);
        assert_eq!(w("vi \"\"", false), ["vi", ""]);
        assert!(w("   ", false).is_empty());
    }

    #[test]
    fn the_editor_is_the_one_set_else_notepad_on_windows_and_nano_then_vi_elsewhere() {
        let none = |_: &std::path::Path| false;
        assert_eq!(editors_from(None, true, none), [["notepad"]]);
        assert_eq!(editors_from(None, false, none), [["nano"], ["vi"]]);
        assert_eq!(editors_from(Some("code --wait".into()), true, none), [["code", "--wait"]]);
        // a path to the program with spaces and no quotes is the program whole
        let path = "C:\\Program Files\\Microsoft VS Code\\Code.exe";
        assert_eq!(editors_from(Some(path.into()), true, |p| p.to_string_lossy() == path), [[path]]);
        assert!(editors_from(Some("\"\"".into()), true, none).is_empty(), "no program: no editor");
        // a bare name off Windows stays itself for the system to find
        if !cfg!(windows) {
            assert_eq!(program("nano"), PathBuf::from("nano"));
        }
    }

    #[test]
    fn only_a_missing_unrunnable_or_signalled_command_is_a_failure() {
        for c in [0, 1, 2, 3, 42, 101, 124, 125, 128, 193, 255] {
            assert!(!exit_is_failure(c), "{c}");
        }
        for c in [126, 127, 129, 134, 137, 139, 143, 192, -1_073_741_819] {
            assert!(exit_is_failure(c), "{c}");
        }
        assert_eq!(exit_code_in("out\n\n[exit code 7]\n[note: x]"), Some(7));
        assert_eq!(exit_code_in("out\n\n[killed by a signal]"), None);
        assert_eq!(exit_code_in("(no output)"), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_shell_runs_a_command() {
        let out = shell().command("echo hi").output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
    }
}
