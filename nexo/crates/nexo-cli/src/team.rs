//! `nexo team`: named lanes that run on their own. Each lane of `<config_dir>/team.toml` is one long-lived
//! session in a folder of its own, run by a detached `nexo team run <name>` that does the lane's routine on a
//! clock and answers mail between rounds. Closing the terminal does not stop a runner; `stop` does, at the
//! runner's next idle point. What the `/team` window shares with this command lives in `nexo_core::team`.

mod runner;

use std::time::Duration;

use clap::Subcommand;
use nexo_core::config::{Lane, TeamConfig};
use nexo_core::team::state::{self, LaneState};
use nexo_core::team::{self, spawn, Started, Stopped};

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

/// The lanes of `team.toml`; keys nexo does not read are warned about.
fn lanes() -> Result<TeamConfig, String> {
    let read = team::read_lanes()?;
    if let Some(u) = read.unread {
        eprintln!("nexo: {}: {u}", nexo_core::config::team_file().display());
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
    match team::set_marker(path, on) {
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
    match team::start(name, spawn::runner) {
        Ok(Started::Held) => println!("held: {name}"),
        Ok(Started::Paused) => println!("paused: {name} (`nexo team resume` first)"),
        Ok(Started::AlreadyRunning) => println!("already running: {name}"),
        Ok(Started::Started(_, unsaved)) => {
            if let Some(e) = unsaved {
                eprintln!("nexo team: {name}: cannot write its state: {e}");
            }
            println!("started: {name}");
        }
        Err(e) => {
            eprintln!("nexo team: {name}: cannot start its runner: {e}");
            return 1;
        }
    }
    0
}

fn stop(lane: &Lane, now: bool) -> i32 {
    let name = &lane.name;
    match team::stop(name, now) {
        Ok(Stopped::NotRunning) => println!("not running: {name}"),
        Ok(Stopped::Asked) => println!("stopping: {name} (after the round in flight)"),
        Ok(Stopped::Ended) => println!("stopped: {name}"),
        Err(e) => {
            eprintln!("nexo team: {name}: {e}");
            return 1;
        }
    }
    0
}

/// `HH:MM` of `ms` in local time.
fn clock(ms: i64) -> String {
    team::clock(ms, nexo_agent::budget::local_offset())
}

fn status(team: &TeamConfig) -> i32 {
    let now = nexo_core::mailbox::now_ms();
    let today = nexo_agent::budget::today();
    if team.lane.is_empty() {
        println!("no lanes in {}", nexo_core::config::team_file().display());
    }
    for c in team::table(team, now, &today, nexo_agent::budget::local_offset()) {
        println!("{}", team::status_line(&c));
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
    let ran = std::process::Command::new(exe).args(["--resume", &id]).current_dir(team::lane_dir(lane)).status();
    if let Err(e) = ran {
        eprintln!("nexo team: cannot open the interface: {e}");
        return 1;
    }
    println!("nexo team start {name} to put it back in the background");
    0
}
