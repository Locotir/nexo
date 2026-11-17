//! The request history: recalled with the arrows, kept between runs.

use super::*;

impl Input {
    pub fn push_history(&mut self, text: &str) {
        if self.history.last().map(String::as_str) != Some(text) {
            self.history.push(text.to_string());
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
            }
            if let Some(f) = &self.history_file {
                use std::io::Write;
                let line = serde_json::to_string(text).unwrap_or_default();
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
        let all: Vec<String> = text.lines().filter_map(|l| serde_json::from_str::<String>(l).ok()).collect();
        let from = all.len().saturating_sub(MAX_HISTORY);
        self.history = all[from..].to_vec();
        if all.len() > 2 * MAX_HISTORY {
            let kept: Vec<String> = self.history.iter().map(|h| serde_json::to_string(h).unwrap_or_default()).collect();
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
                self.history.len() - 1
            }
            Some(0) => return Edit::Ignored,
            Some(i) => i - 1,
        };
        let text = self.history[next].clone();
        self.set_text(&text);
        self.hist = Some(next);
        Edit::Recalled
    }

    pub(super) fn history_down(&mut self) -> Edit {
        match self.hist {
            None => Edit::Ignored,
            Some(i) if i + 1 < self.history.len() => {
                let text = self.history[i + 1].clone();
                self.set_text(&text);
                self.hist = Some(i + 1);
                Edit::Recalled
            }
            Some(_) => {
                let s = self.stash.clone();
                self.set_text(&s);
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
