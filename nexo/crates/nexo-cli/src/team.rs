//! `nexo team`: named lanes that run on their own. Each lane of `<config_dir>/team.toml` is one long-lived
//! session in a folder of its own, run by a detached `nexo team run <name>` that does the lane's routine on a
//! clock and answers mail between rounds. Closing the terminal does not stop a runner; `stop` does, at the
//! runner's next idle point.

mod runner;
mod spawn;
pub mod state;

use std::path::PathBuf;
use std::time::Duration;

use clap::Subcommand;
use nexo_core::config::{Lane, TeamConfig};

use state::LaneState;

#[derive(Subcommand, Debug)]
pub enum TeamCmd {
    /// One line per lane: its state, last round, spend today and session.
    Status,
    /// Start every lane (or the ones named) that is not held, paused or running, in the background.
    Start { names: Vec<String> },
    /// Ask lanes to end after the round in flight (`--now`: at once).
    Stop {
        names: Vec<String>,
        #[arg(long)]
        now: bool,
    },
    /// No lane starts a round until `resume` (a round in flight finishes).
    Pause,
    /// Let the lanes run their rounds again.
    Resume,
    /// `start` skips this lane until `unhold`.
    Hold { name: String },
    /// Let `start` start this lane again.
    Unhold { name: String },
    /// Stop the lane at its next idle point and open its session in the interface.
    Open { name: String },
    #[command(hide = true)]
    /// The runner of one lane (what `start` launches).
    Run { name: String },
}

/// The lanes of `team.toml`; a file that does not parse is an error, keys nexo does not read are warned about.
fn lanes() -> Result<TeamConfig, String> {
    let f = nexo_core::config::team_file();
    let text = match std::fs::read_to_string(&f) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("no lanes: {} does not exist (see docs/TEAM.md)", f.display()))
        }
        Err(e) => return Err(format!("cannot read {}: {e}", f.display())),
    };
    let read = TeamConfig::parse(&text).map_err(|e| format!("{}: {e}", f.display()))?;
    if let Some(u) = read.unread {
        eprintln!("nexo: {}: {u}", f.display());
    }
    Ok(read.team)
}

/// The lanes `names` asks for (all of them when empty), by their names as written in the file.
fn chosen<'a>(team: &'a TeamConfig, names: &[String]) -> Result<Vec<&'a Lane>, String> {
    if names.is_empty() {
        return Ok(team.lane.iter().collect());
    }
    names.iter().map(|n| team.get(n).ok_or_else(|| format!("no lane {n:?} in team.toml"))).collect()
}

/// The lane's folder: `~` is the home folder.
pub fn lane_dir(lane: &Lane) -> PathBuf {
    let d = lane.dir.trim();
    match (d.strip_prefix("~"), nexo_core::paths::user_home()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            home.join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(d),
    }
}

pub async fn run(cmd: &TeamCmd) -> i32 {
    let team = match lanes() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("nexo team: {e}");
            return 2;
        }
    };
    match cmd {
        TeamCmd::Status => status(&team),
        TeamCmd::Start { names } => with_lanes(&team, names, start),
        TeamCmd::Stop { names, now } => with_lanes(&team, names, |l| stop(l, *now)),
        TeamCmd::Pause => {
            marker(&state::paused_file(), true, "paused: no lane starts a round until `nexo team resume`")
        }
        TeamCmd::Resume => marker(&state::paused_file(), false, "resumed"),
        TeamCmd::Hold { name } => match team.get(name) {
            Some(l) => marker(&state::hold_file(&l.name), true, &format!("held: {}", l.name)),
            None => no_lane(name),
        },
        TeamCmd::Unhold { name } => match team.get(name) {
            Some(l) => marker(&state::hold_file(&l.name), false, &format!("unheld: {}", l.name)),
            None => no_lane(name),
        },
        TeamCmd::Open { name } => match team.get(name) {
            Some(l) => open(l).await,
            None => no_lane(name),
        },
        TeamCmd::Run { name } => match team.get(name) {
            Some(l) => runner::run(l).await,
            None => no_lane(name),
        },
    }
}

fn no_lane(name: &str) -> i32 {
    eprintln!("nexo team: no lane {name:?} in team.toml");
    2
}

fn with_lanes(team: &TeamConfig, names: &[String], each: impl Fn(&Lane) -> i32) -> i32 {
    match chosen(team, names) {
        Ok(ls) => ls.into_iter().map(each).max().unwrap_or(0),
        Err(e) => {
            eprintln!("nexo team: {e}");
            2
        }
    }
}

/// Creates (`on`) or removes a marker file and says `said`.
fn marker(path: &std::path::Path, on: bool, said: &str) -> i32 {
    let r = if on {
        state::touch(path)
    } else {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    };
    match r {
        Ok(()) => {
            println!("{said}");
            0
        }
        Err(e) => {
            eprintln!("nexo team: {}: {e}", path.display());
            1
        }
    }
}

fn start(lane: &Lane) -> i32 {
    let name = &lane.name;
    if state::held(name) {
        println!("held: {name}");
        return 0;
    }
    if state::paused() {
        println!("paused: {name} (`nexo team resume` first)");
        return 0;
    }
    let mut st = LaneState::load(name);
    if st.live_pid(name).is_some() {
        println!("already running: {name}");
        return 0;
    }
    // a stop asked of a runner that is gone would end the new one at once
    let _ = std::fs::remove_file(state::stop_file(name));
    match spawn::runner(name) {
        Ok(pid) => {
            // recorded now, so a second start right after sees this runner before it has written anything
            st.pid = Some(pid);
            st.state = "idle".into();
            st.why = None;
            if let Err(e) = st.save(name) {
                eprintln!("nexo team: {name}: cannot write its state: {e}");
            }
            println!("started: {name}");
            0
        }
        Err(e) => {
            eprintln!("nexo team: {name}: cannot start its runner: {e}");
            1
        }
    }
}

fn stop(lane: &Lane, now: bool) -> i32 {
    let name = &lane.name;
    let mut st = LaneState::load(name);
    let Some(pid) = st.live_pid(name) else {
        let _ = std::fs::remove_file(state::stop_file(name));
        println!("not running: {name}");
        return 0;
    };
    if let Err(e) = state::touch(&state::stop_file(name)) {
        eprintln!("nexo team: {name}: {e}");
        return 1;
    }
    if !now {
        println!("stopping: {name} (after the round in flight)");
        return 0;
    }
    if !state::kill_runner(pid, name) {
        eprintln!("nexo team: {name}: could not end process {pid}");
        return 1;
    }
    let _ = std::fs::remove_file(state::stop_file(name));
    st.pid = None;
    st.next_round = None;
    st.state = "stopped".into();
    let _ = st.save(name);
    println!("stopped: {name}");
    0
}

/// `HH:MM` of `ms` in local time.
fn clock(ms: i64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(ms / 1000).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let t = nexo_agent::budget::local_offset().map_or(t, |o| t.to_offset(o));
    format!("{:02}:{:02}", t.hour(), t.minute())
}

/// `45s`, `12m`, `3h`, `2d`.
fn age(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

/// What a lane is doing, as `status` says it.
fn state_word(name: &str, st: &LaneState, alive: bool) -> String {
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
            Some(at) => format!("idle until {}", clock(at)),
            None => "idle (mail only)".into(),
        },
    }
}

/// The line `status` prints for a lane.
fn status_line(name: &str, st: &LaneState, alive: bool, now: i64, today: &str) -> String {
    let last = match &st.last_round {
        Some(r) => format!("last {} {} ago", r.result, age(now - r.at)),
        None => "no round yet".into(),
    };
    format!(
        "{name:<12} {:<24} {last:<20} ${:.2} today  {}",
        state_word(name, st, alive),
        st.spent_on(today),
        st.session_id.as_deref().unwrap_or("-")
    )
}

fn status(team: &TeamConfig) -> i32 {
    let now = nexo_core::mailbox::now_ms();
    let today = nexo_agent::budget::today();
    if team.lane.is_empty() {
        println!("no lanes in {}", nexo_core::config::team_file().display());
    }
    for l in &team.lane {
        let st = LaneState::load(&l.name);
        println!("{}", status_line(&l.name, &st, st.live_pid(&l.name).is_some(), now, &today));
    }
    if state::paused() {
        println!("(all lanes paused: `nexo team resume`)");
    }
    0
}

/// Asks the runner to stop at its next idle point, waits for it, then opens the interface on the lane's session.
async fn open(lane: &Lane) -> i32 {
    let name = &lane.name;
    if LaneState::load(name).live_pid(name).is_some() {
        if let Err(e) = state::touch(&state::stop_file(name)) {
            eprintln!("nexo team: {name}: {e}");
            return 1;
        }
        eprintln!("nexo team: waiting for {name} to reach an idle point (up to 60 s)");
        let mut gone = false;
        for _ in 0..240 {
            if LaneState::load(name).live_pid(name).is_none() {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        if !gone {
            eprintln!("nexo team: {name} is still in its round; it stops when the round ends, then try again");
            return 1;
        }
    }
    let Some(id) = LaneState::load(name).session_id else {
        eprintln!("nexo team: {name} has no session yet; `nexo team start {name}` first");
        return 1;
    };
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("nexo team: {e}");
            return 1;
        }
    };
    let ran = std::process::Command::new(exe).args(["--resume", &id]).current_dir(lane_dir(lane)).status();
    if let Err(e) = ran {
        eprintln!("nexo team: cannot open the interface: {e}");
        return 1;
    }
    println!("nexo team start {name} to put it back in the background");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_line_says_what_the_lane_does_and_spent() {
        let mut st = LaneState { session_id: Some("s-1".into()), ..Default::default() };
        st.spend("2026-11-15", 0.13);
        st.last_round = Some(state::Round { at: 1_000, result: "ok".into(), spend: 0.1 });
        st.state = "running".into();
        let line = status_line("WINDOWS", &st, true, 181_000, "2026-11-15");
        assert!(line.starts_with("WINDOWS      running a round"), "{line}");
        assert!(line.contains("last ok 3m ago") && line.contains("$0.13 today") && line.ends_with("s-1"), "{line}");
        st.state = "idle".into();
        st.next_round = None;
        assert!(status_line("W", &st, true, 0, "x").contains("idle (mail only)"));
        st.state = "failed".into();
        st.why = Some("bad key".into());
        let line = status_line("W", &st, false, 0, "2026-11-16");
        assert!(line.contains("failed: bad key") && line.contains("$0.00 today"), "{line}");
        st.state = "stopped".into();
        assert!(status_line("W", &st, false, 0, "x").contains("stopped"));
    }

    #[test]
    fn a_lane_folder_starting_with_a_tilde_is_in_the_home_folder() {
        let lane = |d: &str| Lane { dir: d.into(), ..Default::default() };
        let home = nexo_core::paths::user_home().unwrap();
        assert_eq!(lane_dir(&lane("~/work/x")), home.join("work/x"));
        assert_eq!(lane_dir(&lane("~")), home);
        assert_eq!(lane_dir(&lane("/srv/a")), PathBuf::from("/srv/a"));
        assert_eq!(lane_dir(&lane("~other/a")), PathBuf::from("~other/a"));
    }
}
