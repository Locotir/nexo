//! `nexo team`: lanes run by the binary itself against the mock provider, in temporary folders.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MOCK: &str =
    "provider = \"mock\"\nmodel = \"mock\"\n[providers.mock]\nprotocol = \"mock\"\nmodels = [\"mock\"]\n";

/// A config and data folder, a work folder for the lanes, and `team.toml` with `lanes` (where `{work}` is the
/// work folder). `script`: the mock plays these replies instead of echoing.
fn setup(lanes: &str, script: Option<&str>) -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let work = d.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let mut cfg = MOCK.to_string();
    if let Some(s) = script {
        let path = d.path().join("script.json");
        std::fs::write(&path, s).unwrap();
        cfg.push_str(&format!("[providers.mock.extra]\nscript = {:?}\n", path.to_string_lossy()));
    }
    cfg.push_str("[context]\npolicy = \"plain\"\n[agent]\nretries = 0\n");
    std::fs::write(d.path().join("config.toml"), cfg).unwrap();
    let work_s = work.to_string_lossy().replace('\\', "/");
    std::fs::write(d.path().join("team.toml"), lanes.replace("{work}", &work_s)).unwrap();
    (d, work)
}

fn nexo(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_nexo"));
    c.current_dir(dir)
        .env("NEXO_CONFIG_DIR", dir)
        .env("NEXO_DATA_DIR", dir.join("data"))
        .env("NEXO_CACHE_DIR", dir.join("cache"));
    c
}

/// A runner the test started: ended by its own handle when the test ends, a failed test included.
struct Held(Child);

impl Drop for Held {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// `nexo team run <name>` in the foreground of the test, its output in `<dir>/<name>.out`.
fn runner(dir: &Path, name: &str) -> Held {
    let out = std::fs::File::create(dir.join(format!("{name}.out"))).unwrap();
    Held(
        nexo(dir)
            .args(["team", "run", name])
            .stdin(Stdio::null())
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .spawn()
            .unwrap(),
    )
}

fn output(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(format!("{name}.out"))).unwrap_or_default()
}

fn state(dir: &Path, name: &str) -> serde_json::Value {
    std::fs::read_to_string(dir.join("data/team").join(format!("{name}.json")))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn wait_for(secs: u64, what: &str, mut ok: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while !ok() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Waits for `child` to end (it is ended by its own pid if it does not).
fn ends(held: &mut Held, secs: u64) -> std::process::ExitStatus {
    let child = &mut held.0;
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        if Instant::now() > end {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the runner did not end");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The sessions kept (their lock files aside).
fn sessions(dir: &Path) -> usize {
    std::fs::read_dir(dir.join("data/sessions"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .count()
}

fn session_text(dir: &Path) -> String {
    let mut all = String::new();
    for e in std::fs::read_dir(dir.join("data/sessions")).unwrap().flatten() {
        all.push_str(&std::fs::read_to_string(e.path()).unwrap_or_default());
    }
    all
}

#[test]
fn a_lane_runs_its_routine_on_a_clock_in_one_session_takes_mail_pauses_and_stops_when_asked() {
    let (d, work) = setup(
        "[[lane]]\nname = \"ALPHA\"\ndir = \"{work}\"\nevery = \"1s\"\nroutine = \"Look at the orders.\"\n",
        None,
    );
    let dir = d.path();
    let mut child = runner(dir, "ALPHA");
    wait_for(60, "two rounds", || state(dir, "ALPHA")["rounds"].as_u64() >= Some(2));
    let st = state(dir, "ALPHA");
    assert_eq!(st["pid"].as_u64(), Some(child.0.id() as u64), "{st}");
    assert_eq!(st["last_round"]["result"], "ok", "{st}");
    assert_eq!(sessions(dir), 1, "every round in one session");
    assert!(output(dir, "ALPHA").contains("echo: Look at the orders."), "{}", output(dir, "ALPHA"));

    // mail sent to the lane by its name is answered between rounds
    let me = nexo_core::mailbox::Mailbox::open(
        &dir.join("data/mail"),
        "tester-0000-0000",
        "tester",
        &work.to_string_lossy(),
        false,
        nexo_core::mail::Inbound::Accept,
    )
    .unwrap();
    me.send("ALPHA", "Are you there?", None, nexo_core::mail::FromKind::Agent).unwrap();
    wait_for(60, "the mail's turn", || session_text(dir).contains("Are you there?"));
    me.close();

    // paused, no round starts
    std::fs::write(dir.join("data/team/paused"), b"").unwrap();
    wait_for(30, "the pause", || state(dir, "ALPHA")["state"] == "paused");
    let rounds = state(dir, "ALPHA")["rounds"].as_u64();
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(state(dir, "ALPHA")["rounds"].as_u64(), rounds, "no round while paused");
    std::fs::remove_file(dir.join("data/team/paused")).unwrap();
    wait_for(30, "a round after resume", || state(dir, "ALPHA")["rounds"].as_u64() > rounds);

    std::fs::write(dir.join("data/team/ALPHA.stop"), b"").unwrap();
    let s = ends(&mut child, 60);
    assert!(s.success(), "{}", output(dir, "ALPHA"));
    let st = state(dir, "ALPHA");
    assert_eq!(st["state"], "stopped", "{st}");
    assert!(st["pid"].is_null(), "{st}");
    assert!(!dir.join("data/team/ALPHA.stop").exists(), "the stop asked is used up");
    assert_eq!(sessions(dir), 1);

    // started again, the lane goes on in the same session
    let id = st["session_id"].clone();
    let mut child = runner(dir, "ALPHA");
    let before = st["rounds"].as_u64().unwrap();
    wait_for(60, "a round of the second runner", || state(dir, "ALPHA")["rounds"].as_u64() > Some(before));
    std::fs::write(dir.join("data/team/ALPHA.stop"), b"").unwrap();
    assert!(ends(&mut child, 60).success());
    assert_eq!(state(dir, "ALPHA")["session_id"], id);
    assert_eq!(sessions(dir), 1);
}

#[test]
fn three_failed_rounds_in_a_row_end_the_lane_with_the_reason() {
    let errs =
        "[[{\"error\":\"upstream exploded\"}],[{\"error\":\"upstream exploded\"}],[{\"error\":\"upstream exploded\"}]]";
    let (d, _) = setup("[[lane]]\nname = \"B\"\ndir = \"{work}\"\nevery = \"1s\"\nroutine = \"Go.\"\n", Some(errs));
    let dir = d.path();
    let mut child = runner(dir, "B");
    let s = ends(&mut child, 90);
    assert_eq!(s.code(), Some(1), "{}", output(dir, "B"));
    let st = state(dir, "B");
    assert_eq!(st["state"], "failed", "{st}");
    assert_eq!(st["failures"], 3, "{st}");
    assert_eq!(st["last_round"]["result"], "failed", "{st}");
    assert!(st["why"].as_str().is_some_and(|w| w.contains("mock had an internal error")), "{st}");
    let shown = nexo(dir).args(["team", "status"]).output().unwrap();
    let text = String::from_utf8_lossy(&shown.stdout);
    assert!(text.contains("failed: mock had an internal error"), "{text}");
}

#[test]
fn a_lane_that_cannot_start_says_why_and_a_bad_team_file_is_refused() {
    let (d, _) = setup("[[lane]]\nname = \"C\"\ndir = \"{work}/nowhere\"\nroutine = \"Go.\"\n", None);
    let dir = d.path();
    let out = nexo(dir).args(["team", "run", "C"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let st = state(dir, "C");
    assert_eq!(st["state"], "failed");
    assert!(st["why"].as_str().is_some_and(|w| w.contains("nowhere")), "{st}");

    std::fs::write(dir.join("team.toml"), "[[lane]]\nname = \"bad name\"\ndir = \"x\"\nroutine = \"Go.\"\n").unwrap();
    let out = nexo(dir).args(["team", "status"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("team.toml"));
    let out = nexo(dir).args(["team", "start", "Z"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// `start` puts the runner in a session of its own: one runner however often it is asked, a hangup to the group
/// that started it does not reach it, and `stop` ends it at its next idle point.
#[cfg(unix)]
#[test]
fn start_detaches_one_runner_that_outlives_a_hangup_and_stop_ends_it() {
    let (d, _) = setup(
        "[[lane]]\nname = \"D\"\ndir = \"{work}\"\nevery = \"1h\"\nroutine = \"Go.\"\n\
         [[lane]]\nname = \"E\"\ndir = \"{work}\"\nroutine = \"Go.\"\n",
        None,
    );
    let dir = d.path();
    // whatever happens in the test, the detached runner is asked to end when it does
    struct StopOnDrop(PathBuf);
    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::write(&self.0, b"");
        }
    }
    let _stop = StopOnDrop(dir.join("data/team/D.stop"));
    std::fs::create_dir_all(dir.join("data/team/hold")).unwrap();
    std::fs::write(dir.join("data/team/hold/E"), b"").unwrap();
    // the starting command runs in a process group of its own, which then gets a hangup, as a closed terminal does
    let starter = {
        use std::os::unix::process::CommandExt;
        let mut c = nexo(dir);
        c.args(["team", "start"]).stdout(Stdio::piped()).process_group(0);
        c.spawn().unwrap()
    };
    let group = starter.id() as i32;
    let out = starter.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("started: D") && said.contains("held: E"), "{said}");
    // SAFETY: the group is the one this test just made for the starter; nothing else is in it but its children
    unsafe { libc::kill(-group, libc::SIGHUP) };
    wait_for(60, "the first round", || state(dir, "D")["last_round"]["result"] == "ok");
    let pid = state(dir, "D")["pid"].as_u64().unwrap() as i32;
    // SAFETY: signal 0 only checks that the process exists
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0, "alive after the hangup");
    // SAFETY: reads only
    assert_eq!(unsafe { libc::getsid(pid) }, pid, "the runner leads a session of its own");

    let again = nexo(dir).args(["team", "start", "D"]).output().unwrap();
    assert!(String::from_utf8_lossy(&again.stdout).contains("already running: D"));
    assert_eq!(state(dir, "D")["pid"].as_u64(), Some(pid as u64));

    let status = nexo(dir).args(["team", "status"]).output().unwrap();
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(text.contains("idle until") && text.contains("last ok"), "{text}");
    assert!(text.lines().any(|l| l.starts_with('E') && l.contains("held")), "{text}");

    let stop = nexo(dir).args(["team", "stop", "D"]).output().unwrap();
    assert!(String::from_utf8_lossy(&stop.stdout).contains("stopping: D"));
    wait_for(60, "the runner's end", || state(dir, "D")["state"] == "stopped");
    assert!(std::fs::read_to_string(dir.join("data/team/D.log")).unwrap().contains("echo: Go."));
}

/// `start --now` runs the first round at once and leaves a runner the other commands still know: a second `start`
/// finds it running, `status` does not call it stopped, and `stop` reaches it.
#[cfg(unix)]
#[test]
fn start_now_runs_a_round_at_once_and_the_runner_stays_known() {
    let (d, _) = setup("[[lane]]\nname = \"N\"\ndir = \"{work}\"\nevery = \"1h\"\nroutine = \"Go.\"\n", None);
    let dir = d.path();
    struct StopOnDrop(PathBuf);
    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::write(&self.0, b"");
        }
    }
    let _stop = StopOnDrop(dir.join("data/team/N.stop"));
    std::fs::create_dir_all(dir.join("data/team")).unwrap();
    let last = nexo_clock_ms() - 10_000;
    let st = format!("{{\"state\":\"stopped\",\"last_round\":{{\"at\":{last},\"result\":\"ok\",\"spend\":0.0}}}}");
    std::fs::write(dir.join("data/team/N.json"), st).unwrap();

    let out = nexo(dir).args(["team", "start", "--now", "N"]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("started: N"), "{out:?}");
    wait_for(60, "the round asked for", || state(dir, "N")["rounds"].as_u64() >= Some(1));
    assert!(!dir.join("data/team/N.now").exists(), "the runner took the request away");

    let again = nexo(dir).args(["team", "start", "N"]).output().unwrap();
    assert!(String::from_utf8_lossy(&again.stdout).contains("already running: N"), "{again:?}");
    let status = nexo(dir).args(["team", "status"]).output().unwrap();
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(!text.contains("stopped") && text.contains("idle until"), "{text}");

    let stop = nexo(dir).args(["team", "stop", "N"]).output().unwrap();
    assert!(String::from_utf8_lossy(&stop.stdout).contains("stopping: N"), "{stop:?}");
    wait_for(60, "the runner's end", || state(dir, "N")["state"] == "stopped");
}

/// With no `team.toml`, `status` and `start` say how to make a lane, with an example, and never point at a doc
/// an installed copy does not have; `status` still ends well.
#[test]
fn without_lanes_status_and_start_say_how_to_add_one() {
    let d = tempfile::tempdir().unwrap();
    for (args, code) in [(["team", "status"], 0), (["team", "start"], 1)] {
        let out = nexo(d.path()).args(args).output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(code), "{args:?}: {said}");
        assert!(said.starts_with("No lanes yet. Create one with: nexo team add\n"), "{said}");
        assert!(said.contains("[[lane]]") && said.contains("routine = "), "{said}");
        assert!(!said.contains("TEAM.md") && !String::from_utf8_lossy(&out.stderr).contains("TEAM.md"), "{said}");
        assert!(!said.contains("does not exist"), "{said}");
    }
    assert!(!d.path().join("team.toml").exists(), "asking does not create the file");
}

/// `add` with flags makes the file, appends a second lane under what is already there, comments included, and
/// the lanes read back as `status` lists them; a lane that does not check out leaves the file as it was.
#[test]
fn add_with_flags_creates_team_toml_then_appends_and_keeps_the_comments() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let add = |args: &[&str]| nexo(dir).args(["team", "add"]).args(args).stdin(Stdio::null()).output().unwrap();
    let work_s = work.to_string_lossy().to_string();

    let out = add(&["DOCS", "--dir", &work_s, "--routine", "Tidy the docs.", "--every", "1h"]);
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{said} {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        said.trim(),
        "added DOCS; no daily cap (--max-spend-day, or max_spend_day in team.toml); start it with: nexo team start DOCS"
    );
    let file = std::fs::read_to_string(dir.join("team.toml")).unwrap();
    assert!(
        file.contains("name = \"DOCS\"") && file.contains("every = \"1h\"") && file.contains("mode = \"accept-edits\""),
        "{file}"
    );

    // the person's own comment goes in; the next lane is added under it
    let mine = format!("# my lanes\n{file}");
    std::fs::write(dir.join("team.toml"), &mine).unwrap();
    let out = add(&[
        "API",
        "--dir",
        &work_s,
        "--routine",
        "Watch the build.",
        "--mode",
        "plan",
        "--max-spend-day",
        "2",
        "--model",
        "mock/mock",
    ]);
    let said = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        said.trim(),
        "added API; start it with: nexo team start API",
        "a lane with a cap does not say it has none"
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let after = std::fs::read_to_string(dir.join("team.toml")).unwrap();
    assert!(after.starts_with(&mine), "what was there stays as it was:\n{after}");
    assert!(after.contains("name = \"API\"") && after.contains("max_spend_day = 2.0"), "{after}");
    let status = nexo(dir).args(["team", "status"]).output().unwrap();
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(text.lines().any(|l| l.starts_with("DOCS")) && text.lines().any(|l| l.starts_with("API")), "{text}");

    // a taken name, a clock that is not one and a missing routine write nothing
    for bad in [vec!["docs", "--routine", "x"], vec!["NEW", "--routine", "x", "--every", "soon"], vec!["NEW"]] {
        let out = add(&bad);
        assert_eq!(out.status.code(), Some(2), "{bad:?}");
        assert!(!out.stderr.is_empty());
    }
    let out = add(&["NEW", "--routine", "x", "--every", "soon"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("every") && !err.contains("lane _"), "the stand-in lane name stays out of the message: {err}");
    assert_eq!(std::fs::read_to_string(dir.join("team.toml")).unwrap(), after);
}

/// A restart does not run a round the clock has not asked for: a lane whose last round was 10 s ago with
/// `every = "1h"` waits (and says when), and `--now` runs one at once.
#[test]
fn a_restarted_lane_waits_for_its_clock_and_runs_a_round_at_once_only_when_asked() {
    let (d, _) = setup(
        "[[lane]]\nname = \"LATE\"\ndir = \"{work}\"\nevery = \"1h\"\nroutine = \"Go.\"\n\
         [[lane]]\nname = \"NOW\"\ndir = \"{work}\"\nevery = \"1h\"\nroutine = \"Go.\"\n",
        None,
    );
    let dir = d.path();
    let last = nexo_clock_ms() - 10_000;
    for name in ["LATE", "NOW"] {
        std::fs::create_dir_all(dir.join("data/team")).unwrap();
        let st = format!("{{\"state\":\"stopped\",\"last_round\":{{\"at\":{last},\"result\":\"ok\",\"spend\":0.0}}}}");
        std::fs::write(dir.join("data/team").join(format!("{name}.json")), st).unwrap();
    }
    let late = runner(dir, "LATE");
    wait_for(60, "the runner to be up", || {
        let st = state(dir, "LATE");
        st["started"].is_number() && st["next_round"].is_number()
    });
    let st = state(dir, "LATE");
    let due = st["next_round"].as_i64().unwrap();
    assert!((due - (last + 3_600_000)).abs() < 5_000, "the round is due an hour after the last one: {st}");
    std::thread::sleep(Duration::from_millis(1500));
    let st = state(dir, "LATE");
    assert_eq!(st["last_round"]["at"].as_i64(), Some(last), "no round ran: {st}");
    assert_eq!(st["rounds"].as_u64().unwrap_or(0), 0, "{st}");
    assert!(
        output(dir, "LATE").contains("min ago)") && output(dir, "LATE").contains("next round at "),
        "{}",
        output(dir, "LATE")
    );
    drop(late);

    let out = std::fs::File::create(dir.join("NOW.out")).unwrap();
    let _now = Held(
        nexo(dir)
            .args(["team", "run", "NOW", "--now"])
            .stdin(Stdio::null())
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .spawn()
            .unwrap(),
    );
    wait_for(60, "the round that was asked for", || state(dir, "NOW")["rounds"].as_u64() >= Some(1));
    assert!(output(dir, "NOW").contains("echo: Go."), "{}", output(dir, "NOW"));
}

fn nexo_clock_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}
