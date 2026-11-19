//! The request history: recalled with the arrows, kept between runs.

use super::*;

/// One line of the history file: a JSON string (a request, or an older run's entry, which never ran as a shell
/// command) or `{"shell":"<text>"}` (a command that ran in the shell).
fn parse_line(line: &str) -> Option<(String, bool)> {
    if let Ok(s) = serde_json::from_str::<String>(line) {
        return Some((s, false));
    }
    let v = serde_json::from_str::<serde_json::Value>(line).ok()?;
    Some((v.get("shell")?.as_str()?.to_string(), true))
}

fn write_line(text: &str, shell: bool) -> String {
    if shell {
        serde_json::json!({ "shell": text }).to_string()
    } else {
        serde_json::to_string(text).unwrap_or_default()
    }
}

impl Input {
    pub fn push_history(&mut self, text: &str) {
        self.push_entry(text, false);
    }

    /// Like [`Input::push_history`], for a command that ran in the shell: recalled, it runs as a shell command again.
    pub fn push_shell_history(&mut self, text: &str) {
        self.push_entry(text, true);
    }

    fn push_entry(&mut self, text: &str, shell: bool) {
        let same = self.history.last().map(String::as_str) == Some(text) && self.history_shell.last() == Some(&shell);
        if !same {
            self.history.push(text.to_string());
            self.history_shell.push(shell);
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
                self.history_shell.remove(0);
            }
            if let Some(f) = &self.history_file {
                use std::io::Write;
                let line = write_line(text, shell);
                if let Ok(mut out) = open_private(f, true) {
                    let _ = writeln!(out, "{line}");
                }
            }
        }
        self.hist = None;
    }

    /// Keeps the history in `file` from now on, and starts from what it holds (the last
    /// `MAX_HISTORY` entries; the file is trimmed when it grows to twice that).
    pub fn persist_history(&mut self, file: std::path::PathBuf) {
        if let Some(d) = file.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        // A file made by an older run is 0644 and holds everything typed: tighten it once.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if file.exists() {
                let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
            }
        }
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let all: Vec<(String, bool)> = text.lines().filter_map(parse_line).collect();
        let from = all.len().saturating_sub(MAX_HISTORY);
        self.history = all[from..].iter().map(|(t, _)| t.clone()).collect();
        self.history_shell = all[from..].iter().map(|(_, s)| *s).collect();
        if all.len() > 2 * MAX_HISTORY {
            let kept: Vec<String> = all[from..].iter().map(|(t, s)| write_line(t, *s)).collect();
            if let Ok(mut out) = open_private(&file, false) {
                use std::io::Write;
                let _ = out.write_all((kept.join("\n") + "\n").as_bytes());
            }
        }
        self.history_file = Some(file);
    }

    /// Every request sent, oldest first.
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// The history entry `i` and whether it ran as a shell command.
    pub(crate) fn history_entry(&self, i: usize) -> Option<(&str, bool)> {
        Some((self.history.get(i)?.as_str(), self.history_shell.get(i).copied().unwrap_or(false)))
    }

    pub(super) fn history_step(&mut self, up: bool) -> Edit {
        if up {
            self.history_up()
        } else {
            self.history_down()
        }
    }

    pub(super) fn history_up(&mut self) -> Edit {
        if self.history.is_empty() {
            return Edit::Ignored;
        }
        let next = match self.hist {
            None => {
                self.stash = self.text();
                self.stash_bang = self.typed_bang();
                self.history.len() - 1
            }
            Some(0) => return Edit::Ignored,
            Some(i) => i - 1,
        };
        let text = self.history[next].clone();
        let shell = self.history_shell.get(next).copied().unwrap_or(false);
        self.recall(&text, shell);
        self.hist = Some(next);
        Edit::Recalled
    }

    pub(super) fn history_down(&mut self) -> Edit {
        match self.hist {
            None => Edit::Ignored,
            Some(i) if i + 1 < self.history.len() => {
                let text = self.history[i + 1].clone();
                let shell = self.history_shell.get(i + 1).copied().unwrap_or(false);
                self.recall(&text, shell);
                self.hist = Some(i + 1);
                Edit::Recalled
            }
            Some(_) => {
                let s = self.stash.clone();
                self.recall(&s, self.stash_bang);
                self.hist = None;
                Edit::Recalled
            }
        }
    }
}

/// Opens the history file for appending or for rewriting. Everything typed lands in it (pasted
/// tokens and customer text too), so on unix a new file is readable by its owner only.
fn open_private(file: &std::path::Path, append: bool) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.create(true);
    if append {
        o.append(true);
    } else {
        o.write(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(file)
}
