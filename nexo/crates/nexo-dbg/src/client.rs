//! The caller's side of `nexo dbg`, shared by the command line and the agent's `debug` tool: parse the
//! words, find or start the session, send one command and print the answer.

use std::path::{Path, PathBuf};

use nexo_bin::{Output, DEFAULT_LINES, MIN_LINES};
use serde_json::json;

use crate::{Error, Kind, Reply};

/// One call: our own options taken out of the words, and the command that is left.
#[derive(Debug, Clone)]
pub struct Call {
    pub session: Option<String>,
    pub lines: usize,
    pub from: Option<u64>,
    pub json: bool,
    pub words: Vec<String>,
}

/// Where a new session's server runs.
#[derive(Debug, Clone)]
pub enum Host {
    /// A separate `nexo dbg __serve` process (the command line: the next call is another process).
    Process(PathBuf),
    /// A thread of this process (the agent: the session ends with the run).
    Thread,
}

/// What a `nexo dbg` command does to the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbgClass {
    /// Looks at, or steers, a program the person already let the debugger start.
    Control,
    /// Writes into the program, or attaches to a process that was not started here.
    Change,
    /// Starts a program (judged like running it in the shell), or is not a known command.
    Other,
}

const CONTROL: &[&str] = &[
    "status", "info", "regs", "mem", "x", "print", "p", "args", "locals", "bt", "threads", "thread", "breaks",
    "watches", "maps", "modules", "find", "history", "out", "log", "dis", "disasm", "sessions", "help", "catches",
    "break", "b", "tbreak", "delete", "del", "enable", "disable", "cont", "c", "continue", "step", "s", "next", "n",
    "finish", "until", "wait", "stop", "kill", "detach", "end", "stdin", "catch", "watch", "core", "strace", "ltrace",
];

/// Classify the words after `nexo dbg`. A `do "a; b"` script is as strong as its strongest command.
pub fn dbg_class(words: &[String]) -> DbgClass {
    let mut it = words.iter().peekable();
    while let Some(w) = it.peek() {
        match w.as_str() {
            "-s" | "--session" | "--lines" | "--from" => {
                it.next();
                it.next();
            }
            "--json" => {
                it.next();
            }
            _ => break,
        }
    }
    let Some(cmd) = it.next() else { return DbgClass::Control };
    match cmd.as_str() {
        "attach" | "write" | "setreg" => DbgClass::Change,
        "run" | "__serve" => DbgClass::Other,
        "do" => {
            let rest: Vec<&str> = it.filter(|w| !w.starts_with("--")).map(String::as_str).collect();
            let mut worst = DbgClass::Control;
            for part in rest.join(" ").split(';').map(str::trim).filter(|p| !p.is_empty()) {
                let w: Vec<String> = part.split_whitespace().map(str::to_string).collect();
                match (w.first().map(String::as_str), dbg_class(&w)) {
                    (Some("do"), _) | (_, DbgClass::Other) => return DbgClass::Other,
                    (_, DbgClass::Change) => worst = DbgClass::Change,
                    _ => {}
                }
            }
            worst
        }
        c if CONTROL.contains(&c) => DbgClass::Control,
        _ => DbgClass::Other,
    }
}

/// Our options out of the words (`-s ID`, `--lines N`, `--from ADDR`, `--json`); a program's own options after
/// `run` are left alone.
pub fn parse(words: &[String]) -> Result<Call, Error> {
    let mut o = Call { session: None, lines: DEFAULT_LINES, from: None, json: false, words: Vec::new() };
    let mut it = words.iter();
    while let Some(w) = it.next() {
        let ours = o.words.first().is_none_or(|c| c != "run");
        match w.as_str() {
            "-s" | "--session" if ours => {
                o.session = Some(it.next().ok_or_else(|| Error::Usage("-s needs a session id".into()))?.clone())
            }
            "--lines" if ours => {
                o.lines = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| Error::Usage("--lines needs a number".into()))?
            }
            "--from" if ours => {
                let v = it.next().ok_or_else(|| Error::Usage("--from needs an address".into()))?;
                o.from = Some(nexo_bin::parse_addr(v)?);
            }
            "--json" if ours => o.json = true,
            _ => o.words.push(w.clone()),
        }
    }
    Ok(o)
}

/// What `run` was given: `run [--main] <path> [args] [< file | <<< text]`.
struct RunWords {
    exe: String,
    args: Vec<String>,
    /// What goes to the program's input before it runs; the input is closed after it.
    input: Option<String>,
    to_main: bool,
}

/// The input is read up to this much; a pipe holds about this much before the program reads it.
const MAX_INPUT: u64 = 60 << 10;

fn run_words(words: &[String]) -> Result<RunWords, Error> {
    let mut w = words;
    let to_main = w.first().is_some_and(|x| x == "--main");
    if to_main {
        w = &w[1..];
    }
    let Some((exe, rest)) = w.split_first() else {
        return Err(Error::Usage("usage: run [--main] <path> [args] [< file | <<< text]".into()));
    };
    let mut args: Vec<String> = rest.iter().filter(|x| *x != "--").cloned().collect();
    let mut input = None;
    if let Some(i) = args.iter().position(|x| x == "<" || x == "<<<") {
        let Some(src) = args.get(i + 1).cloned() else {
            return Err(Error::Usage(format!("{} needs a file or a text after it", args[i])));
        };
        input = Some(if args[i] == "<<<" {
            format!("{}\n", src.replace("\\n", "\n").replace("\\t", "\t"))
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
            String::from_utf8(b).map_err(|_| {
                Error::Fail(format!("{src} is not text: send its bytes with `write`, or feed text with `stdin`"))
            })?
        });
        args.drain(i..i + 2);
    }
    Ok(RunWords { exe: exe.clone(), args, input, to_main })
}

fn pick_session(dir: &Path, want: &Option<String>) -> Result<String, Error> {
    if let Some(s) = want {
        return Ok(s.clone());
    }
    let live = crate::sessions(dir);
    // a session whose program ended still answers (out, history), but it is never the one meant by default
    let running: Vec<&(String, String, u64)> = live.iter().filter(|s| crate::server::alive(s.2 as u32)).collect();
    match (live.len(), running.len()) {
        (1, _) => Ok(live[0].0.clone()),
        (0, _) => Err(Error::Fail("no debug session: use `run <path>` or `attach <pid>`".into())),
        (_, 1) => Ok(running[0].0.clone()),
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
    let page = out.page_as(o.from, o.lines.max(MIN_LINES), o.json);
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
        "run" | "attach" | "core" => {
            let (mut input, mut to_main) = (None, false);
            let kind = if cmd == "core" {
                let Some(file) = o.words.get(1) else {
                    return Err(Error::Usage("usage: core <file> [--exe <path>]".into()));
                };
                let exe = o.words.iter().position(|w| w == "--exe").and_then(|i| o.words.get(i + 1)).map(PathBuf::from);
                Kind::Core { file: PathBuf::from(file), exe }
            } else if cmd == "run" {
                let r = run_words(&o.words[1..])?;
                input = r.input;
                to_main = r.to_main;
                Kind::Run { exe: PathBuf::from(r.exe), args: r.args }
            } else {
                let pid = o
                    .words
                    .get(1)
                    .and_then(|p| p.parse().ok())
                    .ok_or_else(|| Error::Usage("usage: attach <pid>".into()))?;
                Kind::Attach(pid)
            };
            let id = match host {
                Host::Process(me) => crate::spawn(me, dir, &kind)?,
                Host::Thread => crate::spawn_thread(dir, &kind)?,
            };
            let mut r = crate::request(dir, &id, &["__first".to_string()])?;
            let mut extra = Vec::new();
            if let Some(bytes) = input {
                let mut w = vec!["stdin".to_string(), "--exact".into(), "--raw".into(), bytes];
                let sent = crate::request(dir, &id, &w)?;
                extra.extend(sent.out.rows.into_iter().map(|r| format!("{} and closed the input", r.text)));
                w.truncate(1);
                w.push("--close".into());
                crate::request(dir, &id, &w)?;
            }
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
            Ok(print(r.out, &id, &["status".to_string()], o, Some(head), &r.state, prefix))
        }
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
        _ => {
            let id = pick_session(dir, &o.session)?;
            let r = crate::request(dir, &id, &o.words)?;
            let head = header(&id, &r, cmd);
            Ok(print(r.out, &id, &o.words, o, Some(head), &r.state, prefix))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn our_options_are_taken_out_of_the_command() {
        let o = parse(&words("-s abc bt 5 --lines 20 --json")).unwrap();
        assert_eq!(o.session.as_deref(), Some("abc"));
        assert_eq!(o.lines, 20);
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
    }
}
