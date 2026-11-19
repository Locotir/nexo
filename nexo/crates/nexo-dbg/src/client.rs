//! The caller's side of `nexo dbg`, shared by the command line and the agent's `debug` tool: parse the
//! words, find or start the session, send one command and print the answer.

use std::path::{Path, PathBuf};

use nexo_bin::{Lines, Output, PageAsk, MIN_LINES};
use serde_json::json;

use crate::platform::LaunchOpts;
use crate::{Error, Kind, Reply};

mod class;
mod runs;

pub use class::*;
pub use runs::*;

/// One call: our own options taken out of the words, and the command that is left.
#[derive(Debug, Clone)]
pub struct Call {
    pub session: Option<String>,
    pub lines: Option<Lines>,
    pub from: Option<u64>,
    pub grep: Option<String>,
    pub json: bool,
    pub words: Vec<String>,
}

impl Call {
    /// The page this call asks for: the same `--lines N|all`, `--from`, `--grep` as `nexo bin`.
    pub fn ask(&self) -> PageAsk {
        PageAsk { lines: self.lines, from: self.from, grep: self.grep.clone(), json: self.json, sized: self.sized() }
    }

    /// True when the call names a range or a length (`mem A..B`, `mem A 4096`, `dis A 40`, `find .. --in RANGE`):
    /// without `--lines` its page is all of it.
    fn sized(&self) -> bool {
        let w = &self.words;
        match w.first().map(String::as_str) {
            Some("mem" | "x" | "dis" | "disasm") => {
                let pos: Vec<&String> = w[1..].iter().filter(|x| !x.starts_with("--")).collect();
                pos.len() > 1 || pos.first().is_some_and(|p| nexo_bin::is_span(p))
            }
            Some("find") => w.iter().any(|x| x == "--in"),
            _ => false,
        }
    }
}

/// Where a new session's server runs.
#[derive(Debug, Clone)]
pub enum Host {
    /// A separate `nexo dbg __serve` process (the command line: the next call is another process).
    Process(PathBuf),
    /// A thread of this process (the agent: the session ends with the run).
    Thread,
}

/// Our options out of the words (`-s ID`, `--lines N|all`, `--from ADDR`, `--grep RE`, `--json`); a program's own options after
/// `run` are left alone.
pub fn parse(words: &[String]) -> Result<Call, Error> {
    let mut o = Call { session: None, lines: None, from: None, grep: None, json: false, words: Vec::new() };
    let mut it = words.iter();
    while let Some(w) = it.next() {
        let ours = o.words.first().is_none_or(|c| c != "run");
        match w.as_str() {
            "-s" | "--session" if ours => {
                o.session = Some(it.next().ok_or_else(|| Error::Usage("-s needs a session id".into()))?.clone())
            }
            "--lines" if ours => {
                let v = it.next().ok_or_else(|| Error::Usage("--lines needs a number or all".into()))?;
                o.lines = Some(Lines::parse(v)?);
            }
            "--grep" if ours => {
                o.grep =
                    Some(it.next().ok_or_else(|| Error::Usage("--grep needs a regular expression".into()))?.clone())
            }
            "--from" if ours => {
                let v = it.next().ok_or_else(|| Error::Usage("--from needs an address".into()))?;
                o.from = Some(nexo_bin::parse_addr(v)?);
            }
            "--json" if ours => o.json = true,
            _ => o.words.push(w.clone()),
        }
    }
    o.ask().check()?;
    // the session may run in another folder: the file a `dump` writes is named from the caller's
    if let Some(i) = crate::dump_path_at(&o.words) {
        let p = PathBuf::from(&o.words[i]);
        if p.is_relative() {
            if let Ok(d) = std::env::current_dir() {
                o.words[i] = d.join(p).display().to_string();
            }
        }
    }
    // gdb's way out: `quit` as a call of its own closes the session, as `end` does
    if matches!(o.words.as_slice(), [w] if matches!(w.as_str(), "quit" | "q" | "exit")) {
        o.words[0] = "end".to_string();
    }
    Ok(o)
}

/// The session a call without `-s` means: the only one, or the one whose program still runs.
pub fn pick_session(dir: &Path, want: &Option<String>) -> Result<String, Error> {
    if let Some(s) = want {
        return Ok(s.clone());
    }
    let live = crate::sessions(dir);
    // a session whose program ended still answers (out, history), but it is never the one meant by default
    let running: Vec<&(String, String, u64)> = live.iter().filter(|s| crate::server::alive(s.2 as u32)).collect();
    // a forked child kept by `follow` is reached with `-s`; the session that started the program stays the default
    let pool: Vec<&(String, String, u64)> = if running.is_empty() { live.iter().collect() } else { running.clone() };
    let started: Vec<&&(String, String, u64)> =
        pool.iter().filter(|s| crate::server::parent_of(dir, &s.0).is_none()).collect();
    match (live.len(), running.len()) {
        (1, _) => Ok(live[0].0.clone()),
        (0, _) => Err(Error::Fail("no debug session: use `run <path>` or `attach <pid>`".into())),
        (_, 1) => Ok(running[0].0.clone()),
        _ if started.len() == 1 => Ok(started[0].0.clone()),
        _ => Err(Error::Fail(format!(
            "more than one debug session is live ({}): add `-s <id>` to the command, or `end -s <id>` the others",
            live.iter().map(session_text).collect::<Vec<_>>().join(", ")
        ))),
    }
}

/// `ab12cd ./prog`, with ` (ended)` when its program is gone.
fn session_text(s: &(String, String, u64)) -> String {
    let name = s.1.rsplit('/').next().unwrap_or(&s.1);
    let ended = if crate::server::alive(s.2 as u32) { "" } else { " (ended)" };
    format!("{} {name}{ended}", s.0)
}

fn shell_word(s: &str) -> String {
    let plain = !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-:+=,@%".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn print(
    mut out: Output,
    id: &str,
    words: &[String],
    o: &Call,
    head: Option<String>,
    state: &str,
    prefix: &str,
) -> String {
    out.command = format!("{prefix} -s {id} {}", words.iter().map(|x| shell_word(x)).collect::<Vec<_>>().join(" "));
    // an answer names a `bin` call in the tool's form; from the shell the same call is `nexo bin`
    if prefix != "debug" {
        for r in out.rows.iter_mut().filter(|r| r.text.contains("`bin ")) {
            r.text = r.text.replace("`bin ", "`nexo bin ");
        }
    }
    let ask = o.ask();
    let page = ask.page(&out).unwrap_or_else(|_| out.page_as(o.from, ask.budget(), o.json));
    if o.json {
        let mut v = page.value;
        if let Some(m) = v.as_object_mut() {
            m.insert("session".into(), json!(id));
            m.insert("state".into(), json!(state));
            m.insert("text".into(), json!(page.lines.join("\n")));
        }
        return serde_json::to_string_pretty(&v).unwrap_or_default();
    }
    let mut text = String::new();
    if let Some(h) = head {
        text.push_str(&h);
        text.push('\n');
    }
    text.push_str(&page.text());
    text
}

/// The answer of a stop in the program's own code, ended with the `bin` call that decompiles the function it is in
/// (the tool's form under the `debug` tool, the shell's form under `nexo dbg`). Not in JSON, and not when the
/// answer already fills the lines the caller asked for.
fn with_bridge(mut text: String, bridge: Option<(String, u64)>, o: &Call, prefix: &str) -> String {
    let Some((exe, addr)) = bridge else { return text };
    if o.json || text.lines().count() >= o.ask().budget().max(MIN_LINES) {
        return text;
    }
    let call = if prefix == "debug" { "bin" } else { "nexo bin" };
    text.push_str(&format!("\nto read this function: {call} decomp {} {addr:#x}", shell_word(&exe)));
    text
}

/// The first line of every answer, always in this shape so a model (and the curator) can find it:
/// `debug <session> #<stop number> stop <event> <place> <0xaddress>` when the call ended in a stop, else
/// `debug <session> #<stop number> <command>` with ` running` or ` exited` after it when the program is not stopped.
pub fn header(id: &str, r: &Reply, cmd: &str) -> String {
    header_of(id, r.stop.as_ref(), &r.state, r.fresh, cmd)
}

fn header_of(id: &str, stop: Option<&crate::StopInfo>, state: &str, fresh: bool, cmd: &str) -> String {
    let n = stop.map_or(0, |s| s.n);
    match stop {
        Some(s) if fresh && state != "exited" => {
            format!("debug {id} #{n} stop {} {} {:#x}", s.event, s.place.replace(' ', "_"), s.addr)
        }
        Some(s) if fresh => format!("debug {id} #{n} stop {}", s.event),
        _ => match state {
            "running" | "exited" => format!("debug {id} #{n} {cmd} {state}"),
            _ => format!("debug {id} #{n} {cmd}"),
        },
    }
}

/// What `header` says, read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub session: String,
    pub n: u64,
    /// `Some((event, place, address))` for a stop line.
    pub stop: Option<(String, String, u64)>,
    /// The command and the state word for any other line.
    pub cmd: String,
    pub state: String,
}

pub fn parse_header(line: &str) -> Option<Header> {
    let w: Vec<&str> = line.split_whitespace().collect();
    if w.len() < 4 || w[0] != "debug" {
        return None;
    }
    let n = w[2].strip_prefix('#')?.parse().ok()?;
    let mut h = Header { session: w[1].into(), n, stop: None, cmd: String::new(), state: "stopped".into() };
    if w[3] == "stop" {
        let event = w.get(4)?.to_string();
        if let (Some(p), Some(a)) = (w.get(5), w.get(6)) {
            h.stop = Some((event, p.to_string(), u64::from_str_radix(a.trim_start_matches("0x"), 16).ok()?));
        } else {
            h.stop = Some((event, String::new(), 0));
            h.state = "exited".into();
        }
    } else {
        h.cmd = w[3].into();
        if let Some(s) = w.get(4) {
            h.state = s.to_string();
        }
    }
    Some(h)
}

/// Run one call. `prefix` is how a continuation line names the command (`nexo dbg`, or `debug`).
pub fn execute(o: &Call, dir: &Path, host: &Host, prefix: &str) -> Result<String, Error> {
    let Some(cmd) = o.words.first().map(String::as_str) else {
        return Ok(print(crate::help(), "-", &["help".to_string()], o, None, "", prefix));
    };
    match cmd {
        "run" | "attach" | "core" | "connect" => {
            let (mut input, mut to_main, mut rep) = (None, false, RepeatOpts::default());
            let kind = if cmd == "connect" {
                let Some(addr) = o.words.get(1) else {
                    return Err(Error::Usage("usage: connect <host:port> [--exe <path>]  (a GDB remote stub: gdbserver, qemu -g, a probe)".into()));
                };
                let exe = o.words.iter().position(|w| w == "--exe").and_then(|i| o.words.get(i + 1)).map(PathBuf::from);
                Kind::Remote { addr: addr.clone(), exe }
            } else if cmd == "core" {
                let Some(file) = o.words.get(1) else {
                    return Err(Error::Usage("usage: core <file> [--exe <path>]".into()));
                };
                let exe = o.words.iter().position(|w| w == "--exe").and_then(|i| o.words.get(i + 1)).map(PathBuf::from);
                Kind::Core { file: PathBuf::from(file), exe }
            } else if cmd == "run" {
                let r = run_words(&o.words[1..])?;
                input = r.input;
                to_main = r.to_main;
                rep = r.rep;
                if r.opts.is_default() {
                    Kind::Run { exe: PathBuf::from(r.exe), args: r.args }
                } else {
                    Kind::RunWith { exe: PathBuf::from(r.exe), args: r.args, opts: r.opts }
                }
            } else {
                let pid = o
                    .words
                    .get(1)
                    .and_then(|p| p.parse().ok())
                    .ok_or_else(|| Error::Usage("usage: attach <pid>".into()))?;
                Kind::Attach(pid)
            };
            let (id, mut r, mut extra) = if rep.n.is_some() {
                run_repeated(dir, host, &kind, &input, &rep)?
            } else {
                launch(dir, host, &kind, &input, &rep.breaks)?
            };
            if to_main && r.state == "stopped" {
                crate::request(dir, &id, &["tbreak".to_string(), "main".into()])?;
                let started = r.out.rows.first().map(|r| r.text.clone());
                r = crate::request(dir, &id, &["cont".to_string()])?;
                extra.splice(0..0, started);
            }
            for (i, t) in extra.into_iter().enumerate() {
                r.out.rows.insert(i, nexo_bin::Row { key: 0, text: t, json: None });
            }
            for (i, row) in r.out.rows.iter_mut().enumerate() {
                row.key = i as u64;
            }
            let head = header_of(&id, r.stop.as_ref(), &r.state, true, "run");
            let bridge = r.bridge.clone();
            let text = print(r.out, &id, &["status".to_string()], o, Some(head), &r.state, prefix);
            Ok(with_bridge(text, bridge, o, prefix))
        }
        "serve" => Err(Error::Usage(
            "`serve --gdb <port> <program> [args]` (or `--attach <pid>`) is a server that stays up: start it from the shell, in the background".into(),
        )),
        "sessions" => {
            let l = crate::sessions(dir);
            if l.is_empty() {
                return Ok("debug - #0 sessions\nno debug sessions: use `run <path>` or `attach <pid>`".into());
            }
            Ok(format!(
                "debug - #0 sessions\n{}",
                l.iter()
                    .map(|(id, exe, pid)| {
                        let ended = if crate::server::alive(*pid as u32) { "" } else { "  (program ended)" };
                        format!("{id}  pid {pid}  {exe}{ended}")
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            ))
        }
        "end" => {
            let id = pick_session(dir, &o.session)?;
            crate::request(dir, &id, &["__end".to_string()])?;
            // the server removes its file when the program is gone: the next call must not find it half done
            for _ in 0..200 {
                if !crate::sessions(dir).iter().any(|s| s.0 == id) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Ok(format!("debug {id} #0 end\nsession {id} ended: use `run <path>` to start another"))
        }
        // the command list needs no program: it is the same text a live session prints
        "help" => Ok(print(crate::help(), "-", &["help".to_string()], o, None, "", prefix)),
        _ => {
            let id = pick_session(dir, &o.session).map_err(|e| match e {
                Error::Fail(m) if m.starts_with("no debug session:") => Error::Fail(format!(
                    "no debug session for `{}`: start one with `run <path>` (or `attach <pid>`), then send it again; `help` lists the commands",
                    o.words.join(" ")
                )),
                e => e,
            })?;
            let r = crate::request(dir, &id, &o.words)?;
            let head = header(&id, &r, cmd);
            let bridge = r.bridge.clone();
            let text = print(r.out, &id, &o.words, o, Some(head), &r.state, prefix);
            Ok(with_bridge(text, bridge, o, prefix))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rw(line: &[&str]) -> Result<RunWords, Error> {
        run_words(&line.iter().map(|x| x.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn repeat_flags_parse_and_cap() {
        let r = rw(&["--repeat", "5", "--until", "crash", "--break", "main", "./p", "a"]).unwrap();
        assert_eq!((r.rep.n, r.rep.until.clone()), (Some(5), Some(Until::Crash)));
        assert_eq!((r.exe.as_str(), r.args, r.rep.breaks), ("./p", vec!["a".to_string()], vec!["main".to_string()]));
        let r = rw(&["--repeat", "5000", "--until", "exit", "--wait", "30", "./p"]).unwrap();
        assert_eq!((r.rep.n, r.rep.asked, r.rep.wait), (Some(1000), 5000, Some(30)));
        let r = rw(&["--env", "A=1", "--repeat", "2", "--until", "output~boom x", "./p"]).unwrap();
        assert_eq!(r.rep.until, Some(Until::Output("boom x".into())));
        assert_eq!(r.opts.env, [("A".to_string(), "1".to_string())]);
        assert_eq!(rw(&["./p"]).unwrap().rep, RepeatOpts::default());
    }

    #[test]
    fn repeat_flags_are_refused_when_they_do_not_fit() {
        for bad in [
            &["--repeat", "0", "--until", "exit", "./p"][..],
            &["--repeat", "x", "--until", "exit", "./p"],
            &["--repeat", "3", "./p"],
            &["--until", "exit", "./p"],
            &["--wait", "5", "./p"],
            &["--repeat", "3", "--until", "hang", "./p"],
            &["--repeat", "3", "--until", "output~", "./p"],
            &["--repeat", "3", "--until", "exit", "--main", "./p"],
            &["--repeat"],
        ] {
            assert!(matches!(rw(bad), Err(Error::Usage(_))), "{bad:?}");
        }
    }

    #[test]
    fn until_conditions_pick_the_run() {
        let ex = Outcome::Exit;
        let sig = Outcome::Crash("SIGSEGV".into());
        let kill = Outcome::Killed("SIGKILL".into());
        assert_eq!(verdict(&Until::Exit, &ex(0), ""), Verdict::Next);
        assert_eq!(verdict(&Until::Exit, &ex(3), ""), Verdict::Matched);
        assert_eq!(verdict(&Until::Exit, &sig, ""), Verdict::Next);
        assert_eq!(verdict(&Until::Crash, &ex(3), ""), Verdict::Next);
        assert_eq!(verdict(&Until::Crash, &sig, ""), Verdict::Matched);
        assert_eq!(verdict(&Until::Crash, &kill, ""), Verdict::Matched);
        let t = Until::Output("boom".into());
        assert_eq!(verdict(&t, &ex(0), "a\nboom 1"), Verdict::Matched);
        assert_eq!(verdict(&t, &ex(0), "fine"), Verdict::Next);
        // a stop the person asked for ends the repeat whatever the condition; a run still going is not decided
        assert_eq!(verdict(&Until::Crash, &Outcome::Stop("breakpoint".into()), ""), Verdict::Stopped);
        assert_eq!(verdict(&Until::Exit, &Outcome::Running, ""), Verdict::Running);
    }

    #[test]
    fn outcomes_are_read_from_the_answer() {
        let l = |s: &str| vec![s.to_string()];
        assert_eq!(Outcome::of("exited", Some("exit"), &l("the program exited with code 3")), Outcome::Exit(3));
        assert_eq!(
            Outcome::of("exited", Some("killed"), &l("the program was killed by SIGTERM")),
            Outcome::Killed("SIGTERM".into())
        );
        assert_eq!(
            Outcome::of("stopped", Some("signal"), &l("SIGSEGV (signal 11) (thread 4, frame #0)")),
            Outcome::Crash("SIGSEGV".into())
        );
        assert_eq!(Outcome::of("stopped", Some("breakpoint"), &[]), Outcome::Stop("breakpoint".into()));
        assert_eq!(Outcome::of("running", None, &[]), Outcome::Running);
    }

    #[test]
    fn the_tally_and_the_summary_lines() {
        let mut t = Tally::default();
        assert_eq!(t.line("earlier runs"), "earlier runs: none");
        for x in ["exit 0", "exit 0", "SIGSEGV", "exit 0"] {
            t.add(x);
        }
        assert_eq!(t.line("runs"), "runs: 3 x exit 0, 1 x SIGSEGV");
        assert_eq!(none_matched_line(10, "crash"), "10 runs, none matched crash");
        assert_eq!(none_matched_line(1, "output~x"), "1 run, none matched output~x");
    }

    #[test]
    fn here_string_forms() {
        let r = rw(&["./p", "<<<", "3 20 X\\n"]).unwrap();
        assert_eq!(r.input.as_deref(), Some(&b"3 20 X\n\n"[..]));
        assert!(r.args.is_empty());
        let r = rw(&["./p", "a", "<<<", "x\\ty", "--"]).unwrap();
        assert_eq!(r.input.as_deref(), Some(&b"x\ty\n"[..]));
        assert_eq!(r.args, ["a"]);
        // `$` that is not shell syntax goes through
        for t in ["cost $5", "a $ b", "$", "50$", "$?"] {
            assert!(rw(&["./p", "<<<", t]).unwrap().input.unwrap().starts_with(t.as_bytes()), "{t}");
        }
    }

    #[test]
    fn here_string_with_shell_syntax_is_refused_before_the_program_starts() {
        for t in ["$(printf '3 20 X\\n')", "`date`", "${HOME}", "$HOME x", "a $_x"] {
            let Err(Error::Usage(m)) = rw(&["./paint", "<<<", t]) else { panic!("{t} must be refused") };
            assert!(m.starts_with("the input after <<< is given to the program as typed, without a shell:"), "{m}");
            assert!(m.contains("or put them in a file and use < file"), "{m}");
        }
    }

    #[test]
    fn unquoted_multi_word_here_string_is_refused() {
        let Err(Error::Usage(m)) = rw(&["./p", "<<<", "3", "20", "X"]) else { panic!("must be refused") };
        assert!(m.contains("put all of it in quotes"), "{m}");
    }

    #[test]
    fn catch_on_off_and_all_are_refused_like_add_and_del_without_the_flag() {
        for c in [
            "catch on SIGSEGV",
            "catch off SIGSEGV",
            "catch all",
            "catch add SIGSEGV",
            "catches on SIGSEGV",
            "catches all",
            "do catch off 11",
        ] {
            let w = crate::split_words(c);
            assert!(read_only_refusal(&w).is_some(), "{c} must be refused");
        }
        for c in ["catch", "catches", "catch syscall"] {
            assert!(read_only_refusal(&crate::split_words(c)).is_none(), "{c} only reads");
        }
    }

    fn words(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn the_header_line_is_stable_and_reads_back() {
        let stop =
            crate::engine::StopInfo { n: 7, event: "breakpoint".into(), place: "main+0x1a".into(), addr: 0x401136 };
        let r = Reply {
            out: crate::engine::rows("stop", vec![], json!({})),
            state: "stopped".into(),
            stop: Some(stop),
            fresh: true,
            bridge: None,
        };
        let h = header("s1", &r, "cont");
        assert_eq!(h, "debug s1 #7 stop breakpoint main+0x1a 0x401136");
        let p = parse_header(&h).unwrap();
        assert_eq!((p.session.as_str(), p.n), ("s1", 7));
        assert_eq!(p.stop, Some(("breakpoint".into(), "main+0x1a".into(), 0x401136)));
        let r2 = Reply { fresh: false, ..r };
        let h2 = header("s1", &r2, "bt");
        assert_eq!(h2, "debug s1 #7 bt");
        let p2 = parse_header(&h2).unwrap();
        assert_eq!((p2.cmd.as_str(), p2.state.as_str(), p2.stop), ("bt", "stopped", None));
        let r3 = Reply { state: "running".into(), ..r2 };
        assert_eq!(parse_header(&header("s1", &r3, "wait")).unwrap().state, "running");
        assert!(parse_header("session s1").is_none());
    }

    #[test]
    fn a_stop_in_the_program_ends_with_the_bin_call_in_the_form_of_the_surface() {
        let b = Some(("/tmp/my prog".to_string(), 0x1139u64));
        let o = parse(&words("cont")).unwrap();
        let tool = with_bridge("debug s #1 stop".into(), b.clone(), &o, "debug");
        assert_eq!(tool, "debug s #1 stop\nto read this function: bin decomp '/tmp/my prog' 0x1139");
        assert!(with_bridge("x".into(), b.clone(), &o, "nexo dbg").ends_with("nexo bin decomp '/tmp/my prog' 0x1139"));
        // nothing for a stop outside the program, for JSON, or when the lines asked for are filled
        assert_eq!(with_bridge("x".into(), None, &o, "debug"), "x");
        let j = parse(&words("cont --json")).unwrap();
        assert_eq!(with_bridge("x".into(), b.clone(), &j, "debug"), "x");
        let few = parse(&words("cont --lines 10")).unwrap();
        assert_eq!(with_bridge(["l"; 10].join("\n"), b.clone(), &few, "debug").lines().count(), 10);
        assert_eq!(with_bridge(["l"; 9].join("\n"), b, &few, "debug").lines().count(), 10);
    }

    #[test]
    fn help_answers_without_a_session_and_other_commands_name_themselves() {
        let dir = std::env::temp_dir().join(format!("nexo-dbg-nosess-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let o = parse(&words("help")).unwrap();
        let h = execute(&o, &dir, &Host::Thread, "nexo dbg").unwrap();
        assert!(h.contains("break:") && h.contains("run: nexo dbg run"), "{h}");
        let o = parse(&words("help break")).unwrap();
        assert_eq!(execute(&o, &dir, &Host::Thread, "nexo dbg").unwrap(), h);
        let o = parse(&words("break main")).unwrap();
        let e = execute(&o, &dir, &Host::Thread, "nexo dbg").unwrap_err().to_string();
        assert!(e.contains("no debug session for `break main`") && e.contains("`help` lists the commands"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn our_options_are_taken_out_of_the_command() {
        let o = parse(&words("-s abc bt 5 --lines 20 --json")).unwrap();
        assert_eq!(o.session.as_deref(), Some("abc"));
        assert_eq!(o.lines, Some(Lines::Rows(20)));
        assert!(o.json);
        assert_eq!(o.words, words("bt 5"));
    }

    #[test]
    fn a_programs_own_options_are_left_alone() {
        let o = parse(&words("run ./p --json --lines 3")).unwrap();
        assert!(!o.json);
        assert_eq!(o.words, words("run ./p --json --lines 3"));
    }

    #[test]
    fn commands_are_told_apart_by_what_they_do_to_the_world() {
        assert_eq!(dbg_class(&words("regs")), DbgClass::Control);
        assert_eq!(dbg_class(&words("-s ab bt 5")), DbgClass::Control);
        assert_eq!(dbg_class(&words("attach 12")), DbgClass::Change);
        assert_eq!(dbg_class(&words("run ./a")), DbgClass::Other);
        assert_eq!(dbg_class(&words("nosuch")), DbgClass::Other);
        assert_eq!(dbg_class(&["do".into(), "break f; cont; setreg rax 1".into()]), DbgClass::Change);
        assert_eq!(dbg_class(&["do".into(), "break f; run x".into()]), DbgClass::Other);
        // a dump writes a file
        assert_eq!(dbg_class(&words("dump 0x1000..0x2000 out.bin")), DbgClass::Change);
        assert_eq!(dbg_class(&["do".into(), "break f; cont; dump module libc out".into()]), DbgClass::Change);
    }

    #[test]
    fn a_dump_names_its_file_from_the_callers_folder_and_its_bin_call_in_the_surfaces_form() {
        let o = parse(&words("dump $rdi 64 out.bin")).unwrap();
        let here = std::env::current_dir().unwrap().join("out.bin");
        assert_eq!(o.words, ["dump", "$rdi", "64", &here.display().to_string()]);
        // an absolute path stays as given; on Windows `/tmp/a.bin` has no drive, so it is relative there
        let abs = if cfg!(windows) { "C:/tmp/a.bin" } else { "/tmp/a.bin" };
        assert_eq!(parse(&words(&format!("dump $rdi 64 {abs}"))).unwrap().words[3], abs);
        let out = || {
            crate::engine::rows(
                "dump",
                vec!["read it: `bin fn /tmp/a.bin 0x1000 --raw --base 0x1000 --arch x86-64`".into()],
                json!({}),
            )
        };
        let tool = print(out(), "s", &o.words, &o, None, "stopped", "debug");
        assert!(tool.contains("`bin fn /tmp/a.bin"), "{tool}");
        let shell = print(out(), "s", &o.words, &o, None, "stopped", "nexo dbg");
        assert!(shell.contains("`nexo bin fn /tmp/a.bin 0x1000 --raw"), "{shell}");
    }

    #[test]
    fn a_call_into_the_program_is_classed_as_a_change_and_a_cast_is_not() {
        for c in ["call add(2, 3)", "print add(1, 2) + 1", "p hash(\"abc\")", "output f(1)", "call *(0x401000)(1)"] {
            assert_eq!(dbg_class(&crate::split_words(c)), DbgClass::Change, "{c}");
            assert!(read_only_refusal(&crate::split_words(c)).is_some_and(|r| r.contains("runs code")), "{c}");
        }
        for c in ["print (int)x", "print sizeof(int)", "print *(u32*)(rdi+8)", "p (rax + 8) * 2"] {
            assert_eq!(dbg_class(&crate::split_words(c)), DbgClass::Control, "{c}");
            assert!(read_only_refusal(&crate::split_words(c)).is_none(), "{c}");
        }
        assert_eq!(dbg_class(&["do".into(), "break f; cont; call g(1)".into()]), DbgClass::Change);
    }

    fn script(s: &str) -> Vec<String> {
        vec!["do".to_string(), crate::gdbdialect::units(s).join("; ")]
    }

    #[test]
    fn gdb_spellings_are_classed_as_the_commands_they_are() {
        // looking and keeping a value
        for c in [
            "x/20bx $rsp",
            "p/x $rax",
            "print $rax",
            "printf \"%d\", 1",
            "echo hi",
            "info registers rip",
            "display/x $rax",
            "output 1",
        ] {
            assert_eq!(dbg_class(&crate::split_words(c)), DbgClass::Control, "{c}");
        }
        assert_eq!(dbg_class(&crate::split_words("set $v = $rsp + 8")), DbgClass::Control);
        assert_eq!(dbg_class(&crate::split_words("set pagination off")), DbgClass::Control);
        assert_eq!(dbg_class(&crate::split_words("backtrace 3")), DbgClass::Control);
        // the record only keeps what ran and `back` only reads it: neither writes into the program
        // undoing writes memory and registers back: it is a change
        for c in ["rstepi 3", "rcont", "reverse-stepi", "rsi 2", "rc", "reverse-continue"] {
            assert_eq!(dbg_class(&crate::split_words(c)), DbgClass::Change, "{c}");
        }
        for c in ["record", "record 500", "record stop", "back 3", "back --reg rax", "back --mem 0x1000"] {
            assert_eq!(dbg_class(&crate::split_words(c)), DbgClass::Control, "{c}");
        }
        assert!(READ_ALLOWED.contains(&"record") && READ_ALLOWED.contains(&"back"));
        // writing into the program is a change, however it is spelled
        for c in
            ["set $rax = 1", "set var $rip = main", "set var x = 5", "set {int}0x1000 = 5", "set *(int*)0x1000 = 5"]
        {
            assert_eq!(dbg_class(&crate::split_words(c)), DbgClass::Change, "{c}");
        }
        // a script: the strongest part, and the lines of a `commands` block count too
        assert_eq!(
            dbg_class(&script("break *0x10\ncommands\nsilent\nx/4x $rsp\ncontinue\nend\ncont")),
            DbgClass::Control
        );
        assert_eq!(dbg_class(&script("break *0x10\ncommands\nset $rax = 1\ncontinue\nend")), DbgClass::Change);
        assert_eq!(dbg_class(&script("break *0x10\ncommands\nwrite 0x10 90\nend")), DbgClass::Change);
        assert_eq!(dbg_class(&script("file ./p\nrun")), DbgClass::Other);
    }

    #[test]
    fn a_server_that_may_not_change_things_refuses_gdbs_writes_too() {
        for c in [
            "set $rax = 1",
            "set var x = 5",
            "set {int}0x1000 = 5",
            "do \"x/4x $rsp; set $rax = 1\"",
            "do \"break f; commands; set var x = 1; end\"",
            "do \"commands; write 0x10 90; end\"",
        ] {
            assert!(read_only_refusal(&crate::split_words(c)).is_some(), "{c} must be refused");
        }
        for c in [
            "x/20bx $rsp",
            "p/x $rax",
            "printf \"%d\", 1",
            "set $v = 1",
            "info registers",
            "display $rax",
            "do \"break *0x10; commands; silent; x/4x $rsp; continue; end; cont\"",
        ] {
            assert!(
                read_only_refusal(&crate::split_words(c)).is_none(),
                "{c} only reads: {:?}",
                read_only_refusal(&crate::split_words(c))
            );
        }
    }

    #[test]
    fn quit_as_a_call_of_its_own_ends_the_session() {
        assert_eq!(parse(&words("quit")).unwrap().words, words("end"));
        assert_eq!(parse(&words("-s ab q")).unwrap().words, words("end"));
        assert_eq!(parse(&words("bt quit")).unwrap().words, words("bt quit"));
    }
}
