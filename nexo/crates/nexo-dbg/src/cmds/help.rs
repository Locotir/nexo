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
        "run: nexo dbg run [--break SPEC] [--repeat N --until crash|exit|output~TEXT [--wait SECS]] <exe> [args] (a flaky bug: up to N runs from scratch, 1000 most, stops at the first that crashes, exits non-zero or prints TEXT, or hits a --break; that run stays live, with a tally of the earlier ones)   attach: nexo dbg attach <pid>   core: nexo dbg core <file>",
        "go:    cont [--wait N] | step [N] | next [N] | finish (also prints the returned value and its type) | until <addr> | stop | wait [--wait N] | kill | detach | kill call | signal <SIG|N|0> (Linux: cont delivering that signal, 0 drops the pending one; gdb's `signal N`) | stops compact|full (compact: a stop is one line, why | where | `display` values as expr=value, a crash stays full; `display <expr>`, `display`, `undisplay <n|all>`) | follow parent|child|both (a forked child kept as a session of its own, breakpoints copied)",
        "record: record [N] | record stop keeps the last N instructions (default 10000, most 1000000: pc, changed registers, memory writes up to 64 bytes) while you `step` and `cont` (cont then goes one instruction at a time: slow, Linux only) | back [N] | back --reg REG | back --mem ADDR read it, newest first; it looks back, it does not undo (`status` shows the record)",
        "calls: calls | calls off    log the real target of every indirect call (call reg / call [mem]) as the program runs; read with `log`",
        "trace: strace [names|file|net|process|memory] | strace show [filter] | strace off | catch syscall <names|groups> | catch syscall off; ltrace [names] (printf/scanf `...` read off the format string; a library loaded later adds its import calls) | ltrace show [names] | ltrace off | hook <addr|name> [bt N] [stop] [set argN V] [ret V] (a float for a float argument or result; a name no loaded file has waits for a library that has it) | hook show [name] | hooks | hook del <id>|all; heap on | heap | heap leaks | heap off (leaks, double frees and frees of bad pointers without valgrind: follows malloc, calloc, realloc, free and the aligned forms, stops on a bad free with where the block was allocated and first freed, `heap` lists the live blocks by allocation site, `heap leaks` after the exit; every call stops the process, so the program runs much slower); cover [funcs] | cover show [func] | cover save <A> | cover diff <A> <B>|now | cover off",
        "break: break <addr|*addr|name|file:line|name+off|import> (a function stops after its prologue) [if <expr>] [after N] [log <text with {expr}>] [once] (a name no loaded file has is pending until a library that has it loads) | catch throw|panic|abort (a breakpoint on __cxa_throw, rust_begin_unwind and rust_panic or runtime.gopanic, abort; pending until a library has it) | catch load|unload [NAME|off] (a stop when a library whose file name holds NAME comes or goes) | breaks | delete <id>|all | enable/disable <id>",
        "look:  regs [name] | regs xmm0|ymm0|st0 [--as f64|f32|u8x16|u16x8|u32x4|u64x2|i32x4|i64x2] | regs --all | bt [N] | dis [addr] [N] | dis <A..B> | mem <addr> [len] | mem <A..B> [--as u8|u32|u64|ptr|str|char[16]|item[4]|struct] | print <expr|var|p->f|a[i]> [--as str|raw|T] (a DWARF variable shows Rust String/Vec/&str/slices/Option/Box/Rc, libstdc++ string/vector/unique_ptr/shared_ptr and Go string/slices as values; --as raw shows the raw layout) | ptype <type|var> | args | locals | threads [--bt N] (name, state and system call of each thread on Linux; --bt N adds each one's backtrace, N frames at most) | thread <tid> | frame [N] | up [N] | down [N] (regs, locals, args, print, mem, dis read the selected frame)",
        "       maps [filter] | modules | find <hex|\"text\"> [--in <map>] | out [--all] | log | history [N] | status | list [file:line|fn] [N] (source around the stop, the current line marked)",
        "force (asks): return [VALUE] (pop the stopped function with the unwinder; VALUE goes in rax, or xmm0 for a float result) | jump <file:line|addr|fn> [--force] [--stay] (not out of the current function without --force; --stay only moves the pc) | gcore [FILE] (an ELF core of the live Linux process, opened later with `core FILE`)",
        "change (asks): setreg <reg> <value> | setreg xmm0 <lane0> [lane1 ...] [--as f32] | setreg st0 <float> | write <addr> <hex> | dump <A..B|A+LEN|fn> [path] | dump module <name> [path] (memory to a file: a range raw, a module rebuilt as ELF/PE; the answer names the bin call) | watch [-l] <place|A..B|expr> [len] [w|r|x] [hw|page|step] [if <expr>] [after N] [log [text]] (any length: debug registers, then guarded pages, then single steps; `watch x` of a local ends with one stop when its function returns, `-l` keeps the place; `p->next->val`, `a[i]` and `*(u32*)(rdi+8)` are worked out once, now; `rwatch`/`awatch` = `watch .. r`, reads and writes; $old, $new, $at hold the change; a log watch does not stop) | stdin <text> | catch add|del <SIG> | call f(1, \"text\") or print f(x) + 1 (runs f in the stopped thread, every register put back after)",
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
    use super::{help, split_words};

    #[test]
    fn the_help_names_the_record_and_says_it_does_not_undo() {
        let text = help().text_all();
        for w in ["record [N]", "record stop", "back [N]", "back --reg REG", "back --mem ADDR", "does not undo"] {
            assert!(text.contains(w), "help lacks {w:?}");
        }
    }

    #[test]
    fn the_text_arguments_of_a_call_keep_their_quotes_and_other_quotes_go() {
        assert_eq!(split_words(r#"call f(1, "a b", "x\"y")"#), [r#"call"#, r#"f(1,"#, r#""a b","#, r#""x\"y")"#]);
        assert_eq!(split_words(r#"print hash("abc") + 1"#), ["print", r#"hash("abc")"#, "+", "1"]);
        assert_eq!(split_words(r#"stdin "a b""#), ["stdin", "a b"]);
        assert_eq!(split_words(r#"run ./p "two words" x"#), ["run", "./p", "two words", "x"]);
        assert_eq!(split_words(r#"find "text""#), ["find", "text"]);
    }
}
