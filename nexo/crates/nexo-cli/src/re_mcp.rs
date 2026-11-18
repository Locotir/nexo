//! `nexo re --mcp`: the nexo RE kit as an MCP server on stdio, so other harnesses can use it. JSON-RPC 2.0, one
//! message per line, written by hand (no SDK). The tools are generated from the clap definition of `nexo bin`
//! (one for each op, so the list cannot drift), `bin_batch` (several ops in one call) plus three for `nexo dbg` sessions: start, command, stop, and one for nexo api. A tool
//! result is the text `nexo bin` or `nexo dbg` prints. Anything that writes saved state, writes into a program,
//! attaches to a process or opens a port is refused unless the server started with `--allow-changes`.

use std::io::{BufRead, Write};

use clap::{Arg, ArgAction, CommandFactory, Parser};
use serde_json::{json, Value};

mod dbg_run;
#[cfg(test)]
mod tests;
mod tools;
mod words;

use dbg_run::{run_dbg, same_user};
use tools::{api_tools, batch_tool, bin_argv, bin_tools, dbg_flags, dbg_tools};
pub(crate) use words::split_words;
use words::{list_of, scalar};

/// The protocol versions this server speaks, newest first.
const VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Bin(String),
    BinBatch,
    DbgStart,
    DbgCommand,
    DbgStop,
    Api,
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
        tools.push(batch_tool());
        tools.extend(dbg_tools());
        tools.extend(api_tools());
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
            // The first four characters, not bytes: byte 4 of a client name such as "aéé" is inside a char, and a byte
            // slice would panic here, before the catch_unwind below, and end the serve loop.
            let pre: String = name.chars().take(4).collect();
            let near: Vec<&str> =
                self.tools.iter().map(|t| t.name.as_str()).filter(|n| n.starts_with(&pre)).take(3).collect();
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
            Kind::BinBatch => self.call_bin_batch(&args),
            Kind::DbgStart => self.call_dbg_start(&args),
            Kind::DbgCommand => self.call_dbg_command(&args),
            Kind::DbgStop => self.call_dbg_stop(&args),
            Kind::Api => self.call_api(&args),
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

    /// Whether this `nexo bin` argv saves names, notes, types or files: what `BinOp::effect` says of its op.
    fn saves(argv: &[String]) -> bool {
        crate::bin_cmd::argv_effect(argv).saves()
    }

    fn call_bin(&self, op: &str, args: &Value) -> Result<String, String> {
        let argv = bin_argv(op, args)?;
        if Self::saves(&argv) && !self.allow_changes {
            return Err(self.refuse(&format!("`nexo bin {op}` saves names, notes, types or files")));
        }
        crate::bin_cmd::script::run_argv(argv, crate::bin_cmd::bridge::Reader::Mcp).map_err(|(m, _)| m)
    }

    /// `bin_batch`: the ops of `ops` in order, each answer under a `== op ==` line. The gate is the one of the
    /// single-op tools, asked for every op before the first runs, so a refused op stops the whole call. An op that
    /// fails is reported in its place and the rest still run (as in the agent tool); the call is an error only
    /// when every op failed.
    fn call_bin_batch(&self, args: &Value) -> Result<String, String> {
        let ops = scalar(args.get("ops")).ok_or("bin_batch needs \"ops\": a string like `info ./p; funcs ./p`")?;
        let units = crate::bin_cmd::script::units(&ops);
        if units.is_empty() {
            return Err("bin_batch: \"ops\" has no op in it".to_string());
        }
        let mut argvs = Vec::new();
        for u in &units {
            let argv = crate::bin_cmd::script::unit_argv(u).map_err(|e| format!("`{u}`: {e}"))?;
            let op = argv.get(2).map(String::as_str).unwrap_or("");
            if Self::saves(&argv) && !self.allow_changes {
                return Err(self.refuse(&format!("`nexo bin {op}` saves names, notes, types or files (in `{u}`)")));
            }
            argvs.push(argv);
        }
        let (mut out, mut failed) = (Vec::new(), 0);
        for (u, argv) in units.iter().zip(argvs) {
            out.push(format!("== {u} =="));
            match crate::bin_cmd::script::run_argv(argv, crate::bin_cmd::bridge::Reader::Mcp) {
                Ok(text) => out.push(text),
                Err((msg, _)) => {
                    failed += 1;
                    out.push(format!("error: {}", msg.strip_prefix("error: ").unwrap_or(&msg)));
                }
            }
        }
        let text = out.join("\n");
        if failed == units.len() {
            Err(text)
        } else {
            Ok(text)
        }
    }

    /// The `api` tool: the same line the agent's `api` tool takes. Reading a map and a dry run always work; a
    /// call that sends a request, a login and anything that writes a file need `--allow-changes`.
    fn call_api(&self, args: &Value) -> Result<String, String> {
        let line = scalar(args.get("command")).ok_or("api needs \"command\": for example `describe map.json`")?;
        let cwd = std::env::current_dir().map_err(|e| format!("api: no working folder: {e}"))?;
        if let crate::api_tool::Parsed::Cmd(cmd) = crate::api_tool::parse(&line)? {
            crate::api_tool::vet(&cmd).map_err(|e| format!("api: {e}"))?;
            if crate::api_tool::kind_in(&cmd, &cwd) != nexo_core::ToolKind::Read && !self.allow_changes {
                return Err(self.refuse("this `nexo api` line sends a request or writes a file"));
            }
        }
        let ctx = nexo_core::ToolContext::new(cwd);
        // this server runs inside the CLI's runtime, where `block_on` panics: the call gets a thread of its own
        let out = std::thread::scope(|s| {
            s.spawn(|| -> Result<nexo_core::ToolOutput, String> {
                let rt =
                    tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
                Ok(rt.block_on(nexo_core::Tool::run(&crate::api_tool::Api, json!({"command": line}), &ctx)))
            })
            .join()
        })
        .map_err(|_| "api: the call stopped working".to_string())??;
        if out.is_error {
            Err(out.content)
        } else {
            Ok(out.content)
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
        if kind == "run" {
            if let Some(Value::Object(env)) = args.get("env") {
                for (k, v) in env {
                    words.extend(["--env".to_string(), format!("{k}={}", scalar(Some(v)).unwrap_or_default())]);
                }
            }
            for k in list_of(args.get("unset"))? {
                words.extend(["--unset".to_string(), k]);
            }
            if let Some(d) = scalar(args.get("cwd")).filter(|d| !d.is_empty()) {
                words.extend(["--cwd".to_string(), d]);
            }
            if args.get("aslr").and_then(Value::as_bool) == Some(true) {
                words.push("--aslr".into());
            }
            if let Some(a) = scalar(args.get("argv0")).filter(|a| !a.is_empty()) {
                words.extend(["--argv0".to_string(), a]);
            }
            // `--repeat N --until COND` is checked by the debugger's own parser, from this one place
            if let Some(n) = scalar(args.get("repeat")).filter(|n| !n.is_empty()) {
                words.extend(["--repeat".to_string(), n]);
                if let Some(u) = scalar(args.get("until")).filter(|u| !u.is_empty()) {
                    words.extend(["--until".to_string(), u]);
                }
            }
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
        for (arg, flag, schema) in dbg_flags() {
            if schema["type"] == "boolean" {
                if args.get(&arg).and_then(Value::as_bool) == Some(true) {
                    prefix.push(flag);
                }
            } else if let Some(v) = scalar(args.get(&arg)) {
                prefix.extend([flag, v]);
            }
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

/// Ops of `nexo bin`, for the tests of the tool count.
#[cfg(test)]
fn bin_op_count() -> usize {
    crate::re_cmd::bin_ops().len()
}
