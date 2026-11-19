//! What a command does to the world, and which commands a read-only server refuses.

/// What a `nexo dbg` command does to the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbgClass {
    /// Looks at, or steers, a program the person already let the debugger start.
    Control,
    /// Writes into the program or (`dump`) into a file, or attaches to a process that was not started here.
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
    "ptype",
    "whatis",
    "bt",
    "threads",
    "thread",
    "frame",
    "up",
    "down",
    "breaks",
    "watches",
    "maps",
    "modules",
    "find",
    "history",
    "record",
    "back",
    "out",
    "log",
    "dis",
    "disasm",
    "list",
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
    "stops",
    "signal",
    "kill",
    "detach",
    "follow",
    "checkpoints",
    "end",
    "stdin",
    "catch",
    "watch",
    "rwatch",
    "awatch",
    "core",
    "strace",
    "ltrace",
    "hooks",
    "heap",
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
            "-s" | "--session" | "--lines" | "--from" | "--grep" => {
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
        "attach" | "connect" | "write" | "setreg" | "return" | "jump" | "gcore" | "call" | "dump" | "checkpoint"
        | "restart" | "rstepi" | "rcont" => DbgClass::Change,
        // kills a stored copy of the program
        "delete" | "del" if deletes_checkpoint(&rest[1..]) => DbgClass::Change,
        // an expression that calls a function of the program runs that code
        "print" | "p" | "output" if crate::expr::has_call(&rest[1..].join(" ")) => DbgClass::Change,
        // a hook that sets an argument or the result changes the program
        "hook" if rest.iter().any(|w| w == "set" || w == "ret") => DbgClass::Change,
        "hook" => DbgClass::Control,
        "unset" => DbgClass::Control,
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

/// `delete checkpoint N|all`: the words after `delete` name a checkpoint, not a breakpoint.
fn deletes_checkpoint(rest: &[String]) -> bool {
    rest.first().is_some_and(|w| w == "checkpoint" || w == "checkpoints")
}

enum SetEffect {
    Keeps,
    Writes,
}

/// What a gdb `set` does: a setting or a convenience variable changes nothing in the program; a register or memory does.
fn set_effect(words: &[String]) -> SetEffect {
    match crate::gdbdialect::set_kind(words, &|n| crate::Regs::default().get(n).is_some()) {
        crate::gdbdialect::SetKind::Noise
        | crate::gdbdialect::SetKind::Var
        | crate::gdbdialect::SetKind::Args(_)
        | crate::gdbdialect::SetKind::Env(..)
        | crate::gdbdialect::SetKind::Cwd(_)
        | crate::gdbdialect::SetKind::Randomization { .. }
        | crate::gdbdialect::SetKind::Follow(_) => SetEffect::Keeps,
        crate::gdbdialect::SetKind::Register(..) | crate::gdbdialect::SetKind::Memory => SetEffect::Writes,
    }
}

/// The commands a server that was not given permission to change things may run: they look at the program,
/// or steer one the person already let the debugger start. Everything else (writes, saved names and notes,
/// attach and connect, `calls`, `serve`, a new `run`, unknown words) is refused by name.
pub(super) const READ_ALLOWED: &[&str] = &[
    "status",
    "info",
    "regs",
    "mem",
    "x",
    "print",
    "p",
    "args",
    "locals",
    "ptype",
    "whatis",
    "bt",
    "threads",
    "thread",
    "frame",
    "up",
    "down",
    "breaks",
    "watches",
    "maps",
    "modules",
    "find",
    "history",
    "record",
    "back",
    "out",
    "log",
    "dis",
    "disasm",
    "list",
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
    "stops",
    "signal",
    "kill",
    "detach",
    "follow",
    "checkpoints",
    "catch",
    "strace",
    "ltrace",
    "hook",
    "hooks",
    "heap",
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
    "unset",
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
            "write" | "setreg" | "return" | "jump" | "rstepi" | "rcont" => format!("`{name}` writes into the program"),
            "gcore" => "`gcore` writes a file".to_string(),
            "call" => "`call` runs code of the program".to_string(),
            "checkpoint" | "restart" => format!("`{name}` makes the program fork itself or replaces it with a copy"),
            "name" | "note" => format!("`{name}` saves a name or note"),
            "attach" | "connect" => format!("`{name}` attaches to a process or a remote stub"),
            "run" => "`run` starts a program".to_string(),
            "serve" | "__serve" => format!("`{name}` opens a port or an internal server"),
            "stdin" | "watch" | "rwatch" | "awatch" | "calls" | "core" | "end" => {
                format!("`{name}` changes the session or the program")
            }
            other => {
                format!("`{other}` is not a command this server may run without permission (unknown or not read-only)")
            }
        });
    }
    let rest = &words[1..];
    match name {
        "print" | "p" | "output" if crate::expr::has_call(&rest.join(" ")) => {
            Some(format!("`{name}` of an expression that calls a function runs code of the program"))
        }
        "hook" if rest.iter().any(|w| w == "set" || w == "ret") => {
            Some("a hook that sets an argument or the result changes the program".to_string())
        }
        "catch" | "catches" if rest.first().is_some_and(|w| w != "syscall") => {
            Some("`catch` with add, del, on, off or all changes how signals reach the program".to_string())
        }
        "cover" if rest.first().is_some_and(|w| w == "save") => Some("`cover save` saves a file".to_string()),
        "delete" | "del" if deletes_checkpoint(rest) => {
            Some("`delete checkpoint` kills a stored copy of the program".to_string())
        }
        _ => None,
    }
}
