//! `debug`: run a program under the debugger, or attach to one, and drive it one call at a time. The
//! session (`nexo-dbg`) lives in a thread of this process between two calls and is shared with the
//! sub-agents of the run; it ends with the run.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use async_trait::async_trait;
use nexo_core::tool::Check;
use nexo_core::{Compact, Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use nexo_dbg::{DbgClass, Host};
use serde_json::{json, Value};

/// Seconds a `--wait` may ask for in one call: a longer wait is cut to this and the call says it is still running.
pub const MAX_WAIT_SECS: u64 = 60;

/// Where the debugger has a backend: the host the binary was built for (x86-64 Linux, Windows and macOS). The same
/// gate as `nexo dbg` and the `bin` answers' line that names a `debug` call, so the three never disagree.
pub const fn available() -> bool {
    nexo_dbg::runs_here()
}

/// The host a program runs on under this tool, from the target the binary was built for: `Linux x86-64`.
pub fn host() -> String {
    host_name(std::env::consts::OS, std::env::consts::ARCH)
}

fn host_name(os: &str, arch: &str) -> String {
    let os = match os {
        "linux" => "Linux",
        "windows" => "Windows",
        "macos" => "macOS",
        o => o,
    };
    let arch = match arch {
        "x86_64" => "x86-64",
        "aarch64" => "arm64",
        a => a,
    };
    format!("{os} {arch}")
}

/// Whether this host has no system call tracing: Windows, and macOS (its tracer, dtruss, needs root).
const NO_SYSCALLS: bool = cfg!(any(windows, target_os = "macos"));

/// The command list as this host has it: where there is no system call tracing those lines are not offered.
fn commands() -> String {
    COMMANDS.lines().filter(|l| !(NO_SYSCALLS && l.starts_with("strace "))).collect::<Vec<_>>().join("\n")
}

const COMMANDS: &str = "\
run [--main] [--env K=V] [--unset K] [--cwd DIR] [--aslr] [--argv0 NAME] <path> [args] [< file|<<< text]  start under the debugger, stop at entry (or main); env, folder, ASLR (off by default) and argv[0] as given; input is sent, then closed
a; b  several commands in one call (`run ./p <<< \"1 2\"; cont` runs to the crash)
attach <pid>  attach (asks)
break <addr|name|fn+off> [if expr] [after N] [log {expr}]  breakpoint; tbreak once
breaks, delete <id>, enable|disable <id>
cont|step|next|finish|until <addr>  [--wait N]  resume; a stop shows where, why, changed regs, next instructions
regs, setreg <reg> <v>  (asks)
call f(1, \"text\"), print f(x) + 1  run a function of the stopped program with these arguments and give its value; every register is put back after; a stop inside it says so, `kill call` ends it (asks)
mem <addr> [len] [--as u32|char[16]|ptr|str|item[4]|a struct], x, print <expr> [--as T], ptype <type|var>, write <addr> <hex> (asks)
bt [N], frame [N], up [N], down [N] (regs, locals, args, print, mem, dis then read that frame), args, threads, thread <id>, maps, modules, find <hex|text>, dis <addr> [N]
watch <addr> <len> [r|w|x], catch <signal>, stdin <text>, out, history
dump <A..B|A+LEN|fn> [path], dump module <name> [path]  memory to a file (a range raw at its address, a whole module rebuilt as ELF/PE); the answer names the `bin` call that reads it (asks)
strace [names|file|net|process|memory], strace show [filter], strace off; catch syscall <names|groups>|off  stop at a call
ltrace [names], ltrace show [names], ltrace off  library calls with typed args and result, written at return
hook <addr|name> [bt N] [stop] [set argN V] [ret V], hook show [name], hooks, hook del <id>|all  typed args, result, counters; set/ret change the program
cover [funcs], cover show [func], cover save <A>, cover diff <A> <B>|now, cover off  basic blocks passed in the program's own code
do \"c1; c2\"  several commands in one call
gdb words work: x/20bx a, p/x e, printf \"%d\\n\", e, set $v = e, display, info registers|breakpoints|frame|proc mappings, break *(f+0x10), `commands N ... end` (log, x, printf, continue at every hit, no round trip); a whole gdb script (newlines or `;`, `file p` or `run p args` last) is one call; `print a --as string|vector`
name <addr> <name>, note [0xaddr] <text>  keep a finding for this binary; its next session starts with it (`saved:` line)
follow parent|child|both  which side of a fork is kept; a kept child is a session of its own (`-s <id>`), breakpoints copied
detach, kill, end, sessions, help";

/// The short text every request carries: the questions the tool answers (run on chosen input, stop, registers and
/// memory, traced calls, hooks, a running process), in the words a model asks them, and the `bin` tool beside it.
const COMPACT: &str = if NO_SYSCALLS {
    "Runs a program on input, stops it (`run ./p <<< \"in\"; cont`: a crash's file:line, cause, locals); reads or sets regs, memory; ltrace, hooks; attaches to a pid. gdb forms work. `bin` reads the file. `help`"
} else {
    "Runs a program on input, stops it (`run ./p <<< \"in\"; cont`: a crash's file:line, cause, locals); reads or sets regs, memory; ltrace, strace, hooks; attaches to a pid. gdb forms work. `bin` reads the file. `help`"
};

/// A debug session for the model. `steer_only` refuses everything that starts, attaches or writes.
pub struct Debug {
    dir: PathBuf,
    steer_only: bool,
}

impl Debug {
    pub fn new() -> Debug {
        Debug { dir: nexo_dbg::state_dir(), steer_only: false }
    }

    /// A tool for sub-agents that may only look at and steer sessions someone else started.
    pub fn steer_only() -> Debug {
        Debug { dir: nexo_dbg::state_dir(), steer_only: true }
    }

    /// Sessions kept under `dir` (tests).
    pub fn in_dir(dir: &Path) -> Debug {
        Debug { dir: dir.to_path_buf(), steer_only: false }
    }

    fn line(args: &Value) -> Result<&str, String> {
        args.get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| "debug needs `command`; `help` lists the commands".into())
    }

    fn words(args: &Value) -> Result<Vec<String>, String> {
        let line = Debug::line(args)?;
        // one command with a comment line or a trailing newline is still one command
        let parts = nexo_dbg::script_units(line);
        let text = if let [one] = parts.as_slice() { one.as_str() } else { line };
        let mut w = nexo_dbg::split_words(text);
        // a wait past the cap is cut: the call must come back
        let mut i = 0;
        while i + 1 < w.len() {
            if w[i] == "--wait" {
                if let Ok(n) = w[i + 1].parse::<u64>() {
                    w[i + 1] = n.min(MAX_WAIT_SECS).to_string();
                }
            }
            i += 1;
        }
        Ok(w)
    }

    /// What a call may do, from every command in it: a script is as strong as its strongest command.
    fn class(args: &Value) -> DbgClass {
        let Ok(line) = Debug::line(args) else { return DbgClass::Other };
        let parts = nexo_dbg::script_units(line);
        match parts.as_slice() {
            [] => DbgClass::Control,
            [_] => Debug::words(args).map_or(DbgClass::Other, |w| nexo_dbg::dbg_class(&w)),
            _ => nexo_dbg::dbg_class(&["do".to_string(), parts.join("; ")]),
        }
    }
}

/// The first file a call's `dump`s write that a permission rule denies, said as the reason nothing ran. The rules
/// that cover `edit` see this call's `command` and not the files named inside it, so each one is asked here; the call
/// was already asked about as one that changes the program, so only a rule that denies the place stops it. A file
/// still relative is relative to the folder the session runs in.
fn denied_dump(files: &[String], ctx: &ToolContext) -> Option<String> {
    let check = ctx.check.as_ref()?;
    files.iter().find_map(|f| {
        let mut p = PathBuf::from(f);
        if p.is_relative() {
            p = std::env::current_dir().map_or(p.clone(), |d| d.join(&p));
        }
        let path = p.display().to_string();
        (check("edit", ToolKind::Write, &json!({ "path": path })) == Check::Deny)
            .then(|| format!("a permission rule denies writing {path}, so this call ran nothing"))
    })
}

/// A wait past the cap inside a script part is cut too: `cont --wait 9999` must come back.
fn cap_waits(part: &str) -> String {
    let (mut out, mut rest) = (String::new(), part);
    while let Some(i) = rest.find("--wait ") {
        let (head, tail) = rest.split_at(i + "--wait ".len());
        out.push_str(head);
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            rest = tail;
            continue;
        }
        let n = digits.parse::<u64>().unwrap_or(u64::MAX);
        out.push_str(&n.min(MAX_WAIT_SECS).to_string());
        rest = &tail[digits.len()..];
    }
    out.push_str(rest);
    out
}

impl Default for Debug {
    fn default() -> Debug {
        Debug::new()
    }
}

/// One call at a time per process: two at once would race each other's stops.
fn gate() -> &'static tokio::sync::Mutex<()> {
    static G: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    G.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[async_trait]
impl Tool for Debug {
    fn name(&self) -> &str {
        "debug"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "debug".into(),
            description: format!(
                "Debugger for programs you run ({}). One command per call; the session stays between calls.\n{}",
                host(),
                commands()
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "One command, e.g. `break main`."},
                    "session": {"type": "string", "description": "Session id when several are live."},
                    "lines": {"type": ["integer", "string"], "description": "Lines per answer (default 120; \"all\" for every row)."},
                    "from": {"type": "string", "description": "Continue a long answer from this row address."}
                },
                "required": ["command"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact { description: COMPACT, params: &[] })
    }

    fn rare(&self) -> bool {
        true
    }

    /// Only `command` is offered in the short text; `session`, `lines` and `from` are still taken as arguments
    /// and as flags at the head of the command (`-s`, `--lines`, `--from`, listed by `help`).
    fn trim_compact(&self, spec: &mut ToolSpec) {
        if let Some(p) = spec.parameters.get_mut("properties").and_then(Value::as_object_mut) {
            p.retain(|k, _| k == "command");
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Exec
    }

    fn call_kind(&self, args: &Value) -> ToolKind {
        match Debug::class(args) {
            DbgClass::Control => ToolKind::Read,
            _ => ToolKind::Exec,
        }
    }

    fn summary(&self, args: &Value) -> String {
        match args.get("command").and_then(Value::as_str) {
            Some(c) => format!("debug: {c}"),
            None => "debug".into(),
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let mut words = match Debug::words(&args) {
            Ok(w) => w,
            Err(e) => return ToolOutput::error(e),
        };
        // `a; b` (or one command per line) at the top is a script; `run ...; b` starts the session, then runs the
        // rest in it; a gdb script that ends with `run` is started first and then run in the session
        let units = nexo_dbg::script_units(Debug::line(&args).unwrap_or(""));
        // the debugger runs no shell: a shell word or operator is named before anything runs, so nothing half-runs
        let mut shell: Vec<String> = Vec::new();
        for item in units.iter().flat_map(|u| nexo_dbg::debug_shell_items(u)) {
            if !shell.contains(&item) {
                shell.push(item);
            }
        }
        if !shell.is_empty() {
            return ToolOutput::error(nexo_dbg::debug_fact(&shell, &ctx.cwd.display().to_string()));
        }
        // the file a `dump` writes is relative to the project, as in the shell, in a script too
        nexo_dbg::dumps_in(&mut words, &ctx.cwd);
        let parts: Vec<String> = units.iter().map(|p| nexo_dbg::dump_part_in(&cap_waits(p), &ctx.cwd)).collect();
        let single = parts.len() < 2 || words.first().is_some_and(|w| w == "do");
        let files = match single {
            true => nexo_dbg::dump_files(&words),
            false => parts.iter().flat_map(|p| nexo_dbg::dump_files(&nexo_dbg::split_words(p))).collect(),
        };
        if let Some(denied) = denied_dump(&files, ctx) {
            return ToolOutput::error(denied);
        }
        if single {
            return self.one(words, &args, ctx).await;
        }
        match nexo_dbg::gdb_plan(&parts) {
            Err(e) => return ToolOutput::error(e),
            Ok(Some(plan)) => return self.gdb_shape(plan, &args, ctx).await,
            Ok(None) => {}
        }
        let first = nexo_dbg::split_words(&parts[0]);
        if !matches!(first.first().map(String::as_str), Some("run" | "attach" | "core")) {
            return self.one(vec!["do".into(), parts.join("; ")], &args, ctx).await;
        }
        let started = self.one(first, &args, ctx).await;
        let id = started.content.split_whitespace().nth(1).map(str::to_string);
        let (true, Some(id)) = (!started.is_error, id) else { return started };
        let rest = self.one(vec!["-s".into(), id, "do".into(), parts[1..].join("; ")], &json!({}), ctx).await;
        let text = format!("{}\n{}", started.content, rest.content);
        if rest.is_error {
            ToolOutput::error(text)
        } else {
            ToolOutput::ok(text)
        }
    }
}

impl Debug {
    /// A gdb script that ends with `run`: the session is started (stopped at the entry), the commands before `run`
    /// are run in it, `cont` stands for `run`, then what followed it, and a closing `quit` ends the session.
    async fn gdb_shape(&self, plan: nexo_dbg::GdbPlan, args: &Value, ctx: &ToolContext) -> ToolOutput {
        let started = self.one(nexo_dbg::split_words(&plan.start), args, ctx).await;
        let id = started.content.split_whitespace().nth(1).map(str::to_string);
        let (true, Some(id)) = (!started.is_error, id) else { return started };
        let mut text = started.content;
        let mut failed = false;
        if plan.dropped > 0 {
            text.push_str(&format!("\nleft out {} gdb setting(s) that change nothing here", plan.dropped));
        }
        if !plan.rest.is_empty() {
            let rest =
                self.one(vec!["-s".into(), id.clone(), "do".into(), plan.rest.join("; ")], &json!({}), ctx).await;
            failed = rest.is_error;
            text = format!("{text}\n{}", rest.content);
        }
        if plan.end {
            let end = self.one(vec!["-s".into(), id, "end".into()], &json!({}), ctx).await;
            text = format!("{text}\n{}", end.content);
        }
        if failed {
            ToolOutput::error(text)
        } else {
            ToolOutput::ok(text)
        }
    }

    async fn one(&self, mut words: Vec<String>, args: &Value, ctx: &ToolContext) -> ToolOutput {
        if self.steer_only && nexo_dbg::dbg_class(&words) != DbgClass::Control {
            return ToolOutput::error(
                "this agent can only look at a session: use regs, bt, mem, print, threads, maps or history",
            );
        }
        // `run ./prog` and its `< file` are relative to the project, as in the shell
        if words.first().map(String::as_str) == Some("run") {
            // launch options come first; a relative `--cwd` is relative to the project too
            let mut at = 1;
            while let Some(flag) = words.get(at).map(String::as_str) {
                match flag {
                    "--main" | "--aslr" => at += 1,
                    "--env" | "--unset" | "--argv0" => at += 2,
                    "--cwd" => {
                        if let Some(d) = words.get(at + 1).filter(|d| Path::new(d.as_str()).is_relative()).cloned() {
                            words[at + 1] = ctx.cwd.join(d).display().to_string();
                        }
                        at += 2;
                    }
                    _ => break,
                }
            }
            if let Some(p) = words.get(at).cloned() {
                if Path::new(&p).is_relative() && (p.contains('/') || ctx.cwd.join(&p).is_file()) {
                    words[at] = ctx.cwd.join(&p).display().to_string();
                }
            }
            if let Some(i) = words.iter().position(|w| w == "<") {
                if let Some(f) = words.get(i + 1).filter(|f| Path::new(f).is_relative()).cloned() {
                    words[i + 1] = ctx.cwd.join(f).display().to_string();
                }
            }
        }
        if let Some(s) = args.get("session").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            words.splice(0..0, ["-s".to_string(), s.to_string()]);
        }
        let lines = match args.get("lines") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            other => other.and_then(Value::as_u64).map(|n| n.to_string()),
        };
        if let Some(n) = lines {
            words.splice(0..0, ["--lines".to_string(), n]);
        }
        if let Some(f) = args.get("from").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            words.splice(0..0, ["--from".to_string(), f.to_string()]);
        }
        let call = match nexo_dbg::parse(&words) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("{e}; `help` lists the commands")),
        };
        let dir = self.dir.clone();
        let _turn = gate().lock().await;
        let done = tokio::task::spawn_blocking(move || nexo_dbg::execute(&call, &dir, &Host::Thread, "debug")).await;
        match done {
            Ok(Ok(text)) => ToolOutput::ok(text),
            Ok(Err(e)) => ToolOutput::error(e.to_string()),
            Err(e) => ToolOutput::error(format!("the debugger stopped working: {e}")),
        }
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod tests {
    use super::*;

    /// The tool is on by default on Linux x86-64, so what every request carries for it stays small: the compact
    /// text only (the tool is `rare`).
    #[test]
    fn what_every_request_carries_for_the_tool_is_the_compact_text_and_stays_small() {
        let mut set = nexo_core::tool::ToolSet::new();
        set.add(std::sync::Arc::new(Debug::new()));
        let spec = set.specs_for(false).pop().unwrap();
        let sent = serde_json::to_string(
            &json!({"name": spec.name, "description": spec.description, "parameters": spec.parameters}),
        )
        .unwrap();
        eprintln!("debug tool as offered: {} chars: {sent}", sent.len());
        // 320 chars before the text said gdb forms work and named `bin` (+27); the questions it names fit the same
        assert!(sent.len() <= 347, "{} chars: {sent}", sent.len());
        // it names the questions a model brings to a running program, and the tool that reads the file
        for w in
            ["input", "crash", "regs", "memory", "ltrace", "strace", "hooks", "attaches to a pid", "gdb forms", "`bin`"]
        {
            assert!(spec.description.contains(w), "{w}: {}", spec.description);
        }
        // the full text keeps every argument, and the flags the short one leaves out parse at the head of a command
        let full = set.specs_for(true).len() + Debug::new().spec().parameters["properties"].as_object().unwrap().len();
        assert!(full >= 5);
        for flags in [["-s", "ab", "bt"], ["--lines", "5", "bt"], ["--from", "0x10", "bt"]] {
            let w: Vec<String> = flags.iter().map(|s| s.to_string()).collect();
            assert!(nexo_dbg::parse(&w).is_ok(), "{flags:?}");
        }
    }

    /// The short text says what is true: gdb forms work (the three most used do), `bin` is named, and the old
    /// claim that they do not is gone.
    #[tokio::test]
    async fn the_gdb_forms_the_short_text_names_work_in_a_live_session() {
        let text = Debug::new().compact().unwrap().description.to_string();
        assert!(text.contains("gdb forms work") && text.contains("`bin`"), "{text}");
        assert!(!text.contains("not gdb"), "{text}");
        let dir = tempfile::tempdir().unwrap();
        let t = Debug::in_dir(dir.path());
        let first = call(&t, &format!("run {} 30", sleep_bin())).await;
        assert!(!first.is_error, "{}", first.content);
        // the three most used, and the spellings the dialect maps next to them
        for c in ["x/8x $rsp", "p/x $rsp", "set $v = 5", "print $v", "backtrace", "info registers"] {
            let o = call(&t, c).await;
            assert!(!o.is_error, "{c}: {}", o.content);
        }
        let v = call(&t, "print $v").await;
        assert!(v.content.contains("0x5") || v.content.contains('5'), "{}", v.content);
        call(&t, "end").await;
    }

    /// The full text names the host the binary was built for, and offers the system call lines Linux has.
    #[test]
    fn the_full_text_names_this_host_and_keeps_the_linux_trace_lines() {
        let d = Debug::new().spec().description;
        assert!(d.starts_with("Debugger for programs you run (Linux x86-64)."), "{d}");
        assert!(d.contains("\nstrace [names|file|net|process|memory]") && d.contains("catch syscall"), "{d}");
        assert!(available());
    }

    /// A Windows program cannot run on this host: the answer says so as a fact and names `bin`, and starts nothing.
    #[tokio::test]
    async fn running_a_pe_file_on_linux_says_this_host_cannot_run_it() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("prog.exe");
        let mut pe = vec![0u8; 0x100];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3c] = 0x80;
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        std::fs::write(&exe, &pe).unwrap();
        let t = Debug::in_dir(&dir.path().join("state"));
        let o = call(&t, &format!("run {}", exe.display())).await;
        assert!(o.is_error, "{}", o.content);
        assert!(o.content.contains("is a Windows (PE) program; this host runs Linux programs only"), "{}", o.content);
        assert!(o.content.contains("`bin` reads it without running it"), "{}", o.content);
        assert!(call(&t, "sessions").await.content.contains("no debug sessions"));
    }

    #[tokio::test]
    async fn help_works_before_any_session_and_other_commands_say_which_one_was_not_run() {
        let dir = tempfile::tempdir().unwrap();
        let t = Debug::in_dir(&dir.path().join("state"));
        let h = call(&t, "help").await;
        assert!(!h.is_error && h.content.contains("break:"), "{}", h.content);
        let b = call(&t, "break main").await;
        assert!(b.is_error && b.content.contains("no debug session for `break main`"), "{}", b.content);
    }

    #[tokio::test]
    async fn shell_in_a_debug_call_is_named_before_anything_runs() {
        let dir = tempfile::tempdir().unwrap();
        let t = Debug::in_dir(&dir.path().join("state"));
        let cwd = std::path::PathBuf::from("/tmp/t1d");
        let d = "/tmp/claude-1000/-home-lctr/0813d0c7-a644-4a6f-8cd3-c8c257f6635e/scratchpad/re-t5/t1d";
        // the two calls of the t1d trial
        for c in [
            format!("cd {d} && printf '0 0 A\\n1048576 0 B\\n' > /tmp/crash.in && run ./paint < /tmp/crash.in; cont"),
            format!(
                "printf '0 0 A\\n1048576 0 B\\n' > /tmp/crash.in; cd {d}; run ./paint < /tmp/crash.in; cont; bt; regs"
            ),
        ] {
            let o = t.run(json!({"command": c}), &ToolContext::new(cwd.clone())).await;
            assert!(o.is_error, "{}", o.content);
            assert!(
                o.content.contains("`cd`")
                    && o.content.contains("`printf`")
                    && o.content.contains("`>`")
                    && o.content.contains(" are shell;"),
                "{}",
                o.content
            );
            assert!(o.content.contains("working directory is /tmp/t1d;"), "{}", o.content);
            assert!(o.content.contains("then `run ./prog < file`"), "{}", o.content);
            assert!(!o.content.contains("no debug session"), "{}", o.content);
        }
        // no session was touched
        assert!(nexo_dbg::sessions(&dir.path().join("state")).is_empty());
    }

    fn sleep_bin() -> String {
        ["/usr/bin/sleep", "/bin/sleep"].iter().find(|p| Path::new(p).is_file()).expect("sleep").to_string()
    }

    async fn call(t: &Debug, cmd: &str) -> ToolOutput {
        t.run(json!({"command": cmd}), &ToolContext::new(std::env::temp_dir())).await
    }

    #[tokio::test]
    async fn a_dump_to_a_place_a_rule_denies_runs_nothing_in_every_form_of_the_call() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let t = Debug::in_dir(&state);
        let locked = dir.path().join("locked");
        let deny = locked.display().to_string();
        let mut ctx = ToolContext::new(dir.path().to_path_buf());
        ctx.check = Some(std::sync::Arc::new(move |tool: &str, kind, args: &Value| {
            assert_eq!((tool, kind), ("edit", ToolKind::Write));
            if args["path"].as_str().unwrap().starts_with(&deny) {
                Check::Deny
            } else {
                Check::Ask
            }
        }));
        let sleep = sleep_bin();
        for c in [
            "dump $rsp 8 locked/a.bin".to_string(),
            "dump $rsp 8 locked/a.bin --all".to_string(),
            format!("run {sleep} 30; dump $rsp 8 locked/a.bin"),
            "do \"break main; commands; dump module libc locked/l.dump; end\"".to_string(),
            format!("dump binary memory {} 0x1000 0x2000", locked.join("m.bin").display()),
        ] {
            let o = t.run(json!({"command": c}), &ctx).await;
            let said = format!("a permission rule denies writing {}", locked.display());
            assert!(o.is_error && o.content.contains(&said), "{c}: {}", o.content);
        }
        assert!(nexo_dbg::sessions(&state).is_empty(), "no program was started");

        // a place no rule denies: the file is written, relative to the project in a script too
        let o = t.run(json!({"command": format!("run {sleep} 30; dump $rsp 8 open.bin")}), &ctx).await;
        assert!(!o.is_error && o.content.contains("wrote 8 bytes"), "{}", o.content);
        assert_eq!(std::fs::metadata(dir.path().join("open.bin")).unwrap().len(), 8);
        let id = o.content.split_whitespace().nth(1).unwrap().to_string();
        let end = t.run(json!({"command": "end", "session": id}), &ctx).await;
        assert!(end.content.contains("ended"), "{}", end.content);
    }

    #[tokio::test]
    async fn a_session_lives_between_calls_and_the_kinds_follow_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let t = Debug::in_dir(dir.path());
        let kind = |c: &str| t.call_kind(&json!({"command": c}));
        assert_eq!(kind("regs"), ToolKind::Read);
        assert_eq!(kind("cont --wait 5"), ToolKind::Read);
        assert_eq!(kind("run ./a.out"), ToolKind::Exec);
        assert_eq!(kind("attach 12"), ToolKind::Exec);
        assert_eq!(kind("write 0x10 90"), ToolKind::Exec);
        assert_eq!(kind("dump module libc out.bin"), ToolKind::Exec);
        // a hook that only reads is control; one that sets an argument or the result asks
        assert_eq!(kind("hook check bt 3"), ToolKind::Read);
        assert_eq!(kind("hook check set arg0 5"), ToolKind::Exec);
        assert_eq!(kind("hook check ret 0"), ToolKind::Exec);

        let first = call(&t, &format!("run {} 30", sleep_bin())).await;
        assert!(!first.is_error, "{}", first.content);
        assert!(first.content.contains("started pid"), "{}", first.content);
        let regs = call(&t, "regs").await;
        assert!(regs.content.contains("rip"), "{}", regs.content);
        let none = call(&t, "frobnicate").await;
        assert!(none.is_error && none.content.contains("help"), "{}", none.content);
        let end = call(&t, "end").await;
        assert!(end.content.contains("ended"), "{}", end.content);
        let gone = call(&t, "regs").await;
        assert!(gone.is_error && gone.content.contains("run <path>"), "{}", gone.content);
    }

    #[tokio::test]
    async fn a_steer_only_tool_refuses_to_start_attach_or_write() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Debug::in_dir(dir.path());
        t.steer_only = true;
        for c in ["run /bin/true", "attach 1", "write 0x10 90", "setreg rax 1"] {
            let o = call(&t, c).await;
            assert!(o.is_error && o.content.contains("only look"), "{c}: {}", o.content);
        }
        assert!(call(&t, "sessions").await.content.contains("no debug sessions"));
    }

    #[tokio::test]
    async fn strace_runs_through_the_tool_with_the_stable_header() {
        let dir = tempfile::tempdir().unwrap();
        let t = Debug::in_dir(dir.path());
        assert_eq!(t.call_kind(&json!({"command": "strace show"})), ToolKind::Read);
        let tru = ["/usr/bin/true", "/bin/true"].iter().find(|p| Path::new(p).is_file()).expect("true").to_string();
        let first = call(&t, &format!("run {tru}")).await;
        assert!(!first.is_error, "{}", first.content);
        assert!(!call(&t, "strace process").await.is_error);
        let end = call(&t, "cont").await;
        assert!(end.content.contains("exited with code 0"), "{}", end.content);
        let shown = call(&t, "strace show").await;
        assert!(shown.content.starts_with("debug "), "{}", shown.content);
        assert!(shown.content.contains("exit_group(0) = ?"), "{}", shown.content);
        call(&t, "end").await;
    }

    #[tokio::test]
    async fn a_run_with_input_and_more_commands_after_a_semicolon_is_one_call() {
        let dir = tempfile::tempdir().unwrap();
        let t = Debug::in_dir(dir.path());
        let cat = ["/usr/bin/cat", "/bin/cat"].iter().find(|p| Path::new(p).is_file()).expect("cat");
        let o = call(&t, &format!("run {cat} <<< \"hello there\"; cont")).await;
        assert!(!o.is_error, "{}", o.content);
        assert!(o.content.contains("started pid") && o.content.contains("closed the input"), "{}", o.content);
        assert!(o.content.contains("hello there") && o.content.contains("exited with code 0"), "{}", o.content);
        // a script without `run` in front runs in the live session
        let o = call(&t, &format!("run {} 30", sleep_bin())).await;
        let id = o.content.split_whitespace().nth(1).unwrap().to_string();
        let o = t.run(json!({"command": "regs; bt 1", "session": id}), &ToolContext::new(std::env::temp_dir())).await;
        assert!(
            o.content.contains("> regs") && o.content.contains("rip") && o.content.contains("> bt 1"),
            "{}",
            o.content
        );
        assert_eq!(
            nexo_dbg::script_units("break f if x == 1; stdin \"a;b\"\n# a comment\ncont"),
            [r#"break f if x == 1"#, r#"stdin "a;b""#, "cont"]
        );
    }

    #[tokio::test]
    async fn ltrace_runs_through_the_tool_with_the_stable_header() {
        let dir = tempfile::tempdir().unwrap();
        let t = Debug::in_dir(dir.path());
        let tru = ["/usr/bin/true", "/bin/true"].iter().find(|p| Path::new(p).is_file()).expect("true").to_string();
        assert!(!call(&t, &format!("run {tru}")).await.is_error);
        let on = call(&t, "ltrace").await;
        assert!(on.content.contains("tracing library calls"), "{}", on.content);
        let end = call(&t, "cont").await;
        assert!(end.content.contains("exited with code 0"), "{}", end.content);
        let shown = call(&t, "ltrace show").await;
        assert!(shown.content.starts_with("debug "), "{}", shown.content);
        assert!(shown.content.contains("__libc_start_main("), "{}", shown.content);
        let hooks = call(&t, "hooks").await;
        assert!(hooks.content.starts_with("debug ") && hooks.content.contains("no hooks"), "{}", hooks.content);
        let cover = call(&t, "cover show").await;
        assert!(cover.is_error && cover.content.contains("`cover` turns it on"), "{}", cover.content);
        call(&t, "end").await;
    }

    #[test]
    fn a_script_is_as_strong_as_its_strongest_command() {
        let dir = tempfile::tempdir().unwrap();
        let t = Debug::in_dir(dir.path());
        let kind = |c: &str| t.call_kind(&json!({"command": c}));
        // looking, in gdb's words and in a block that logs and goes on, asks nothing
        assert_eq!(kind("x/20bx $rsp"), ToolKind::Read);
        assert_eq!(kind("p/x $rax; printf \"%d\\n\", 1; info registers rip"), ToolKind::Read);
        assert_eq!(
            kind("break *0x10\ncommands\nsilent\nprintf \"x\\n\"\nx/4x $rsp\ncontinue\nend\ncont"),
            ToolKind::Read
        );
        assert_eq!(kind("set $v = $rsp + 8"), ToolKind::Read);
        // a write hides in no spelling: a register, memory, a command after a look, or inside a block
        assert_eq!(kind("set $rax = 5"), ToolKind::Exec);
        assert_eq!(kind("set var x = 5"), ToolKind::Exec);
        assert_eq!(kind("set {int}0x1000 = 5"), ToolKind::Exec);
        assert_eq!(kind("bt; write 0x10 90"), ToolKind::Exec);
        assert_eq!(kind("regs; setreg rax 1"), ToolKind::Exec);
        assert_eq!(kind("break main\ncommands\nwrite 0x10 90\ncontinue\nend"), ToolKind::Exec);
        assert_eq!(kind("file ./p\nbreak main\nrun"), ToolKind::Exec);
        assert_eq!(kind("run ./p; cont"), ToolKind::Exec);
        assert_eq!(kind("do \"bt; write 0x10 90\""), ToolKind::Exec);
    }

    #[test]
    fn a_wait_inside_a_script_part_is_cut_to_the_cap() {
        assert_eq!(cap_waits("cont --wait 9999"), "cont --wait 60");
        assert_eq!(cap_waits("cont --wait 5"), "cont --wait 5");
        assert_eq!(cap_waits("finish --wait 99999999999999999999999 --lines 3"), "finish --wait 60 --lines 3");
        assert_eq!(cap_waits("printf \"--wait\""), "printf \"--wait\"");
    }

    #[test]
    fn a_long_wait_is_cut_to_the_cap() {
        let w = Debug::words(&json!({"command": "cont --wait 9999"})).unwrap();
        assert_eq!(w, ["cont", "--wait", "60"]);
    }
}

#[cfg(test)]
mod host_tests {
    use super::*;

    #[test]
    fn the_host_is_named_from_the_target_the_binary_was_built_for() {
        assert_eq!(host_name("linux", "x86_64"), "Linux x86-64");
        assert_eq!(host_name("windows", "x86_64"), "Windows x86-64");
        assert_eq!(host_name("macos", "aarch64"), "macOS arm64");
        assert_eq!(host(), host_name(std::env::consts::OS, std::env::consts::ARCH));
        let d = Debug::new().spec().description;
        assert!(d.contains(&format!("({})", host())), "{d}");
    }

    #[test]
    fn the_trace_lines_are_offered_only_where_the_host_has_system_calls() {
        let c = commands();
        assert_eq!(NO_SYSCALLS, cfg!(any(windows, target_os = "macos")));
        assert_eq!(c.contains("strace "), !NO_SYSCALLS, "{c}");
        assert_eq!(c.contains("catch syscall"), !NO_SYSCALLS, "{c}");
        // everything else is the same on every host
        assert!(
            c.contains("ltrace [names]")
                && c.contains("run [--main] [--env K=V]")
                && c.ends_with("detach, kill, end, sessions, help")
        );
        assert_eq!(COMPACT.contains("strace"), !NO_SYSCALLS);
    }

    #[test]
    fn the_tool_is_offered_where_nexo_dbg_runs_programs() {
        assert_eq!(available(), nexo_dbg::runs_here());
        assert_eq!(
            available(),
            cfg!(all(target_arch = "x86_64", any(target_os = "linux", windows, target_os = "macos")))
        );
    }
}
