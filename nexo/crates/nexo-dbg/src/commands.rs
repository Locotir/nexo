//! The commands of `nexo dbg`, one row each, for the map `nexo re` prints. A test reads the match arms of the
//! command dispatcher and fails when a command is missing here, so the map cannot drift from the debugger.

/// `(names, one line)`: the first name is the one the map shows, the rest are spellings the debugger also takes.
pub const COMMANDS: &[(&str, &str)] = &[
    ("run", "start under the debugger: run [--main] [--env K=V] [--unset K] [--cwd DIR] [--aslr] [--argv0 N] [--break SPEC] [--repeat N --until crash|exit|output~TEXT] <exe> [args] [< file | <<< \"text\"] (no shell)"),
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
    ("signal", "go on delivering a signal: signal <SIG|N|0> [--wait N] (Linux; 0 drops the pending one)"),
    ("stops", "how a stop is told: stops compact (one line, a crash stays full) | stops full"),
    ("checkpoint checkpoints", "fork the stopped program into a stopped copy: checkpoint | checkpoint del N|all | checkpoints (Linux, one thread; the pid differs, files opened before are shared)"),
    ("restart", "go back to a checkpoint: restart N (the copy becomes the program, N stays; the process left is killed)"),
    ("kill", "end the program; `kill call` ends a call under way and puts its registers back"),
    ("detach", "let the program go on alone"),
    ("follow", "which side of a fork is kept: follow parent|child|both (a kept child is a session of its own)"),
    ("break b tbreak", "stop at an address, name, file:line or import, with if/after/log; a name in a library not loaded yet is pending"),
    ("breaks watches", "list breakpoints and watches"),
    ("delete del", "remove a breakpoint: delete <id>|all"),
    ("enable disable", "switch a breakpoint on or off"),
    ("watch", "stop or log when a range of any length is written or read (asks)"),
    ("catch catches", "stop on a signal, a system call, or a library load or unload"),
    ("calls", "log (and save for decomp) the real targets of indirect calls: all, or the sites given"),
    ("strace", "log the system calls the program makes"),
    ("ltrace", "log the library calls the program makes, libraries loaded later included; printf/scanf arguments read off the format string"),
    ("hook hooks", "run a note, a stop or a changed argument at a function; int, float and stack arguments, float results"),
    ("heap", "find leaks, double frees and frees of bad pointers: heap on | heap | heap leaks | heap off; every malloc and free stops the process, so the program runs slower"),
    ("cover", "which functions ran; save and diff two runs"),
    ("regs", "registers [name]; xmm0|ymm0|st0 [--as f64|f32|u32x4]; --all adds xmm and x87"),
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
    ("record", "keep the last N instructions as they run (Linux; cont then single-steps, slow): record [N] | record stop"),
    ("back", "look at the last N recorded instructions [N] | --reg REG | --mem ADDR; it looks back, it does not undo"),
    ("rstepi", "undo the last N recorded instructions of this thread: memory and registers go back (needs `record`; refuses across a system call or another thread; asks)"),
    ("rcont", "undo back to an enabled breakpoint's address, a watched change, or the end of the record (asks)"),
    ("stdin", "send text to the program's input"),
    ("setreg", "change a register; setreg xmm0 <lanes> [--as f32] (asks)"),
    ("return", "pop the stopped function's frame, VALUE as its result (rax or xmm0 by its return type): return [VALUE] (asks)"),
    ("jump", "move the pc to file:line, an address or a function and go on: jump <place> [--force] [--stay] (asks)"),
    ("gcore", "write an ELF core of the live Linux process: gcore [FILE] (asks); `core FILE` opens it"),
    ("list", "source around the stop or a place, the current line marked: list [file:line|fn] [N]"),
    ("call", "run a function of the stopped program and give its value: call f(1, \"abc\"); print f(x) + 1 too; registers back after (asks)"),
    ("write", "write bytes into memory (asks)"),
    ("dump", "memory to a file: dump <A..B|fn> [path] | dump module <name> [path]; raw or rebuilt ELF/PE, with the bin call (asks)"),
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

    /// The commands that run the program, change it or replace the session's target, by their first name. The
    /// curator's `DBG_MOVE` must hold each of them and every spelling.
    const MOVES: &[&str] = &[
        "run", "attach", "connect", "core", "cont", "step", "next", "finish", "until", "kill", "detach", "end",
        "signal", "restart", "call", "setreg", "write", "stdin", "return", "jump", "rstepi", "rcont",
    ];

    fn curator_moves() -> Vec<String> {
        let src = include_str!("../../nexo-curator/src/tools.rs");
        let body = src.split("const DBG_MOVE").nth(1).expect("DBG_MOVE is there");
        let body = body.split("];").next().expect("the list ends");
        body.split('"').skip(1).step_by(2).map(String::from).collect()
    }

    /// `(spellings, target)` of each arm of the gdb alias table.
    fn aliases() -> Vec<(Vec<String>, String)> {
        let src = include_str!("gdbdialect.rs");
        let body = src.split("pub fn alias(name: &str) -> &str {").nth(1).expect("alias is there");
        let body = body.split("other => other").next().expect("the table ends");
        let mut v = Vec::new();
        for line in body.lines().map(str::trim) {
            let Some((l, r)) = line.split_once("=>") else { continue };
            let names = l.split('|').map(|n| n.trim().trim_matches('"').to_string()).collect();
            v.push((names, r.trim().trim_matches(',').trim_matches('"').to_string()));
        }
        v
    }

    #[test]
    fn the_curators_move_list_is_every_move_spelling_and_nothing_else() {
        let list = curator_moves();
        let mut want: Vec<String> = Vec::new();
        for (names, _) in COMMANDS {
            let first = names.split(' ').next().unwrap_or("");
            if MOVES.contains(&first) {
                want.extend(names.split(' ').map(String::from));
            }
        }
        for (names, target) in aliases() {
            if canonical(&target).is_some_and(|c| MOVES.contains(&c)) {
                want.extend(names);
            }
        }
        for m in MOVES {
            assert!(canonical(m).is_some(), "{m} is not a command");
        }
        for w in &want {
            assert!(list.contains(w), "{w} runs or changes the program but is not in the curator's DBG_MOVE");
        }
        for l in &list {
            assert!(want.contains(l), "{l} is in the curator's DBG_MOVE but is not a move command or alias");
        }
    }

    #[test]
    fn a_spelling_finds_its_first_name() {
        assert_eq!(canonical("c"), Some("cont"));
        assert_eq!(canonical("disasm"), Some("dis"));
        assert_eq!(canonical("nope"), None);
    }
}
