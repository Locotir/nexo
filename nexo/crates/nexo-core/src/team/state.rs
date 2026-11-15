//! What a lane leaves on disk under `<data_dir>/team/`: `<name>.json` (its state), `<name>.log` (what its
//! runner printed), `<name>.stop` (asks the runner to end at its next idle point), `hold/<name>` (start skips
//! it) and `paused` (no lane starts a round). And whether a recorded pid is still that lane's runner.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The last round a lane ran.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Round {
    /// Milliseconds since the Unix epoch, when the round ended.
    pub at: i64,
    /// `ok`, `failed`, `denied` (a call was refused: nobody can be asked) or `capped` (the spend limit).
    pub result: String,
    /// Dollars the round spent.
    pub spend: f64,
}

/// `<name>.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LaneState {
    /// The lane's one session: every round and mail goes into it.
    pub session_id: Option<String>,
    /// The runner's process while it runs.
    pub pid: Option<u32>,
    /// When the runner started (ms).
    pub started: Option<i64>,
    pub last_round: Option<Round>,
    /// When the next round is due (ms); none: no round is planned (no `every`, or not running).
    pub next_round: Option<i64>,
    /// `running`, `idle`, `paused`, `stopped` or `failed`.
    pub state: String,
    /// Why the lane failed, or the last failed round's error.
    pub why: Option<String>,
    /// Rounds run by this lane.
    pub rounds: u64,
    /// Failed rounds in a row; three end the runner.
    pub failures: u32,
    /// The day `spend_today` counts (`2026-11-15`, local time).
    pub spend_day: String,
    /// Dollars the lane spent that day, rounds and mail together.
    pub spend_today: f64,
}

pub fn dir() -> PathBuf {
    crate::paths::data_dir().join("team")
}

pub fn state_file(name: &str) -> PathBuf {
    dir().join(format!("{name}.json"))
}

pub fn log_file(name: &str) -> PathBuf {
    dir().join(format!("{name}.log"))
}

pub fn stop_file(name: &str) -> PathBuf {
    dir().join(format!("{name}.stop"))
}

pub fn hold_file(name: &str) -> PathBuf {
    dir().join("hold").join(name)
}

pub fn paused_file() -> PathBuf {
    dir().join("paused")
}

pub fn paused() -> bool {
    paused_file().exists()
}

pub fn held(name: &str) -> bool {
    hold_file(name).exists()
}

/// Creates an empty marker file (and its folder).
pub fn touch(p: &std::path::Path) -> std::io::Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(p, b"")
}

impl LaneState {
    /// The state on disk; a lane that never ran (or a file that does not parse) starts empty.
    pub fn load(name: &str) -> LaneState {
        std::fs::read_to_string(state_file(name)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    /// Writes the state whole: a temporary file, then a rename, so a reader never sees half of it.
    pub fn save(&self, name: &str) -> std::io::Result<()> {
        let f = state_file(name);
        std::fs::create_dir_all(dir())?;
        let tmp = f.with_extension(format!("json.{}.tmp", std::process::id()));
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &f)).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    /// Adds `usd` to today's spending, starting the count again on a new day.
    pub fn spend(&mut self, today: &str, usd: f64) {
        if self.spend_day != today {
            self.spend_day = today.to_string();
            self.spend_today = 0.0;
        }
        self.spend_today += usd.max(0.0);
    }

    /// What the lane spent on `today`.
    pub fn spent_on(&self, today: &str) -> f64 {
        if self.spend_day == today {
            self.spend_today
        } else {
            0.0
        }
    }

    /// The recorded pid, when that process is still this lane's runner.
    pub fn live_pid(&self, name: &str) -> Option<u32> {
        self.pid.filter(|p| is_runner(*p, name))
    }
}

/// Whether the words of a command line are `<nexo> team run <name>`: the program is a nexo binary and nothing
/// else follows the lane's name. Only this pid's own command line is read; nothing is matched by pattern.
pub fn runner_words(words: &[String], name: &str) -> bool {
    let [exe, team, run, lane] = words else { return false };
    // the program's file name, whichever separator its path uses (a Windows path read on any system)
    let file = exe.trim_matches('"').rsplit(['/', '\\']).next().unwrap_or_default().to_lowercase();
    file.starts_with("nexo") && team == "team" && run == "run" && lane.trim_matches('"') == name
}

/// Whether process `pid` is alive and is the runner of lane `name` (its command line is `nexo team run <name>`).
pub fn is_runner(pid: u32, name: &str) -> bool {
    if pid == 0 {
        return false;
    }
    command_line(pid).is_some_and(|w| runner_words(&w, name))
}

/// The words of process `pid`'s command line; None when there is no such process.
#[cfg(target_os = "linux")]
fn command_line(pid: u32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let words: Vec<String> =
        raw.split(|b| *b == 0).filter(|w| !w.is_empty()).map(|w| String::from_utf8_lossy(w).into_owned()).collect();
    (!words.is_empty()).then_some(words)
}

/// The words of process `pid`'s command line, as `ps` prints it (the program's path may hold spaces: the last
/// three words are the arguments, the rest the program).
#[cfg(all(unix, not(target_os = "linux")))]
fn command_line(pid: u32) -> Option<Vec<String>> {
    let out = std::process::Command::new("ps").args(["-o", "args=", "-p", &pid.to_string()]).output().ok()?;
    split_tail(String::from_utf8_lossy(&out.stdout).trim())
}

/// The words of process `pid`'s command line, asked of the system's process list for that one id.
#[cfg(windows)]
fn command_line(pid: u32) -> Option<Vec<String>> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let query = format!("(Get-CimInstance Win32_Process -Filter \"ProcessId={pid}\").CommandLine");
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &query])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    split_tail(String::from_utf8_lossy(&out.stdout).trim())
}

/// A command line as one string, as four words: the program (spaces and all) and the last three words.
#[cfg(not(target_os = "linux"))]
fn split_tail(line: &str) -> Option<Vec<String>> {
    let mut parts: Vec<&str> = line.rsplitn(4, char::is_whitespace).collect();
    if parts.len() != 4 {
        return None;
    }
    parts.reverse();
    Some(parts.into_iter().map(|p| p.trim().to_string()).collect())
}

/// Ends the runner `pid` of lane `name` at once, after checking it is that runner. On Unix the signal goes to
/// its process group (the runner leads its own), so the commands its tools started end with it.
pub fn kill_runner(pid: u32, name: &str) -> bool {
    if !is_runner(pid, name) {
        return false;
    }
    #[cfg(unix)]
    {
        // SAFETY: getpgid and kill only read and signal; the pid was just checked to be the lane's runner.
        let group = unsafe { libc::getpgid(pid as libc::pid_t) };
        let target = if group == pid as libc::pid_t { -(pid as libc::pid_t) } else { pid as libc::pid_t };
        unsafe { libc::kill(target, libc::SIGTERM) == 0 }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .is_ok_and(|s| s.success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn only_the_exact_runner_command_line_of_the_lane_counts() {
        assert!(runner_words(&w(&["/usr/bin/nexo", "team", "run", "WINDOWS"]), "WINDOWS"));
        assert!(runner_words(&w(&["C:\\bin\\nexo.exe", "team", "run", "\"ios\""]), "ios"));
        assert!(!runner_words(&w(&["/usr/bin/nexo", "team", "run", "WINDOWS"]), "IOS"));
        assert!(!runner_words(&w(&["/usr/bin/vim", "team", "run", "WINDOWS"]), "WINDOWS"));
        assert!(!runner_words(&w(&["/usr/bin/nexo", "team", "run", "WINDOWS", "x"]), "WINDOWS"));
        assert!(!runner_words(&w(&["/usr/bin/nexo", "team", "start", "WINDOWS"]), "WINDOWS"));
        assert!(!is_runner(0, "WINDOWS"));
        assert!(!is_runner(std::process::id(), "WINDOWS"), "a test binary is no runner");
    }

    #[test]
    fn spending_starts_again_on_a_new_day() {
        let mut s = LaneState::default();
        s.spend("2026-11-15", 0.5);
        s.spend("2026-11-15", 0.25);
        assert_eq!(s.spent_on("2026-11-15"), 0.75);
        assert_eq!(s.spent_on("2026-11-16"), 0.0);
        s.spend("2026-11-16", 0.1);
        assert_eq!(s.spent_on("2026-11-16"), 0.1);
    }
}
