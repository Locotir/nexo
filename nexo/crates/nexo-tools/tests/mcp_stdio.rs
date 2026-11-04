//! The MCP stdio client against a fake server: this same binary, run again with
//! `NEXO_FAKE_MCP_SERVER=1`, speaks the protocol over stdio (three tools, paginated listing).
//! `harness = false`, so the server role runs before any test machinery.

use std::io::{BufRead, Write};

use nexo_core::config::McpServerConfig;
use nexo_core::{ToolContext, ToolKind};
use nexo_tools::mcp::McpClient;
use serde_json::{json, Value};

fn main() {
    if std::env::var_os("NEXO_FAKE_MCP_SERVER").is_some() {
        serve();
        return;
    }
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(connects_lists_and_calls());
    println!("test connects_lists_and_calls ... ok");
}

fn tools() -> Vec<Value> {
    vec![
        json!({"name": "echo", "description": "Echoes text.",
               "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}),
        json!({"name": "add", "description": "Adds two numbers.",
               "inputSchema": {"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}}},
               "annotations": {"readOnlyHint": true}}),
        json!({"name": "fail", "description": "Always fails.", "inputSchema": {"type": "object", "properties": {}}}),
    ]
}

fn reply(out: &mut impl Write, id: &Value, result: Option<Value>, error: Option<Value>) {
    let mut msg = json!({"jsonrpc": "2.0", "id": id});
    match error {
        Some(e) => msg["error"] = e,
        None => msg["result"] = result.unwrap_or(Value::Null),
    }
    writeln!(out, "{msg}").expect("stdout");
    out.flush().expect("flush");
}

fn serve() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let all = tools();
    for line in stdin.lock().lines() {
        let line = line.expect("stdin");
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = serde_json::from_str(line).expect("json-rpc");
        let method = msg["method"].as_str().unwrap_or("");
        let id = msg["id"].clone();
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        match method {
            "initialize" => reply(
                &mut out,
                &id,
                Some(json!({"protocolVersion": params["protocolVersion"], "capabilities": {"tools": {}},
                            "serverInfo": {"name": "fake", "version": "0"}})),
                None,
            ),
            "notifications/initialized" => {}
            "tools/list" => {
                if params.get("cursor").is_none_or(Value::is_null) {
                    reply(&mut out, &id, Some(json!({"tools": all[..2], "nextCursor": "page2"})), None);
                } else {
                    reply(&mut out, &id, Some(json!({"tools": all[2..]})), None);
                }
            }
            "tools/call" => {
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match params["name"].as_str().unwrap_or("") {
                    "echo" => {
                        let text = format!("echo: {}", args["text"].as_str().unwrap_or(""));
                        reply(&mut out, &id, Some(json!({"content": [{"type": "text", "text": text}]})), None);
                    }
                    "add" => {
                        let sum = args["a"].as_f64().unwrap_or(0.0) + args["b"].as_f64().unwrap_or(0.0);
                        reply(
                            &mut out,
                            &id,
                            Some(json!({"content": [{"type": "text", "text": sum.to_string()}]})),
                            None,
                        );
                    }
                    "fail" => reply(
                        &mut out,
                        &id,
                        Some(json!({"content": [{"type": "text", "text": "boom"}], "isError": true})),
                        None,
                    ),
                    _ => reply(&mut out, &id, None, Some(json!({"code": -32602, "message": "unknown tool"}))),
                }
            }
            _ if !id.is_null() => {
                reply(&mut out, &id, None, Some(json!({"code": -32601, "message": "method not found"})))
            }
            _ => {}
        }
    }
}

fn fake_server() -> McpServerConfig {
    let me = std::env::current_exe().expect("current exe");
    let mut env = std::collections::BTreeMap::new();
    env.insert("NEXO_FAKE_MCP_SERVER".to_string(), "1".to_string());
    McpServerConfig { command: me.display().to_string(), env, ..Default::default() }
}

async fn connects_lists_and_calls() {
    let client = McpClient::connect("fake", &fake_server()).await.expect("connect");
    assert_eq!(client.protocol_version, "2025-06-18");
    assert_eq!(client.server_info["name"], "fake");
    let names: Vec<&str> = client.defs().iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, vec!["echo", "add", "fail"], "paginated listing");
    let tools = client.tools();
    assert_eq!(tools[0].spec().name, "mcp__fake__echo");
    assert_eq!(tools[0].name(), "mcp__fake__echo", "the set keys by this name");
    assert!(client.defs()[1].read_only, "the server marks `add` read-only");
    assert_eq!(tools[1].kind(), ToolKind::Other, "mcp_read_only_hint_asks_by_default");
    assert_eq!(tools[0].kind(), ToolKind::Other);
    let ctx = ToolContext::new(std::env::temp_dir());
    let out = tools[0].run(json!({"text": "hello"}), &ctx).await;
    assert!(!out.is_error);
    assert_eq!(out.content, "echo: hello");
    let out = tools[1].run(json!({"a": 2, "b": 3}), &ctx).await;
    assert_eq!(out.content, "5");
    let out = tools[2].run(json!({}), &ctx).await;
    assert!(out.is_error && out.content.contains("boom"));
    assert!(tools[0].summary(&json!({"text": "x"})).starts_with("mcp fake:echo"));
    client.close().await;
    let trusting = McpServerConfig { trust_read_only: true, ..fake_server() };
    let client = McpClient::connect("fake", &trusting).await.expect("connect");
    let tools = client.tools();
    assert_eq!(tools[1].kind(), ToolKind::Read, "readOnlyHint believed with trust_read_only");
    assert_eq!(tools[0].kind(), ToolKind::Other);
    client.close().await;
}
