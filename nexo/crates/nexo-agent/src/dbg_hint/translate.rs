//! A gdb script as the debug commands that do it.

use std::path::Path;

use super::detect::plain_cmd;
use crate::hint_cmd::{self, Input};

/// A gdb script, as the debug commands that do it.
#[derive(Default)]
pub(super) struct Plan {
    /// The commands in the order the script gave them, `run` left out.
    pub(super) cmds: Vec<String>,
    /// What a breakpoint's `commands` block does when it is hit: it goes after the `cont`.
    pub(super) hit: Vec<String>,
    /// Where gdb's `run` stood among them.
    pub(super) run_at: Option<usize>,
    /// The program's arguments and input when the script gave them (`run ARGS`, `set args`).
    pub(super) args: Option<(Vec<String>, Option<Input>)>,
    /// The breakpoints, watches and catches the script set (outside a `commands` block), and how many the call keeps.
    pub(super) stops_wanted: usize,
    pub(super) stops_kept: usize,
}

/// A translated command, kept when the call can carry it; in a `commands` block it waits for the stop.
pub(super) fn say(plan: &mut Plan, hit: bool, c: String) {
    let stop = !hit && matches!(c.split(' ').next(), Some("break" | "tbreak" | "watch" | "catch"));
    plan.stops_wanted += stop as usize;
    if plain_cmd(&c) && c.len() <= 160 {
        plan.stops_kept += stop as usize;
        if hit { &mut plan.hit } else { &mut plan.cmds }.push(c);
    }
}

pub(super) fn translate(lines: &[String], dir: &Path) -> Plan {
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
            "frame" | "f" | "up" | "down" => {
                let verb = if name == "f" { "frame" } else { name };
                let n = rest.split_whitespace().next().and_then(number).map(|n| format!(" {n}")).unwrap_or_default();
                say(&mut plan, in_commands, format!("{verb}{n}"));
            }
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
                    "frame" | "f" => "info frame".into(),
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
pub(super) fn args_of(rest: &str, dir: &Path) -> Option<(Vec<String>, Option<Input>)> {
    let segs = hint_cmd::segments(&format!("x {rest}"), dir)?;
    match segs.as_slice() {
        [s] => Some((s.words[1..].to_vec(), s.input.clone())),
        _ => None,
    }
}

/// A breakpoint's place as the debugger writes it: `*(sym+0x170)` is `*sym+0x170`; `if` and the rest stay.
pub(super) fn place(rest: &str) -> Option<String> {
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
pub(super) fn expr(e: &str) -> String {
    e.trim().replace("$pc", "$rip").replace("$sp", "$rsp").replace("$fp", "$rbp")
}

/// gdb's `x/NFU addr` as `mem` or `dis`.
pub(super) fn examine(fmt: &str, rest: &str) -> Option<String> {
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
