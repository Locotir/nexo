use super::*;
use std::sync::Arc;

const GET: &str = "GET /v1/items";
const POST: &str = "POST /v1/items";

fn entry(method: &str, url: &str, post: Option<&str>) -> Value {
    let mut req = json!({
        "method": method,
        "url": url,
        "headers": [
            {"name": "Authorization", "value": "Bearer HEADERSECRET1234"},
            {"name": "Accept", "value": "application/json"}
        ]
    });
    if let Some(b) = post {
        req["postData"] = json!({"mimeType": "application/json", "text": b});
    }
    json!({
        "_resourceType": "XHR",
        "request": req,
        "response": {
            "status": 200,
            "headers": [{"name": "Content-Type", "value": "application/json"}],
            "content": {"mimeType": "application/json", "text": "{\"items\":[1,2]}"}
        }
    })
}

/// A small capture: one GET and one POST of the same path, a secret in the query and in a header.
fn har() -> String {
    json!({"log": {"version": "1.2", "entries": [
        entry("GET", "https://api.test/v1/items?api_key=QUERYSECRET1234&page=1", None),
        entry("POST", "https://api.test/v1/items", Some("{\"name\":\"a\"}"))
    ]}})
    .to_string()
}

/// A folder with `cap.har` and the map `map.json` made from it.
fn folder() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cap.har"), har()).unwrap();
    let text = crate::api_cmd::analyze_text(
        &dir.path().join("cap.har"),
        &[],
        None,
        false,
        6000,
        Some(&dir.path().join("map.json")),
    )
    .unwrap();
    assert!(text.contains("/v1/items"), "{text}");
    dir
}

fn cmd(line: &str) -> ApiCmd {
    match parse(line) {
        Ok(Parsed::Cmd(c)) => c,
        other => panic!("{line:?} did not parse as an op: {other:?}"),
    }
}

async fn run(cwd: &Path, line: &str) -> ToolOutput {
    Api.run(json!({"command": line}), &ToolContext::new(cwd.to_path_buf())).await
}

#[test]
fn the_line_is_read_as_nexo_api_reads_it() {
    for line in [
        "call m.json \"GET /v1/items\" -p id=3 -p query.q=a --auth bearer=MY_TOKEN --dry-run",
        "api call m.json \"GET /v1/items\" -p id=3 -p query.q=a --auth bearer=MY_TOKEN --dry-run",
        "nexo api call m.json \"GET /v1/items\" -p id=3 -p query.q=a --auth bearer=MY_TOKEN --dry-run",
    ] {
        match cmd(line) {
            ApiCmd::Call { target, dry_run, reveal, json } => {
                assert_eq!(target.map, PathBuf::from("m.json"));
                assert_eq!(target.endpoint, GET);
                assert_eq!(target.params, vec!["id=3".to_string(), "query.q=a".to_string()]);
                assert_eq!(target.auth, vec!["bearer=MY_TOKEN".to_string()]);
                assert!(dry_run && !reveal && !json);
            }
            other => panic!("not a call: {other:?}"),
        }
    }
    assert!(matches!(cmd("describe m.json"), ApiCmd::Describe { endpoint: None, .. }));
    assert!(matches!(cmd("analyze x.har --save m.json"), ApiCmd::Analyze { save: Some(_), .. }));
    assert!(matches!(parse("").unwrap(), Parsed::Help(h) if h.contains("analyze")));
    assert!(matches!(parse("call --help").unwrap(), Parsed::Help(_)));
    assert!(parse("call").is_err(), "a call without its map and endpoint is a usage error");
    assert!(parse("describe \"unclosed").is_err());
}

#[test]
fn watch_and_reveal_are_refused_with_a_reason() {
    let w = vet(&cmd("watch m.json \"GET /v1/items\" --every 30s")).unwrap_err();
    assert!(w.contains("person runs it"), "{w}");
    let r = vet(&cmd("call m.json \"GET /v1/items\" --reveal")).unwrap_err();
    assert!(r.contains("--reveal") && r.contains("never sees"), "{r}");
    assert!(vet(&cmd("call m.json \"GET /v1/items\"")).is_ok());
}

#[test]
fn a_secret_looking_value_is_refused_and_an_env_name_is_not() {
    let bad = [
        "call m.json EP -p password=hunter2",
        "call m.json EP -p body.api_key=abc",
        "call m.json EP -p q=AbCd1234EfGh5678IjKl9012MnOp",
        "call m.json EP --login --login-param password=hunter2",
        "call m.json EP --body '{\"password\":\"hunter2\"}'",
        "call m.json EP --body '{\"a\":{\"b\":[\"AbCd1234EfGh5678IjKl9012MnOp\"]}}'",
        "call m.json EP --body @body.json",
        "call m.json EP --auth bearer=eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
        "call m.json EP --auth bearer=not-a-name",
    ];
    for line in bad {
        let why = vet(&cmd(line)).unwrap_err();
        assert!(why.contains("--") || why.contains("-p"), "{line}: {why}");
        assert!(!why.contains("hunter2"), "the refusal must not repeat the value: {why}");
    }
    let good = [
        "call m.json EP -p password=@env:PW",
        "call m.json EP -p q=hello -p page=2",
        "call m.json EP --body '{\"name\":\"a\"}'",
        "call m.json EP --auth bearer=MY_TOKEN --auth cookie=SESSION_ID",
        "call m.json EP --login --login-param user=me --login-param password=@env:PW",
    ];
    for line in good {
        assert!(vet(&cmd(line)).is_ok(), "{line}");
    }
}

#[test]
fn the_refusal_for_a_param_tells_the_model_to_pass_an_env_var() {
    let why = vet(&cmd("call m.json EP -p password=hunter2")).unwrap_err();
    assert!(why.contains("@env:VAR"), "{why}");
}

#[test]
fn the_method_and_the_flags_decide_the_kind() {
    let dir = folder();
    let d = dir.path();
    let map = d.join("map.json");
    let k = |tail: &str| kind_in(&cmd(&format!("call {} {tail}", map.display())), d);
    assert_eq!(k(&format!("\"{GET}\"")), ToolKind::Network);
    assert_eq!(k(&format!("\"{GET}\" -p page=2 --json")), ToolKind::Network);
    assert_eq!(k(&format!("\"{POST}\"")), ToolKind::Exec);
    assert_eq!(k(&format!("\"{POST}\" --body '{{\"name\":\"b\"}}'")), ToolKind::Exec);
    // a login is a call that changes state, whatever the endpoint is
    assert_eq!(k(&format!("\"{GET}\" --login")), ToolKind::Exec);
    // a dry run sends nothing
    assert_eq!(k(&format!("\"{POST}\" --dry-run")), ToolKind::Read);
    assert_eq!(k(&format!("\"{GET}\" --login --dry-run")), ToolKind::Read);
    // the call goes out of the person's own browser: never a plain read, with or without a value after the flag
    assert_eq!(k(&format!("\"{GET}\" --via-browser")), ToolKind::Exec);
    assert_eq!(k(&format!("\"{GET}\" --via-browser=abc")), ToolKind::Exec);
    assert_eq!(k(&format!("\"{GET}\" --via-browser --dry-run")), ToolKind::Read);
    // a map that cannot be read, or an endpoint it does not have, is the strict kind
    assert_eq!(kind_in(&cmd("call nope.json \"GET /v1/items\""), d), ToolKind::Exec);
    assert_eq!(k("\"GET /nothing\""), ToolKind::Exec);
    // a relative map is looked up from the folder given
    assert_eq!(kind_in(&cmd(&format!("call map.json \"{GET}\"")), d), ToolKind::Network);
    // the rest of the ops
    assert_eq!(kind_in(&cmd("describe map.json"), d), ToolKind::Read);
    assert_eq!(kind_in(&cmd("analyze cap.har"), d), ToolKind::Read);
    assert_eq!(kind_in(&cmd("analyze cap.har --save m.json"), d), ToolKind::Write);
    assert_eq!(kind_in(&cmd("analyze cap.har --openapi o.json"), d), ToolKind::Write);
    // a client is a write when it goes to a file, a read when it is only printed
    assert_eq!(kind_in(&cmd("client map.json --lang ts"), d), ToolKind::Read);
    assert_eq!(kind_in(&cmd("client map.json --lang ts --out c.ts"), d), ToolKind::Write);
    assert_eq!(kind_in(&cmd("client map.json --lang sh --out c.sh --force"), d), ToolKind::Write);
}

#[test]
fn call_kind_is_the_same_decision_from_the_tool_arguments() {
    let dir = folder();
    let map = dir.path().join("map.json");
    let kind = |tail: &str| Api.call_kind(&json!({"command": format!("call {} {tail}", map.display())}));
    assert_eq!(kind(&format!("\"{GET}\"")), ToolKind::Network);
    assert_eq!(kind(&format!("\"{POST}\"")), ToolKind::Exec);
    assert_eq!(kind(&format!("\"{POST}\" --dry-run")), ToolKind::Read);
    assert_eq!(Api.call_kind(&json!({"command": "analyze a.har --save m.json"})), ToolKind::Write);
    assert_eq!(Api.call_kind(&json!({"command": "describe m.json"})), ToolKind::Read);
    assert_eq!(Api.call_kind(&json!({})), ToolKind::Read);
}

#[test]
fn a_dry_run_does_not_read_the_environment() {
    let mut list = vec!["q=@env:SOME_VAR".to_string(), "page=2".to_string()];
    hide_env(&mut list);
    assert_eq!(list, vec!["q=<SOME_VAR>".to_string(), "page=2".to_string()]);
}

#[test]
fn the_answer_is_cut_with_the_count() {
    assert_eq!(capped("abc".into(), 5), "abc");
    let cut = capped("a".repeat(20), 5);
    assert!(cut.starts_with("aaaaa\n[cut: 15 more characters"), "{cut}");
    // by characters, not bytes
    assert!(capped("é".repeat(10), 4).starts_with("éééé\n[cut: 6 "));
}

#[test]
fn the_tool_is_offered_as_a_rare_strict_tool_with_a_short_text() {
    assert_eq!(Api.name(), "api");
    assert!(Api.rare());
    assert_eq!(Api.kind(), ToolKind::Exec);
    let d = Api.spec().description;
    for w in ["analyze", "describe", "call", "--dry-run", "environment variable", "No watch", "--reveal"] {
        assert!(d.contains(w), "{w}");
    }
    let c = Api.compact().unwrap().description;
    assert!(c.len() < 260 && c.contains("ENVVAR"), "{c}");
}

#[tokio::test]
async fn analyze_prints_the_summary_with_no_secret_and_save_writes_the_map() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cap.har"), har()).unwrap();
    let o = run(dir.path(), "analyze cap.har --save out.json").await;
    assert!(!o.is_error, "{}", o.content);
    assert!(o.content.contains("/v1/items"), "{}", o.content);
    for secret in ["QUERYSECRET1234", "HEADERSECRET1234"] {
        assert!(!o.content.contains(secret), "{}", o.content);
    }
    let saved = std::fs::read_to_string(dir.path().join("out.json")).unwrap();
    assert!(!saved.contains("QUERYSECRET1234") && !saved.contains("HEADERSECRET1234"));
    // the map it wrote is the one `describe` reads
    let d = run(dir.path(), "describe out.json").await;
    assert!(!d.is_error && d.content.contains("/v1/items"), "{}", d.content);
    let e = run(dir.path(), &format!("describe out.json \"{GET}\"")).await;
    assert!(!e.is_error, "{}", e.content);
}

#[tokio::test]
async fn a_dry_run_prints_the_request_and_sends_nothing() {
    let dir = folder();
    let o = run(dir.path(), &format!("call map.json \"{POST}\" --body '{{\"name\":\"b\"}}' --dry-run")).await;
    assert!(!o.is_error, "{}", o.content);
    assert!(o.content.contains("POST") && o.content.contains("api.test"), "{}", o.content);
    assert!(!o.content.contains("HEADERSECRET1234"), "{}", o.content);
}

#[tokio::test]
async fn watch_reveal_and_a_secret_value_stop_before_anything_runs() {
    let dir = folder();
    for line in [
        format!("watch map.json \"{GET}\" --every 30s"),
        format!("call map.json \"{GET}\" --reveal"),
        format!("call map.json \"{GET}\" -p password=hunter2"),
    ] {
        let o = run(dir.path(), &line).await;
        assert!(o.is_error && o.content.starts_with("api: "), "{line}: {}", o.content);
    }
    assert!(run(dir.path(), "").await.content.contains("analyze"));
    assert!(Api.run(json!({}), &ToolContext::new(dir.path().to_path_buf())).await.is_error);
}

#[tokio::test]
async fn a_rule_that_denies_a_read_a_write_or_a_host_stops_the_call() {
    let dir = folder();
    let deny = |what: &'static str| -> ToolContext {
        let mut ctx = ToolContext::new(dir.path().to_path_buf());
        ctx.check = Some(Arc::new(
            move |tool: &str, _kind: ToolKind, _args: &Value| {
                if tool == what {
                    Check::Deny
                } else {
                    Check::Allow
                }
            },
        ));
        ctx
    };
    let args = |line: &str| json!({"command": line});
    let r = Api.run(args("describe map.json"), &deny("read")).await;
    assert!(r.is_error && r.content.contains("denies reading"), "{}", r.content);
    let w = Api.run(args("analyze cap.har --save again.json"), &deny("edit")).await;
    assert!(w.is_error && w.content.contains("denies writing"), "{}", w.content);
    assert!(!dir.path().join("again.json").exists());
    let h = Api.run(args(&format!("call map.json \"{GET}\"")), &deny("web_fetch")).await;
    assert!(h.is_error && h.content.contains("denies https://api.test"), "{}", h.content);
    // a dry run is not a call to the host
    let dry = Api.run(args(&format!("call map.json \"{GET}\" --dry-run")), &deny("web_fetch")).await;
    assert!(!dry.is_error, "{}", dry.content);
}

#[test]
fn describe_honors_the_budget_like_analyze() {
    let dir = tempfile::tempdir().unwrap();
    // paths of different lengths, so none of them is folded into another as a parameter
    let words = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet"];
    let entries: Vec<Value> = (1..=words.len())
        .map(|n| entry("GET", &format!("https://api.test/{}/list", words[..n].join("/")), None))
        .collect();
    let cap = json!({"log": {"version": "1.2", "entries": entries}}).to_string();
    std::fs::write(dir.path().join("cap.har"), cap).unwrap();
    let map = dir.path().join("map.json");
    crate::api_cmd::analyze_text(&dir.path().join("cap.har"), &[], None, false, 6000, Some(&map)).unwrap();
    let whole = crate::api_call::describe_out(&map, None, 6000, false).unwrap().text;
    let small = crate::api_call::describe_out(&map, None, 300, false).unwrap().text;
    assert!(small.len() < whole.len(), "{} vs {}", small.len(), whole.len());
    assert!(small.contains("more endpoints") && !whole.contains("more endpoints"), "{small}");
    // the tool's own line takes the flag too
    assert!(matches!(cmd("describe m.json --budget 300"), ApiCmd::Describe { budget: 300, .. }));
}
