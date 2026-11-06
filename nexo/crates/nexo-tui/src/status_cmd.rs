//! `[ui] status_command`: the status bar drawn by the person's own script, as in other coding
//! agents. nexo runs it through the shell with a JSON of what it knows on stdin; the first line of
//! stdout becomes the bar, ANSI colours included.
//!
//! It never holds the interface: each run happens on its own thread, gets 500 ms, and runs again
//! when the turn ends or the model changes, at most once a second. A run that fails or is late
//! leaves the last good line (or, before the first one, the usual pieces); the reason goes to the
//! log once, never to the bar.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthChar;

/// How long a run may take.
pub const TIMEOUT: Duration = Duration::from_millis(500);
/// The least time between two runs.
pub const GAP: Duration = Duration::from_secs(1);

type Outcome = Result<String, String>;

pub struct StatusCommand {
    command: String,
    cwd: PathBuf,
    running: Option<mpsc::Receiver<Outcome>>,
    started: Option<Instant>,
    /// The state the last run was asked for; a different one asks for a new run.
    key: Option<String>,
    wanted: bool,
    /// A run for this key already failed once and was asked again: a slow machine gets one more
    /// try, a broken script does not run on every tick.
    retried: bool,
    logged: bool,
    /// The last good line, as the script printed it (escapes included).
    pub line: Option<String>,
    /// Why the last run gave no line (kept for tests and the log, never drawn).
    pub error: Option<String>,
}

impl StatusCommand {
    /// None for an empty command.
    pub fn new(command: &str, cwd: &Path) -> Option<StatusCommand> {
        let command = command.trim();
        (!command.is_empty()).then(|| StatusCommand {
            command: command.to_string(),
            cwd: cwd.to_path_buf(),
            running: None,
            started: None,
            key: None,
            wanted: true,
            retried: false,
            logged: false,
            line: None,
            error: None,
        })
    }

    /// Called on every tick. `key` names the state the line depends on (a new one asks for a run);
    /// `input` builds the JSON only when a run starts. True when the line changed.
    pub fn poll(&mut self, key: &str, input: impl FnOnce() -> String) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.running {
            match rx.try_recv() {
                Ok(Ok(line)) => {
                    changed = self.line.as_deref() != Some(line.as_str());
                    self.line = Some(line);
                    self.running = None;
                    self.retried = false;
                }
                Ok(Err(why)) => {
                    if !self.logged {
                        self.logged = true;
                        tracing::warn!("status_command `{}`: {why}", self.command);
                    }
                    self.error = Some(why);
                    self.running = None;
                    if !self.retried {
                        self.retried = true;
                        self.wanted = true;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.running = None,
            }
        }
        if self.key.as_deref() != Some(key) {
            self.key = Some(key.to_string());
            self.wanted = true;
            self.retried = false;
        }
        if self.wanted && self.running.is_none() && !self.started.is_some_and(|t| t.elapsed() < GAP) {
            self.wanted = false;
            self.started = Some(Instant::now());
            let (tx, rx) = mpsc::channel();
            let (cmd, cwd, json) = (self.command.clone(), self.cwd.clone(), input());
            std::thread::spawn(move || {
                let _ = tx.send(run(&cmd, &cwd, &json, TIMEOUT));
            });
            self.running = Some(rx);
        }
        changed
    }

    /// True while a run is out (for tests).
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }
}

fn shell(cmd: &str) -> Command {
    if cfg!(windows) {
        // the shell of the `bash` tool and the hooks (Git Bash, else PowerShell): a status script
        // written for a POSIX shell runs as it does elsewhere
        nexo_core::shell::shell().command(cmd)
    } else {
        // by its path: a PATH changed or emptied around nexo still finds the shell
        let mut c = Command::new("/bin/sh");
        c.args(["-c", cmd]);
        c
    }
}

#[cfg(unix)]
fn cut_off(s: &std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;
    s.signal() == Some(13)
}

#[cfg(not(unix))]
fn cut_off(_: &std::process::ExitStatus) -> bool {
    false
}

/// Runs `cmd` once with `json` on stdin: its first line, or why there is none.
pub fn run(cmd: &str, cwd: &Path, json: &str, timeout: Duration) -> Outcome {
    let deadline = Instant::now() + timeout;
    let mut command = shell(cmd);
    // a folder gone since the start leaves the script where nexo runs
    if cwd.is_dir() {
        command.current_dir(cwd);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start: {e}"))?;
    // fed from aside, so a script that never reads its stdin cannot hold this thread
    if let Some(mut stdin) = child.stdin.take() {
        let json = json.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(json.as_bytes());
        });
    }
    let mut out = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // up to the first line: a script that leaves something running still gives its line
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        while let Ok(n) = out.read(&mut chunk) {
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.contains(&b'\n') || buf.len() > 64 * 1024 {
                break;
            }
        }
        let _ = tx.send(buf);
    });
    let bytes = match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(b) => b,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("took more than {} ms", timeout.as_millis()));
        }
    };
    // the exit status, when it comes in time; a script still running after its line is fine
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    // a script that writes more lines dies of SIGPIPE once its first is taken: that is no failure
    if let Some(s) = status.filter(|s| !s.success() && !(cut_off(s) && bytes.contains(&b'\n'))) {
        return Err(format!("exited with {s}"));
    }
    let text = String::from_utf8_lossy(&bytes);
    let line = text.lines().next().unwrap_or("").trim_end_matches('\r');
    if visible(line).trim().is_empty() {
        return Err("printed nothing".into());
    }
    Ok(line.to_string())
}

/// The text of a line without its escapes.
pub fn visible(line: &str) -> String {
    spans(line, Style::default()).into_iter().map(|(t, _)| t).collect()
}

/// A line with ANSI SGR colours (16, 256 and true colour; bold, dim, italic, underline, reverse)
/// as styled pieces over `base`. Other escapes and control characters are dropped; a tab is a space.
pub fn spans(line: &str, base: Style) -> Vec<(String, Style)> {
    let mut out: Vec<(String, Style)> = Vec::new();
    let mut style = base;
    let mut text = String::new();
    let mut chars = line.chars().peekable();
    let flush = |text: &mut String, style: Style, out: &mut Vec<(String, Style)>| {
        if !text.is_empty() {
            out.push((std::mem::take(text), style));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    let mut params = String::new();
                    let mut fin = None;
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            fin = Some(c);
                            break;
                        }
                        params.push(c);
                    }
                    if fin == Some('m') {
                        flush(&mut text, style, &mut out);
                        style = sgr(&params, style, base);
                    }
                }
                Some(']') => {
                    // OSC (titles, links): up to BEL or ESC \
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\t' => text.push(' '),
            c if c.is_control() => {}
            c => text.push(c),
        }
    }
    flush(&mut text, style, &mut out);
    out
}

fn sgr(params: &str, mut s: Style, base: Style) -> Style {
    let n: Vec<u16> = params.split([';', ':']).map(|p| p.parse().unwrap_or(0)).collect();
    let mut i = 0;
    while i < n.len() {
        match n[i] {
            0 => s = base,
            1 => s = s.add_modifier(Modifier::BOLD),
            2 => s = s.add_modifier(Modifier::DIM),
            // italics are drawn nowhere in nexo: SGR 3 and 23 change nothing
            4 => s = s.add_modifier(Modifier::UNDERLINED),
            7 => s = s.add_modifier(Modifier::REVERSED),
            22 => s = s.remove_modifier(Modifier::BOLD | Modifier::DIM),
            24 => s = s.remove_modifier(Modifier::UNDERLINED),
            27 => s = s.remove_modifier(Modifier::REVERSED),
            c @ 30..=37 => s = s.fg(Color::Indexed((c - 30) as u8)),
            c @ 90..=97 => s = s.fg(Color::Indexed((c - 90 + 8) as u8)),
            39 => s.fg = base.fg,
            c @ 40..=47 => s = s.bg(Color::Indexed((c - 40) as u8)),
            c @ 100..=107 => s = s.bg(Color::Indexed((c - 100 + 8) as u8)),
            49 => s.bg = base.bg,
            c @ (38 | 48) => {
                let colour = match n.get(i + 1) {
                    Some(5) => {
                        i += 2;
                        n.get(i).map(|&v| Color::Indexed(v.min(255) as u8))
                    }
                    Some(2) => {
                        i += 4;
                        match (n.get(i - 2), n.get(i - 1), n.get(i)) {
                            (Some(&r), Some(&g), Some(&b)) => {
                                Some(Color::Rgb(r.min(255) as u8, g.min(255) as u8, b.min(255) as u8))
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                if let Some(colour) = colour {
                    s = if c == 38 { s.fg(colour) } else { s.bg(colour) };
                }
            }
            _ => {}
        }
        i += 1;
    }
    s
}

/// The pieces cut to `width` columns, with `…` as the last one when something was left out.
pub fn fit(pieces: Vec<(String, Style)>, width: usize) -> Vec<(String, Style)> {
    let total: usize = pieces.iter().flat_map(|(t, _)| t.chars()).map(|c| c.width().unwrap_or(0)).sum();
    if total <= width {
        return pieces;
    }
    let room = width.saturating_sub(1);
    let mut used = 0;
    let mut out = Vec::new();
    let mut last = Style::default();
    'all: for (t, s) in pieces {
        let mut kept = String::new();
        last = s;
        for c in t.chars() {
            let w = c.width().unwrap_or(0);
            if used + w > room {
                if !kept.is_empty() {
                    out.push((kept, s));
                }
                break 'all;
            }
            used += w;
            kept.push(c);
        }
        out.push((kept, s));
    }
    if width > 0 {
        out.push(("…".to_string(), last));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_and_styles_become_pieces() {
        let p = spans("\u{1b}[1;32mok\u{1b}[0m · \u{1b}[38;5;208mx\u{1b}[38;2;1;2;3my\u{1b}[39mz", Style::default());
        assert_eq!(p[0], ("ok".into(), Style::default().fg(Color::Indexed(2)).add_modifier(Modifier::BOLD)));
        assert_eq!(p[1], (" · ".into(), Style::default()));
        assert_eq!(p[2], ("x".into(), Style::default().fg(Color::Indexed(208))));
        assert_eq!(p[3], ("y".into(), Style::default().fg(Color::Rgb(1, 2, 3))));
        assert_eq!(p[4], ("z".into(), Style::default()));
    }

    #[test]
    fn italics_from_the_script_change_nothing() {
        let p = spans("\u{1b}[3;1ma\u{1b}[23mb", Style::default());
        assert!(p.iter().all(|(_, s)| !s.add_modifier.contains(Modifier::ITALIC)), "{p:?}");
        assert_eq!(p[0].1, Style::default().add_modifier(Modifier::BOLD));
    }

    #[test]
    fn other_escapes_and_controls_are_dropped() {
        assert_eq!(visible("a\u{1b}]8;;http://x\u{7}b\u{1b}]8;;\u{1b}\\c\u{1b}[2Kd\te\u{7}"), "abcd e");
    }

    #[test]
    fn a_long_line_ends_in_an_ellipsis() {
        let p = fit(spans("ab\u{1b}[31mcdef", Style::default()), 4);
        let text: String = p.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(text, "abc…");
        assert_eq!(fit(spans("abc", Style::default()), 3).len(), 1);
        // wide characters are never cut in half
        let text: String = fit(spans("日本語", Style::default()), 4).iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(text, "日…");
    }

    #[cfg(unix)]
    #[test]
    fn runs_the_script_with_the_json_on_stdin() {
        let dir = std::env::temp_dir();
        let line = run("read j; printf 'got %s\\nsecond\\n' \"$j\"", &dir, "{\"a\":1}\n", TIMEOUT);
        assert_eq!(line.as_deref(), Ok("got {\"a\":1}"));
        // many lines after the first: the pipe closes under the script, which is fine
        let line = run("echo first; i=0; while [ $i -lt 2000 ]; do echo more $i; i=$((i+1)); done", &dir, "", TIMEOUT);
        assert_eq!(line.as_deref(), Ok("first"));
        assert!(run("exit 3", &dir, "", TIMEOUT).unwrap_err().contains("exited"));
        assert!(run("true", &dir, "", TIMEOUT).unwrap_err().contains("nothing"));
        let t = Instant::now();
        // a loop of the shell itself: tests next door change PATH, so no `sleep`
        assert!(run("while :; do :; done", &dir, "", TIMEOUT).unwrap_err().contains("500 ms"));
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn a_run_that_fails_once_is_tried_again_without_a_new_state_and_a_broken_one_only_once() {
        let d = tempfile::tempdir().unwrap();
        let mark = d.path().join("ran");
        // fails the first time (as a run cut by a busy machine), prints its line the second
        let cmd = format!("if [ -e '{0}' ]; then echo ok; else : > '{0}'; exit 1; fi", mark.display());
        let wait = |s: &mut StatusCommand, key: &str| {
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(20) {
                s.poll(key, String::new);
                if s.line.is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let mut s = StatusCommand::new(&cmd, d.path()).unwrap();
        wait(&mut s, "k");
        assert_eq!(s.line.as_deref(), Some("ok"), "{:?}", s.error);
        // a script that always fails runs twice for a state, then waits for the next one
        let count = d.path().join("count");
        let cmd = format!("echo x >> '{}'; exit 1", count.display());
        let mut s = StatusCommand::new(&cmd, d.path()).unwrap();
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(3500) {
            s.poll("k", String::new);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 2);
    }
}
