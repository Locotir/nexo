//! Three one-line hints that name the `debug` call doing what the model just did by hand, each once per session:
//!
//! - `gdb` on a program of the project: the same session as one `debug` call (its `-ex` lines, the script of
//!   `-x file` or of a heredoc, with the breakpoints, `x`, `p`, `info registers` it holds);
//! - `strace` or `ltrace` on one: the `strace`/`ltrace` of the debugger, then `cont` and `show`;
//! - a program of the project that died by a fault signal (exit 128+N, a segfault is 139): the run that stops at it.
//!
//! Only when the `debug` tool is offered and the file is one `nexo bin` opens. Nothing is run or loaded: the command
//! is split by `hint_cmd` (heredocs, redirections, `cd x &&`, wrappers) and a gdb script is only read as text. A
//! script line the debugger has no word for is left out of the call (a register is kept, a gdb convenience variable
//! is not); the call is what the line says it is, a way to the same stops, not a promise of every gdb feature, and
//! when the script's every breakpoint, watch and catch was left out there is no line at all: `run; cont` would stop
//! nowhere the script did.

use std::path::Path;

use nexo_core::message::{Message, Part};
use serde_json::Value;

use crate::hint_cmd::{self, Input, Seg};

/// Started every line before there were three kinds, so a session saved then has it for a gdb and for a fault.
pub const HEAD: &str = "[nexo: the debug tool answers this in one call:";
/// Starts the line for a fault.
pub const HEAD_FAULT: &str = "[nexo: the debug tool runs this to the fault in one call:";
/// Starts the line for a gdb session.
pub const HEAD_GDB: &str = "[nexo: the debug tool takes this gdb session in one call:";
/// Starts the line for strace and ltrace.
pub const HEAD_TRACE: &str = "[nexo: the debug tool traces these calls in one call:";

/// Exit codes of a shell whose program died by a fault signal: SIGILL, SIGABRT, SIGBUS, SIGFPE, SIGSEGV.
const FAULTS: [i32; 5] = [132, 134, 135, 136, 139];

/// gdb options that take a value: the word after them is not the program.
const GDB_VALUED: [&str; 18] = [
    "-iex",
    "-ix",
    "-c",
    "-d",
    "-s",
    "-e",
    "-se",
    "-cd",
    "-tty",
    "-b",
    "-l",
    "-i",
    "--core",
    "--directory",
    "--cd",
    "--exec",
    "--symbols",
    "--tty",
];
/// strace options that take a value.
const STRACE_VALUED: [&str; 13] = ["-o", "-s", "-u", "-E", "-a", "-b", "-P", "-I", "-S", "-X", "-O", "-U", "-H"];
/// ltrace options that take a value.
const LTRACE_VALUED: [&str; 12] = ["-o", "-s", "-u", "-a", "-A", "-D", "-F", "-l", "-n", "-w", "-x", "-L"];

/// The most commands of a translated session, and the most characters of the call.
const MAX_CMDS: usize = 12;
const MAX_CALL: usize = 420;

/// Which of the hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Gdb,
    Trace,
    Fault,
}

/// Which hints the session has had.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Told {
    pub gdb: bool,
    pub trace: bool,
    pub fault: bool,
}

impl Told {
    fn has(&self, k: Kind) -> bool {
        match k {
            Kind::Gdb => self.gdb,
            Kind::Trace => self.trace,
            Kind::Fault => self.fault,
        }
    }

    fn set(&mut self, k: Kind) {
        match k {
            Kind::Gdb => self.gdb = true,
            Kind::Trace => self.trace = true,
            Kind::Fault => self.fault = true,
        }
    }

    fn all(&self) -> bool {
        self.gdb && self.trace && self.fault
    }
}

/// The hint a command earns, from its result text, among the kinds not told yet.
pub fn line(command: &str, result: &str, cwd: &Path, told: &Told) -> Option<(Kind, String)> {
    let segs = hint_cmd::segments(command, cwd)?;
    for (k, s) in segs.iter().enumerate() {
        let Some(first) = s.words.first() else { continue };
        let (kind, call) = match hint_cmd::program(first).as_str() {
            "gdb" if !told.gdb => (Kind::Gdb, gdb_call(&segs, k, cwd)),
            "strace" | "ltrace" if !told.trace => (Kind::Trace, trace_call(&segs, k, cwd)),
            _ => continue,
        };
        if let Some(call) = call {
            let head = if kind == Kind::Gdb { HEAD_GDB } else { HEAD_TRACE };
            return Some((kind, format!("{head} {}]", hint_cmd::tool_call("debug", &call))));
        }
    }
    if told.fault {
        return None;
    }
    fault(&segs, result, cwd).map(|l| (Kind::Fault, l))
}

/// A program of the project that died by a fault signal: the run that stops there. Only the last command of the
/// line gives the shell's exit code (`./p | head` says nothing about `./p`).
fn fault(segs: &[Seg], result: &str, cwd: &Path) -> Option<String> {
    let code = exit_code(result).filter(|c| FAULTS.contains(c))?;
    let k = segs.len().checked_sub(1)?;
    let s = &segs[k];
    let prog = s.words.first().filter(|w| hint_cmd::opens(&s.dir, w))?;
    let run = run_text(cwd, s, prog, &s.words[1..], input_for(segs, k).as_deref())?;
    let why = format!("stops at the fault (exit {code}) with file:line, cause and locals");
    Some(format!("{HEAD_FAULT} {} {why}]", hint_cmd::tool_call("debug", &format!("{run}; cont"))))
}

/// A word the call can carry: no double quote (the debug command language quotes with them), no `;`, no newline.
/// For what the shell wrote (the program, its arguments, its input): a `$` there was never expanded.
fn plain(w: &str) -> bool {
    !w.contains(['"', ';', '\n', '\\', '$', '`'])
}

/// A debugger command the call can carry: `plain`, except that a `$name` is fine when it is a register the debugger
/// reads (`$rdi`, `$rsp`, `$r8d`). A gdb convenience variable (`$x` from `set $x = ...`, `$_`, `$1`) has no meaning
/// there, so a command that uses one is left out.
fn plain_cmd(c: &str) -> bool {
    if c.contains(['"', ';', '\n', '\\', '`']) {
        return false;
    }
    let mut rest = c;
    while let Some(at) = rest.find('$') {
        rest = &rest[at + 1..];
        let end = rest.find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_')).unwrap_or(rest.len());
        if !register(&rest[..end]) {
            return false;
        }
        rest = &rest[end..];
    }
    true
}

/// The register names of the debugger's expressions (x86-64): the 64, 32, 16 and 8-bit names, `rip`, the flags and
/// the segments.
fn register(n: &str) -> bool {
    const NAMED: [&str; 46] = [
        "rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "rsp", "rip", "pc", "sp", "fp", "eflags", "rflags", "flags",
        "fs_base", "gs_base", "cs", "ss", "ds", "es", "fs", "gs", "eax", "ebx", "ecx", "edx", "esi", "edi", "ebp",
        "esp", "ax", "bx", "cx", "dx", "si", "di", "bp", "al", "bl", "cl", "dl", "sil", "dil", "bpl", "spl",
    ];
    let n = n.to_ascii_lowercase();
    if NAMED.contains(&n.as_str()) {
        return true;
    }
    // r8 to r15 and their d, w and b parts
    let Some(rest) = n.strip_prefix('r') else { return false };
    let num = rest.trim_end_matches(['d', 'w', 'b']);
    rest.len() - num.len() <= 1 && num.parse::<u8>().is_ok_and(|k| (8..16).contains(&k))
}

/// `run <prog> <args> [< file | <<< "text">]`.
fn run_text(cwd: &Path, s: &Seg, prog: &str, args: &[String], input: Option<&str>) -> Option<String> {
    let shown = hint_cmd::shown(cwd, &s.dir, prog);
    if !plain(&shown) || !args.iter().all(|a| plain(a)) {
        return None;
    }
    let mut run = format!("run {}", hint_cmd::quoted(&shown));
    for a in args {
        run.push(' ');
        run.push_str(&hint_cmd::quoted(a));
    }
    if let Some(i) = input {
        run.push(' ');
        run.push_str(i);
    }
    Some(run)
}

/// What the command at `k` reads, in the debug command language: `< file` or `<<< "text"` (one line).
fn input_for(segs: &[Seg], k: usize) -> Option<String> {
    let direct = segs[k].input.clone().or_else(|| {
        let prev = segs.get(k.checked_sub(1)?)?;
        match hint_cmd::program(prev.words.first()?).as_str() {
            "cat" => prev.input.clone(),
            "echo" | "printf" => echoed(&prev.words).map(Input::Text),
            _ => None,
        }
    })?;
    input_text(&direct)
}

fn input_text(i: &Input) -> Option<String> {
    match i {
        Input::File(f) => plain(f).then(|| format!("< {}", hint_cmd::quoted(f))),
        Input::Text(t) => {
            let t = t.trim_end_matches('\n');
            (!t.contains('\n') && plain(t)).then(|| format!("<<< \"{t}\""))
        }
    }
}

/// `echo a b` or `printf 'a\n'` before the program: the text it fed in.
fn echoed(words: &[String]) -> Option<String> {
    let (cmd, args) = words.split_first()?;
    let args: Vec<&String> = match hint_cmd::program(cmd).as_str() {
        "echo" => args.iter().filter(|a| !a.starts_with('-')).collect(),
        "printf" if args.len() == 1 && !args[0].contains('%') => args.iter().collect(),
        _ => return None,
    };
    let t = args.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ");
    Some(t.trim_end_matches("\\n").to_string())
}

/// The text a command reads as its input: its own heredoc or `<<<`, or what the command before it in a pipe gave.
fn stdin_text(segs: &[Seg], k: usize) -> Option<String> {
    if let Some(Input::Text(t)) = &segs[k].input {
        return Some(t.clone());
    }
    let prev = segs.get(k.checked_sub(1)?)?;
    // a command whose output went to a file (`cat > g1.gdb <<EOF`) gave nothing to this one
    if prev.output.is_some() {
        return None;
    }
    match hint_cmd::program(prev.words.first()?).as_str() {
        "cat" => match &prev.input {
            Some(Input::Text(t)) => Some(t.clone()),
            _ => None,
        },
        "echo" | "printf" => echoed(&prev.words).map(|t| format!("{t}\n")),
        _ => None,
    }
}

/// The lines of the gdb script `name`: the body a command of the same line wrote into it, or the file as it is.
fn script_lines(segs: &[Seg], k: usize, name: &str) -> Vec<String> {
    let dir = &segs[k].dir;
    let here = dir.join(name);
    let written =
        segs[..k].iter().rev().find(|p| p.output.as_deref().is_some_and(|o| p.dir.join(o) == here)).and_then(|p| {
            match &p.input {
                Some(Input::Text(t)) => Some(t.clone()),
                _ => None,
            }
        });
    let text = written.or_else(|| {
        std::fs::metadata(&here).ok().filter(|m| m.is_file() && m.len() <= 16 * 1024)?;
        std::fs::read_to_string(&here).ok()
    });
    text.map(|t| t.lines().map(str::to_string).collect()).unwrap_or_default()
}

/// The debug call for the `gdb` command at `k`.
fn gdb_call(segs: &[Seg], k: usize, cwd: &Path) -> Option<String> {
    let s = &segs[k];
    let args = &s.words[1..];
    let mut target: Option<String> = None;
    let mut core: Option<String> = None;
    let mut shell_args: Vec<String> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;
        match a {
            "--args" | "-args" => {
                target = args.get(i).cloned();
                shell_args = args.get(i + 1..).map(<[String]>::to_vec).unwrap_or_default();
                break;
            }
            "-ex" | "--ex" | "-eval-command" | "--eval-command" => {
                lines.extend(args.get(i).cloned());
                i += 1;
            }
            "-x" | "--command" | "-command" => {
                if let Some(f) = args.get(i) {
                    lines.extend(script_lines(segs, k, f));
                }
                i += 1;
            }
            // attaching to a process is not a program of the project
            "-p" | "-pid" | "--pid" => return None,
            _ if a.starts_with("--eval-command=") => lines.push(a["--eval-command=".len()..].to_string()),
            _ if a.starts_with("--command=") => lines.extend(script_lines(segs, k, &a["--command=".len()..])),
            _ if a.starts_with("--core=") => core = Some(a["--core=".len()..].to_string()),
            _ if GDB_VALUED.contains(&a) => i += 1,
            _ if a.starts_with('-') => {}
            _ => {
                if target.is_none() {
                    target = Some(a.to_string());
                } else if core.is_none() {
                    core = Some(a.to_string());
                }
            }
        }
    }
    let target = target.filter(|t| hint_cmd::opens(&s.dir, t))?;
    let target = &target;
    // the second word is a core file only when it is one; a process id or a missing name is no program of ours
    let core = core.filter(|c| std::fs::metadata(s.dir.join(c)).is_ok_and(|m| m.is_file()));
    let core = core.as_ref();
    if let Some(t) = stdin_text(segs, k) {
        lines.extend(t.lines().map(str::to_string));
    }
    let plan = translate(&lines, &s.dir);
    // every breakpoint, watch and catch of the script was left out: the call would stop nowhere the script did
    if plan.stops_wanted > 0 && plan.stops_kept == 0 {
        return None;
    }
    let (run_args, input) = plan.args.unwrap_or((shell_args, None));
    let input = input.as_ref().and_then(input_text);
    let mut calls: Vec<String> = Vec::new();
    match core {
        Some(c) => {
            let shown = hint_cmd::shown(cwd, &s.dir, c);
            let exe = hint_cmd::shown(cwd, &s.dir, target);
            if !plain(&shown) || !plain(&exe) {
                return None;
            }
            calls.push(format!("core {} --exe {}", hint_cmd::quoted(&shown), hint_cmd::quoted(&exe)));
            calls.extend(plan.cmds.iter().chain(&plan.hit).cloned());
            if plan.cmds.is_empty() && plan.hit.is_empty() {
                calls.push("bt".into());
            }
        }
        None => {
            calls.push(run_text(cwd, s, target, &run_args, input.as_deref())?);
            // breakpoints come after `run`, which stops at the entry: gdb's `run` is the `cont` that follows them
            // no `run` in the script: the breakpoints and watches still go before it
            let at = plan.run_at.unwrap_or_else(|| {
                plan.cmds
                    .iter()
                    .rposition(|c| ["break", "tbreak", "watch", "catch"].iter().any(|v| c.split(' ').next() == Some(v)))
                    .map_or(0, |i| i + 1)
            });
            let (pre, post) = plan.cmds.split_at(at);
            calls.extend(pre.iter().cloned());
            calls.push("cont".into());
            calls.extend(post.iter().skip(post.first().is_some_and(|c| c == "cont") as usize).cloned());
            calls.extend(plan.hit.iter().cloned());
        }
    }
    // `next; next` is two steps; the same word twice in a row is otherwise said once
    calls.dedup_by(|a, b| a == b && !(a.starts_with("next") || a.starts_with("step")));
    fit(&mut calls).then(|| calls.join("; "))
}

/// Keeps the call within `MAX_CMDS` commands and `MAX_CALL` characters without losing the `cont` that runs the
/// program: what follows it goes first, then the last of the breakpoints before it. False when `run` and `cont`
/// alone do not fit.
fn fit(calls: &mut Vec<String>) -> bool {
    let too_big = |c: &[String]| c.len() > MAX_CMDS || c.join("; ").len() > MAX_CALL;
    let mut cont = calls.iter().position(|c| c == "cont");
    while too_big(calls) {
        match cont {
            Some(at) if calls.len() > at + 1 => {
                calls.pop();
            }
            Some(at) if at >= 2 => {
                calls.remove(at - 1);
                cont = Some(at - 1);
            }
            None if calls.len() > 2 => {
                calls.pop();
            }
            _ => return false,
        }
    }
    true
}

/// A gdb script, as the debug commands that do it.
#[derive(Default)]
struct Plan {
    /// The commands in the order the script gave them, `run` left out.
    cmds: Vec<String>,
    /// What a breakpoint's `commands` block does when it is hit: it goes after the `cont`.
    hit: Vec<String>,
    /// Where gdb's `run` stood among them.
    run_at: Option<usize>,
    /// The program's arguments and input when the script gave them (`run ARGS`, `set args`).
    args: Option<(Vec<String>, Option<Input>)>,
    /// The breakpoints, watches and catches the script set (outside a `commands` block), and how many the call keeps.
    stops_wanted: usize,
    stops_kept: usize,
}

/// A translated command, kept when the call can carry it; in a `commands` block it waits for the stop.
fn say(plan: &mut Plan, hit: bool, c: String) {
    let stop = !hit && matches!(c.split(' ').next(), Some("break" | "tbreak" | "watch" | "catch"));
    plan.stops_wanted += stop as usize;
    if plain_cmd(&c) && c.len() <= 160 {
        plan.stops_kept += stop as usize;
        if hit { &mut plan.hit } else { &mut plan.cmds }.push(c);
    }
}

fn translate(lines: &[String], dir: &Path) -> Plan {
    let mut plan = Plan::default();
    let mut skip = 0usize;
    let mut in_commands = false;
    for raw in lines {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (word, rest) = line.split_once(char::is_whitespace).map_or((line, ""), |(a, b)| (a, b.trim()));
        let (name, fmt) = word.split_once('/').unwrap_or((word, ""));
        let opens_block = |n: &str| match n {
            "if" | "while" | "define" | "document" => true,
            "python" | "py" | "python-interactive" | "pi" => rest.is_empty(),
            _ => false,
        };
        if skip > 0 {
            match name {
                "end" => skip -= 1,
                n if opens_block(n) || n == "commands" => skip += 1,
                _ => {}
            }
            continue;
        }
        if opens_block(name) {
            skip = 1;
            continue;
        }
        if in_commands {
            match name {
                "end" => {
                    in_commands = false;
                    continue;
                }
                "silent" | "continue" | "c" | "cont" => continue,
                _ => {}
            }
        }
        let number =
            |s: &str| (!s.is_empty() && s.len() <= 6 && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.to_string());
        match name {
            "commands" => in_commands = true,
            "run" | "r" | "start" | "starti" => {
                if name.starts_with("start") && name != "starti" {
                    say(&mut plan, in_commands, "tbreak main".into());
                }
                if !rest.is_empty() {
                    plan.args = args_of(rest, dir).or(plan.args.take());
                }
                plan.run_at.get_or_insert(plan.cmds.len());
            }
            "break" | "b" | "br" | "brea" | "tbreak" | "tb" | "hbreak" | "hb" | "thbreak" | "thb" => {
                let verb = if name.starts_with('t') { "tbreak" } else { "break" };
                match place(rest) {
                    Some(spec) => say(&mut plan, in_commands, format!("{verb} {spec}")),
                    None => plan.stops_wanted += !in_commands as usize,
                }
            }
            "continue" | "c" | "cont" | "fg" => say(&mut plan, in_commands, "cont".into()),
            "next" | "n" | "nexti" | "ni" | "step" | "s" | "stepi" | "si" => {
                let verb = if name.starts_with('n') { "next" } else { "step" };
                let count = number(rest).map(|n| format!(" {n}")).unwrap_or_default();
                say(&mut plan, in_commands, format!("{verb}{count}"));
            }
            "finish" | "fin" => say(&mut plan, in_commands, "finish".into()),
            "until" | "u" | "advance" if !rest.is_empty() => {
                say(&mut plan, in_commands, format!("until {}", expr(rest)))
            }
            "backtrace" | "bt" | "where" => {
                let n = rest.split_whitespace().find_map(number).map(|n| format!(" {n}")).unwrap_or_default();
                say(&mut plan, in_commands, format!("bt{n}"));
            }
            "info" | "i" => {
                let (sub, more) = rest.split_once(char::is_whitespace).map_or((rest, ""), |(a, b)| (a, b.trim()));
                let c = match sub {
                    "registers" | "reg" | "r" | "all-registers" => {
                        let r = more
                            .split_whitespace()
                            .next()
                            .map(|r| r.trim_start_matches('$'))
                            .filter(|r| r.bytes().all(|b| b.is_ascii_alphanumeric()));
                        format!("regs{}", r.map(|r| format!(" {r}")).unwrap_or_default())
                    }
                    "breakpoints" | "break" | "b" | "watchpoints" => "breaks".into(),
                    "locals" => "locals".into(),
                    "args" => "args".into(),
                    "sharedlibrary" | "shared" | "dll" => "modules".into(),
                    "threads" => "threads".into(),
                    "frame" | "f" => "status".into(),
                    "proc" if more.starts_with("mappings") => "maps".into(),
                    _ => continue,
                };
                say(&mut plan, in_commands, c);
            }
            "print" | "p" | "inspect" | "output" | "display" if !rest.is_empty() => {
                say(&mut plan, in_commands, format!("print {}", expr(rest)));
            }
            "x" if !rest.is_empty() => {
                if let Some(c) = examine(fmt, rest) {
                    say(&mut plan, in_commands, c);
                }
            }
            "disassemble" | "disas" => {
                let at = rest.split_whitespace().find(|w| !w.starts_with('/') && !w.contains(','));
                say(&mut plan, in_commands, format!("dis{}", at.map(|a| format!(" {}", expr(a))).unwrap_or_default()));
            }
            "watch" | "rwatch" | "awatch" if !rest.is_empty() => {
                say(&mut plan, in_commands, format!("watch {}", expr(rest)))
            }
            "catch" => match rest.strip_prefix("syscall").map(str::trim).filter(|r| !r.is_empty()) {
                Some(r) => say(&mut plan, in_commands, format!("catch syscall {r}")),
                None => plan.stops_wanted += !in_commands as usize,
            },
            "set" => {
                if let Some(r) = rest.strip_prefix("args") {
                    plan.args = args_of(r.trim(), dir).or(plan.args.take());
                }
            }
            _ => {}
        }
    }
    plan
}

/// The program's arguments and input from what follows gdb's `run` or `set args`.
fn args_of(rest: &str, dir: &Path) -> Option<(Vec<String>, Option<Input>)> {
    let segs = hint_cmd::segments(&format!("x {rest}"), dir)?;
    match segs.as_slice() {
        [s] => Some((s.words[1..].to_vec(), s.input.clone())),
        _ => None,
    }
}

/// A breakpoint's place as the debugger writes it: `*(sym+0x170)` is `*sym+0x170`; `if` and the rest stay.
fn place(rest: &str) -> Option<String> {
    let rest = rest.trim();
    let text = match rest.strip_prefix("*(") {
        Some(r) => {
            let mut depth = 1;
            let mut end = None;
            for (i, c) in r.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let end = end?;
            format!("*{}{}", r[..end].trim(), &r[end + 1..])
        }
        None => rest.to_string(),
    };
    let text = expr(&text);
    (!text.is_empty() && text.len() <= 160 && !text.contains(" thread ")).then_some(text)
}

/// An expression as the debugger reads registers: `$pc`, `$sp` and `$fp` are `$rip`, `$rsp` and `$rbp`.
fn expr(e: &str) -> String {
    e.trim().replace("$pc", "$rip").replace("$sp", "$rsp").replace("$fp", "$rbp")
}

/// gdb's `x/NFU addr` as `mem` or `dis`.
fn examine(fmt: &str, rest: &str) -> Option<String> {
    let digits: String = fmt.chars().take_while(char::is_ascii_digit).collect();
    let count: u64 = if digits.is_empty() { 1 } else { digits.parse().ok()? };
    let letters = &fmt[digits.len()..];
    let addr = expr(rest);
    if letters.contains('s') {
        return Some(format!("mem {addr} --as str"));
    }
    if letters.contains('i') {
        return Some(format!("dis {addr} {}", count.min(200)));
    }
    let size = match letters.chars().find(|c| matches!(c, 'b' | 'h' | 'w' | 'g')) {
        Some('b') => 1,
        Some('h') => 2,
        Some('g') => 8,
        _ => 4,
    };
    Some(format!("mem {addr} {}", count.saturating_mul(size).min(4096)))
}

/// `strace`/`ltrace` of a program of the project as the debugger's own: the trace on at the entry, the run, the log.
fn trace_call(segs: &[Seg], k: usize, cwd: &Path) -> Option<String> {
    let s = &segs[k];
    let tool = hint_cmd::program(s.words.first()?);
    let valued: &[&str] = if tool == "strace" { &STRACE_VALUED } else { &LTRACE_VALUED };
    let mut filter: Option<String> = None;
    let mut i = 1;
    while i < s.words.len() {
        let a = s.words[i].as_str();
        if !a.starts_with('-') || a == "-" {
            break;
        }
        i += 1;
        if a == "--" {
            break;
        }
        let numeric = |w: Option<&String>| w.is_some_and(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()));
        if a == "-p" || (!a.starts_with("--") && a.ends_with('p') && numeric(s.words.get(i))) {
            return None;
        }
        if a == "-e" {
            filter = s.words.get(i).cloned();
            i += 1;
        } else if let Some(v) = a
            .strip_prefix("--trace=")
            .or_else(|| a.strip_prefix("-e").filter(|v| !a.starts_with("--") && !v.is_empty()))
        {
            filter = Some(v.to_string());
        } else if valued.contains(&a) {
            i += 1;
        }
    }
    let prog = s.words.get(i).filter(|w| hint_cmd::opens(&s.dir, w))?;
    let run = run_text(cwd, s, prog, &s.words[i + 1..], input_for(segs, k).as_deref())?;
    let names = filter.and_then(|f| {
        let f =
            f.trim_start_matches("trace=").trim_start_matches("t=").trim_start_matches("syscall=").replace('+', ",");
        let ok = !f.is_empty()
            && f.len() <= 80
            && f.split(',').all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        ok.then_some(f)
    });
    let on = match &names {
        Some(n) => format!("{tool} {n}"),
        None => tool.clone(),
    };
    Some(format!("{run}; {on}; cont; {tool} show"))
}

/// The exit code the shell tool wrote at the end of a result.
fn exit_code(text: &str) -> Option<i32> {
    text.lines().rev().map(str::trim).find_map(|l| l.strip_prefix("[exit code ")?.strip_suffix(']')?.parse().ok())
}

/// Which hints `messages` already hold in a tool result.
pub fn told(messages: &[Message]) -> Told {
    let mut t = Told::default();
    for m in messages {
        for p in &m.content {
            let Part::ToolResult { content, .. } = p else { continue };
            for text in content.iter().filter_map(Part::as_text) {
                t.gdb |= text.contains(HEAD_GDB) || text.contains(HEAD);
                t.trace |= text.contains(HEAD_TRACE);
                t.fault |= text.contains(HEAD_FAULT) || text.contains(HEAD);
            }
        }
    }
    t
}

/// The results of one round with the hint added to the first that earns each kind; `told` is set as they are.
pub fn round(told: &mut Told, calls: &[(String, String, Value)], results: Vec<Part>, cwd: &Path) -> Vec<Part> {
    if told.all() {
        return results;
    }
    results
        .into_iter()
        .map(|p| {
            let Part::ToolResult { call_id, name, mut content, is_error } = p else { return p };
            let command = calls
                .iter()
                .find(|(id, n, _)| *id == call_id && n == "bash")
                .filter(|(_, _, a)| {
                    !nexo_tools::util::bool_arg(a, "background") && !nexo_tools::util::bool_arg(a, "detach")
                })
                .and_then(|(_, _, a)| a.get("command").and_then(Value::as_str));
            let text: String = content.iter().filter_map(Part::as_text).collect::<Vec<_>>().join("\n");
            // a command that was refused, blocked or interrupted never ran: it spends nothing
            if !told.all() && (!is_error || hint_cmd::ran(&text)) {
                if let Some((kind, l)) = command.and_then(|c| line(c, &text, cwd, told)) {
                    debug_assert!(!told.has(kind));
                    told.set(kind);
                    content = vec![Part::text(format!("{text}\n\n{l}"))];
                }
            }
            Part::ToolResult { call_id, name, content, is_error }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SEGV: &str = "Segmentation fault (core dumped)\n\n[exit code 139]";

    fn dir_with_prog() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::copy(std::env::current_exe().unwrap(), d.path().join("paint")).unwrap();
        d
    }

    /// A path as a command and a call write it: `/` for the separator on Windows too.
    fn slashed(p: impl AsRef<Path>) -> String {
        hint_cmd::slash(&p.as_ref().display().to_string())
    }

    fn fresh(command: &str, result: &str, cwd: &Path) -> Option<(Kind, String)> {
        line(command, result, cwd, &Told::default())
    }

    /// The call inside the first `debug {"command": "..."}` of a line, JSON escapes undone.
    fn call_of(l: &str) -> String {
        let at = l.find("debug {\"command\": \"").unwrap() + "debug {\"command\": \"".len();
        let rest = &l[at..];
        let end = rest.find("\"}").unwrap();
        rest[..end].replace("\\\"", "\"").replace("\\\\", "\\")
    }

    #[test]
    fn a_segfault_of_a_project_program_names_the_debug_call_with_its_input() {
        let d = dir_with_prog();
        let (kind, l) = fresh("echo '1 -1 #' | ./paint", SEGV, d.path()).unwrap();
        assert_eq!(kind, Kind::Fault);
        assert_eq!(
            l,
            r#"[nexo: the debug tool runs this to the fault in one call: debug {"command": "run ./paint <<< \"1 -1 #\"; cont"} stops at the fault (exit 139) with file:line, cause and locals]"#
        );
        assert!(fresh("./paint big < in.txt", SEGV, d.path())
            .unwrap()
            .1
            .contains(r#""run ./paint big < in.txt; cont""#));
        let l = fresh("./paint <<< '3 4 x'", "[exit code 134]", d.path()).unwrap().1;
        assert!(l.contains(r#"run ./paint <<< \"3 4 x\"; cont"#), "{l}");
        // a heredoc of one line, an output redirected away, a folder changed, an absolute path, a wrapper
        let l = fresh("./paint <<EOF\n3 4\nEOF", SEGV, d.path()).unwrap().1;
        assert!(l.contains(r#"run ./paint <<< \"3 4\"; cont"#), "{l}");
        assert!(fresh("./paint big > out.txt 2>&1", SEGV, d.path()).unwrap().1.contains(r#""run ./paint big; cont""#));
        let abs = d.path().join("paint");
        let l = fresh(&format!("timeout 5 {}", slashed(&abs)), SEGV, d.path()).unwrap().1;
        assert!(
            l.contains(&hint_cmd::tool_call("debug", &format!("run {}; cont", hint_cmd::quoted(&slashed(&abs))))),
            "{l}"
        );
        let sub = d.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::copy(abs, sub.join("tool")).unwrap();
        let l = fresh("cd sub && ./tool", SEGV, d.path()).unwrap().1;
        assert!(
            l.contains(&hint_cmd::tool_call(
                "debug",
                &format!("run {}; cont", hint_cmd::quoted(&format!("{}/sub/./tool", slashed(d.path()))))
            )),
            "{l}"
        );
    }

    #[test]
    fn the_shapes_of_the_paste_one_script_in_a_heredoc_and_gdb_on_an_absolute_path() {
        let d = dir_with_prog();
        let abs = slashed(d.path().join("paint"));
        let shown = hint_cmd::quoted(&abs);
        let command = format!(
            "cat > g1.gdb <<'EOF'\nset pagination off\nbreak *(_ZN6LCRYPT7encryptERKNSt7__cxx1112basic_stringIcSt11char_traitsIcESaIcEEEi+0x170)\ncommands\nsilent\nx/s $rdi\ninfo registers rax\ncontinue\nend\nrun -e t.bin -p 0 -P test123\nEOF\ngdb -q -batch -x g1.gdb {abs} 2>&1 | tail -40"
        );
        let (kind, l) = fresh(&command, "out", d.path()).unwrap();
        assert_eq!(kind, Kind::Gdb);
        assert!(l.starts_with(HEAD_GDB), "{l}");
        assert_eq!(
            call_of(&l),
            format!(
                "run {shown} -e t.bin -p 0 -P test123; break *_ZN6LCRYPT7encryptERKNSt7__cxx1112basic_stringIcSt11char_traitsIcESaIcEEEi+0x170; cont; mem $rdi --as str; regs rax"
            )
        );
        // the same script from a file written by an earlier command
        std::fs::write(d.path().join("g2.gdb"), "break main\nrun 1 2\nbt\ninfo locals\n").unwrap();
        let l = fresh(&format!("gdb -batch -x g2.gdb {abs}"), "", d.path()).unwrap().1;
        assert_eq!(call_of(&l), format!("run {shown} 1 2; break main; cont; bt; locals"));
    }

    #[test]
    fn gdb_in_each_of_its_forms() {
        let d = dir_with_prog();
        let call = |c: &str| call_of(&fresh(c, "", d.path()).unwrap_or_else(|| panic!("no line for {c}")).1);
        assert_eq!(call("gdb -batch -ex run -ex bt ./paint"), "run ./paint; cont; bt");
        assert_eq!(call("gdb -q -batch -ex r --args ./paint 1 2"), "run ./paint 1 2; cont");
        assert_eq!(call("gdb ./paint"), "run ./paint; cont");
        assert_eq!(
            call("gdb -batch -ex 'break parse if n > 3' -ex 'tbreak *0x401000' -ex run -ex 'x/4gx $rsp' -ex 'p $rax' ./paint"),
            "run ./paint; break parse if n > 3; tbreak *0x401000; cont; mem $rsp 32; print $rax"
        );
        assert_eq!(call("gdb -batch -ex 'run < in.txt' ./paint"), "run ./paint < in.txt; cont");
        assert_eq!(
            call("gdb -batch -ex 'set args a \"b c\"' -ex start ./paint"),
            "run ./paint a \"b c\"; tbreak main; cont"
        );
        assert_eq!(
            call("gdb -batch -ex 'x/5i $pc' -ex 'watch counter' -ex 'catch syscall write' ./paint"),
            "run ./paint; dis $rip 5; watch counter; catch syscall write; cont"
        );
        // lines the debugger has no word for are left out; nothing runs
        assert_eq!(
            call("gdb -batch -ex 'python print(1)' -ex 'shell touch touched' -ex 'call f()' -ex bt ./paint"),
            "run ./paint; cont; bt"
        );
        assert!(!d.path().join("touched").exists());
        // a script on stdin: a heredoc, a here-string, a pipe from echo or cat
        assert_eq!(call("gdb -batch ./paint <<EOF\nbreak main\nrun\nbt\nEOF"), "run ./paint; break main; cont; bt");
        assert_eq!(call("echo 'run' | gdb -batch ./paint"), "run ./paint; cont");
        assert_eq!(call("cat <<EOF | gdb -batch ./paint\nbreak main\nrun\nEOF"), "run ./paint; break main; cont");
        // the options that take a value, the wrappers, the folder
        assert_eq!(call("gdb -q -nx -iex 'set a b' -ex run ./paint"), "run ./paint; cont");
        assert_eq!(call("sudo -n gdb -batch -ex bt ./paint"), "run ./paint; cont; bt");
        let sub = d.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::copy(d.path().join("paint"), sub.join("paint")).unwrap();
        assert_eq!(
            call("cd sub; gdb -batch ./paint"),
            format!("run {}; cont", hint_cmd::quoted(&format!("{}/sub/./paint", slashed(d.path()))))
        );
    }

    #[test]
    fn registers_stay_and_gdb_variables_the_debugger_does_not_have_are_left_out() {
        let d = dir_with_prog();
        let call = |c: &str| call_of(&fresh(c, "", d.path()).unwrap_or_else(|| panic!("no line for {c}")).1);
        // every register spelling of the debugger passes
        assert_eq!(
            call("gdb -batch -ex 'break main' -ex run -ex 'p $rax + $r8d' -ex 'x/2gx $sp' -ex 'p $eflags' -ex 'x/3i $pc' ./paint"),
            "run ./paint; break main; cont; print $rax + $r8d; mem $rsp 16; print $eflags; dis $rip 3"
        );
        // a convenience variable, gdb's own `$_`, a history value: no word for them there, the line is dropped
        assert_eq!(
            call("gdb -batch -ex 'break main' -ex 'set $n = 3' -ex run -ex 'p $n' -ex 'p $_exitcode' -ex 'p $1' -ex 'p $rdi' ./paint"),
            "run ./paint; break main; cont; print $rdi"
        );
        for r in ["rax", "RDI", "r8", "r15d", "r9w", "r10b", "sil", "eflags", "fs_base", "ax", "pc"] {
            assert!(register(r), "{r}");
        }
        for r in ["", "n", "_", "_exitcode", "1", "r16", "r7", "r8dd", "rxx", "eip", "xmm0"] {
            assert!(!register(r), "{r}");
        }
        assert!(plain_cmd("print $rax") && !plain_cmd("print $") && !plain_cmd("print \"a\"") && !plain_cmd("p $n"));
    }

    #[test]
    fn a_script_whose_every_stop_was_left_out_gets_no_line() {
        let d = dir_with_prog();
        // the only breakpoint is on something the debugger cannot place: `run; cont` would stop nowhere it did
        assert_eq!(fresh("gdb -batch -ex 'break foo thread 2' -ex run -ex bt ./paint", "", d.path()), None);
        assert_eq!(fresh("gdb -batch -ex 'catch throw' -ex run ./paint", "", d.path()), None);
        assert_eq!(fresh("gdb -batch -ex 'watch $n' -ex run ./paint", "", d.path()), None);
        // one kept stop is enough, and a script with none asked for none
        let call = |c: &str| call_of(&fresh(c, "", d.path()).unwrap_or_else(|| panic!("no line for {c}")).1);
        assert_eq!(
            call("gdb -batch -ex 'catch throw' -ex 'break main' -ex run ./paint"),
            "run ./paint; break main; cont"
        );
        assert_eq!(call("gdb -batch -ex run -ex bt ./paint"), "run ./paint; cont; bt");
    }

    #[test]
    fn a_long_script_is_cut_but_never_loses_the_cont_and_two_steps_stay_two() {
        let d = dir_with_prog();
        let call = |c: &str| call_of(&fresh(c, "", d.path()).unwrap_or_else(|| panic!("no line for {c}")).1);
        let breaks: String = (0..14).map(|i| format!("-ex 'break f{i}' ")).collect();
        let c = call(&format!("gdb -batch {breaks}-ex run -ex bt ./paint"));
        let words: Vec<&str> = c.split("; ").collect();
        assert!(words.len() <= MAX_CMDS && words.contains(&"cont") && words[0] == "run ./paint", "{c}");
        assert!(c.contains("break f0") && !c.contains("break f13"), "{c}");
        // long places: the character bound cuts the same way
        let long = "x".repeat(150);
        let breaks: String = (0..6).map(|i| format!("-ex 'break {long}{i}' ")).collect();
        let c = call(&format!("gdb -batch {breaks}-ex run -ex bt ./paint"));
        assert!(c.len() <= MAX_CALL && c.contains("; cont"), "{c}");
        assert_eq!(
            call("gdb -batch -ex 'break main' -ex run -ex next -ex next -ex bt ./paint"),
            "run ./paint; break main; cont; next; next; bt"
        );
    }

    #[test]
    fn x_with_a_huge_count_does_not_overflow() {
        assert_eq!(examine("18446744073709551615g", "$rsp"), Some("mem $rsp 4096".to_string()));
        assert_eq!(examine("99999999999999999999g", "$rsp"), None);
        assert_eq!(examine("2w", "$rsp"), Some("mem $rsp 8".to_string()));
    }

    #[test]
    fn gdb_with_a_core_file_asks_for_the_core_command() {
        let d = dir_with_prog();
        std::fs::write(d.path().join("core.123"), b"core").unwrap();
        let l = fresh("gdb -batch -ex bt ./paint core.123", "", d.path()).unwrap().1;
        assert_eq!(call_of(&l), "core core.123 --exe ./paint; bt");
        // a process id is not a core file
        let l = fresh("gdb -batch ./paint 1234", "", d.path()).unwrap().1;
        assert_eq!(call_of(&l), "run ./paint; cont");
        // attaching is not a program of the project
        assert_eq!(fresh("gdb -p 1234", "", d.path()), None);
    }

    #[test]
    fn strace_and_ltrace_on_a_project_program_name_the_debugger_traces() {
        let d = dir_with_prog();
        let call = |c: &str| {
            let (k, l) = fresh(c, "", d.path()).unwrap_or_else(|| panic!("no line for {c}"));
            assert_eq!(k, Kind::Trace);
            assert!(l.starts_with(HEAD_TRACE), "{l}");
            call_of(&l)
        };
        assert_eq!(call("strace ./paint a b"), "run ./paint a b; strace; cont; strace show");
        assert_eq!(call("strace -f -o t.txt ./paint"), "run ./paint; strace; cont; strace show");
        assert_eq!(call("strace -e trace=openat,read ./paint"), "run ./paint; strace openat,read; cont; strace show");
        assert_eq!(
            call("strace -etrace=write ./paint x 2>&1 | head"),
            "run ./paint x; strace write; cont; strace show"
        );
        assert_eq!(call("strace -e trace=%file ./paint"), "run ./paint; strace; cont; strace show");
        assert_eq!(call("ltrace -e malloc+free ./paint"), "run ./paint; ltrace malloc,free; cont; ltrace show");
        assert_eq!(call("ltrace ./paint < in.txt"), "run ./paint < in.txt; ltrace; cont; ltrace show");
        // a process, a program that is not ours
        assert_eq!(fresh("strace -p 1234", "", d.path()), None);
        assert_eq!(fresh("strace -fp 1234", "", d.path()), None);
        assert_eq!(fresh("strace ls", "", d.path()), None);
    }

    #[test]
    fn no_line_for_clean_exits_other_programs_or_a_pipe_after_the_program() {
        let d = dir_with_prog();
        for (c, r) in [
            ("./paint", "ok"),
            ("./paint", "[exit code 1]"),
            ("./paint | head", "[exit code 139]"),
            ("./paint; echo done", "[exit code 139]"),
            ("python3 x.py", "[exit code 139]"),
            ("./missing", "[exit code 139]"),
            ("gdb ./missing", ""),
            ("gdb --version", ""),
            ("objdump -d ./paint", "ok"),
            ("ls $(touch touched)", "[exit code 139]"),
        ] {
            assert_eq!(fresh(c, r, d.path()), None, "{c} {r}");
        }
        assert!(!d.path().join("touched").exists());
    }

    #[test]
    fn once_per_session_per_kind_and_found_again_in_a_kept_history() {
        let d = dir_with_prog();
        let bash = |id: &str, c: &str| (id.to_string(), "bash".to_string(), json!({"command": c}));
        let calls = vec![
            bash("a", "./paint"),
            bash("b", "gdb -batch -ex run ./paint"),
            bash("c", "strace ./paint"),
            bash("d", "gdb ./paint"),
        ];
        let res = vec![
            Part::tool_result("a", "boom\n\n[exit code 139]", true),
            Part::tool_result("b", "gdb out", false),
            Part::tool_result("c", "trace", false),
            Part::tool_result("d", "gdb again", false),
        ];
        let mut now = Told::default();
        let out = round(&mut now, &calls, res.clone(), d.path());
        assert_eq!(now, Told { gdb: true, trace: true, fault: true });
        let text = |p: &Part| match p {
            Part::ToolResult { content, .. } => content.iter().filter_map(Part::as_text).collect::<String>(),
            _ => String::new(),
        };
        assert!(
            text(&out[0]).contains(HEAD_FAULT)
                && text(&out[1]).contains(HEAD_GDB)
                && text(&out[2]).contains(HEAD_TRACE)
        );
        assert_eq!(text(&out[3]), "gdb again", "one line per kind");
        assert_eq!(told(&[Message::tool_results(out)]), Told { gdb: true, trace: true, fault: true });
        let again = round(&mut now, &calls, res, d.path());
        assert_eq!(text(&again[1]), "gdb out");
        assert_eq!(told(&[Message::tool_results(again)]), Told::default());
        // a fault line is a fault and nothing else: a later gdb still earns its line
        let fault =
            Message::tool_results(vec![Part::tool_result("x", format!("out\n\n{HEAD_FAULT} debug {{}}]"), false)]);
        assert_eq!(told(&[fault]), Told { gdb: false, trace: false, fault: true });
        // a line of a session saved before the kinds covers a fault and a gdb
        let old = Message::tool_results(vec![Part::tool_result("x", format!("out\n\n{HEAD} debug {{}}]"), false)]);
        assert_eq!(told(&[old]), Told { gdb: true, trace: false, fault: true });
    }

    #[test]
    fn a_refused_command_spends_nothing_and_a_failed_one_does() {
        let d = dir_with_prog();
        let calls = vec![("a".to_string(), "bash".to_string(), json!({"command": "gdb -batch ./paint"}))];
        let mut now = Told::default();
        let refused = vec![Part::tool_result("a", "Permission denied: the person said no", true)];
        round(&mut now, &calls, refused, d.path());
        assert_eq!(now, Told::default());
        let failed = vec![Part::tool_result("a", "sh: gdb: command not found\n\n[exit code 127]", true)];
        round(&mut now, &calls, failed, d.path());
        assert!(now.gdb);
    }
}
