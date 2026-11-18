//! `nexo team`: named lanes that run on their own. Each lane of `<config_dir>/team.toml` is one long-lived
//! session in a folder of its own, run by a detached `nexo team run <name>` that does the lane's routine on a
//! clock and answers mail between rounds. Closing the terminal does not stop a runner; `stop` does, at the
//! runner's next idle point. What the `/team` window shares with this command lives in `nexo_core::team`.

mod runner;

use std::io::{BufRead, IsTerminal, Write};
use std::time::Duration;

use clap::Subcommand;
use nexo_core::config::{Lane, TeamConfig};
use nexo_core::team::state::{self, LaneState};
use nexo_core::team::{self, spawn, Started, Stopped};

#[derive(Subcommand, Debug)]
pub enum TeamCmd {
    /// Add a lane to team.toml (creating it): asks one question at a time on a terminal, or takes flags.
    Add {
        /// The lane's name (letters, digits, `-`, `_`).
        name: Option<String>,
        /// The folder it works in (default: this folder).
        #[arg(long)]
        dir: Option<String>,
        /// A round every 30s, 15m, 2h; none: mail only.
        #[arg(long)]
        every: Option<String>,
        /// What the lane does each round.
        #[arg(long)]
        routine: Option<String>,
        /// provider/model; none: your default.
        #[arg(long)]
        model: Option<String>,
        /// ask, accept-edits, plan, yolo or read-only (default: accept-edits).
        #[arg(long)]
        mode: Option<String>,
        /// Dollars the lane may spend a day.
        #[arg(long)]
        max_spend_day: Option<f64>,
    },
    /// One line per lane: its state, last round, spend today and session.
    Status,
    /// Start every lane (or the ones named) that is not held, paused or running, in the background.
    Start {
        names: Vec<String>,
        /// Run a round right away, as a lane never run before does; otherwise a lane whose last round is
        /// less than `every` ago waits for the rest of it.
        #[arg(long)]
        now: bool,
    },
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
    Run {
        name: String,
        #[arg(long)]
        now: bool,
    },
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
    if let TeamCmd::Add { name, dir, every, routine, model, mode, max_spend_day } = cmd {
        let given = Given { name, dir, every, routine, model, mode, max_spend_day: *max_spend_day };
        return add_lane(given, std::io::stdin().is_terminal());
    }
    let team = match lanes() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("nexo team: {e}");
            return 2;
        }
    };
    match cmd {
        TeamCmd::Add { .. } => unreachable!("handled before the lanes are read"),
        TeamCmd::Status => status(&team),
        TeamCmd::Start { .. } if team.lane.is_empty() => no_lanes_yet(1),
        TeamCmd::Start { names, now } => with_lanes(&team, names, |l| start(l, *now)),
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
        TeamCmd::Run { name, now } => match team.get(name) {
            Some(l) => runner::run(l, *now).await,
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

fn start(lane: &Lane, now: bool) -> i32 {
    let name = &lane.name;
    let launch = if now { spawn::runner_now } else { spawn::runner };
    match team::start(name, launch) {
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
        return no_lanes_yet(0);
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

/// What a person with no lanes is told, and how the command ends (`code`).
fn no_lanes_yet(code: i32) -> i32 {
    println!("{}", team::add::no_lanes_help(&nexo_core::config::team_file()));
    code
}

/// What `nexo team add` was given on the command line.
struct Given<'a> {
    name: &'a Option<String>,
    dir: &'a Option<String>,
    every: &'a Option<String>,
    routine: &'a Option<String>,
    model: &'a Option<String>,
    mode: &'a Option<String>,
    max_spend_day: Option<f64>,
}

/// `nexo team add`: the lane from the flags, and from questions for whatever the flags left out when `tty`.
fn add_lane(given: Given, tty: bool) -> i32 {
    let mut lane = Lane { max_spend_day: given.max_spend_day, ..Lane::default() };
    let flags = [
        (team::add::Field::Name, given.name),
        (team::add::Field::Dir, given.dir),
        (team::add::Field::Routine, given.routine),
        (team::add::Field::Every, given.every),
        (team::add::Field::Model, given.model),
        (team::add::Field::Mode, given.mode),
    ];
    let existing = match team::read_lanes() {
        Ok(f) => f.team,
        Err(e) => {
            eprintln!("nexo team: {e}");
            return 2;
        }
    };
    let (stdin, stdout) = (std::io::stdin(), std::io::stdout());
    match fill(&mut lane, &flags, &existing, tty, &mut stdin.lock(), &mut stdout.lock()) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("nexo team: {e}");
            return 2;
        }
    }
    if let Err(e) = team::add::add(&lane) {
        eprintln!("nexo team: {e}");
        return 2;
    }
    if !team::lane_dir(&lane).is_dir() {
        eprintln!("nexo team: note: {} does not exist yet", team::lane_dir(&lane).display());
    }
    // A lane without a cap runs until it is stopped; the runner only enforces one when it is set.
    let cap = if lane.max_spend_day.is_none() {
        "no daily cap (--max-spend-day, or max_spend_day in team.toml); "
    } else {
        ""
    };
    println!("added {}; {cap}start it with: nexo team start {}", lane.name, lane.name);
    0
}

/// Fills `lane` from the flags, and on a terminal asks the rest one question at a time (an empty answer takes
/// the default; one that is refused is asked again). Off a terminal the flags must be enough.
fn fill(
    lane: &mut Lane,
    flags: &[(team::add::Field, &Option<String>)],
    existing: &TeamConfig,
    tty: bool,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<(), String> {
    for (field, flag) in flags {
        let field = *field;
        if let Some(a) = flag {
            team::add::set(lane, field, a);
            if let Some(p) = team::add::problem(existing, lane, field) {
                return Err(format!("{}: {p}", field.label()));
            }
            continue;
        }
        if !tty {
            // nobody to ask: an answer with a default or none needed is left to it, a required one is missing
            if field.required() {
                return Err(format!(
                    "{:?} is needed: give it on the command line (the name as an argument, the routine as --routine) or run this in a terminal",
                    field.label()
                ));
            }
            team::add::set(lane, field, "");
            continue;
        }
        loop {
            let default = field.default().map(|d| format!(" [{d}]")).unwrap_or_default();
            let _ = write!(out, "{} ({}){default}: ", field.label(), field.hint());
            let _ = out.flush();
            let mut line = String::new();
            if input.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                return Err("no answer: input ended before the lane was complete; nothing was written".into());
            }
            let mut tried = lane.clone();
            team::add::set(&mut tried, field, &line);
            match team::add::problem(existing, &tried, field) {
                None => {
                    *lane = tried;
                    break;
                }
                Some(p) => {
                    let _ = writeln!(out, "  {p}");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use team::add::Field;

    fn none() -> Option<String> {
        None
    }

    #[test]
    fn the_questions_come_one_at_a_time_each_with_its_default_and_a_refused_answer_is_asked_again() {
        let mut lane = Lane::default();
        let flags = [
            (Field::Name, &none()),
            (Field::Dir, &none()),
            (Field::Routine, &none()),
            (Field::Every, &none()),
            (Field::Model, &none()),
            (Field::Mode, &none()),
        ];
        // a name with a space is refused, "soon" is no clock, an empty folder, model and mode take their defaults
        let mut input: &[u8] = b"my lane\nDOCS\n\nTidy the docs.\nsoon\n15m\n\n\n";
        let mut out = Vec::new();
        fill(&mut lane, &flags, &TeamConfig::default(), true, &mut input, &mut out).unwrap();
        let said = String::from_utf8(out).unwrap();
        assert_eq!(said.matches("Name (").count(), 2, "{said}");
        assert!(said.contains("letters, digits"), "{said}");
        assert!(said.contains("Folder (the folder it works in) ["), "the default is shown: {said}");
        assert_eq!(said.matches("How often (").count(), 2, "{said}");
        assert_eq!(
            (lane.name.as_str(), lane.routine.as_deref(), lane.every.as_deref()),
            ("DOCS", Some("Tidy the docs."), Some("15m"))
        );
        assert_eq!((lane.model.as_deref(), lane.mode.as_deref()), (None, Some("accept-edits")));
        assert_eq!(lane.dir, std::env::current_dir().unwrap().display().to_string());
    }

    #[test]
    fn off_a_terminal_the_flags_must_name_the_lane_and_its_routine() {
        let mut lane = Lane::default();
        let (n, r) = (Some("A".to_string()), none());
        let flags = [(Field::Name, &n), (Field::Dir, &none()), (Field::Routine, &r)];
        let e = fill(&mut lane, &flags, &TeamConfig::default(), false, &mut &b""[..], &mut Vec::new()).unwrap_err();
        assert!(e.contains("What should the lane do\" is needed"), "{e}");
    }
}
