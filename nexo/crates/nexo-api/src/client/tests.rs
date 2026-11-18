use super::*;
use crate::paging::Paging;
use crate::{analyze_har, spec};
use serde_json::json;
use std::process::{Command, Stdio};

const TOKEN: &str = "tkAAAA1111bbbbCCCC2222dddd";
const SESSION: &str = "sessVALUE9876zzzzYYYY";
const CSRF: &str = "csrfVALUE5555qqqqWWWW";
const PASSWORD: &str = "hunter2hunter2";

fn pairs(list: &[(&str, &str)]) -> Value {
    Value::Array(list.iter().map(|(n, v)| json!({ "name": n, "value": v })).collect())
}

/// One HAR entry: method, url, request headers, request body, status, response headers, response body.
fn entry(
    method: &str,
    url: &str,
    req: &[(&str, &str)],
    body: Option<&str>,
    resp: &[(&str, &str)],
    out: Option<(&str, &str)>,
) -> Value {
    let mut request = json!({ "method": method, "url": url, "headers": pairs(req) });
    if let Some(b) = body {
        request["postData"] = json!({ "mimeType": "application/json", "text": b });
    }
    let mut content = json!({});
    if let Some((mime, text)) = out {
        content = json!({ "mimeType": mime, "text": text });
    }
    json!({
        "_resourceType": "xhr",
        "request": request,
        "response": { "status": 200, "headers": pairs(resp), "content": content },
    })
}

/// Bearer auth, a session cookie with a CSRF echo, a path parameter, a list that pages by cursor, a POST with a
/// body and one GraphQL operation.
fn api() -> Api {
    let auth = format!("Bearer {TOKEN}");
    let cookies = format!("csrftoken={CSRF}; sessionid={SESSION}");
    let csrf_cookie = format!("csrftoken={CSRF}; Path=/");
    let session_cookie = format!("sessionid={SESSION}; Path=/; HttpOnly");
    let login = format!(r#"{{"email":"ann@example.com","password":"{PASSWORD}"}}"#);
    let login_resp = format!(r#"{{"access_token":"{TOKEN}","user":{{"id":1,"name":"Ann"}}}}"#);
    let gql = r#"{"operationName":"GetUser","query":"query GetUser($id: ID!) { user(id: $id) { id name } }","variables":{"id":"7"}}"#;
    let json = "application/json";
    let sent = [("authorization", auth.as_str()), ("cookie", cookies.as_str()), ("x-csrftoken", CSRF)];
    let page1 = r#"{"items":[{"id":1,"name":"a"},{"id":2,"name":"b"}],"next_cursor":"c1"}"#;
    let page2 = r#"{"items":[{"id":3,"name":"c"}],"next_cursor":null}"#;
    let create = r#"{"name":"widget","tags":["x","y"],"price":2.5}"#;
    let entries = vec![
        entry(
            "GET",
            "https://app.test/",
            &[],
            None,
            &[("set-cookie", csrf_cookie.as_str())],
            Some(("text/html", "<html></html>")),
        ),
        entry(
            "POST",
            "https://app.test/api/login",
            &[("cookie", format!("csrftoken={CSRF}").as_str())],
            Some(login.as_str()),
            &[("set-cookie", session_cookie.as_str())],
            Some((json, login_resp.as_str())),
        ),
        entry("GET", "https://app.test/api/items?limit=10", &sent, None, &[], Some((json, page1))),
        entry("GET", "https://app.test/api/items?limit=10&cursor=c1", &sent, None, &[], Some((json, page2))),
        entry("GET", "https://app.test/api/items/42", &sent, None, &[], Some((json, r#"{"id":42,"name":"a"}"#))),
        entry("GET", "https://app.test/api/items/43", &sent, None, &[], Some((json, r#"{"id":43,"name":"b"}"#))),
        entry(
            "POST",
            "https://app.test/api/items",
            &sent,
            Some(create),
            &[],
            Some((json, r#"{"id":9,"name":"widget"}"#)),
        ),
        entry(
            "POST",
            "https://app.test/graphql",
            &sent,
            Some(gql),
            &[],
            Some((json, r#"{"data":{"user":{"id":"7","name":"Ann"}}}"#)),
        ),
    ];
    let har = json!({ "log": { "version": "1.2", "entries": entries } }).to_string();
    let mut api = analyze_har(&har, &spec::Options::default()).unwrap();
    // The paging is set by hand so these tests do not depend on how the analysis spots it.
    let list = api.endpoints.iter_mut().find(|e| e.method == "GET" && e.path == "/api/items").unwrap();
    list.paging = Some(Paging {
        style: "cursor",
        param: Some("cursor".into()),
        size: Some("limit".into()),
        next: Some("$.next_cursor".into()),
        items: Some("$.items".into()),
    });
    api
}

fn id_of(api: &Api, method: &str, path: &str) -> String {
    api.endpoints.iter().find(|e| e.method == method && e.path == path).unwrap().id.clone()
}

fn all(api: &Api) -> Vec<(Lang, String)> {
    [Lang::Ts, Lang::Py, Lang::Sh].iter().map(|&l| (l, render(api, l, &[], "2026-11-18").unwrap())).collect()
}

fn get(api: &Api, lang: Lang) -> String {
    render(api, lang, &[], "2026-11-18").unwrap()
}

#[test]
fn the_test_map_has_what_the_tests_need() {
    let api = api();
    assert_eq!(api.endpoints.len(), 6, "{:?}", api.endpoints.iter().map(|e| &e.id).collect::<Vec<_>>());
    assert!(api.endpoints.iter().any(|e| e.gql.is_some()));
    assert!(api.auth.len() >= 2);
}

#[test]
fn typescript_has_the_key_lines() {
    let api = api();
    let t = get(&api, Lang::Ts);
    let item = id_of(&api, "GET", "/api/items/{item_id}");
    let list = id_of(&api, "GET", "/api/items");
    let make = id_of(&api, "POST", "/api/items");
    let gql = id_of(&api, "POST", "/graphql");
    assert!(t.starts_with("// nexo api client: TypeScript"), "{t}");
    assert!(t.contains("Written by `nexo api client` on 2026-11-18."));
    assert!(t.contains("Origin: https://app.test"));
    assert!(t.contains("NEXO_API_BEARER: bearer token"));
    assert!(t.contains(&format!(
        "export async function {item}(item_id: string | number, query: Record<string, unknown> = {{}})"
    )));
    assert!(t.contains(&format!("export async function* {list}_all(")));
    assert!(t.contains(
        "style: \"cursor\", place: \"query\", path: [\"cursor\"], next: [\"next_cursor\"], items: [\"items\"]"
    ));
    assert!(t.contains(&format!("export async function {make}(query: Record<string, unknown> = {{}}, body?: ")));
    assert!(t.contains(&format!("export async function {gql}(variables: ")));
    assert!(t.contains("operationName") && t.contains("gql: \"GetUser\""));
    assert!(t.contains("AbortSignal.timeout(NX_TIMEOUT_MS)"));
    assert!(t.contains("nexo-api-client/1 (generated by nexo)"));
    assert!(t.contains("set the environment variable"));
    assert!(t.contains("nxCookie(nxEnv(s.jar), s.cookie)"));
    assert!(t.contains("cookie: \"csrftoken\""));
    assert!(t.contains("export interface"), "{t}");
    assert!(!t.contains("import "));
}

#[test]
fn python_has_the_key_lines() {
    let api = api();
    let t = get(&api, Lang::Py);
    let item = id_of(&api, "GET", "/api/items/{item_id}");
    let list = id_of(&api, "GET", "/api/items");
    let gql = id_of(&api, "POST", "/graphql");
    assert!(t.starts_with("# nexo api client: Python"), "{t}");
    assert!(t.contains("import urllib.request"));
    assert!(t.contains("NEXO_API_COOKIE"));
    assert!(t.contains(&format!("def {item}(item_id, query=None):")));
    assert!(t.contains(&format!("def {list}_all(query=None, limit=1000):")));
    assert!(t.contains("yield from _nx_pages("));
    assert!(t.contains("\"style\": \"cursor\", \"place\": \"query\", \"path\": [\"cursor\"]"));
    assert!(t.contains(&format!("def {gql}(variables=None, query=None, extensions=None):")));
    assert!(t.contains("NX_TIMEOUT = 30"));
    assert!(t.contains("nexo-api-client/1 (generated by nexo)"));
    assert!(!t.contains("import requests"));
}

#[test]
fn shell_has_the_key_lines() {
    let api = api();
    let t = get(&api, Lang::Sh);
    let item = id_of(&api, "GET", "/api/items/{item_id}");
    let list = id_of(&api, "GET", "/api/items");
    let gql = id_of(&api, "POST", "/graphql");
    assert!(t.starts_with("#!/bin/sh\n# nexo api client: POSIX sh"), "{t}");
    assert!(t.contains(&format!("{item}() {{")));
    assert!(t.contains("'/api/items/'\"$(_nx_enc \"$1\")\""));
    assert!(t.contains("curl -sS --max-time 30 -K -"));
    assert!(t.contains("NEXO_API_BEARER"));
    assert!(t.contains(&format!("{gql}() {{")));
    assert!(t.contains("\\\"operationName\\\"") || t.contains("\"operationName\":\"GetUser\""));
    assert!(t.contains("Paging (cursor"), "{t}");
    assert!(!t.contains(&format!("{list}_all")));
    assert!(!t.contains("Authorization: Bearer"));
}

#[test]
fn the_header_lists_the_operations_and_where_each_secret_comes_from() {
    let api = api();
    for (lang, t) in all(&api) {
        for e in &api.endpoints {
            assert!(t.contains(&format!("  {}: {} ", e.id, e.method)), "{lang:?} misses {}", e.id);
        }
        assert!(t.contains("Secrets come only from environment variables"), "{lang:?}");
        assert!(t.contains("NEXO_API_BEARER:"), "{lang:?}");
        assert!(t.contains("not sent unless set"), "{lang:?}");
        assert!(t.contains("2026-11-18"), "{lang:?}");
    }
}

#[test]
fn the_date_is_the_one_given_and_the_text_is_stable() {
    let api = api();
    let a = render(&api, Lang::Py, &[], "2030-01-02").unwrap();
    let b = render(&api, Lang::Py, &[], "2030-01-02").unwrap();
    assert_eq!(a, b);
    assert!(a.contains("on 2030-01-02."));
    assert!(!a.contains("2026-11-18"));
}

#[test]
fn only_keeps_the_endpoints_named() {
    let api = api();
    let item = id_of(&api, "GET", "/api/items/{item_id}");
    let t = render(&api, Lang::Ts, std::slice::from_ref(&item), "2026-11-18").unwrap();
    assert!(t.contains(&format!("export async function {item}(")));
    assert!(!t.contains("graphql"));
    // the auth note may say where a token came from (`set by POST /api/login`); the call itself is not written
    assert!(!t.contains("export async function post_api_login("));
    assert!(render(&api, Lang::Ts, &["nope".to_string()], "2026-11-18").is_err());
}

#[test]
fn no_secret_marker_or_value_is_written() {
    let mut api = api();
    // A map that holds markers and token-like examples must not leak them.
    for e in api.endpoints.iter_mut() {
        for p in e.query.iter_mut().chain(e.path_params.iter_mut()) {
            p.shape.example = Some(Value::String("<secret>".into()));
        }
        if let Some(b) = e.body.as_mut() {
            for f in b.shape.fields.values_mut() {
                f.example = Some(Value::String("<opaque>".into()));
            }
        }
    }
    for (lang, t) in all(&api) {
        for bad in [TOKEN, SESSION, CSRF, PASSWORD, "<secret>", "<opaque>", "ann@example.com", "hunter2"] {
            assert!(!t.contains(bad), "{lang:?} wrote {bad}");
        }
    }
}

#[test]
fn a_plain_example_stays_in_a_comment() {
    let api = api();
    let t = get(&api, Lang::Ts);
    let line = t.lines().find(|l| l.starts_with(" * query:") && l.contains("limit")).unwrap_or("");
    assert!(line.contains("e.g.") || line.is_empty(), "{line}");
}

const BAD_NAMES: &[&str] =
    &["class", "1st", "for-bar", "return", "nx_hack", "def", "done", "a b", "nxSend", "json", ""];

fn valid(name: &str) -> bool {
    let mut c = name.chars();
    matches!(c.next(), Some(f) if f.is_ascii_alphabetic() || f == '_')
        && c.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[test]
fn names_are_valid_identifiers_in_every_language() {
    let mut api = api();
    let spare = api.endpoints[0].clone();
    while api.endpoints.len() < BAD_NAMES.len() {
        api.endpoints.push(spare.clone());
    }
    for (e, bad) in api.endpoints.iter_mut().zip(BAD_NAMES) {
        e.id = bad.to_string();
    }
    let m = model(&api, &[], "2026-11-18").unwrap();
    for (reserved, pages) in [(ts::FUNC_RESERVED, true), (py::RESERVED, true), (sh::RESERVED, false)] {
        let nm = names(&m, reserved, pages);
        let mut seen = HashSet::new();
        for n in &nm {
            for name in std::iter::once(&n.func).chain(n.pages.iter()) {
                assert!(valid(name), "{name:?} is not an identifier");
                assert!(!reserved.contains(&name.as_str()), "{name:?} is reserved");
                assert!(!name.starts_with("nx") && !name.starts_with("_nx"), "{name:?} may clash with the runtime");
                assert!(seen.insert(name.clone()), "{name:?} is taken twice");
            }
        }
    }
    assert_eq!(ident("1st", &[]), "_1st");
    assert_eq!(ident("class", &["class"]), "class_");
    assert_eq!(ident("a-b.c", &[]), "a_b_c");
    assert_eq!(ident("", &[]), "_");
    // and the three files still render
    for (lang, t) in all(&api) {
        assert!(t.contains("nexo api client"), "{lang:?}");
    }
}

#[test]
fn path_names_are_found_anywhere_in_a_template() {
    assert_eq!(template_names("/a/{x}/b/{y}.json/{x}"), vec!["x".to_string(), "y".to_string()]);
    assert!(template_names("/plain").is_empty());
    assert_eq!(json_path("$.a.b"), Some(vec!["a".to_string(), "b".to_string()]));
    assert_eq!(json_path("$"), Some(Vec::new()));
    assert_eq!(json_path("$.a[].b"), None);
}

#[test]
fn text_helpers_escape() {
    assert_eq!(shq("it's"), "'it'\\''s'");
    assert_eq!(lit("a\"b\n"), "\"a\\\"b\\n\"");
    assert_eq!(clean("a\n\tb <secret> */"), "a b ? * /");
    assert!(!safe("<secret>"));
    assert!(!safe("tkAAAA1111bbbbCCCC2222dddd"));
    assert!(safe("hello world"));
    assert_eq!(Lang::parse("TS"), Ok(Lang::Ts));
    assert!(Lang::parse("go").is_err());
}

#[test]
fn channels_are_skipped_with_a_pointer_to_watch() {
    let mut api = api();
    api.channels.push(crate::channel::Channel {
        id: "ws_socket".into(),
        kind: crate::channel::Kind::Ws,
        origin: "wss://app.test".into(),
        path: "/socket".into(),
        connections: 1,
        auth: Vec::new(),
        field: None,
        messages: 0,
        span: None,
        types: Vec::new(),
        beat: None,
        subscribe: Vec::new(),
    });
    for (lang, t) in all(&api) {
        assert!(t.contains("ws_socket (ws) is not in this client"), "{lang:?}");
        assert!(t.contains("nexo api watch"), "{lang:?}");
    }
}

// ------------------------------------------------------------ syntax checks, when the tools are there

fn has(cmd: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(cmd).args(args).stdin(Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).to_string())
}

fn run_ok(cmd: &mut Command) -> Result<(), String> {
    let o = cmd.stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).to_string())
    }
}

/// Node 22.18 or newer strips types by itself.
fn node_strips_types() -> bool {
    let Some(v) = has("node", &["--version"]) else { return false };
    let mut it = v.trim().trim_start_matches('v').split('.').filter_map(|x| x.parse::<u32>().ok());
    let (major, minor) = (it.next().unwrap_or(0), it.next().unwrap_or(0));
    major > 22 || (major == 22 && minor >= 18)
}

#[test]
fn the_generated_files_pass_a_syntax_check_where_the_tools_exist() {
    let api = api();
    let dir = std::env::temp_dir().join(format!("nexo-client-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures: Vec<String> = Vec::new();
    for (lang, text) in all(&api) {
        let path = dir.join(format!("client.{}", lang.ext()));
        std::fs::write(&path, &text).unwrap();
        let p = path.to_string_lossy().to_string();
        let res = match lang {
            Lang::Ts if node_strips_types() => {
                // importing a module with only declarations runs the parser and the type stripper
                let url = format!("file://{p}");
                run_ok(Command::new("node").args([
                    "-e",
                    "import(process.argv[1]).then(() => {}, (e) => { console.error(String(e)); process.exit(1); })",
                    &url,
                ]))
            }
            Lang::Py if has("python3", &["--version"]).is_some() => {
                run_ok(Command::new("python3").args(["-I", "-m", "py_compile", &p]))
            }
            Lang::Sh if has("sh", &["-c", "echo ok"]).is_some() => run_ok(Command::new("sh").args(["-n", &p])),
            _ => continue,
        };
        if let Err(e) = res {
            failures.push(format!("{lang:?}: {e}"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
