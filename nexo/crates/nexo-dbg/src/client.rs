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
    "status",
    "info",
    "regs",
    "mem",
    "x",
    "print",
    "p",
    "args",
    "locals",
    "bt",
    "threads",
    "thread",
    "breaks",
    "watches",
    "maps",
    "modules",
    "find",
    "history",
    "out",
    "log",
    "dis",
    "disasm",
    "sessions",
    "help",
    "catches",
    "break",
    "b",
    "tbreak",
    "delete",
    "del",
    "enable",
    "disable",
    "cont",
    "c",
    "continue",
    "step",
    "s",
    "next",
    "n",
    "finish",
    "until",
    "wait",
    "stop",
    "kill",
    "detach",
    "end",
    "stdin",
    "catch",
    "watch",
    "core",
    "strace",
    "ltrace",
    "hooks",
    "cover",
    "printf",
    "echo",
    "output",
    "show",
    "display",
    "undisplay",
    "condition",
    "ignore",
    "commands",
    "silent",
    "quit",
];

/// Classify the words after `nexo dbg`. A `do "a; b"` script is as strong as its strongest command. gdb's spellings
/// count as the debugger's own (`x/20bx` is `x`, `backtrace` is `bt`); `set $v = ..` only keeps a value, while
/// `set $rax = ..` and `set var x = ..` write into the program.
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
    let rest: Vec<String> = it.cloned().collect();
    let Some(first) = rest.first() else { return DbgClass::Control };
    let head = crate::gdbdialect::base_name(first);
    match head {
        "attach" | "connect" | "write" | "setreg" => DbgClass::Change,
        // a hook that sets an argument or the result changes the program
        "hook" if rest.iter().any(|w| w == "set" || w == "ret") => DbgClass::Change,
        "hook" => DbgClass::Control,
        "set" => match set_effect(&rest) {
            SetEffect::Keeps => DbgClass::Control,
            SetEffect::Writes => DbgClass::Change,
        },
        "run" | "__serve" => DbgClass::Other,
        "do" => {
            let Ok(parts) = do_parts(&rest[1..]) else { return DbgClass::Other };
            let mut worst = DbgClass::Control;
            let mut block = false;
            for w in parts {
                let name = crate::gdbdialect::base_name(&w[0]);
                // `commands` ... `end`: the lines between are kept for the breakpoint, and each is judged like any command
                if !block && name == "commands" {
                    block = true;
                    continue;
                }
                if block && name == "end" {
                    block = false;
                    continue;
                }
                match (name, dbg_class(&w)) {
                    ("do", _) | (_, DbgClass::Other) => return DbgClass::Other,
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

enum SetEffect {
    Keeps,
    Writes,
}

/// What a gdb `set` does: a setting or a convenience variable changes nothing in the program; a register or memory does.
fn set_effect(words: &[String]) -> SetEffect {
    match crate::gdbdialect::set_kind(words, &|n| crate::Regs::default().get(n).is_some()) {
        crate::gdbdialect::SetKind::Noise | crate::gdbdialect::SetKind::Var | crate::gdbdialect::SetKind::Args(_) => {
            SetEffect::Keeps
        }
        crate::gdbdialect::SetKind::Register(..) | crate::gdbdialect::SetKind::Memory => SetEffect::Writes,
    }
}

/// The commands a server that was not given permission to change things may run: they look at the program,
/// or steer one the person already let the debugger start. Everything else (writes, saved names and notes,
/// attach and connect, `calls`, `serve`, a new `run`, unknown words) is refused by name.
const READ_ALLOWED: &[&str] = &[
    "status",
    "info",
    "regs",
    "mem",
    "x",
    "print",
    "p",
    "args",
    "locals",
    "bt",
    "threads",
    "thread",
    "breaks",
    "watches",
    "maps",
    "modules",
    "find",
    "history",
    "out",
    "log",
    "dis",
    "disasm",
    "sessions",
    "help",
    "catches",
    "break",
    "b",
    "tbreak",
    "delete",
    "del",
    "enable",
    "disable",
    "cont",
    "c",
    "continue",
    "step",
    "s",
    "next",
    "n",
    "finish",
    "until",
    "wait",
    "stop",
    "kill",
    "detach",
    "catch",
    "strace",
    "ltrace",
    "hook",
    "hooks",
    "cover",
    "printf",
    "echo",
    "output",
    "show",
    "display",
    "undisplay",
    "condition",
    "ignore",
    "commands",
    "silent",
    "quit",
    "set",
];

/// The commands inside a `do` script, split exactly as the server splits them: the plain words after `do` are
/// joined, cut at `;` and at line ends outside double quotes (`#` lines left out) and each part is split with
/// `split_words` (so a quoted command word is unquoted here too).
pub fn do_parts(words_after_do: &[String]) -> Result<Vec<Vec<String>>, String> {
    let a = crate::Args::new(words_after_do).map_err(|e| e.to_string())?;
    let script = a.pos.join(" ");
    Ok(crate::gdbdialect::units(&script).iter().map(|p| crate::split_words(p)).filter(|w| !w.is_empty()).collect())
}

/// Why a server without permission to change things must not run these words (the command first, our own
/// options already taken out by `parse`), or None when it only reads. It parses with the server's own word
/// splitting and allows only the commands in the read list, so unknown words are refused too.
pub fn read_only_refusal(words: &[String]) -> Option<String> {
    refusal_in(words, 0)
}

fn refusal_in(words: &[String], depth: usize) -> Option<String> {
    let name = crate::gdbdialect::base_name(words.first()?.as_str());
    if name == "do" {
        if depth > 0 {
            return Some("`do` inside `do` is not allowed".to_string());
        }
        return match do_parts(&words[1..]) {
            Err(e) => Some(format!("the script cannot be read ({e})")),
            Ok(parts) => {
                let mut block = false;
                parts.iter().find_map(|p| {
                    let n = crate::gdbdialect::base_name(&p[0]);
                    if !block && n == "commands" {
                        block = true;
                        return None;
                    }
                    if block && n == "end" {
                        block = false;
                        return None;
                    }
                    refusal_in(p, depth + 1)
                })
            }
        };
    }
    if name == "set" && matches!(set_effect(words), SetEffect::Writes) {
        return Some(
            "`set` of a register or of memory writes into the program (`set $name = ...` only keeps a value)"
                .to_string(),
        );
    }
    if !READ_ALLOWED.contains(&name) {
        return Some(match name {
            "write" | "setreg" => format!("`{name}` writes into the program"),
            "name" | "note" => format!("`{name}` saves a name or note"),
            "attach" | "connect" => format!("`{name}` attaches to a process or a remote stub"),
            "run" => "`run` starts a program".to_string(),
            "serve" | "__serve" => format!("`{name}` opens a port or an internal server"),
            "stdin" | "watch" | "calls" | "core" | "end" => format!("`{name}` changes the session or the program"),
            other => {
                format!("`{other}` is not a command this server may run without permission (unknown or not read-only)")
            }
        });
    }
    let rest = &words[1..];
    match name {
        "hook" if rest.iter().any(|w| w == "set" || w == "ret") => {
            Some("a hook that sets an argument or the result changes the program".to_string())
        }
        "catch" | "catches" if rest.first().is_some_and(|w| w != "syscall") => {
            Some("`catch` with add, del, on, off or all changes how signals reach the program".to_string())
        }
        "cover" if rest.first().is_some_and(|w| w == "save") => Some("`cover save` saves a file".to_string()),
        _ => None,
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
    // gdb's way out: `quit` as a call of its own closes the session, as `end` does
    if matches!(o.words.as_slice(), [w] if matches!(w.as_str(), "quit" | "q" | "exit")) {
        o.words[0] = "end".to_string();
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
        "run" | "attach" | "core" | "connect" => {
            let (mut input, mut to_main) = (None, false);
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
