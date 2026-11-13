//! `debug`: run a program under the debugger, or attach to one, and drive it one call at a time. The
//! session (`nexo-dbg`) lives in a thread of this process between two calls and is shared with the
//! sub-agents of the run; it ends with the run.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use async_trait::async_trait;
use nexo_core::{Compact, Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use nexo_dbg::{DbgClass, Host};
use serde_json::{json, Value};

/// Seconds a `--wait` may ask for in one call: a longer wait is cut to this and the call says it is still running.
pub const MAX_WAIT_SECS: u64 = 60;

const COMMANDS: &str = "\
run [--main] <path> [args] [< file|<<< text]  start under the debugger, stop at entry (or main); input is sent, then closed
a; b  several commands in one call (`run ./p <<< \"1 2\"; cont` runs to the crash)
attach <pid>  attach (asks)
break <addr|name|fn+off> [if expr] [after N] [log {expr}]  breakpoint; tbreak once
breaks, delete <id>, enable|disable <id>
cont|step|next|finish|until <addr>  [--wait N]  resume; a stop shows where, why, changed regs, next instructions
regs, setreg <reg> <v>  (asks)
mem <addr> <len> [--as u8|u32|u64|ptr|str], x, print <expr>, write <addr> <hex> (asks)
bt [N], args, threads, thread <id>, maps, modules, find <hex|text>, dis <addr> [N]
watch <addr> <len> [r|w|x], catch <signal>, stdin <text>, out, history
strace [names|file|net|process|memory], strace show [filter], strace off; catch syscall <names|groups>|off  stop at a call
ltrace [names], ltrace show [names], ltrace off  library calls with typed args and result, written at return
hook <addr|name> [bt N] [stop] [set argN V] [ret V], hook show [name], hooks, hook del <id>|all  typed args, result, counters; set/ret change the program
cover [funcs], cover show [func], cover save <A>, cover diff <A> <B>|now, cover off  basic blocks passed in the program's own code
do \"c1; c2\"  several commands in one call
gdb words work: x/20bx a, p/x e, printf \"%d\\n\", e, set $v = e, display, info registers|breakpoints|frame|proc mappings, break *(f+0x10), `commands N ... end` (log, x, printf, continue at every hit, no round trip); a whole gdb script (newlines or `;`, `file p` or `run p args` last) is one call; `print a --as string|vector`
name <addr> <name>, note [0xaddr] <text>  keep a finding for this binary; its next session starts with it (`saved:` line)
detach, kill, end, sessions, help";

/// A debug session for the model. `steer_only` refuses everything that starts, attaches or writes.
pub struct Debug {
    dir: PathBuf,
    steer_only: bool,
}

impl Debug {
    pub fn new() -> Debug {
        nexo_dbg::set_data_dir(nexo_core::paths::data_dir());
        Debug { dir: nexo_dbg::state_dir(), steer_only: false }
    }

    /// A tool for sub-agents that may only look at and steer sessions someone else started.
    pub fn steer_only() -> Debug {
        nexo_dbg::set_data_dir(nexo_core::paths::data_dir());
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
                "Debugger for programs you run (Linux x86-64). One command per call; the session stays between calls.\n{COMMANDS}"
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "One command, e.g. `break main`."},
                    "session": {"type": "string", "description": "Session id when several are live."},
                    "lines": {"type": "integer", "description": "Lines per answer (default 40)."},
                    "from": {"type": "string", "description": "Continue a long answer from this row address."}
                },
                "required": ["command"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: "Run and debug a local program, instead of gdb: `run ./p <<< \"in\"; cont` stops at a crash with file:line, cause, locals. Takes gdb-style commands (break, bt, regs, mem, strace). `help`",
            params: &[],
        })
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
        let words = match Debug::words(&args) {
            Ok(w) => w,
            Err(e) => return ToolOutput::error(e),
        };
        // `a; b` (or one command per line) at the top is a script; `run ...; b` starts the session, then runs the
        // rest in it; a gdb script that ends with `run` is started first and then run in the session
        let parts: Vec<String> =
            nexo_dbg::script_units(Debug::line(&args).unwrap_or("")).iter().map(|p| cap_waits(p)).collect();
        if parts.len() < 2 || words.first().is_some_and(|w| w == "do") {
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
            let at = if words.get(1).is_some_and(|w| w == "--main") { 2 } else { 1 };
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
        if let Some(n) = args.get("lines").and_then(Value::as_u64) {
            words.splice(0..0, ["--lines".to_string(), n.to_string()]);
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
        // 229 chars before the text said it takes gdb-style commands and replaces gdb (+86 for that, nothing else)
        assert!(sent.len() <= 316, "{} chars: {sent}", sent.len());
        // the full text keeps every argument, and the flags the short one leaves out parse at the head of a command
        let full = set.specs_for(true).len() + Debug::new().spec().parameters["properties"].as_object().unwrap().len();
        assert!(full >= 5);
        for flags in [["-s", "ab", "bt"], ["--lines", "5", "bt"], ["--from", "0x10", "bt"]] {
            let w: Vec<String> = flags.iter().map(|s| s.to_string()).collect();
            assert!(nexo_dbg::parse(&w).is_ok(), "{flags:?}");
        }
    }

    fn sleep_bin() -> String {
        ["/usr/bin/sleep", "/bin/sleep"].iter().find(|p| Path::new(p).is_file()).expect("sleep").to_string()
    }

    async fn call(t: &Debug, cmd: &str) -> ToolOutput {
        t.run(json!({"command": cmd}), &ToolContext::new(std::env::temp_dir())).await
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
