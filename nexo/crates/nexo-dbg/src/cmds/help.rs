//! Words split like a shell would, and the help text.

use super::*;

/// Words split like a shell would with double quotes (no escapes).
pub fn split_words(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut q, mut any) = (Vec::new(), String::new(), false, false);
    for c in s.chars() {
        match c {
            '"' => {
                q = !q;
                any = true;
            }
            c if c.is_whitespace() && !q => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn help() -> Output {
    let l = [
        "run: nexo dbg run <exe> [args]   attach: nexo dbg attach <pid>   core: nexo dbg core <file>",
        "go:    cont [--wait N] | step [N] | next [N] | finish | until <addr> | stop | wait [--wait N] | kill | detach",
        "calls: calls | calls off    log the real target of every indirect call (call reg / call [mem]) as the program runs; read with `log`",
        "trace: strace [names|file|net|process|memory] | strace show [filter] | strace off | catch syscall <names|groups> | catch syscall off; ltrace [names] | ltrace show [names] | ltrace off | hook <addr|name> [bt N] [stop] [set argN V] [ret V] | hook show [name] | hooks | hook del <id>|all; cover [funcs] | cover show [func] | cover save <A> | cover diff <A> <B>|now | cover off",
        "break: break <addr|*addr|name|file:line|name+off|import> (a function stops after its prologue) [if <expr>] [after N] [log <text with {expr}>] [once] | breaks | delete <id>|all | enable/disable <id>",
        "look:  regs [name] | bt [N] | dis [addr] [N] | mem <addr> [len] [--as u8|u32|u64|ptr|str] | print <expr|var|p->f|a[i]> [--as str] | args | locals | threads | thread <tid>",
        "       maps [filter] | modules | find <hex|\"text\"> [--in <map>] | out [--all] | log | history [N] | status",
        "change (asks): setreg <reg> <value> | write <addr> <hex> | watch <addr> [len] [r|w|x] | stdin <text> | catch add|del <SIG>",
        "script: do \"break f; cont; args; bt\" [--keep-going]     expressions: rax+8, *(u32*)(rdi+8), main+4, rsp, sym == 3",
        "flags, first in the command: -s <id> (the session when several are live) | --lines N (rows per answer, default 40) | --from <addr> (continue a long answer from this row)",
        "addresses: a number inside the program's sections is a file address (as `nexo bin` shows); any other number is a memory address",
    ];
    rows("help", l.iter().map(|s| s.to_string()).collect(), json!({}))
}
