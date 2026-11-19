//! `run` words: launch options, `--repeat` and starting the session.

use super::*;

const RUN_USAGE: &str = "usage: run [--main] [--env K=V]... [--unset K]... [--cwd DIR] [--aslr] [--argv0 NAME] [--break SPEC]... [--repeat N --until crash|exit|output~TEXT [--wait SECS]] <path> [args] [< file | <<< text]";

/// The most runs one `--repeat` makes.
const MAX_REPEAT: u32 = 1000;

/// What ends a `--repeat`: the first run that crashes, exits with a code other than 0, or prints a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Until {
    Crash,
    Exit,
    Output(String),
}

impl Until {
    /// `crash`, `exit` or `output~TEXT` (the text is matched as it is, not as a pattern).
    pub fn parse(s: &str) -> Result<Until, Error> {
        match s {
            "crash" => Ok(Until::Crash),
            "exit" => Ok(Until::Exit),
            _ => match s.strip_prefix("output~") {
                Some(t) if !t.is_empty() => Ok(Until::Output(t.to_string())),
                _ => Err(Error::Usage(format!("--until takes crash, exit or output~TEXT; {s:?} is none of them"))),
            },
        }
    }

    pub fn label(&self) -> String {
        match self {
            Until::Crash => "crash".into(),
            Until::Exit => "exit".into(),
            Until::Output(t) => format!("output~{t}"),
        }
    }
}

/// The launch words that are not about the environment: breakpoints to set at the start, and the repeat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepeatOpts {
    /// Runs to make (already capped at [`MAX_REPEAT`]), and what was asked for.
    pub n: Option<u32>,
    pub asked: u64,
    pub until: Option<Until>,
    /// Seconds each run may take.
    pub wait: Option<u64>,
    /// `--break SPEC`: set before every run goes on.
    pub breaks: Vec<String>,
}

/// How many leading words of a `run` are launch options (`--main`, `--env K=V`, `--unset K`, `--cwd DIR`,
/// `--aslr`, `--argv0 NAME`), and what they set. The program path is the word after them.
pub fn launch_flags(
    words: &[String],
    to_main: &mut bool,
    opts: &mut LaunchOpts,
    rep: &mut RepeatOpts,
) -> Result<usize, Error> {
    let mut i = 0;
    let value = |i: usize, flag: &str| -> Result<&String, Error> {
        words.get(i + 1).ok_or_else(|| Error::Usage(format!("{flag} needs a value: {RUN_USAGE}")))
    };
    while let Some(w) = words.get(i) {
        match w.as_str() {
            "--main" => *to_main = true,
            "--aslr" => opts.aslr = true,
            "--env" => {
                let v = value(i, "--env")?;
                let Some((k, val)) = v.split_once('=').filter(|(k, _)| !k.is_empty()) else {
                    return Err(Error::Usage(format!("--env takes NAME=VALUE; {v:?} has no name before an =")));
                };
                if k.contains(['\0', '=']) || val.contains('\0') {
                    return Err(Error::Usage(format!("{k:?} cannot be an environment variable (a zero byte)")));
                }
                // the later word wins, as in a shell
                opts.env.retain(|(n, _)| n != k);
                opts.unset.retain(|n| n != k);
                opts.env.push((k.to_string(), val.to_string()));
                i += 1;
            }
            "--unset" => {
                let k = value(i, "--unset")?;
                if k.is_empty() || k.contains(['\0', '=']) {
                    return Err(Error::Usage(format!("--unset takes a variable name; {k:?} is not one")));
                }
                opts.env.retain(|(n, _)| n != k);
                if !opts.unset.contains(k) {
                    opts.unset.push(k.clone());
                }
                i += 1;
            }
            "--cwd" => {
                let d = PathBuf::from(value(i, "--cwd")?);
                let abs = if d.is_relative() { std::env::current_dir().map(|h| h.join(&d)).unwrap_or(d) } else { d };
                if !abs.is_dir() {
                    return Err(Error::Usage(format!("--cwd {} is not a folder", abs.display())));
                }
                opts.cwd = Some(abs);
                i += 1;
            }
            "--argv0" => {
                let a = value(i, "--argv0")?;
                if a.is_empty() || a.contains('\0') {
                    return Err(Error::Usage("--argv0 takes the name the program sees as its own (not empty)".into()));
                }
                opts.argv0 = Some(a.clone());
                i += 1;
            }
            "--repeat" => {
                let v = value(i, "--repeat")?;
                let Some(n) = v.parse::<u64>().ok().filter(|n| *n >= 1) else {
                    return Err(Error::Usage(format!("--repeat takes a number of runs, 1 or more; {v:?} is not one")));
                };
                rep.asked = n;
                rep.n = Some(n.min(MAX_REPEAT as u64) as u32);
                i += 1;
            }
            "--until" => {
                rep.until = Some(Until::parse(value(i, "--until")?)?);
                i += 1;
            }
            "--wait" => {
                let v = value(i, "--wait")?;
                let max = crate::cmds::MAX_WAIT;
                let Some(s) = v.parse::<u64>().ok().filter(|s| (1..=max).contains(s)) else {
                    return Err(Error::Usage(format!("--wait takes seconds, 1 to {max}; {v:?} is not that")));
                };
                rep.wait = Some(s);
                i += 1;
            }
            "--break" => {
                let v = value(i, "--break")?;
                if v.trim().is_empty() {
                    return Err(Error::Usage("--break takes a place: an address, a name or file:line".into()));
                }
                rep.breaks.push(v.clone());
                i += 1;
            }
            _ => break,
        }
        i += 1;
    }
    Ok(i)
}

/// What `run` was given: `run [--main] [--env K=V] [--unset K] [--cwd DIR] [--aslr] [--argv0 NAME] <path> [args] [< file | <<< text]`.
pub(super) struct RunWords {
    pub(super) opts: LaunchOpts,
    pub(super) exe: String,
    pub(super) args: Vec<String>,
    /// What goes to the program's input before it runs; the input is closed after it.
    pub(super) input: Option<Vec<u8>>,
    pub(super) to_main: bool,
    pub(super) rep: RepeatOpts,
}

/// The input is read up to this much; a pipe holds about this much before the program reads it.
const MAX_INPUT: u64 = 60 << 10;

/// The first piece of shell syntax in `s` (command substitution, parameter expansion, a variable): the debugger
/// has no shell, so text holding it would reach the program literally. `cost $5` and a lone `$` are plain text.
fn shell_syntax(s: &str) -> Option<&'static str> {
    if s.contains("$(") {
        return Some("`$(`");
    }
    if s.contains('`') {
        return Some("a backtick");
    }
    if s.contains("${") {
        return Some("`${`");
    }
    let b = s.as_bytes();
    if b.windows(2).any(|w| w[0] == b'$' && (w[1].is_ascii_alphabetic() || w[1] == b'_')) {
        return Some("`$VAR`");
    }
    None
}

pub(super) fn run_words(words: &[String]) -> Result<RunWords, Error> {
    let (mut to_main, mut opts, mut rep) = (false, LaunchOpts::default(), RepeatOpts::default());
    let flags = launch_flags(words, &mut to_main, &mut opts, &mut rep)?;
    if rep.n.is_none() && (rep.until.is_some() || rep.wait.is_some()) {
        return Err(Error::Usage("--until and --wait go with --repeat N".into()));
    }
    if rep.n.is_some() && rep.until.is_none() {
        return Err(Error::Usage("--repeat N needs --until crash|exit|output~TEXT: what ends the repeat".into()));
    }
    if rep.n.is_some() && to_main {
        return Err(Error::Usage("--repeat cannot go with --main: use --break main".into()));
    }
    let Some((exe, rest)) = words[flags..].split_first() else {
        return Err(Error::Usage(RUN_USAGE.into()));
    };
    let mut args: Vec<String> = rest.iter().filter(|x| *x != "--").cloned().collect();
    let mut input = None;
    if let Some(i) = args.iter().position(|x| x == "<" || x == "<<<") {
        let Some(src) = args.get(i + 1).cloned() else {
            return Err(Error::Usage(format!("{} needs a file or a text after it", args[i])));
        };
        input = Some(if args[i] == "<<<" {
            if args.len() > i + 2 {
                return Err(Error::Usage(format!(
                    "<<< takes one text: put all of it in quotes (<<< \"{} ...\"); the words after it would reach the program as arguments",
                    src
                )));
            }
            if let Some(syntax) = shell_syntax(&src) {
                return Err(Error::Usage(format!(
                    "the input after <<< is given to the program as typed, without a shell: {src:?} would arrive as those characters ({syntax} is shell syntax). Write the bytes with escapes: \\n \\t \\0 \\\\ \\\" and \\xNN for any byte (e.g. <<< \"3 20 X\\n\" or <<< \"A\\x00\\xff\\n\"), or put them in a file and use < file"
                )));
            }
            let mut v = crate::stdin::decode(&src).map_err(|e| Error::Usage(format!("the input after <<<: {e}")))?;
            v.push(b'\n');
            v
        } else {
            let mut b = Vec::new();
            let f = std::fs::File::open(&src).map_err(|e| Error::Fail(format!("cannot read {src}: {e}")))?;
            std::io::Read::read_to_end(&mut std::io::Read::take(f, MAX_INPUT + 1), &mut b)?;
            if b.len() as u64 > MAX_INPUT {
                return Err(Error::Fail(format!(
                    "{src} is over {} KiB: start with `run <path>` and feed it with `stdin <text>` in parts",
                    MAX_INPUT >> 10
                )));
            }
            b
        });
        args.drain(i..i + 2);
    }
    Ok(RunWords { exe: exe.clone(), args, input, to_main, opts, rep })
}

/// Start the program of `kind` as a session stopped at its entry: the first answer, and the lines about the input
/// it was given and the breakpoints set (`--break`).
pub(super) fn launch(
    dir: &Path,
    host: &Host,
    kind: &Kind,
    input: &Option<Vec<u8>>,
    breaks: &[String],
) -> Result<(String, Reply, Vec<String>), Error> {
    let id = match host {
        Host::Process(me) => crate::spawn(me, dir, kind)?,
        Host::Thread => crate::spawn_thread(dir, kind)?,
    };
    let r = crate::request(dir, &id, &["__first".to_string()])?;
    let mut extra = Vec::new();
    if let Some(bytes) = input {
        let mut w = vec!["stdin".to_string(), "--hex".into(), "--raw".into(), crate::rsp::hex(bytes)];
        let sent = crate::request(dir, &id, &w)?;
        extra.extend(sent.out.rows.into_iter().map(|r| format!("{} and closed the input", r.text)));
        w.truncate(1);
        w.push("--close".into());
        crate::request(dir, &id, &w)?;
    }
    for b in breaks {
        let mut w = vec!["break".to_string()];
        w.extend(crate::split_words(b));
        match crate::request(dir, &id, &w) {
            Ok(set) => extra.extend(set.out.rows.into_iter().map(|r| r.text)),
            Err(e) => {
                close_session(dir, &id);
                return Err(e);
            }
        }
    }
    Ok((id, r, extra))
}

/// End a session and wait until its file is gone, as `end` does; a failure is not worth a word here.
fn close_session(dir: &Path, id: &str) {
    let _ = crate::request(dir, id, &["__end".to_string()]);
    for _ in 0..200 {
        if !crate::sessions(dir).iter().any(|s| s.0 == id) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// How one run of a `--repeat` ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Outcome {
    Exit(i32),
    /// Ended by a signal the debugger did not stop on.
    Killed(String),
    /// Stopped on a fault (the session's crash stop, with its triage).
    Crash(String),
    /// Any other stop: a breakpoint, a watch, a hook, a catch.
    Stop(String),
    Running,
}

impl Outcome {
    /// Read from an answer of `cont`: the state word, the event of its stop and its rows.
    pub(super) fn of(state: &str, event: Option<&str>, lines: &[String]) -> Outcome {
        if state == "running" {
            return Outcome::Running;
        }
        match event {
            Some("exit") => Outcome::Exit(
                lines
                    .iter()
                    .find_map(|l| l.split("exited with code ").nth(1))
                    .and_then(|c| c.trim().parse().ok())
                    .unwrap_or(-1),
            ),
            Some("killed") => Outcome::Killed(
                lines
                    .iter()
                    .find_map(|l| l.split("killed by ").nth(1))
                    .map_or_else(|| "a signal".to_string(), |s| s.trim().to_string()),
            ),
            Some("signal") => Outcome::Crash(
                lines
                    .first()
                    .and_then(|l| l.split(" (").next())
                    .map_or_else(|| "fault".to_string(), |s| s.trim().to_string()),
            ),
            Some(e) => Outcome::Stop(e.to_string()),
            None => Outcome::Stop("stop".to_string()),
        }
    }

    /// The word the tally counts it under.
    fn label(&self) -> String {
        match self {
            Outcome::Exit(c) => format!("exit {c}"),
            Outcome::Killed(s) => format!("killed by {s}"),
            Outcome::Crash(s) | Outcome::Stop(s) => s.clone(),
            Outcome::Running => "still running".into(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    /// The condition hit.
    Matched,
    /// An ordinary end that is not the condition: the next run follows.
    Next,
    /// A stop the person asked for (a breakpoint, a watch): the repeat ends there.
    Stopped,
    /// Not over within its time.
    Running,
}

pub(super) fn verdict(until: &Until, o: &Outcome, output: &str) -> Verdict {
    match o {
        Outcome::Running => Verdict::Running,
        Outcome::Stop(_) => Verdict::Stopped,
        _ => {
            let hit = match until {
                Until::Crash => matches!(o, Outcome::Killed(_) | Outcome::Crash(_)),
                Until::Exit => matches!(o, Outcome::Exit(c) if *c != 0),
                Until::Output(t) => output.contains(t.as_str()),
            };
            if hit {
                Verdict::Matched
            } else {
                Verdict::Next
            }
        }
    }
}

/// What the runs ended with, counted, in the order first seen.
#[derive(Debug, Default)]
pub(super) struct Tally(Vec<(String, u32)>);

impl Tally {
    pub(super) fn add(&mut self, label: &str) {
        match self.0.iter_mut().find(|(l, _)| l == label) {
            Some((_, c)) => *c += 1,
            None => self.0.push((label.to_string(), 1)),
        }
    }

    /// `earlier runs: 7 x exit 0, 2 x SIGSEGV`
    pub(super) fn line(&self, word: &str) -> String {
        if self.0.is_empty() {
            return format!("{word}: none");
        }
        let parts: Vec<String> = self.0.iter().map(|(l, c)| format!("{c} x {l}")).collect();
        format!("{word}: {}", parts.join(", "))
    }
}

pub(super) fn none_matched_line(done: u32, label: &str) -> String {
    format!("{done} run{}, none matched {label}", if done == 1 { "" } else { "s" })
}

/// `--repeat N --until COND`: start the program from scratch up to N times, each with the same input and
/// breakpoints, going on to its end or its first stop. It returns the last run's session and answer, with the lines
/// that tell how the repeat went in front: that run is left live exactly as a normal stop or exit leaves it.
pub(super) fn run_repeated(
    dir: &Path,
    host: &Host,
    kind: &Kind,
    input: &Option<Vec<u8>>,
    rep: &RepeatOpts,
) -> Result<(String, Reply, Vec<String>), Error> {
    use std::time::{Duration, Instant};
    let n = rep.n.unwrap_or(1);
    let until = rep.until.clone().unwrap_or(Until::Exit);
    let label = until.label();
    let budget = Duration::from_secs(crate::cmds::MAX_WAIT);
    let started = Instant::now();
    let mut tally = Tally::default();
    let mut head: Vec<String> = Vec::new();
    if rep.asked > n as u64 {
        head.push(format!("--repeat {} is capped to {n} runs", rep.asked));
    }
    for k in 1..=n {
        let (id, _, _) = launch(dir, host, kind, input, &rep.breaks)?;
        let left = budget.saturating_sub(started.elapsed()).as_secs().max(1);
        let wait = rep.wait.unwrap_or(crate::cmds::DEFAULT_WAIT).min(left);
        let go = vec!["cont".to_string(), "--wait".into(), wait.to_string()];
        let r = match crate::request(dir, &id, &go) {
            Ok(r) => r,
            Err(e) => {
                close_session(dir, &id);
                return Err(e);
            }
        };
        let lines: Vec<String> = r.out.rows.iter().map(|x| x.text.clone()).collect();
        let event = r.stop.as_ref().filter(|_| r.fresh).map(|s| s.event.as_str());
        let outcome = Outcome::of(&r.state, event, &lines);
        let mut output = String::new();
        if matches!(until, Until::Output(_)) && outcome != Outcome::Running {
            if let Ok(o) = crate::request(dir, &id, &["out".to_string(), "--all".into()]) {
                let text: Vec<&str> = o.out.rows.iter().map(|x| x.text.as_str()).collect();
                if text != ["no new output"] {
                    output = text.join("\n");
                }
            }
        }
        match verdict(&until, &outcome, &output) {
            Verdict::Matched => {
                head.push(format!("run {k} of {n} matched {label}"));
                head.push(tally.line("earlier runs"));
                return Ok((id, r, head));
            }
            Verdict::Stopped => {
                head.push(format!(
                    "run {k} of {n} stopped ({}): a stop you asked for, so the repeat ends here",
                    outcome.label()
                ));
                head.push(tally.line("earlier runs"));
                return Ok((id, r, head));
            }
            Verdict::Running => {
                head.push(format!(
                    "run {k} of {n} was still running after {wait}s, so the repeat ends here (`wait N` goes on, `stop` stops it); {label} not seen yet"
                ));
                head.push(tally.line("earlier runs"));
                return Ok((id, r, head));
            }
            Verdict::Next => {
                tally.add(&outcome.label());
                if k == n {
                    head.push(none_matched_line(k, &label));
                    head.push(tally.line("runs"));
                    return Ok((id, r, head));
                }
                if started.elapsed() >= budget {
                    head.push(format!(
                        "stopped after {k} of {n} runs: the {}s budget of this call is used; {}",
                        budget.as_secs(),
                        none_matched_line(k, &label)
                    ));
                    head.push(tally.line("runs"));
                    return Ok((id, r, head));
                }
                close_session(dir, &id);
            }
        }
    }
    Err(Error::Fail("--repeat made no run".into()))
}
