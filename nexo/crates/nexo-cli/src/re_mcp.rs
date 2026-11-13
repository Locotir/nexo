//! `nexo re --mcp`: the nexo RE kit as an MCP server on stdio, so other harnesses can use it. JSON-RPC 2.0, one
//! message per line, written by hand (no SDK). The tools are generated from the clap definition of `nexo bin`
//! (one for each op, so the list cannot drift) plus three for `nexo dbg` sessions: start, command, stop. A tool
//! result is the text `nexo bin` or `nexo dbg` prints. Anything that writes saved state, writes into a program,
//! attaches to a process or opens a port is refused unless the server started with `--allow-changes`.

use std::io::{BufRead, Write};

use clap::{Arg, ArgAction, CommandFactory, Parser};
use serde_json::{json, Value};

use crate::dbg_cmd::DbgArgs;

/// The protocol versions this server speaks, newest first.
const VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Ops of `nexo bin` that save names, notes, types or files; `crypto --save` does too.
const SAVING_OPS: &[&str] = &["name", "note", "proto", "struct", "var", "sigs", "patch"];

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Bin(String),
    DbgStart,
    DbgCommand,
    DbgStop,
}

struct Tool {
    name: String,
    description: String,
    schema: Value,
    kind: Kind,
}

pub struct Server {
    allow_changes: bool,
    tools: Vec<Tool>,
}

/// Read requests from `input` and write answers to `output` until the input ends.
pub fn serve<R: BufRead, W: Write>(input: R, mut output: W, allow_changes: bool) -> std::io::Result<()> {
    let server = Server::new(allow_changes);
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(answer) = server.handle_line(&line) {
            writeln!(output, "{answer}")?;
            output.flush()?;
        }
    }
    Ok(())
}

/// `nexo re --mcp`: serve on stdin and stdout.
pub fn run_stdio(allow_changes: bool) -> i32 {
    match serve(std::io::stdin().lock(), std::io::stdout().lock(), allow_changes) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("nexo re --mcp: {e}");
            1
        }
    }
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn text_result(text: String, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

impl Server {
    pub fn new(allow_changes: bool) -> Server {
        let mut tools = bin_tools();
        tools.extend(dbg_tools());
        Server { allow_changes, tools }
    }

    /// The answer to one line, as the text to write; none for a notification.
    pub fn handle_line(&self, line: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return Some(rpc_error(Value::Null, -32700, format!("not JSON: {e}")).to_string()),
        };
        if let Value::Array(items) = msg {
            let answers: Vec<Value> = items.iter().filter_map(|m| self.handle(m)).collect();
            return (!answers.is_empty()).then(|| Value::Array(answers).to_string());
        }
        self.handle(&msg).map(|v| v.to_string())
    }

    fn handle(&self, msg: &Value) -> Option<Value> {
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // a response from the client, or something that is not a request: nothing to answer
            return msg
                .get("id")
                .filter(|_| msg.get("result").is_none() && msg.get("error").is_none())
                .map(|id| rpc_error(id.clone(), -32600, "a request needs a \"method\""));
        };
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let outcome = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.list()),
            "tools/call" => self.call(&params),
            m if m.starts_with("notifications/") => return None,
            m => Err((
                -32601,
                format!("method {m:?} is not supported; this server has tools only (tools/list, tools/call)"),
            )),
        };
        let id = id?;
        Some(match outcome {
            Ok(result) => rpc_ok(id, result),
            Err((code, message)) => rpc_error(id, code, message),
        })
    }

    fn initialize(&self, params: &Value) -> Value {
        let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
        let version = VERSIONS.iter().find(|v| **v == asked).unwrap_or(&VERSIONS[0]);
        let changes = if self.allow_changes {
            "allowed: running programs, attaching to your own processes, connecting to stubs, memory and register writes, patches, saved names and notes"
        } else {
            "refused (start the server with --allow-changes to enable running programs, attaching, connecting, memory and register writes, patches, saved names and notes); only reading a core file or minidump and read-only commands work"
        };
        json!({
            "protocolVersion": version,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "nexo-re", "version": env!("CARGO_PKG_VERSION")},
            "instructions": format!(
                "nexo RE: bin_* tools read a file and never run it; dbg_start, dbg_command and dbg_stop drive a debugger session. Changes: {changes}."
            ),
        })
    }

    fn list(&self) -> Value {
        let tools: Vec<Value> = self
            .tools
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "inputSchema": t.schema}))
            .collect();
        json!({"tools": tools})
    }

    fn call(&self, params: &Value) -> Result<Value, (i64, String)> {
        let name =
            params.get("name").and_then(Value::as_str).ok_or((-32602, "tools/call needs a \"name\"".to_string()))?;
        let Some(tool) = self.tools.iter().find(|t| t.name == name) else {
            let near: Vec<&str> = self
                .tools
                .iter()
                .map(|t| t.name.as_str())
                .filter(|n| n.starts_with(&name[..name.len().min(4)]))
                .take(3)
                .collect();
            return Err((
                -32602,
                format!("unknown tool {name:?}; tools/list has them all (nearest: {})", near.join(", ")),
            ));
        };
        let args = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(v @ Value::Object(_)) => v.clone(),
            Some(_) => return Err((-32602, "\"arguments\" must be an object".to_string())),
        };
        let run = std::panic::AssertUnwindSafe(|| match &tool.kind {
            Kind::Bin(op) => self.call_bin(op, &args),
            Kind::DbgStart => self.call_dbg_start(&args),
            Kind::DbgCommand => self.call_dbg_command(&args),
            Kind::DbgStop => self.call_dbg_stop(&args),
        });
        let (text, is_error) = match std::panic::catch_unwind(run) {
            Ok(Ok(text)) => (text, false),
            Ok(Err(text)) => (text, true),
            Err(_) => (
                "nexo re --mcp: the tool failed inside nexo; the server is still up. Try a smaller question."
                    .to_string(),
                true,
            ),
        };
        Ok(text_result(text, is_error))
    }

    fn refuse(&self, what: &str) -> String {
        format!("refused: {what}. This server was started without --allow-changes; restart it with `nexo re --mcp --allow-changes` to allow it.")
    }

    fn call_bin(&self, op: &str, args: &Value) -> Result<String, String> {
        let saving =
            SAVING_OPS.contains(&op) || (op == "crypto" && args.get("save").and_then(Value::as_bool) == Some(true));
        if saving && !self.allow_changes {
            return Err(self.refuse(&format!("`nexo bin {op}` saves names, notes, types or files")));
        }
        let argv = bin_argv(op, args)?;
        let parsed = crate::cli::Args::try_parse_from(argv).map_err(|e| e.to_string())?;
        match parsed.cmd {
            Some(crate::cli::Cmd::Bin(b)) => crate::bin_cmd::execute(&b).map_err(|e| format!("nexo bin: {e}")),
            _ => Err("nexo bin: not a bin op".to_string()),
        }
    }

    fn call_dbg_start(&self, args: &Value) -> Result<String, String> {
        let kind = args.get("kind").and_then(Value::as_str).unwrap_or("");
        if !["run", "core", "attach", "connect"].contains(&kind) {
            return Err("dbg_start needs \"kind\": run, core, attach or connect".to_string());
        }
        let target =
            scalar(args.get("target")).ok_or("dbg_start needs \"target\": the program, core file, pid or host:port")?;
        // running a program executes code, attach and connect reach other processes: all need the flag
        if kind != "core" && !self.allow_changes {
            return Err(self.refuse(&format!(
                "dbg_start kind {kind} {} (without the flag only kind core, reading a dump, is allowed)",
                match kind {
                    "run" => "starts and runs a program",
                    "attach" => "attaches to a process that was not started here",
                    _ => "connects to a remote stub",
                }
            )));
        }
        let mut words = vec![kind.to_string()];
        if kind == "run" && args.get("main").and_then(Value::as_bool) == Some(true) {
            words.push("--main".into());
        }
        let target = if kind == "attach" { same_user(&target)?.to_string() } else { target };
        if kind == "core" && target.starts_with('-') {
            return Err(format!("a core file name cannot start with '-' (it would be read as a flag): use ./{target}"));
        }
        words.push(target);
        if kind == "run" {
            words.extend(list_of(args.get("args"))?);
        }
        if let Some(exe) = scalar(args.get("exe")) {
            if kind == "core" || kind == "connect" {
                words.extend(["--exe".to_string(), exe]);
            }
        }
        run_dbg(words)
    }

    fn call_dbg_command(&self, args: &Value) -> Result<String, String> {
        let command = scalar(args.get("command"))
            .ok_or("dbg_command needs \"command\", for example `break main` or `do \"break f; cont; bt\"`")?;
        let mut words = split_words(&command)?;
        let call = nexo_dbg::parse(&words).map_err(|e| format!("nexo dbg: {e}"))?;
        let first = call.words.first().map(String::as_str).unwrap_or("");
        match first {
            "run" | "core" | "attach" | "connect" => {
                return Err(format!(
                    "`{first}` starts a session: use dbg_start (kind {first}). dbg_command drives a live one."
                ))
            }
            "end" => return Err("use dbg_stop to end a session".to_string()),
            "__serve" => return Err("`__serve` is internal".to_string()),
            _ => {}
        }
        if !self.allow_changes {
            if let Some(why) = nexo_dbg::read_only_refusal(&call.words) {
                return Err(self.refuse(&why));
            }
        }
        let mut prefix = Vec::new();
        if let Some(s) = scalar(args.get("session")) {
            prefix.extend(["-s".to_string(), s]);
        }
        if let Some(n) = scalar(args.get("lines")) {
            prefix.extend(["--lines".to_string(), n]);
        }
        if let Some(f) = scalar(args.get("from")) {
            prefix.extend(["--from".to_string(), f]);
        }
        if args.get("json").and_then(Value::as_bool) == Some(true) {
            prefix.push("--json".to_string());
        }
        prefix.append(&mut words);
        run_dbg(prefix)
    }

    fn call_dbg_stop(&self, args: &Value) -> Result<String, String> {
        let mut words = Vec::new();
        if let Some(s) = scalar(args.get("session")) {
            words.extend(["-s".to_string(), s]);
        }
        words.push("end".to_string());
        run_dbg(words)
    }
}

fn run_dbg(words: Vec<String>) -> Result<String, String> {
    crate::dbg_cmd::execute(&DbgArgs { words }).map_err(|e| format!("nexo dbg: {e}"))
}

/// Attach stays same-user: the process must belong to this user. Returns the parsed process id, which is the
/// one to pass on (only plain decimal digits are a pid: "+123" is not).
fn same_user(pid: &str) -> Result<u32, String> {
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("attach needs a process id of plain digits, not {pid:?}"));
    }
    let n: u32 = pid.parse().map_err(|_| format!("attach needs a process id, not {pid:?}"))?;
    check_owner(n).map(|()| n)
}

#[cfg(unix)]
fn check_owner(n: u32) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let owner = std::fs::metadata(format!("/proc/{n}")).map(|m| m.uid());
    let me = unsafe { libc::geteuid() };
    match owner {
        Ok(u) if u != me => {
            Err(format!("refused: process {n} belongs to another user; attach is for your own processes only"))
        }
        Err(_) => Err(format!("no process {n} to attach to; check the id with `ps`")),
        Ok(_) => Ok(()),
    }
}

#[cfg(not(unix))]
fn check_owner(n: u32) -> Result<(), String> {
    Err(format!(
        "refused: attaching to process {n} is not available on this system because the owner of a process cannot be checked here; read a core file or minidump instead (kind core)"
    ))
}

fn scalar(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn list_of(v: Option<&Value>) -> Result<Vec<String>, String> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => {
            a.iter().map(|x| scalar(Some(x)).ok_or("array items must be strings or numbers".to_string())).collect()
        }
        Some(other) => scalar(Some(other)).map(|s| vec![s]).ok_or("expected a list of strings".to_string()),
    }
}

/// Split a command line into words; single and double quotes group words, a backslash escapes in double quotes.
pub(crate) fn split_words(s: &str) -> Result<Vec<String>, String> {
    let (mut words, mut cur, mut quote, mut has) = (Vec::new(), String::new(), None::<char>, false);
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => match chars.next() {
                Some(n @ ('"' | '\\')) => cur.push(n),
                Some(n) => {
                    cur.push('\\');
                    cur.push(n);
                }
                None => cur.push('\\'),
            },
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    words.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if quote.is_some() {
        return Err("the command has an unclosed quote".to_string());
    }
    if has || !cur.is_empty() {
        words.push(cur);
    }
    Ok(words)
}

// ---- tools generated from the clap definition of `nexo bin` ----

fn bin_command() -> clap::Command {
    crate::cli::Args::command().find_subcommand("bin").expect("bin is a command").clone()
}

/// Arguments that are not tool inputs: clap's own, and the one that prints on stderr.
fn hidden_arg(a: &Arg) -> bool {
    matches!(a.get_id().as_str(), "help" | "version" | "timing")
}

fn is_flag(a: &Arg) -> bool {
    !a.get_action().takes_values()
}

fn is_list(a: &Arg) -> bool {
    matches!(a.get_action(), ArgAction::Append) || a.get_num_args().is_some_and(|r| r.max_values() > 1)
}

fn arg_schema(a: &Arg) -> Value {
    let help = a.get_help().map(|h| h.to_string()).unwrap_or_default();
    let mut s = if is_flag(a) {
        json!({"type": "boolean"})
    } else if is_list(a) {
        json!({"type": "array", "items": {"type": "string"}})
    } else if a.get_default_values().first().is_some_and(|d| d.to_str().is_some_and(|d| d.parse::<i64>().is_ok())) {
        json!({"type": ["integer", "string"]})
    } else {
        json!({"type": "string"})
    };
    let possible: Vec<String> = a.get_possible_values().iter().map(|p| p.get_name().to_string()).collect();
    if !possible.is_empty() && !is_flag(a) && !is_list(a) {
        s["enum"] = json!(possible);
    }
    if let Some(d) = a.get_default_values().first().and_then(|d| d.to_str()) {
        s["default"] = json!(d);
    }
    if !help.is_empty() {
        s["description"] = json!(help);
    }
    s
}

fn bin_tools() -> Vec<Tool> {
    bin_command()
        .get_subcommands()
        .filter(|c| c.get_name() != "help")
        .map(|c| {
            let mut props = serde_json::Map::new();
            let mut required = Vec::new();
            for a in c.get_arguments().filter(|a| !hidden_arg(a)) {
                props.insert(a.get_id().to_string(), arg_schema(a));
                if a.is_required_set() {
                    required.push(a.get_id().to_string());
                }
            }
            let about = c.get_about().map(|a| a.to_string()).unwrap_or_default();
            let name = c.get_name();
            let gate = if SAVING_OPS.contains(&name) { " Saves data: needs --allow-changes." } else { "" };
            Tool {
                name: format!("bin_{name}"),
                description: format!("nexo bin {name}: {about}{gate}"),
                schema: json!({"type": "object", "properties": props, "required": required}),
                kind: Kind::Bin(name.to_string()),
            }
        })
        .collect()
}

/// The words after `nexo bin <op>` for these JSON arguments: flags first, then the positionals after `--`.
fn bin_argv(op: &str, args: &Value) -> Result<Vec<String>, String> {
    let bin = bin_command();
    let c = bin.find_subcommand(op).ok_or_else(|| format!("no nexo bin op {op:?}"))?;
    let known: Vec<&Arg> = c.get_arguments().filter(|a| !hidden_arg(a)).collect();
    let obj = args.as_object().ok_or("arguments must be an object")?;
    for k in obj.keys() {
        if !known.iter().any(|a| a.get_id() == k.as_str()) {
            let valid: Vec<&str> = known.iter().map(|a| a.get_id().as_str()).collect();
            return Err(format!("bin_{op} has no argument {k:?}; valid: {}", valid.join(", ")));
        }
    }
    let mut argv = vec!["nexo".to_string(), "bin".to_string(), op.to_string()];
    let mut positional: Vec<&Arg> = known.iter().copied().filter(|a| a.is_positional()).collect();
    positional.sort_by_key(|a| a.get_index());
    for a in known.iter().filter(|a| !a.is_positional()) {
        let Some(v) = obj.get(a.get_id().as_str()).filter(|v| !v.is_null()) else { continue };
        let flag = match (a.get_long(), a.get_short()) {
            (Some(l), _) => format!("--{l}"),
            (None, Some(s)) => format!("-{s}"),
            _ => continue,
        };
        if is_flag(a) {
            if v.as_bool().ok_or_else(|| format!("{} must be true or false", a.get_id()))? {
                argv.push(flag);
            }
        } else if is_list(a) {
            for item in list_of(Some(v))? {
                argv.extend([flag.clone(), item]);
            }
        } else {
            let s = scalar(Some(v)).ok_or_else(|| format!("{} must be a string or a number", a.get_id()))?;
            argv.extend([flag, s]);
        }
    }
    argv.push("--".to_string());
    let mut missing: Option<&str> = None;
    for a in positional {
        let id = a.get_id().as_str();
        match obj.get(id).filter(|v| !v.is_null()) {
            None => missing = missing.or(Some(id)),
            Some(v) => {
                if let Some(m) = missing {
                    return Err(format!("{id} was given but {m} (before it) was not; give {m} too"));
                }
                argv.extend(list_of(Some(v))?);
            }
        }
    }
    Ok(argv)
}

// ---- the three debugger tools ----

fn dbg_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "dbg_start".into(),
            description: "nexo dbg: start a session that stays alive between calls. kind core (core file or minidump, reading only) always works; run (starts a program, so it executes code), attach (a pid of yours) and connect (GDB stub) need --allow-changes. Then use dbg_command.".into(),
            schema: json!({"type": "object", "properties": {
                "kind": {"type": "string", "enum": ["run", "core", "attach", "connect"]},
                "target": {"type": ["string", "integer"], "description": "program path, core file, pid or host:port"},
                "args": {"type": "array", "items": {"type": "string"}, "description": "program arguments (run)"},
                "main": {"type": "boolean", "description": "run: stop at main"},
                "exe": {"type": "string", "description": "core or connect: the program the core or stub belongs to"}
            }, "required": ["kind", "target"]}),
            kind: Kind::DbgStart,
        },
        Tool {
            name: "dbg_command".into(),
            description: "nexo dbg: one command on the live session, such as `break main`, `cont`, `bt`, `regs`, `mem <addr> 32`, or `do \"break f; cont; bt\"` (`help` lists them). Without --allow-changes only reading commands run; write, setreg, name, note, serve, calls, stdin, watch, hooks that set values and any unknown command need --allow-changes.".into(),
            schema: json!({"type": "object", "properties": {
                "command": {"type": "string"},
                "session": {"type": "string", "description": "session id, when more than one is live"},
                "lines": {"type": "integer", "description": "most lines in the answer"},
                "from": {"type": "string", "description": "continue a cut answer at this address or position"},
                "json": {"type": "boolean"}
            }, "required": ["command"]}),
            kind: Kind::DbgCommand,
        },
        Tool {
            name: "dbg_stop".into(),
            description: "nexo dbg: end a session and the program under it.".into(),
            schema: json!({"type": "object", "properties": {
                "session": {"type": "string", "description": "session id, when more than one is live"}
            }}),
            kind: Kind::DbgStop,
        },
    ]
}

/// Ops of `nexo bin`, for the tests of the tool count.
#[cfg(test)]
fn bin_op_count() -> usize {
    crate::re_cmd::bin_ops().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Send these lines to a server and return its answers, one value each.
    fn talk(allow: bool, lines: &[Value]) -> Vec<Value> {
        let input = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n") + "\n";
        let mut out = Vec::new();
        serve(Cursor::new(input), &mut out, allow).unwrap();
        String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn call(id: u64, tool: &str, args: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": args}})
    }

    fn result_text(v: &Value) -> String {
        v["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string()
    }

    /// A program of my own, built from C; the bytes of a fixture when there is no compiler.
    fn fixture(dir: &std::path::Path) -> String {
        let c = dir.join("p.c");
        std::fs::write(&c, "#include <stdio.h>\nint add(int a, int b) { return a + b; }\nint main(int n, char **v) { printf(\"%d\\n\", add(n, 2)); return 0; }\n").unwrap();
        let exe = dir.join("p.out");
        let built = std::process::Command::new("cc")
            .args(["-O0", "-fno-pie", "-no-pie", "-o"])
            .arg(&exe)
            .arg(&c)
            .output()
            .is_ok_and(|o| o.status.success());
        if !built {
            std::fs::write(&exe, &nexo_bin::fixtures::elf64(false).bytes).unwrap();
        }
        exe.display().to_string()
    }

    #[test]
    fn initialize_answers_with_the_tools_capability_and_a_notification_gets_no_answer() {
        let a = talk(
            false,
            &[
                json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}),
                json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
            ],
        );
        assert_eq!(a.len(), 2, "{a:?}");
        assert_eq!(a[0]["result"]["protocolVersion"], "2024-11-05");
        assert!(a[0]["result"]["capabilities"]["tools"].is_object());
        assert_eq!(a[1]["result"], json!({}));
    }

    #[test]
    fn tools_list_has_one_tool_per_bin_op_plus_the_three_debugger_tools() {
        let a = talk(false, &[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})]);
        let tools = a[0]["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), bin_op_count() + 3);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for n in ["bin_info", "bin_decomp", "bin_emu", "dbg_start", "dbg_command", "dbg_stop"] {
            assert!(names.contains(&n), "{n} missing from {names:?}");
        }
        assert!(tools.iter().all(|t| t["inputSchema"]["type"] == "object"));
        let info = tools.iter().find(|t| t["name"] == "bin_info").unwrap();
        assert_eq!(info["inputSchema"]["required"], json!(["file"]));
        assert_eq!(info["inputSchema"]["properties"]["lines"]["type"], json!(["integer", "string"]));
    }

    #[test]
    fn assembly_is_read_only_and_copy_patches_need_allow_changes() {
        let dir = tempfile::tempdir().unwrap();
        let f = nexo_bin::fixtures::elf64(false);
        let input = dir.path().join("input");
        let out = dir.path().join("output");
        std::fs::write(&input, &f.bytes).unwrap();
        let args = json!({"file": input, "out": out, "at": format!("{:#x}", f.b), "asm": "mov eax, 7; ret"});
        let a = talk(
            false,
            &[call(1, "bin_asm", json!({"source": "nop; ret", "at": "0x401000"})), call(2, "bin_patch", args.clone())],
        );
        assert_eq!(a[0]["result"]["isError"], false, "{a:?}");
        assert!(result_text(&a[0]).contains("90 c3"));
        assert_eq!(a[1]["result"]["isError"], true);
        assert!(result_text(&a[1]).contains("--allow-changes"));
        assert!(!out.exists());
        let a = talk(true, &[call(3, "bin_patch", args)]);
        assert_eq!(a[0]["result"]["isError"], false, "{a:?}");
        assert_eq!(nexo_bin::Image::open(&out).unwrap().read_at(f.b, 6).unwrap(), [0xb8, 7, 0, 0, 0, 0xc3]);
        assert_eq!(std::fs::read(input).unwrap(), f.bytes);
    }

    #[test]
    fn tools_call_info_returns_the_text_nexo_bin_prints() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fixture(dir.path());
        let a = talk(false, &[call(1, "bin_info", json!({"file": exe}))]);
        assert_eq!(a[0]["result"]["isError"], false, "{a:?}");
        let text = result_text(&a[0]);
        let expect = {
            let parsed = crate::cli::Args::try_parse_from(["nexo", "bin", "info", &exe]).unwrap();
            let Some(crate::cli::Cmd::Bin(b)) = parsed.cmd else { panic!("bin") };
            crate::bin_cmd::execute(&b).unwrap()
        };
        assert_eq!(text, expect);
        // the format line names what `cc` really produced: ELF on Linux, PE on Windows (MinGW)
        let magic = std::fs::read(&exe).unwrap();
        let want = if magic.starts_with(b"MZ") { "pe32" } else { "elf" };
        assert!(text.to_lowercase().contains(want), "expected {want} in\n{text}");
        // a positional and a flag together
        let a = talk(false, &[call(2, "bin_funcs", json!({"file": exe, "lines": 12}))]);
        assert_eq!(a[0]["result"]["isError"], false, "{a:?}");
    }

    #[test]
    fn a_bad_call_is_an_error_result_that_names_the_next_step() {
        let a = talk(
            false,
            &[
                call(1, "bin_info", json!({"fil": "x"})),
                call(2, "bin_info", json!({"file": "/nonexistent/none"})),
                call(3, "bin_nope", json!({})),
                json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}),
            ],
        );
        assert_eq!(a[0]["result"]["isError"], true);
        assert!(result_text(&a[0]).contains("valid: file"), "{}", result_text(&a[0]));
        assert_eq!(a[1]["result"]["isError"], true);
        assert!(result_text(&a[1]).starts_with("nexo bin:"), "{}", result_text(&a[1]));
        assert_eq!(a[2]["error"]["code"], -32602);
        assert_eq!(a[3]["error"]["code"], -32601);
    }

    #[test]
    fn a_write_is_refused_without_the_flag_and_the_refusal_names_the_flag() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fixture(dir.path());
        let a = talk(
            false,
            &[
                call(1, "bin_name", json!({"file": exe, "target": "main", "name": "entry"})),
                call(2, "bin_crypto", json!({"file": exe, "save": true})),
                call(3, "dbg_command", json!({"command": "write 0x1000 90"})),
                call(4, "dbg_command", json!({"command": "do \"break main; setreg rax 1\""})),
                call(5, "dbg_command", json!({"command": "note 0x1000 hello"})),
                call(6, "dbg_start", json!({"kind": "attach", "target": std::process::id()})),
                call(7, "dbg_start", json!({"kind": "connect", "target": "127.0.0.1:1"})),
            ],
        );
        for (i, v) in a.iter().enumerate() {
            assert_eq!(v["result"]["isError"], true, "{i}: {v}");
            let t = result_text(v);
            assert!(t.starts_with("refused:") && t.contains("--allow-changes"), "{i}: {t}");
        }
    }

    #[test]
    fn reads_are_not_refused_and_session_commands_are_routed() {
        let a = talk(
            false,
            &[
                call(1, "dbg_command", json!({"command": "run ./x"})),
                call(2, "dbg_command", json!({"command": "end"})),
                call(3, "dbg_command", json!({"command": "unterminated \"quote"})),
                call(4, "dbg_start", json!({"kind": "attach"})),
            ],
        );
        assert!(result_text(&a[0]).contains("use dbg_start"), "{}", result_text(&a[0]));
        assert!(result_text(&a[1]).contains("dbg_stop"));
        assert!(result_text(&a[2]).contains("unclosed quote"));
        assert!(result_text(&a[3]).contains("needs \"target\""));
    }

    #[test]
    fn attach_is_for_processes_of_the_same_user_even_with_the_flag() {
        #[cfg(unix)]
        if unsafe { libc::geteuid() } != 0 {
            assert!(same_user("1").unwrap_err().contains("another user"));
        }
        assert!(same_user("abc").is_err());
        #[cfg(unix)]
        assert!(same_user(&std::process::id().to_string()).is_ok());
    }

    #[test]
    fn broken_input_gets_a_parse_error_and_the_server_goes_on() {
        let input = "not json\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n";
        let mut out = Vec::new();
        serve(Cursor::new(input), &mut out, false).unwrap();
        let lines: Vec<Value> =
            String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines[0]["error"]["code"], -32700);
        assert_eq!(lines[1]["id"], 7);
    }

    #[test]
    fn command_lines_split_like_a_shell_would() {
        assert_eq!(split_words("break main").unwrap(), ["break", "main"]);
        assert_eq!(split_words("do \"break f; cont; bt\"").unwrap(), ["do", "break f; cont; bt"]);
        assert_eq!(split_words("a 'b c' \"\"").unwrap(), ["a", "b c", ""]);
    }

    fn refused(a: &Value) -> bool {
        a["result"]["isError"] == true
            && result_text(a).starts_with("refused:")
            && result_text(a).contains("--allow-changes")
    }

    #[test]
    fn a_quoted_command_word_does_not_skip_the_gate() {
        let cmds = [
            "do '\"write\" 0x1000 90'",
            "do 'h\"ook\" f set arg1 5'",
            "do 'wr\"i\"te 0x1000 90'",
            "do 'break main; \"setreg\" rax 1'",
        ];
        let calls: Vec<Value> =
            cmds.iter().enumerate().map(|(i, c)| call(i as u64, "dbg_command", json!({"command": c}))).collect();
        let a = talk(false, &calls);
        for (c, v) in cmds.iter().zip(&a) {
            assert!(refused(v), "{c}: {v}");
        }
        // with the flag the same lines pass the gate (and then fail only because no session is live)
        let a = talk(true, &calls);
        for (c, v) in cmds.iter().zip(&a) {
            assert!(!result_text(v).starts_with("refused:"), "{c}: {v}");
        }
    }

    #[test]
    fn a_nested_do_and_unknown_words_and_calls_and_run_are_refused_without_the_flag() {
        let cmds = [
            "do 'break main; do \"write 0x1000 90\"'",
            "do 'bt; frobnicate'",
            "frobnicate",
            "calls",
            "do 'bt; calls'",
            "do '__serve'",
            "do 'run /bin/sh'",
            "stdin hello",
            "serve --gdb 1 x",
        ];
        let calls: Vec<Value> =
            cmds.iter().enumerate().map(|(i, c)| call(i as u64, "dbg_command", json!({"command": c}))).collect();
        let a = talk(false, &calls);
        for (c, v) in cmds.iter().zip(&a) {
            assert!(refused(v), "{c}: {v}");
        }
    }

    #[test]
    fn reading_commands_and_a_read_only_script_pass_the_gate_without_the_flag() {
        let cmds = ["bt", "regs rax", "do 'break main; cont; \"bt\"; mem 0x10 8'", "hook f stop", "cover show"];
        let calls: Vec<Value> =
            cmds.iter().enumerate().map(|(i, c)| call(i as u64, "dbg_command", json!({"command": c}))).collect();
        for (c, v) in cmds.iter().zip(&talk(false, &calls)) {
            assert!(!result_text(v).starts_with("refused:"), "{c}: {v}");
        }
    }

    #[test]
    fn running_a_program_or_attaching_needs_the_flag_and_only_a_core_is_free() {
        let a = talk(
            false,
            &[
                call(1, "dbg_start", json!({"kind": "run", "target": "/bin/sh", "args": ["-c", "id"]})),
                call(2, "dbg_start", json!({"kind": "run", "target": "/bin/cat", "args": ["<", "/etc/hostname"]})),
                call(3, "dbg_start", json!({"kind": "core", "target": "/nonexistent/core"})),
                call(4, "dbg_start", json!({"kind": "core", "target": "-s"})),
            ],
        );
        assert!(refused(&a[0]) && refused(&a[1]), "{a:?}");
        assert!(!result_text(&a[2]).starts_with("refused:"), "{}", result_text(&a[2]));
        assert!(result_text(&a[3]).contains("cannot start with '-'"), "{}", result_text(&a[3]));
        let a = talk(true, &[call(1, "dbg_start", json!({"kind": "run", "target": "/nonexistent/prog"}))]);
        assert!(!result_text(&a[0]).starts_with("refused:"), "{}", result_text(&a[0]));
    }

    #[test]
    fn a_pid_with_a_plus_sign_or_other_junk_is_not_a_pid() {
        for bad in ["+123", "-1", " 12", "1_0", "", "0x10"] {
            assert!(same_user(bad).is_err(), "{bad:?}");
        }
        #[cfg(unix)]
        {
            let me = std::process::id();
            assert_eq!(same_user(&format!("{me:010}")).unwrap(), me);
        }
    }

    #[test]
    fn the_initialize_text_says_what_the_flag_enables() {
        let off = talk(false, &[json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})]);
        let t = off[0]["result"]["instructions"].as_str().unwrap().to_string();
        for w in ["running programs", "attaching", "memory and register writes", "patches", "--allow-changes"] {
            assert!(t.contains(w), "{w}: {t}");
        }
    }
}
