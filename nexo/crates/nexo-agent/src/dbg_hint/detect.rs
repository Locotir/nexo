//! Reading the command of a hint: the fault a program died by, and the words, input and gdb script it carries.

use std::path::Path;

use super::{FAULTS, HEAD_FAULT};
use crate::hint_cmd::{self, Input, Seg};

/// A program of the project that died by a fault signal: the run that stops there. Only the last command of the
/// line gives the shell's exit code (`./p | head` says nothing about `./p`).
pub(super) fn fault(segs: &[Seg], result: &str, cwd: &Path) -> Option<String> {
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
pub(super) fn plain(w: &str) -> bool {
    !w.contains(['"', ';', '\n', '\\', '$', '`'])
}

/// A debugger command the call can carry: `plain`, except that a `$name` is fine when it is a register the debugger
/// reads (`$rdi`, `$rsp`, `$r8d`). A gdb convenience variable (`$x` from `set $x = ...`, `$_`, `$1`) has no meaning
/// there, so a command that uses one is left out.
pub(super) fn plain_cmd(c: &str) -> bool {
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
pub(super) fn register(n: &str) -> bool {
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
pub(super) fn run_text(cwd: &Path, s: &Seg, prog: &str, args: &[String], input: Option<&str>) -> Option<String> {
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
pub(super) fn input_for(segs: &[Seg], k: usize) -> Option<String> {
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

pub(super) fn input_text(i: &Input) -> Option<String> {
    match i {
        Input::File(f) => plain(f).then(|| format!("< {}", hint_cmd::quoted(f))),
        Input::Text(t) => {
            let t = t.trim_end_matches('\n');
            (!t.contains('\n') && plain(t)).then(|| format!("<<< \"{t}\""))
        }
    }
}

/// `echo a b` or `printf 'a\n'` before the program: the text it fed in.
pub(super) fn echoed(words: &[String]) -> Option<String> {
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
pub(super) fn stdin_text(segs: &[Seg], k: usize) -> Option<String> {
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
pub(super) fn script_lines(segs: &[Seg], k: usize, name: &str) -> Vec<String> {
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

/// The exit code the shell tool wrote at the end of a result.
pub(super) fn exit_code(text: &str) -> Option<i32> {
    text.lines().rev().map(str::trim).find_map(|l| l.strip_prefix("[exit code ")?.strip_suffix(']')?.parse().ok())
}
