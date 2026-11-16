//! The commands of `nexo dbg`, one row each, for the map `nexo re` prints. A test reads the match arms of the
//! command dispatcher and fails when a command is missing here, so the map cannot drift from the debugger.

/// `(names, one line)`: the first name is the one the map shows, the rest are spellings the debugger also takes.
pub const COMMANDS: &[(&str, &str)] = &[
    ("run", "start under the debugger: run [--main] [--env K=V] [--unset K] [--cwd DIR] [--aslr] [--argv0 N] <exe> [args] [< file | <<< \"text\"] (no shell)"),
    ("attach", "attach to a process of yours: attach <pid>"),
    ("core", "open a core file or minidump: core <file> [--exe <path>]"),
    ("connect", "talk to a GDB remote stub: connect <host:port>"),
    ("serve", "offer the debugger as a GDB stub: serve --gdb <port> <program>"),
    ("sessions", "list the live sessions"),
    ("end", "end a session"),
    ("help", "the debugger's own command list"),
    ("status info", "where the program is and why it stopped"),
    ("cont c continue", "run on until the next stop [--wait N]"),
    ("step s", "one instruction or source line, into calls [N]"),
    ("next n", "one line, over calls [N]"),
    ("finish", "run until the current function returns"),
    ("until", "run to an address"),
    ("wait", "wait for a running program to stop [--wait N]"),
    ("stop", "stop a running program now"),
    ("kill", "end the program; `kill call` ends a call under way and puts its registers back"),
    ("detach", "let the program go on alone"),
    ("break b tbreak", "stop at an address, name, file:line or import, with if/after/log"),
    ("breaks watches", "list breakpoints and watches"),
    ("delete del", "remove a breakpoint: delete <id>|all"),
    ("enable disable", "switch a breakpoint on or off"),
    ("watch", "stop when an address is read or written (asks)"),
    ("catch catches", "stop on a signal or a system call"),
    ("calls", "log (and save for decomp) the real targets of indirect calls: all, or the sites given"),
    ("strace", "log the system calls the program makes"),
    ("ltrace", "log the library calls the program makes"),
    ("hook hooks", "run a note, a stop or a changed argument at a function"),
    ("cover", "which functions ran; save and diff two runs"),
    ("regs", "registers [name]"),
    ("bt", "the call stack [N]"),
    ("dis disasm", "disassemble at an address or the current place [N]"),
    ("mem x", "read memory: mem <addr> [len] [--as u32|u64|ptr|str|char[16]|item[4]|struct]"),
    ("print p", "evaluate an expression, variable or field; --as T reads that address as a type"),
    ("ptype whatis", "a type: base, array, a struct named with `struct` or from DWARF/PDB, or a variable's type"),
    ("args", "the arguments of the current function"),
    ("locals", "the local variables of the current function"),
    ("threads", "list the threads"),
    ("thread", "switch to a thread: thread <tid>"),
    ("frame", "select a frame of the call stack for regs, locals, args, print, mem, dis: frame N; alone, describe it"),
    ("up down", "select the caller's frame, or the one it called [N]"),
    ("maps", "the memory map [filter]"),
    ("modules", "the loaded modules"),
    ("find", "search memory for bytes or text"),
    ("out", "what the program printed [--all]"),
    ("log", "what hooks and calls logged"),
    ("history", "the last stops [N]"),
    ("stdin", "send text to the program's input"),
    ("setreg", "change a register (asks)"),
    ("call", "run a function of the stopped program and give its value: call f(1, \"abc\"); print f(x) + 1 too; registers back after (asks)"),
    ("write", "write bytes into memory (asks)"),
    ("name note", "save a name or a note for an address"),
    ("do", "several commands in one call: do \"break f; cont; bt\""),
];

/// The command a word names (any of its spellings), by its first name.
pub fn canonical(word: &str) -> Option<&'static str> {
    COMMANDS.iter().find(|(names, _)| names.split(' ').any(|n| n == word)).and_then(|(n, _)| n.split(' ').next())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The quoted names on the left of each `=>` of the arms between `from` and `to` in `src`.
    fn arms(src: &str, from: &str, to: &str) -> Vec<String> {
        let body = src.split(from).nth(1).expect("the match is there");
        let body = body.split(to).next().expect("the match ends");
        let mut names = Vec::new();
        for line in body.lines().map(str::trim) {
            let Some((left, _)) = line.split_once("=>") else { continue };
            if !left.starts_with('"') {
                continue;
            }
            let left = left.split(" if ").next().unwrap_or(left);
            names.extend(left.split('|').map(|n| n.trim().trim_matches('"').to_string()));
        }
        names
    }

    fn spellings() -> Vec<&'static str> {
        COMMANDS.iter().flat_map(|(n, _)| n.split(' ')).collect()
    }

    #[test]
    fn every_command_the_session_takes_is_in_the_map() {
        let known = spellings();
        let found = arms(include_str!("cmds/mod.rs"), "match cmd.as_str() {", "other =>");
        assert!(found.len() > 40, "the arms were not found: {found:?}");
        for n in found {
            assert!(known.contains(&n.as_str()), "{n} is a debugger command but not in COMMANDS");
        }
    }

    #[test]
    fn every_command_the_client_takes_is_in_the_map() {
        let known = spellings();
        let found = arms(include_str!("client.rs"), "match cmd {", "        _ => {");
        for n in found.iter().filter(|n| *n != "__serve") {
            assert!(known.contains(&n.as_str()), "{n} is a debugger command but not in COMMANDS");
        }
        assert!(found.iter().any(|n| n == "sessions"), "{found:?}");
    }

    #[test]
    fn every_row_names_a_real_command() {
        let mut real = arms(include_str!("cmds/mod.rs"), "match cmd.as_str() {", "other =>");
        real.extend(["run", "attach", "core", "connect", "serve", "sessions", "end"].map(String::from));
        for n in spellings() {
            assert!(real.iter().any(|r| r == n), "{n} is in COMMANDS but the debugger does not take it");
        }
    }

    #[test]
    fn a_spelling_finds_its_first_name() {
        assert_eq!(canonical("c"), Some("cont"));
        assert_eq!(canonical("disasm"), Some("dis"));
        assert_eq!(canonical("nope"), None);
    }
}
