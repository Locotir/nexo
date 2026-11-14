//! The MCP stdio client against misbehaving servers. Each toy server is a few lines of POSIX `sh`
//! (`read`, `printf`, `head`, `tr`) that answers by the method name it sees in the request line.
//! Unix only.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nexo_core::config::McpServerConfig;
use nexo_core::ToolContext;
use nexo_tools::mcp::McpClient;
use nexo_tools::mcp_hub::McpHub;
use serde_json::{json, Value};

const INIT_REPLY: &str = r#"printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"toy"}}}\n' "$id""#;

/// A reply to the request `$id` whose result is `$1` (an sh word that expands to JSON).
fn ok_reply(result: &str) -> String {
    format!(r#"printf '{{"jsonrpc":"2.0","id":%s,"result":%s}}\n' "$id" {result}"#)
}

/// The usual tool list: two plain tools.
const LIST: &str = r#"'{"tools":[{"name":"t","inputSchema":{"type":"object"}}]}'"#;

/// A server. `pre` runs before the loop; `init` and `list` are sh commands answering `initialize`
/// and `tools/list` (`$id` is the request's id); `call` runs for each `tools/call` (`$l` is the
/// request line).
fn script(pre: &str, init: &str, list: &str, call: &str) -> String {
    format!(
        r#"{pre}
while IFS= read -r l; do
  id=$(printf '%s' "$l" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$l" in
    *'"method":"initialize"'*) {init} ;;
    *'"method":"tools/list"'*) {list} ;;
    *'"method":"tools/call"'*) {call} ;;
  esac
done
"#
    )
}

fn good_script(call: &str) -> String {
    script("", INIT_REPLY, &ok_reply(LIST), call)
}

fn cfg(script: String) -> McpServerConfig {
    McpServerConfig { command: "sh".into(), args: vec!["-c".into(), script], ..Default::default() }
}

fn ctx() -> ToolContext {
    ToolContext::new(std::env::temp_dir())
}

async fn call_tool(client: &McpClient, name: &str, args: Value) -> nexo_core::ToolOutput {
    let tools = client.tools();
    let t = tools.iter().find(|t| t.name().ends_with(&format!("__{name}"))).unwrap_or_else(|| panic!("tool {name}"));
    t.run(args, &ctx()).await
}

/// Waits (up to 15 s) for the process to be gone; a zombie counts as still there.
async fn gone(pid: u32) -> bool {
    for _ in 0..300 {
        if std::fs::read_to_string(format!("/proc/{pid}/stat")).is_err() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// A server that writes its pid to a file, then serves a plain handshake.
fn pid_server(dir: &Path, call: &str) -> (McpServerConfig, PathBuf) {
    let pidfile = dir.join("pid");
    let pre = format!("echo $$ > '{}'", pidfile.display());
    (cfg(script(&pre, INIT_REPLY, &ok_reply(LIST), call)), pidfile)
}

async fn read_pid(file: &Path) -> u32 {
    for _ in 0..200 {
        if let Ok(s) = std::fs::read_to_string(file) {
            if let Ok(p) = s.trim().parse() {
                return p;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no pid file");
}

// ---- 1 ----

#[tokio::test]
async fn failing_servers_are_named_and_the_others_work() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfgs = std::collections::BTreeMap::new();
    cfgs.insert("exits".to_string(), cfg("exit 3".into()));
    cfgs.insert(
        "missing".to_string(),
        McpServerConfig { command: "/nonexistent/nexo-no-such-server".into(), ..Default::default() },
    );
    cfgs.insert("fine".to_string(), cfg(good_script(&ok_reply(r#"'{"content":[{"type":"text","text":"hi"}]}'"#))));
    let hub = McpHub::start(&cfgs, &dir.path().join("auth.json"), None).await;
    let failures = hub.failures();
    let names: Vec<&str> = failures.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["exits", "missing"], "{failures:?}");
    assert!(failures[0].1.contains("exited") || failures[0].1.contains("closed the connection"), "{failures:?}");
    assert!(failures[1].1.contains("cannot start"), "{failures:?}");
    let tools = hub.tools();
    let t = tools.iter().find(|t| t.name() == "mcp__fine__t").expect("the good server's tool");
    assert_eq!(t.run(json!({}), &ctx()).await.content, "hi");
    hub.close().await;
}

// ---- 2 ----

#[tokio::test]
async fn an_unanswered_initialize_times_out() {
    let dir = tempfile::tempdir().unwrap();
    let (c, pidfile) = pid_server(dir.path(), "");
    // never answers anything
    let c =
        McpServerConfig { args: vec!["-c".into(), format!("echo $$ > '{}'; read l; read l", pidfile.display())], ..c };
    let t0 = Instant::now();
    let e = match McpClient::connect("silent", &c).await {
        Ok(_) => panic!("connected"),
        Err(e) => e.to_string(),
    };
    assert!(e.contains("initialize timed out"), "{e}");
    assert!(t0.elapsed() < Duration::from_secs(50), "bounded by 30 s, took {:?}", t0.elapsed());
    assert!(gone(read_pid(&pidfile).await).await, "the silent server is killed");
}

#[tokio::test]
async fn an_unanswered_tools_list_times_out() {
    let s = script("", INIT_REPLY, "read l", "");
    let t0 = Instant::now();
    let e = match McpClient::connect("nolist", &cfg(s)).await {
        Ok(_) => panic!("connected"),
        Err(e) => e.to_string(),
    };
    assert!(e.contains("tools/list") && e.contains("timed out"), "{e}");
    assert!(t0.elapsed() < Duration::from_secs(50), "bounded by 30 s, took {:?}", t0.elapsed());
}

// ---- 3 and 4 ----

#[tokio::test]
async fn noise_on_stdout_is_skipped_and_replies_reach_the_right_caller() {
    let noise = r#"printf 'Server v1.0 starting\n\n   \n{"hello":1}\n[1,2]\n"text"\n{"jsonrpc":"2.0","id":999,"result":{}}\n{"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info"}}\n'"#;
    // the first call is held; the second is answered first, then the first
    let call = format!(
        r#"{noise}
        who=$(printf '%s' "$l" | sed 's/.*"who":"\([^"]*\)".*/\1/')
        if [ -z "$held" ]; then held=$id; heldwho=$who; else
          printf '{{"jsonrpc":"2.0","id":%s,"result":{{"content":[{{"type":"text","text":"got %s"}}]}}}}\n' "$id" "$who"
          printf '{{"jsonrpc":"2.0","id":%s,"result":{{"content":[{{"type":"text","text":"got %s"}}]}}}}\n' "$held" "$heldwho"
          held=
        fi"#
    );
    let s = script(noise, INIT_REPLY, &ok_reply(LIST), &call);
    let client = McpClient::connect("noisy", &cfg(s)).await.expect("noise before the handshake is skipped");
    let (a, b) = tokio::join!(call_tool(&client, "t", json!({"who": "alice"})), async {
        // the second caller comes a moment later so that the order of arrival is known
        tokio::time::sleep(Duration::from_millis(300)).await;
        call_tool(&client, "t", json!({"who": "bob"})).await
    });
    assert_eq!((a.content.as_str(), b.content.as_str()), ("got alice", "got bob"), "{a:?} {b:?}");
    client.close().await;
}

// ---- 5 ----

#[tokio::test]
async fn odd_results_become_short_clear_text() {
    let call = r#"case "$l" in
      *'"name":"iserr"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"disk full"}],"isError":true}}\n' "$id" ;;
      *'"name":"rpcerr"'*) printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32000,"message":"boom"}}\n' "$id" ;;
      *'"name":"image"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"image","data":"AAAA","mimeType":"image/png"}]}}\n' "$id" ;;
      *'"name":"empty"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[]}}\n' "$id" ;;
      *'"name":"bare"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
    esac"#;
    let list = r#"'{"tools":[{"name":"iserr"},{"name":"rpcerr"},{"name":"image"},{"name":"empty"},{"name":"bare"}]}'"#;
    let s = script("", INIT_REPLY, &ok_reply(list), call);
    let client = McpClient::connect("odd", &cfg(s)).await.expect("connect");
    let o = call_tool(&client, "iserr", json!({})).await;
    assert!(o.is_error && o.content == "disk full", "{o:?}");
    let o = call_tool(&client, "rpcerr", json!({})).await;
    assert!(o.is_error && o.content.contains("boom") && o.content.contains("odd:rpcerr"), "{o:?}");
    let o = call_tool(&client, "image", json!({})).await;
    assert!(!o.is_error && o.content == "[image]", "{o:?}");
    for n in ["empty", "bare"] {
        let o = call_tool(&client, n, json!({})).await;
        assert!(!o.is_error && o.content == "(no content)", "{n}: {o:?}");
    }
    client.close().await;
}

// ---- 6 ----

fn big_call(bytes: usize, lines: bool) -> String {
    // `lines`: 5 MB as many 1 KiB lines of text joined by escaped newlines; else one long run
    let body = if lines {
        format!(r#"head -c {bytes} /dev/zero | tr '\0' 'x' | fold -w 1023 | sed 's/$/\\n/' | tr -d '\n'"#)
    } else {
        format!(r#"head -c {bytes} /dev/zero | tr '\0' 'x'"#)
    };
    format!(
        r#"printf '{{"jsonrpc":"2.0","id":%s,"result":{{"content":[{{"type":"text","text":"' "$id"; {body}; printf '"}}]}}}}\n'"#
    )
}

#[tokio::test]
async fn a_huge_result_is_capped_before_it_reaches_the_model() {
    let cases = [(5_000_000, true), (10_000_000, false)];
    for (bytes, lines) in cases {
        let client = McpClient::connect("big", &cfg(good_script(&big_call(bytes, lines)))).await.expect("connect");
        let t0 = Instant::now();
        let o =
            tokio::time::timeout(Duration::from_secs(50), call_tool(&client, "t", json!({}))).await.expect("no hang");
        assert!(!o.is_error);
        let n = o.content.chars().count();
        assert!(n < 40_000, "{bytes} bytes (lines: {lines}) came back as {n} characters");
        assert!(o.content.contains("omitted"), "the cut is marked");
        assert!(t0.elapsed() < Duration::from_secs(50));
        client.close().await;
    }
}

// ---- 7 ----

#[tokio::test]
async fn a_server_that_dies_mid_call_fails_fast_and_stays_gone() {
    let dir = tempfile::tempdir().unwrap();
    let (c, pidfile) = pid_server(dir.path(), "exit 1");
    let client = McpClient::connect("mortal", &c).await.expect("connect");
    let pid = read_pid(&pidfile).await;
    let t0 = Instant::now();
    let o = call_tool(&client, "t", json!({})).await;
    assert!(o.is_error && o.content.contains("mortal:t"), "{o:?}");
    assert!(t0.elapsed() < Duration::from_secs(10), "no wait for the 60 s timeout: {:?}", t0.elapsed());
    let t0 = Instant::now();
    let o = call_tool(&client, "t", json!({})).await;
    assert!(o.is_error, "{o:?}");
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    client.close().await;
    assert!(gone(pid).await, "the dead server is reaped");
}

#[tokio::test]
async fn a_server_that_closes_stdout_but_lives_fails_fast() {
    // after the handshake it closes its stdout and goes on reading: a request written later
    // must not wait for the call timeout
    let call = "exec 1>&-";
    let client = McpClient::connect("mute", &cfg(good_script(call))).await.expect("connect");
    let o = call_tool(&client, "t", json!({})).await;
    assert!(o.is_error, "{o:?}");
    let t0 = Instant::now();
    let o = tokio::time::timeout(Duration::from_secs(20), call_tool(&client, "t", json!({})))
        .await
        .expect("a call after the server's stdout closed must not hang");
    assert!(o.is_error, "{o:?}");
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    client.close().await;
}

// ---- 8 ----

#[tokio::test]
async fn a_noisy_stderr_never_blocks_the_server() {
    let pre = r#"head -c 1000000 /dev/zero | tr '\0' 'e' >&2"#;
    let call = format!(r#"{pre}; {}"#, ok_reply(r#"'{"content":[{"type":"text","text":"done"}]}'"#));
    let s = script(pre, INIT_REPLY, &ok_reply(LIST), &call);
    let client = tokio::time::timeout(Duration::from_secs(30), McpClient::connect("noisy", &cfg(s)))
        .await
        .expect("no hang")
        .expect("connect");
    let o = tokio::time::timeout(Duration::from_secs(30), call_tool(&client, "t", json!({}))).await.expect("no hang");
    assert_eq!(o.content, "done");
    client.close().await;
}

// ---- 9 ----

fn valid_api_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 64 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[tokio::test]
async fn tool_names_are_valid_for_provider_apis() {
    let long = "a".repeat(200);
    let list = format!(
        r#"'{{"tools":[{{"name":"dup"}},{{"name":"dup"}},{{"name":"has space"}},{{"name":"{long}"}},{{"name":"{long}b"}},{{"name":"dots.and/slash"}},{{"name":"nullschema","inputSchema":null}},{{"name":"noschema"}}]}}'"#
    );
    // the call answers with the name it was asked for
    let call = r#"n=$(printf '%s' "$l" | sed 's/.*"params":{.*"name":"\([^"]*\)".*/\1/'); printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s"}]}}\n' "$id" "$n""#;
    let s = script("", INIT_REPLY, &ok_reply(&list), call);
    let client = McpClient::connect("weird", &cfg(s)).await.expect("connect");
    let tools = client.tools();
    let mut seen = std::collections::HashSet::new();
    for t in &tools {
        let spec = t.spec();
        assert!(valid_api_name(&spec.name), "invalid name {:?}", spec.name);
        assert!(seen.insert(spec.name.clone()), "duplicate name {:?}", spec.name);
        assert!(spec.parameters.is_object(), "{}: parameters {:?}", spec.name, spec.parameters);
    }
    assert!(tools.len() >= 6, "the odd tools are kept, renamed: {}", tools.len());
    // a renamed tool still calls the server's real name
    let t = tools.iter().find(|t| t.name().contains("has_space")).expect("renamed `has space`");
    assert_eq!(t.run(json!({}), &ctx()).await.content, "has space");
    client.close().await;
}

// ---- 10 ----

#[tokio::test]
async fn the_child_is_gone_when_the_client_or_hub_ends() {
    // closed
    let dir = tempfile::tempdir().unwrap();
    let (c, pidfile) = pid_server(dir.path(), "");
    let client = McpClient::connect("a", &c).await.expect("connect");
    let pid = read_pid(&pidfile).await;
    client.close().await;
    assert!(gone(pid).await, "close() leaves no process");

    // dropped without close
    let dir = tempfile::tempdir().unwrap();
    let (c, pidfile) = pid_server(dir.path(), "");
    let client = McpClient::connect("b", &c).await.expect("connect");
    let pid = read_pid(&pidfile).await;
    drop(client);
    assert!(gone(pid).await, "dropping the client leaves no process");

    // the hub
    let dir = tempfile::tempdir().unwrap();
    let (c, pidfile) = pid_server(dir.path(), "");
    let mut cfgs = std::collections::BTreeMap::new();
    cfgs.insert("h".to_string(), c);
    let hub = McpHub::start(&cfgs, &dir.path().join("auth.json"), None).await;
    let pid = read_pid(&pidfile).await;
    hub.close().await;
    assert!(gone(pid).await, "hub.close() leaves no process");
    let dir = tempfile::tempdir().unwrap();
    let (c, pidfile) = pid_server(dir.path(), "");
    let mut cfgs = std::collections::BTreeMap::new();
    cfgs.insert("h".to_string(), c);
    let hub = McpHub::start(&cfgs, &dir.path().join("auth.json"), None).await;
    let pid = read_pid(&pidfile).await;
    drop(hub);
    assert!(gone(pid).await, "dropping the hub leaves no process");
}
