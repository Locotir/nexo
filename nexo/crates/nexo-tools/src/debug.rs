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
run <path> [args]  start under the debugger, stop at entry
attach <pid>  attach (asks)
break <addr|name|fn+off> [if expr] [after N] [log {expr}]  breakpoint; tbreak once
breaks, delete <id>, enable|disable <id>
cont|step|next|finish|until <addr>  [--wait N]  resume; a stop shows where, why, changed regs, next instructions
regs, setreg <reg> <v>  (asks)
mem <addr> <len> [--as u8|u32|u64|ptr|str], x, print <expr>, write <addr> <hex> (asks)
bt [N], args, threads, thread <id>, maps, modules, find <hex|text>, dis <addr> [N]
watch <addr> <len> [r|w|x], catch <signal>, stdin <text>, out, history
strace [names|file|net|process|memory], strace show [filter], strace off; catch syscall <names|groups>|off  stop at a call
do \"c1; c2\"  several commands in one call
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

    fn words(args: &Value) -> Result<Vec<String>, String> {
        let Some(line) = args.get("command").and_then(Value::as_str) else {
            return Err("debug needs `command`; `help` lists the commands".into());
        };
        let mut w = nexo_dbg::split_words(line);
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
            description: "Debugger: `run <path>`, break, cont, step, bt, regs, print. `help` lists all.",
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
        match Debug::words(args) {
            Ok(w) if nexo_dbg::dbg_class(&w) == DbgClass::Control => ToolKind::Read,
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
        if self.steer_only && nexo_dbg::dbg_class(&words) != DbgClass::Control {
            return ToolOutput::error(
                "this agent can only look at a session: use regs, bt, mem, print, threads, maps or history",
            );
        }
        // `run ./prog` is relative to the project, as in the shell
        if words.first().map(String::as_str) == Some("run") {
            if let Some(p) = words.get(1).cloned() {
                if Path::new(&p).is_relative() && (p.contains('/') || ctx.cwd.join(&p).is_file()) {
                    words[1] = ctx.cwd.join(&p).display().to_string();
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
        assert!(sent.len() <= 210, "{} chars: {sent}", sent.len());
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

    #[test]
    fn a_long_wait_is_cut_to_the_cap() {
        let w = Debug::words(&json!({"command": "cont --wait 9999"})).unwrap();
        assert_eq!(w, ["cont", "--wait", "60"]);
    }
}
