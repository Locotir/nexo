//! What `nexo team` and the `/team` window share: the lanes of `team.toml`, what a lane leaves on disk
//! ([`state`]), starting its runner detached ([`spawn`]), the table that says how each lane stands, and the
//! few actions (start, stop, a marker file) that both interfaces do the same way.

pub mod spawn;
pub mod state;

use std::path::{Path, PathBuf};

use crate::config::{team_file, Lane, TeamConfig};
use state::LaneState;

/// The lanes of `team.toml`; a file that does not parse is an error. The keys nexo does not read are in `unread`.
pub fn read_lanes() -> Result<crate::config::TeamFile, String> {
    let f = team_file();
    let text = match std::fs::read_to_string(&f) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("no lanes: {} does not exist (see docs/TEAM.md)", f.display()))
        }
        Err(e) => return Err(format!("cannot read {}: {e}", f.display())),
    };
    TeamConfig::parse(&text).map_err(|e| format!("{}: {e}", f.display()))
}

/// The lane's folder: `~` is the home folder.
pub fn lane_dir(lane: &Lane) -> PathBuf {
    let d = lane.dir.trim();
    match (d.strip_prefix("~"), crate::paths::user_home()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            home.join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(d),
    }
}

/// `HH:MM` of `ms` in local time (`offset`: the local UTC offset, None for UTC).
pub fn clock(ms: i64, offset: Option<time::UtcOffset>) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(ms / 1000).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let t = offset.map_or(t, |o| t.to_offset(o));
    format!("{:02}:{:02}", t.hour(), t.minute())
}

/// `45s`, `12m`, `3h`, `2d`.
pub fn age(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

/// What a lane is doing, as `status` says it.
pub fn state_word(name: &str, st: &LaneState, alive: bool, offset: Option<time::UtcOffset>) -> String {
    if !alive {
        return match (st.state.as_str(), &st.why) {
            ("failed", Some(why)) => format!("failed: {why}"),
            ("failed", None) => "failed".into(),
            _ if state::held(name) => "held".into(),
            _ => "stopped".into(),
        };
    }
    match st.state.as_str() {
        "running" => "running a round".into(),
        _ if state::paused() => "paused".into(),
        _ => match st.next_round {
            Some(at) => format!("idle until {}", clock(at, offset)),
            None => "idle (mail only)".into(),
        },
    }
}

/// One lane of the table, as `nexo team status` and `/team` show it.
#[derive(Debug, Clone, PartialEq)]
pub struct Cells {
    pub name: String,
    /// Whether the lane's runner is alive.
    pub alive: bool,
    pub state: String,
    /// `last ok 3m ago`, or `no round yet`.
    pub last: String,
    /// Dollars spent today.
    pub spend: f64,
    pub session: Option<String>,
}

/// The cells of lane `name` from its state, `now` (ms) and `today` (as the spend file keys it).
pub fn cells(name: &str, st: &LaneState, alive: bool, now: i64, today: &str, offset: Option<time::UtcOffset>) -> Cells {
    let last = match &st.last_round {
        Some(r) => format!("last {} {} ago", r.result, age(now - r.at)),
        None => "no round yet".into(),
    };
    Cells {
        name: name.to_string(),
        alive,
        state: state_word(name, st, alive, offset),
        last,
        spend: st.spent_on(today),
        session: st.session_id.clone(),
    }
}

/// The cells of every lane, read from disk, at `now` (ms) and `today`.
pub fn table(team: &TeamConfig, now: i64, today: &str, offset: Option<time::UtcOffset>) -> Vec<Cells> {
    team.lane
        .iter()
        .map(|l| {
            let st = LaneState::load(&l.name);
            cells(&l.name, &st, st.live_pid(&l.name).is_some(), now, today, offset)
        })
        .collect()
}

/// The line `nexo team status` prints for a lane.
pub fn status_line(c: &Cells) -> String {
    format!(
        "{:<12} {:<24} {:<20} ${:.2} today  {}",
        c.name,
        c.state,
        c.last,
        c.spend,
        c.session.as_deref().unwrap_or("-")
    )
}

/// Creates (`on`) or removes (`!on`) a marker file such as `paused` or a lane's hold file.
pub fn set_marker(path: &Path, on: bool) -> std::io::Result<()> {
    if on {
        return state::touch(path);
    }
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// What `start` did for a lane.
#[derive(Debug, Clone, PartialEq)]
pub enum Started {
    Held,
    Paused,
    AlreadyRunning,
    /// A runner was started, with this pid; the second is why its state could not be written, if so.
    Started(u32, Option<String>),
}

/// Starts the lane's runner when it is not held, paused or running; `spawn` starts it (the detached
/// `nexo team run <name>` of [`spawn::runner`]) and gives its pid.
pub fn start(name: &str, spawn: impl FnOnce(&str) -> std::io::Result<u32>) -> std::io::Result<Started> {
    if state::held(name) {
        return Ok(Started::Held);
    }
    if state::paused() {
        return Ok(Started::Paused);
    }
    let mut st = LaneState::load(name);
    if st.live_pid(name).is_some() {
        return Ok(Started::AlreadyRunning);
    }
    // a stop asked of a runner that is gone would end the new one at once
    let _ = std::fs::remove_file(state::stop_file(name));
    let pid = spawn(name)?;
    // recorded now, so a second start right after sees this runner before it has written anything
    st.pid = Some(pid);
    st.state = "idle".into();
    st.why = None;
    let unsaved = st.save(name).err().map(|e| e.to_string());
    Ok(Started::Started(pid, unsaved))
}

/// What `stop` did for a lane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stopped {
    NotRunning,
    /// The runner was asked to end after the round in flight.
    Asked,
    /// The runner was ended at once.
    Ended,
}

/// Asks the lane's runner to end after the round in flight; with `now`, ends it at once (only the recorded pid,
/// after checking that its command line is `nexo team run <name>`).
pub fn stop(name: &str, now: bool) -> Result<Stopped, String> {
    let mut st = LaneState::load(name);
    let Some(pid) = st.live_pid(name) else {
        let _ = std::fs::remove_file(state::stop_file(name));
        return Ok(Stopped::NotRunning);
    };
    state::touch(&state::stop_file(name)).map_err(|e| e.to_string())?;
    if !now {
        return Ok(Stopped::Asked);
    }
    if !state::kill_runner(pid, name) {
        return Err(format!("could not end process {pid}"));
    }
    let _ = std::fs::remove_file(state::stop_file(name));
    st.pid = None;
    st.next_round = None;
    st.state = "stopped".into();
    let _ = st.save(name);
    Ok(Stopped::Ended)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_line_says_what_the_lane_does_and_spent() {
        let line = |name: &str, st: &LaneState, alive, now, today: &str| {
            status_line(&cells(name, st, alive, now, today, None))
        };
        let mut st = LaneState { session_id: Some("s-1".into()), ..Default::default() };
        st.spend("2026-11-15", 0.13);
        st.last_round = Some(state::Round { at: 1_000, result: "ok".into(), spend: 0.1 });
        st.state = "running".into();
        let l = line("WINDOWS", &st, true, 181_000, "2026-11-15");
        assert!(l.starts_with("WINDOWS      running a round"), "{l}");
        assert!(l.contains("last ok 3m ago") && l.contains("$0.13 today") && l.ends_with("s-1"), "{l}");
        st.state = "idle".into();
        st.next_round = None;
        assert!(line("W", &st, true, 0, "x").contains("idle (mail only)"));
        st.state = "failed".into();
        st.why = Some("bad key".into());
        let l = line("W", &st, false, 0, "2026-11-16");
        assert!(l.contains("failed: bad key") && l.contains("$0.00 today"), "{l}");
        st.state = "stopped".into();
        assert!(line("W", &st, false, 0, "x").contains("stopped"));
    }

    #[test]
    fn a_lane_folder_starting_with_a_tilde_is_in_the_home_folder() {
        let lane = |d: &str| Lane { dir: d.into(), ..Default::default() };
        let home = crate::paths::user_home().unwrap();
        assert_eq!(lane_dir(&lane("~/work/x")), home.join("work/x"));
        assert_eq!(lane_dir(&lane("~")), home);
        assert_eq!(lane_dir(&lane("/srv/a")), PathBuf::from("/srv/a"));
        assert_eq!(lane_dir(&lane("~other/a")), PathBuf::from("~other/a"));
    }
}
