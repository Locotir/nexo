//! Typing into a session of another program, so the text arrives as that session's own input with Enter
//! pressed. Two ways in: the inbox socket Claude Code opens for each of its sessions, and a tmux pane. A nexo
//! session is reached through its mailbox instead (see [`crate::mailbox`]). Nothing here asks the person:
//! every caller does, since a send types into another program.
//!
//! Claude Code's inbox: a Unix socket at `$XDG_RUNTIME_DIR/cc-socks/<pid>.sock` (or `/tmp/cc-socks-<uid>/`)
//! that reads JSON lines. An optional first line `{"type":"auth","token":…}` carries the key the session
//! publishes for local peers in `<claude home>/sessions/<pid>.<sha256 of the socket path>.key`; then
//! `{"type":"user","message":{"role":"user","content":…}}` is queued as a prompt and runs when the session
//! is free.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Where to look. The defaults are the ones the programs use; tests point each at their own.
#[derive(Clone, Debug, Default)]
pub struct Reach {
    /// The folder of Claude Code's session sockets; None: `$XDG_RUNTIME_DIR/cc-socks`, then `/tmp/cc-socks-<uid>`.
    pub cc_socks: Option<PathBuf>,
    /// Claude Code's configuration folder; None: `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
    pub claude_home: Option<PathBuf>,
    /// A tmux server by name (`tmux -L`); None: the default server.
    pub tmux_name: Option<String>,
}

/// A session of another program, by the way it is reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A Claude Code session, by its process id.
    Claude(u32),
    /// A tmux pane: `%3`, or `session:window.pane`.
    Tmux(String),
    /// A process: its Claude Code inbox if it has one, else the tmux pane it runs in.
    Pid(u32),
}

/// A session of another program that can be typed into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reachable {
    /// What a send takes: `claude:<pid>` or `tmux:<pane>`.
    pub address: String,
    /// `claude`, or the command running in the pane.
    pub program: String,
    /// The folder it works in (empty when unknown).
    pub cwd: String,
    pub pid: u32,
}

/// The longest text typed in one send.
pub const TYPE_MAX_BYTES: usize = 8192;

const HELP: &str = "use claude:<pid>, tmux:<pane> or pid:<n>";

/// `to` as an address of another program: None when it has no `word:` prefix (then it names a nexo session).
pub fn parse(to: &str) -> Option<Result<Target, String>> {
    let to = to.trim();
    let (kind, rest) = to.split_once(':')?;
    if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let rest = rest.trim();
    let pid = || rest.parse::<u32>().ok().filter(|p| *p > 0);
    let bad = || Err(format!("Not sent: \"{to}\" is not an address nexo can type into; {HELP}"));
    Some(match kind.to_ascii_lowercase().as_str() {
        "claude" => pid().map(Target::Claude).map_or_else(bad, Ok),
        "tmux" if !rest.is_empty() && !rest.starts_with('-') && !rest.chars().any(char::is_control) => {
            Ok(Target::Tmux(rest.to_string()))
        }
        "tmux" => bad(),
        // any other program (codex, a shell…) is reached by its process
        _ => pid().map(Target::Pid).map_or_else(bad, Ok),
    })
}

/// The text as one line: line breaks and other control characters become one space each run, since a
/// terminal reads a line break as Enter.
pub fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut gap = false;
    for c in text.trim().chars() {
        if c.is_control() {
            gap = true;
            continue;
        }
        if gap {
            out.push(' ');
            gap = false;
        }
        out.push(c);
    }
    out
}

impl Reach {
    /// Types `text` into `target` and presses Enter. Ok: who got it, for a person to read.
    pub fn send(&self, target: &Target, text: &str) -> Result<String, String> {
        if text.trim().is_empty() {
            return Err("Not sent: the message is empty".into());
        }
        if text.len() > TYPE_MAX_BYTES {
            return Err(format!("Not sent: too big ({} > {TYPE_MAX_BYTES} bytes)", text.len()));
        }
        match target {
            Target::Claude(pid) => self.send_claude(*pid, text).map(|_| format!("Claude Code session {pid}")),
            Target::Tmux(pane) => self.send_tmux(pane, text).map(|_| format!("tmux pane {pane}")),
            Target::Pid(pid) => {
                if self.claude_socket(*pid).is_some() {
                    return self.send_claude(*pid, text).map(|_| format!("Claude Code session {pid}"));
                }
                match self.pane_of(*pid) {
                    Some(pane) => self.send_tmux(&pane, text).map(|_| format!("process {pid} in tmux pane {pane}")),
                    None => Err(format!(
                        "Not sent: process {pid} has no inbox nexo knows and does not run inside tmux, so nothing can \
                         type into it. Start it inside tmux to reach it."
                    )),
                }
            }
        }
    }

    /// The sessions of other programs that can be typed into now: Claude Code inboxes of live processes,
    /// then the tmux panes (not the one this process runs in).
    pub fn discover(&self) -> Vec<Reachable> {
        let mut out = Vec::new();
        for dir in self.socket_dirs() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            let mut found: Vec<Reachable> = rd
                .flatten()
                .filter_map(|e| e.file_name().to_str()?.strip_suffix(".sock")?.parse::<u32>().ok())
                .filter(|pid| alive(*pid) && !out.iter().any(|r: &Reachable| r.pid == *pid))
                .map(|pid| Reachable {
                    address: format!("claude:{pid}"),
                    program: "claude".into(),
                    cwd: cwd_of(pid),
                    pid,
                })
                .collect();
            found.sort_by_key(|r| r.pid);
            out.extend(found);
        }
        let mine = if self.tmux_name.is_none() { std::env::var("TMUX_PANE").ok() } else { None };
        for (pane, pid, cwd, cmd) in self.panes() {
            if Some(&pane) != mine.as_ref() {
                out.push(Reachable { address: format!("tmux:{pane}"), program: cmd, cwd, pid });
            }
        }
        out
    }

    /// The live sessions of `program` working in `cwd`: how the panel finds where to type for one of its cards.
    pub fn find(&self, program: &str, cwd: &str) -> Vec<Reachable> {
        let want = program.to_ascii_lowercase();
        let cwd = cwd.trim_end_matches('/');
        self.discover()
            .into_iter()
            .filter(|r| !cwd.is_empty() && r.cwd.trim_end_matches('/') == cwd)
            .filter(|r| r.program.to_ascii_lowercase().contains(&want))
            .collect()
    }

    fn socket_dirs(&self) -> Vec<PathBuf> {
        if let Some(d) = &self.cc_socks {
            return vec![d.clone()];
        }
        let mut v = Vec::new();
        if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR").filter(|s| !s.is_empty()) {
            v.push(PathBuf::from(rt).join("cc-socks"));
        }
        v.push(PathBuf::from(format!("/tmp/cc-socks-{}", uid())));
        v
    }

    /// The inbox socket of Claude Code session `pid`, when one exists.
    pub fn claude_socket(&self, pid: u32) -> Option<PathBuf> {
        self.socket_dirs().into_iter().map(|d| d.join(format!("{pid}.sock"))).find(|p| is_socket(p))
    }

    #[cfg(unix)]
    fn claude_home(&self) -> Option<PathBuf> {
        if let Some(h) = &self.claude_home {
            return Some(h.clone());
        }
        if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|s| !s.is_empty()) {
            return Some(PathBuf::from(d));
        }
        dirs::home_dir().map(|h| h.join(".claude"))
    }

    /// The key session `pid` published for local peers: `sessions/<pid>.<sha256 of the socket path>.key`,
    /// else the only key file of that pid. Never shown anywhere.
    #[cfg(unix)]
    fn claude_token(&self, pid: u32, socket: &Path) -> Option<String> {
        let dir = self.claude_home()?.join("sessions");
        let exact = dir.join(format!("{pid}.{}.key", sha256_hex(socket.to_string_lossy().as_bytes())));
        let file = if exact.is_file() {
            exact
        } else {
            let prefix = format!("{pid}.");
            let mut c: Vec<PathBuf> = std::fs::read_dir(&dir)
                .ok()?
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".key"))
                })
                .collect();
            if c.len() != 1 {
                return None;
            }
            c.pop()?
        };
        if std::fs::metadata(&file).ok()?.len() > 4096 {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
        let t = v.get("peerToken")?.as_str()?;
        (t.len() == 32 && t.bytes().all(|b| b.is_ascii_hexdigit())).then(|| t.to_string())
    }

    #[cfg(unix)]
    fn send_claude(&self, pid: u32, text: &str) -> Result<(), String> {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        let Some(path) = self.claude_socket(pid) else {
            return Err(format!(
                "Not sent: Claude Code session {pid} has no inbox (it is not running, or it was started without one)"
            ));
        };
        let mut s = UnixStream::connect(&path)
            .map_err(|e| format!("Not sent: Claude Code session {pid} does not answer ({e})"))?;
        let _ = s.set_write_timeout(Some(Duration::from_secs(3)));
        let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
        let mut lines = String::new();
        if let Some(token) = self.claude_token(pid, &path) {
            lines.push_str(&serde_json::json!({ "type": "auth", "token": token }).to_string());
            lines.push('\n');
        }
        lines.push_str(
            &serde_json::json!({ "type": "user", "message": { "role": "user", "content": text } }).to_string(),
        );
        lines.push('\n');
        s.write_all(lines.as_bytes())
            .and_then(|_| s.flush())
            .map_err(|e| format!("Not sent: writing to Claude Code session {pid} failed ({e})"))?;
        let _ = s.shutdown(std::net::Shutdown::Write);
        // the session closes its side once it has read the lines; wait a little for that, never long
        let mut sink = [0u8; 256];
        let _ = s.read(&mut sink);
        Ok(())
    }

    #[cfg(not(unix))]
    fn send_claude(&self, pid: u32, _text: &str) -> Result<(), String> {
        Err(format!("Not sent: typing into Claude Code session {pid} works on Linux and macOS only"))
    }

    fn tmux(&self) -> Command {
        let mut c = Command::new("tmux");
        if let Some(n) = &self.tmux_name {
            c.arg("-L").arg(n);
        }
        c
    }

    /// Every pane of the server: id, process, folder, command.
    fn panes(&self) -> Vec<(String, u32, String, String)> {
        let out = self
            .tmux()
            .args(["list-panes", "-a", "-F", "#{pane_id}\t#{pane_pid}\t#{pane_current_path}\t#{pane_current_command}"])
            .stderr(std::process::Stdio::null())
            .output();
        let Ok(out) = out else { return Vec::new() };
        if !out.status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let mut f = l.split('\t');
                let id = f.next()?.to_string();
                let pid = f.next()?.parse().ok()?;
                Some((id, pid, f.next().unwrap_or("").to_string(), f.next().unwrap_or("").to_string()))
            })
            .collect()
    }

    /// The pane `pid` runs in: the pane whose process is `pid` or one of its parents.
    fn pane_of(&self, pid: u32) -> Option<String> {
        let panes = self.panes();
        let mut p = pid;
        for _ in 0..64 {
            if let Some((id, ..)) = panes.iter().find(|(_, pp, ..)| *pp == p) {
                return Some(id.clone());
            }
            p = parent_of(p)?;
            if p <= 1 {
                return None;
            }
        }
        None
    }

    fn send_tmux(&self, pane: &str, text: &str) -> Result<(), String> {
        let run = |args: &[&str]| -> Result<(), String> {
            let out = self.tmux().args(args).output().map_err(|e| format!("Not sent: tmux could not start ({e})"))?;
            if out.status.success() {
                Ok(())
            } else {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                Err(format!("Not sent: tmux pane {pane}: {}", if err.is_empty() { "tmux refused" } else { &err }))
            }
        };
        run(&["send-keys", "-t", pane, "-l", "--", &one_line(text)])?;
        // a program that reads fast typing as a paste would take an Enter in the same burst as a new line
        std::thread::sleep(Duration::from_millis(150));
        run(&["send-keys", "-t", pane, "Enter"])
    }
}

#[cfg(unix)]
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(unix)]
fn is_socket(p: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(p).is_ok_and(|m| m.file_type().is_socket())
}

#[cfg(not(unix))]
fn is_socket(_p: &Path) -> bool {
    false
}

fn uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: getuid has no failure mode and touches no memory.
        unsafe { libc::getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks that the process exists.
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        pid != 0
    }
}

fn cwd_of(pid: u32) -> String {
    std::fs::read_link(format!("/proc/{pid}/cwd")).map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
}

/// The parent process of `pid` (Linux; elsewhere unknown).
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `pid (comm) state ppid …`: the command may hold spaces and parentheses, so read after the last `)`
    stat.rsplit_once(')')?.1.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
#[path = "reach/tests.rs"]
mod tests;
