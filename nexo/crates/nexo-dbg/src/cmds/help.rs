//! Words split like a shell would, and the help text.

use super::*;

/// Words split like a shell would with double quotes (no escapes), but for the text arguments of a call.
pub fn split_words(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut q, mut any) = (Vec::new(), String::new(), false, false);
    // A string that opens right after `(` or `,` is an argument of a call (`f("abc")`, `f(1, "a b")`): it keeps
    // its quotes and escapes, which the expression needs to tell text from a name.
    let (mut kept, mut esc, mut last) = (false, false, ' ');
    for c in s.chars() {
        match c {
            c if kept && esc => {
                cur.push(c);
                esc = false;
            }
            '\\' if kept => {
                cur.push(c);
                esc = true;
            }
            '"' if kept => {
                cur.push(c);
                q = false;
                kept = false;
            }
            '"' if !q && matches!(last, '(' | ',') => {
                cur.push(c);
                q = true;
                kept = true;
            }
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
        if !c.is_whitespace() {
            last = c;
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
        "go:    cont [--wait N] | step [N] | next [N] | finish | until <addr> | stop | wait [--wait N] | kill | detach | kill call | follow parent|child|both (a forked child kept as a session of its own, breakpoints copied)",
        "calls: calls | calls off    log the real target of every indirect call (call reg / call [mem]) as the program runs; read with `log`",
        "trace: strace [names|file|net|process|memory] | strace show [filter] | strace off | catch syscall <names|groups> | catch syscall off; ltrace [names] | ltrace show [names] | ltrace off | hook <addr|name> [bt N] [stop] [set argN V] [ret V] | hook show [name] | hooks | hook del <id>|all; cover [funcs] | cover show [func] | cover save <A> | cover diff <A> <B>|now | cover off",
        "break: break <addr|*addr|name|file:line|name+off|import> (a function stops after its prologue) [if <expr>] [after N] [log <text with {expr}>] [once] | breaks | delete <id>|all | enable/disable <id>",
        "look:  regs [name] | bt [N] | dis [addr] [N] | dis <A..B> | mem <addr> [len] | mem <A..B> [--as u8|u32|u64|ptr|str|char[16]|item[4]|struct] | print <expr|var|p->f|a[i]> [--as str|T] | ptype <type|var> | args | locals | threads | thread <tid> | frame [N] | up [N] | down [N] (regs, locals, args, print, mem, dis read the selected frame)",
        "       maps [filter] | modules | find <hex|\"text\"> [--in <map>] | out [--all] | log | history [N] | status",
        "change (asks): setreg <reg> <value> | write <addr> <hex> | dump <A..B|A+LEN|fn> [path] | dump module <name> [path] (memory to a file: a range raw, a module rebuilt as ELF/PE; the answer names the bin call) | watch <addr> [len] [r|w|x] | stdin <text> | catch add|del <SIG> | call f(1, \"text\") or print f(x) + 1 (runs f in the stopped thread, every register put back after)",
        "script: do \"break f; cont; args; bt\" [--keep-going]     expressions: rax+8, *(u32*)(rdi+8), main+4, rsp, sym == 3, $rax, $v = expr, (char*)p, p+4",
        "gdb:   x/20bx addr | p/x expr | printf \"fmt\", args | echo | set $v = expr | display/FMT expr | info registers|breakpoints|frame|sharedlibrary|proc mappings | break *(f+0x10) | commands N ... end (silent, printf, x, continue: run at every hit, no round trip) | a whole gdb script goes in one `do` call, newlines or `;` between commands",
        "views: print <addr> --as string | --as vector[:u8|u32|u64|ptr]   (libstdc++ std::string and std::vector at that address)       logpoint: log \"{rax:x} {mem:rdi:16}\"",
        "flags, first in the command: -s <id> (the session when several are live) | --lines N|all (rows per answer, default 120) | --from <addr> (continue a long answer from this row) | --grep RE (only the rows that match)",
        "addresses: a number inside the program's sections is a file address (as `nexo bin` shows); any other number is a memory address; also name, name+0x10, name-0x10, .text, and expressions; A..B is a range",
    ];
    rows("help", l.iter().map(|s| s.to_string()).collect(), json!({}))
}

#[cfg(test)]
mod tests {
    use super::split_words;

    #[test]
    fn the_text_arguments_of_a_call_keep_their_quotes_and_other_quotes_go() {
        assert_eq!(split_words(r#"call f(1, "a b", "x\"y")"#), [r#"call"#, r#"f(1,"#, r#""a b","#, r#""x\"y")"#]);
        assert_eq!(split_words(r#"print hash("abc") + 1"#), ["print", r#"hash("abc")"#, "+", "1"]);
        assert_eq!(split_words(r#"stdin "a b""#), ["stdin", "a b"]);
        assert_eq!(split_words(r#"run ./p "two words" x"#), ["run", "./p", "two words", "x"]);
        assert_eq!(split_words(r#"find "text""#), ["find", "text"]);
    }
}
