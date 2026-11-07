//! The MCP stdio client against a fake server: this same binary, run again with
//! `NEXO_FAKE_MCP_SERVER=1`, speaks the protocol over stdio (four tools with a paginated listing, a
//! resource, a template, a prompt with arguments, and a `bump` tool that grows all three lists and
//! says so with the `list_changed` notifications).
//! `harness = false`, so the server role runs before any test machinery.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nexo_core::agent::McpStatus;
use nexo_core::config::McpServerConfig;
use nexo_core::{ToolContext, ToolKind, ToolSet};
use nexo_tools::mcp::{parse_prompt_args, McpClient};
use nexo_tools::mcp_hub::McpHub;
use serde_json::{json, Value};

fn main() {
    if std::env::var_os("NEXO_FAKE_MCP_SERVER").is_some() {
        serve();
        return;
    }
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(connects_lists_and_calls());
    println!("test connects_lists_and_calls ... ok");
    rt.block_on(resources_and_prompts());
    println!("test resources_and_prompts ... ok");
    rt.block_on(list_changes_reach_the_hub());
    println!("test list_changes_reach_the_hub ... ok");
    rt.block_on(mentions_prompts_and_controls());
    println!("test mentions_prompts_and_controls ... ok");
}

fn tools() -> Vec<Value> {
    vec![
        json!({"name": "echo", "description": "Echoes text.",
               "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}),
        json!({"name": "add", "description": "Adds two numbers.",
               "inputSchema": {"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}}},
               "annotations": {"readOnlyHint": true}}),
        json!({"name": "fail", "description": "Always fails.", "inputSchema": {"type": "object", "properties": {}}}),
        json!({"name": "bump", "description": "Grows the lists.", "inputSchema": {"type": "object", "properties": {}}}),
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
    let mut all = tools();
    let mut bumped = false;
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
                Some(json!({"protocolVersion": params["protocolVersion"],
                            "capabilities": {"tools": {"listChanged": true}, "resources": {"listChanged": true},
                                             "prompts": {"listChanged": true}},
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
            "resources/list" => {
                let mut r = vec![json!({"uri": "file:///notes.md", "name": "notes", "mimeType": "text/markdown"})];
                if bumped {
                    r.push(json!({"uri": "file:///todo.md", "name": "todo"}));
                }
                reply(&mut out, &id, Some(json!({"resources": r})), None);
            }
            "resources/templates/list" => reply(
                &mut out,
                &id,
                Some(
                    json!({"resourceTemplates": [{"uriTemplate": "db://{table}", "name": "table", "description": "A table"}]}),
                ),
                None,
            ),
            "resources/read" => match params["uri"].as_str().unwrap_or("") {
                "file:///notes.md" => reply(
                    &mut out,
                    &id,
                    Some(json!({"contents": [{"uri": "file:///notes.md", "text": "# Notes\nremember the milk"}]})),
                    None,
                ),
                "db://users" => reply(
                    &mut out,
                    &id,
                    Some(json!({"contents": [{"uri": "db://users", "text": "id,name\n1,ana"}]})),
                    None,
                ),
                _ => reply(&mut out, &id, None, Some(json!({"code": -32002, "message": "resource not found"}))),
            },
            "prompts/list" => {
                let mut p = vec![json!({"name": "review", "description": "Review a file",
                    "arguments": [{"name": "file", "required": true}, {"name": "focus"}]})];
                if bumped {
                    p.push(json!({"name": "extra", "description": "Added later"}));
                }
                reply(&mut out, &id, Some(json!({"prompts": p})), None);
            }
            "prompts/get" => {
                let a = &params["arguments"];
                let text = format!(
                    "Review {} focusing on {}",
                    a["file"].as_str().unwrap_or("?"),
                    a["focus"].as_str().unwrap_or("everything")
                );
                let msg = json!({"role": "user", "content": {"type": "text", "text": text}});
                reply(&mut out, &id, Some(json!({"description": "Review", "messages": [msg]})), None);
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
                    "bump" => {
                        bumped = true;
                        all.push(json!({"name": "extra", "description": "Appears after bump.",
                                        "inputSchema": {"type": "object", "properties": {}}}));
                        reply(&mut out, &id, Some(json!({"content": [{"type": "text", "text": "bumped"}]})), None);
                        for m in ["tools", "resources", "prompts"] {
                            let note = json!({"jsonrpc": "2.0", "method": format!("notifications/{m}/list_changed")});
                            writeln!(out, "{note}").expect("stdout");
                        }
                        out.flush().expect("flush");
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
    let mut env = BTreeMap::new();
    env.insert("NEXO_FAKE_MCP_SERVER".to_string(), "1".to_string());
    McpServerConfig { command: me.display().to_string(), env, ..Default::default() }
}

async fn connects_lists_and_calls() {
    let client = McpClient::connect("fake", &fake_server()).await.expect("connect");
    assert_eq!(client.protocol_version, "2025-06-18");
    assert_eq!(client.server_info["name"], "fake");
    let defs = client.defs();
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, vec!["echo", "add", "fail", "bump"], "paginated listing");
    let tools = client.tools();
    assert_eq!(tools[0].spec().name, "mcp__fake__echo");
    assert_eq!(tools[0].name(), "mcp__fake__echo", "the set keys by this name");
    assert!(defs[1].read_only, "the server marks `add` read-only");
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

async fn resources_and_prompts() {
    let client = McpClient::connect("fake", &fake_server()).await.expect("connect");
    let res = client.resources();
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].uri, "file:///notes.md");
    assert_eq!(res[0].mime_type, "text/markdown");
    let tpl = client.templates();
    assert_eq!(tpl[0].uri_template, "db://{table}");
    let text = client.read_resource("file:///notes.md").await.expect("read");
    assert!(text.contains("remember the milk"), "{text}");
    assert!(client.read_resource("db://users").await.expect("template uri").contains("ana"));
    let err = client.read_resource("file:///nope").await.expect_err("unknown resource");
    assert!(err.contains("not found"), "{err}");

    let prompts = client.prompts();
    assert_eq!(prompts.len(), 1);
    let def = &prompts[0];
    assert_eq!(def.name, "review");
    let a = parse_prompt_args(def, "a.rs").expect("positional");
    assert_eq!(a.get("file").map(String::as_str), Some("a.rs"));
    assert!(!a.contains_key("focus"));
    let a = parse_prompt_args(def, "focus=speed a.rs").expect("key=value and positional");
    assert_eq!((a["file"].as_str(), a["focus"].as_str()), ("a.rs", "speed"));
    let a = parse_prompt_args(def, "a.rs \"the hot loop\"").expect("quoted");
    assert_eq!(a["focus"], "the hot loop");
    let a = parse_prompt_args(def, "a.rs the hot loop").expect("the rest joins the last argument");
    assert_eq!(a["focus"], "the hot loop");
    let err = parse_prompt_args(def, "").expect_err("file is required");
    assert!(err.contains("file"), "{err}");
    let a = parse_prompt_args(def, "a.rs focus=speed").unwrap();
    let got = client.get_prompt("review", &a).await.expect("prompts/get");
    assert_eq!(got.as_request(), "Review a.rs focusing on speed");
    client.close().await;
}

/// A hub over the fake server, with its credential store in a temporary folder.
async fn hub(persist: Option<nexo_tools::mcp_hub::Persist>) -> (Arc<McpHub>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cfgs = BTreeMap::new();
    cfgs.insert("fake".to_string(), fake_server());
    let hub = McpHub::start(&cfgs, &dir.path().join("auth.json"), persist).await;
    (hub, dir)
}

async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..250 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn list_changes_reach_the_hub() {
    let (hub, _dir) = hub(None).await;
    let info = hub.info();
    assert_eq!(info[0].status, McpStatus::Connected);
    assert_eq!((info[0].tools, info[0].resources.len(), info[0].prompts.len()), (4, 1, 1));
    let mut set = ToolSet::new();
    for t in hub.tools() {
        set.add(t);
    }
    for n in ["mcp__fake__bump", "list_mcp_resources", "read_mcp_resource"] {
        assert!(set.get(n).is_some(), "{n} is offered");
    }
    let mut seen = hub.generation();
    assert!(!hub.sync_tools(&mut set, &mut seen), "nothing changed yet");

    let ctx = ToolContext::new(std::env::temp_dir());
    let out = set.get("mcp__fake__bump").expect("bump").run(json!({}), &ctx).await;
    assert_eq!(out.content, "bumped");
    // the three notifications make the hub list again
    until("the lists", || {
        let i = &hub.info()[0];
        i.tools == 5 && i.resources.len() == 2 && i.prompts.len() == 2
    })
    .await;
    assert!(hub.generation() > seen, "the hub counted the change");
    // the tool set the model has does not change until it is synced (the start of the next turn)
    assert!(set.get("mcp__fake__extra").is_none());
    assert!(hub.sync_tools(&mut set, &mut seen));
    assert!(set.get("mcp__fake__extra").is_some(), "the new tool takes effect after the sync");
    assert!(set.get("list_mcp_resources").is_some(), "the resource tools stay");
    assert!(!hub.sync_tools(&mut set, &mut seen), "synced once");
    hub.close().await;
}

async fn mentions_prompts_and_controls() {
    let saved: Arc<Mutex<Vec<(String, bool)>>> = Arc::default();
    let log = saved.clone();
    let persist: nexo_tools::mcp_hub::Persist = Arc::new(move |name, on| {
        log.lock().unwrap().push((name.to_string(), on));
        Ok("saved in the test".to_string())
    });
    let (hub, _dir) = hub(Some(persist)).await;

    // @server:uri becomes the resource
    let text = hub.expand_mentions("look at @fake:file:///notes.md, please".to_string()).await;
    assert!(text.starts_with("look at @fake:file:///notes.md, please"), "{text}");
    assert!(text.contains("[MCP resource file:///notes.md from server fake]"), "{text}");
    assert!(text.contains("remember the milk"), "{text}");
    let text = hub.expand_mentions("and @fake:file:///gone.md".to_string()).await;
    assert!(text.contains("not attached") && text.contains("not found"), "{text}");
    let text = hub.expand_mentions("mail me@fake:x or @other:y".to_string()).await;
    assert_eq!(text, "mail me@fake:x or @other:y", "only a known server at the start of a word");

    // /mcp__fake__review a.rs focus=speed
    assert_eq!(
        hub.prompt_request("fake", "review", "a.rs focus=speed").await.unwrap(),
        "Review a.rs focusing on speed"
    );
    assert!(hub.prompt_request("fake", "review", "").await.is_err(), "the required argument");
    assert!(hub.prompt_request("fake", "nope", "").await.unwrap_err().contains("no prompt"));

    // the tools of the model
    let mut set = ToolSet::new();
    for t in hub.tools() {
        set.add(t);
    }
    let ctx = ToolContext::new(std::env::temp_dir());
    let listed = set.get("list_mcp_resources").unwrap().run(json!({}), &ctx).await;
    assert!(
        listed.content.contains("fake: file:///notes.md") && listed.content.contains("db://{table}"),
        "{}",
        listed.content
    );
    let read = set.get("read_mcp_resource").unwrap().run(json!({"server": "fake", "uri": "db://users"}), &ctx).await;
    assert!(!read.is_error && read.content.contains("ana"), "{}", read.content);
    let bad = set.get("read_mcp_resource").unwrap().run(json!({"server": "fake"}), &ctx).await;
    assert!(bad.is_error);

    // the controls of /mcp
    let said = hub.set_enabled("fake", false, true).await.unwrap();
    assert!(said.contains("off") && said.contains("saved in the test"), "{said}");
    assert_eq!(hub.info()[0].status, McpStatus::Disabled);
    assert!(hub.tools().is_empty(), "an off server offers nothing");
    assert!(hub.reconnect("fake").await.is_err(), "an off server is turned on, not reconnected");
    let said = hub.set_enabled("fake", true, false).await.unwrap();
    assert!(said.contains("connected"), "{said}");
    assert_eq!(hub.info()[0].status, McpStatus::Connected);
    assert!(hub.reconnect("fake").await.unwrap().contains("connected"));
    let open = |_: &str| {};
    assert!(hub.login("fake", &open).await.unwrap_err().contains("local command"));
    assert!(hub.set_enabled("nope", true, false).await.is_err());
    assert_eq!(*saved.lock().unwrap(), vec![("fake".to_string(), false)], "only the persistent change was saved");
    hub.close().await;
}
