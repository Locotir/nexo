//! `nexo api check MAP [ENDPOINT...]`: calls the safe endpoints of a saved map and says, per endpoint, whether the live
//! answer still has the status, the content type and the shape the map recorded (the comparison itself is
//! `nexo_api::drift`). It is one shot, for a cron or a watcher to run; `watch` is the one that repeats.
//!
//! Each call goes through `api_call::call_out`, the same request builder and the same scrub `call` uses: auth comes
//! from environment variables only, never from a login (`check` never passes `--login`), and the body it reads back is
//! the scrubbed one. Only field names, types and statuses are printed, never a value.

use crate::api_call;
use crate::api_cmd::Target;
use crate::headless::EXIT_USAGE;
use nexo_api::drift::{self, Report, Verdict};
use nexo_api::spec::Endpoint;
use nexo_api::{call, store};
use serde_json::{json, Value};
use std::path::Path;

/// Exit code: some endpoint drifted.
const EXIT_DRIFT: i32 = 1;
/// Exit code: some endpoint is broken.
const EXIT_BROKEN: i32 = 2;
/// Exit code: the map or the arguments cannot be used.
const EXIT_BAD: i32 = 3;
/// How many detail lines one endpoint prints.
const MAX_PER_ENDPOINT: usize = 12;
/// How many detail lines the whole report prints.
const MAX_DETAIL: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ok,
    Drift,
    Broken,
    Skipped,
}

impl State {
    fn word(self) -> &'static str {
        match self {
            State::Ok => "ok",
            State::Drift => "drift",
            State::Broken => "broken",
            State::Skipped => "skipped",
        }
    }
}

/// What one endpoint came to.
#[derive(Debug)]
struct Row {
    id: String,
    method: String,
    path: String,
    state: State,
    /// The live status, when there was an answer.
    status: Option<u16>,
    content_type: String,
    /// Why it was skipped, or what went wrong when there was no answer.
    note: Option<String>,
    report: Option<Report>,
}

fn base(ep: &Endpoint, state: State) -> Row {
    Row {
        id: ep.id.clone(),
        method: ep.method.clone(),
        path: ep.path.clone(),
        state,
        status: None,
        content_type: String::new(),
        note: None,
        report: None,
    }
}

fn skipped(ep: &Endpoint, why: String) -> Row {
    Row { note: Some(why), ..base(ep, State::Skipped) }
}

fn no_answer(ep: &Endpoint, why: String) -> Row {
    Row { note: Some(why), ..base(ep, State::Broken) }
}

fn answered(ep: &Endpoint, status: u16, content_type: String, body: Option<Value>) -> Row {
    let report = drift::check(ep, status, &content_type, body.as_ref());
    let state = match report.verdict {
        Verdict::Ok => State::Ok,
        Verdict::Drift => State::Drift,
        Verdict::Broken => State::Broken,
    };
    Row { status: Some(status), content_type, report: Some(report), ..base(ep, state) }
}

fn lookup(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn is_safe(method: &str) -> bool {
    method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD")
}

/// Whether a `-p` name belongs to this endpoint, so a param meant for one endpoint is not an error for the others.
fn takes(ep: &Endpoint, name: &str) -> bool {
    let body_has = |n: &str| {
        !is_safe(&ep.method)
            && ep.body.as_ref().is_some_and(|b| b.shape.fields.contains_key(n.split('.').next().unwrap_or(n)))
    };
    if let Some(n) = name.strip_prefix("query.") {
        return ep.query.iter().any(|p| p.name == n);
    }
    if let Some(n) = name.strip_prefix("body.") {
        return body_has(n);
    }
    ep.path_params.iter().any(|p| p.name == name) || ep.query.iter().any(|p| p.name == name) || body_has(name)
}

/// What `call` said when it could not build the request, as a short "needs ...". It is the usage error of
/// `api_call::call_out`: missing parameters, or auth variables that are not set.
fn skip_reason(msg: &str) -> String {
    if let Some(rest) = msg.strip_prefix("missing required parameters: ") {
        let list = rest.split("; give each").next().unwrap_or(rest);
        return format!("needs {list}");
    }
    if let Some(rest) = msg.strip_prefix("set ") {
        let list = rest.split(" (or name").next().unwrap_or(rest);
        return format!("needs {list} set");
    }
    msg.lines().next().unwrap_or("").to_string()
}

/// The status, the content type and the body (when it is JSON) out of what `call_out` printed with `json` on.
fn parse_answer(text: &str) -> Option<(u16, String, Option<Value>)> {
    let v: Value = serde_json::from_str(text).ok()?;
    let status = u16::try_from(v.get("status")?.as_u64()?).ok()?;
    let ct = v.get("content_type").and_then(Value::as_str).unwrap_or("").to_string();
    let body = v.get("body").cloned().unwrap_or(Value::Null);
    // `call_out` gives text that did not parse as a JSON string
    let json_body = if body.is_string() && !ct.to_ascii_lowercase().contains("json") { None } else { Some(body) };
    Some((status, ct, json_body))
}

async fn check_one(map: &Path, ep: &Endpoint, params: &[String], auth: &[String]) -> Row {
    let given: Vec<String> =
        params.iter().filter(|p| takes(ep, p.split_once('=').map_or(p.as_str(), |(n, _)| n).trim())).cloned().collect();
    let t = Target {
        map: map.to_path_buf(),
        endpoint: ep.id.clone(),
        params: given,
        body: None,
        auth: auth.to_vec(),
        login: false,
        login_params: Vec::new(),
        login_body: None,
        via_browser: None,
        select: Vec::new(),
    };
    match api_call::call_out(&t, false, true, false).await {
        Ok(out) => match parse_answer(&out.text) {
            Some((status, ct, body)) => answered(ep, status, ct, body),
            None => no_answer(ep, "the answer could not be read".to_string()),
        },
        Err((code, msg)) if code == EXIT_USAGE => skipped(ep, skip_reason(&msg)),
        Err((_, msg)) => no_answer(ep, msg),
    }
}

async fn run(
    map: &Path,
    names: &[String],
    params: &[String],
    auth: &[String],
    allow_unsafe: bool,
) -> Result<Vec<Row>, String> {
    let text = std::fs::read_to_string(map).map_err(|e| format!("cannot read {}: {e}", map.display()))?;
    let api = store::from_text(&text).map_err(|e| format!("{}: {e}", map.display()))?;
    for p in params {
        call::parse_param(p, &lookup)?;
    }
    call::auth_map(&api, auth)?;
    let mut chosen: Vec<&Endpoint> = Vec::new();
    if names.is_empty() {
        chosen.extend(api.endpoints.iter());
    } else {
        for n in names {
            let e = call::find(&api, n)?;
            if !chosen.iter().any(|c| c.id == e.id) {
                chosen.push(e);
            }
        }
    }
    let mut rows = Vec::new();
    for ep in chosen {
        if !is_safe(&ep.method) && !allow_unsafe {
            rows.push(skipped(ep, format!("{} is not GET or HEAD (--unsafe calls it)", ep.method)));
            continue;
        }
        rows.push(check_one(map, ep, params, auth).await);
    }
    Ok(rows)
}

/// `nexo api check`.
pub async fn check(
    map: &Path,
    names: &[String],
    params: &[String],
    auth: &[String],
    allow_unsafe: bool,
    as_json: bool,
) -> i32 {
    match run(map, names, params, auth, allow_unsafe).await {
        Ok(rows) => {
            let code = exit_code(&rows);
            if as_json {
                print!("{}", render_json(&rows, code));
            } else {
                print!("{}", render_text(&rows));
            }
            code
        }
        Err(e) => {
            eprintln!("nexo api: {e}");
            EXIT_BAD
        }
    }
}

fn exit_code(rows: &[Row]) -> i32 {
    if rows.iter().any(|r| r.state == State::Broken) {
        EXIT_BROKEN
    } else if rows.iter().any(|r| r.state == State::Drift) {
        EXIT_DRIFT
    } else {
        0
    }
}

fn count(rows: &[Row], s: State) -> usize {
    rows.iter().filter(|r| r.state == s).count()
}

/// The first word of a line: `ok`, `drift 2`, `broken`, `skipped`.
fn tag(r: &Row) -> String {
    match (r.state, &r.report) {
        (State::Drift, Some(rep)) => format!("drift {}", rep.drift_count()),
        (s, _) => s.word().to_string(),
    }
}

/// What a line says after the endpoint: the statuses and the content type, or why there is no answer.
fn gist(r: &Row) -> String {
    match (&r.report, r.status) {
        (Some(rep), Some(st)) => {
            let ct = r.content_type.split(';').next().unwrap_or("").trim();
            let ct = if ct.is_empty() { "no content type" } else { ct };
            format!("{st} (recorded {}) {ct}", drift::status_text(&rep.recorded_status))
        }
        _ => r.note.clone().unwrap_or_default(),
    }
}

/// The lines below the list for one endpoint.
fn details(r: &Row) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(rep) = &r.report {
        v.extend(rep.broken.iter().map(|b| format!("broken: {b}")));
        v.extend(rep.changes.iter().map(|c| format!("drift: {c}")));
        v.extend(rep.findings.iter().map(|f| format!("{}: {}", f.severity(), f.text())));
    }
    v
}

fn render_text(rows: &[Row]) -> String {
    let mut out = String::new();
    for r in rows {
        out.push_str(&format!("{:<9} {}  {} {}  {}\n", tag(r), r.id, r.method, r.path, gist(r)));
    }
    let mut left = MAX_DETAIL;
    let mut cut = 0usize;
    for r in rows {
        let d = details(r);
        if d.is_empty() {
            continue;
        }
        let room = left.min(MAX_PER_ENDPOINT);
        if room == 0 {
            cut += d.len();
            continue;
        }
        out.push_str(&format!("\n{}:\n", r.id));
        for line in d.iter().take(room) {
            out.push_str(&format!("  {line}\n"));
        }
        let shown = d.len().min(room);
        left -= shown;
        if d.len() > shown {
            out.push_str(&format!("  ... {} more (--json has all)\n", d.len() - shown));
        }
    }
    if cut > 0 {
        out.push_str(&format!("\n... {cut} more lines (--json has all)\n"));
    }
    out.push_str(&format!(
        "\n{} endpoints: {} ok, {} drift, {} broken, {} skipped\n",
        rows.len(),
        count(rows, State::Ok),
        count(rows, State::Drift),
        count(rows, State::Broken),
        count(rows, State::Skipped)
    ));
    out
}

fn row_json(r: &Row) -> Value {
    let rep = r.report.as_ref();
    let findings: Vec<Value> = rep
        .map(|x| {
            x.findings
                .iter()
                .map(|f| json!({ "severity": f.severity(), "kind": f.name(), "path": f.path, "text": f.text() }))
                .collect()
        })
        .unwrap_or_default();
    json!({
        "id": r.id,
        "method": r.method,
        "path": r.path,
        "state": r.state.word(),
        "status": r.status,
        "recorded_status": rep.map(|x| x.recorded_status.clone()),
        "content_type": r.content_type,
        "note": r.note,
        "drift": rep.map_or(0, Report::drift_count),
        "info": rep.map_or(0, Report::info_count),
        "broken": rep.map(|x| x.broken.clone()).unwrap_or_default(),
        "changes": rep.map(|x| x.changes.clone()).unwrap_or_default(),
        "findings": findings,
    })
}

fn render_json(rows: &[Row], code: i32) -> String {
    let v = json!({
        "summary": {
            "endpoints": rows.len(),
            "ok": count(rows, State::Ok),
            "drift": count(rows, State::Drift),
            "broken": count(rows, State::Broken),
            "skipped": count(rows, State::Skipped),
            "exit": code,
        },
        "endpoints": rows.iter().map(row_json).collect::<Vec<Value>>(),
    });
    format!("{}\n", serde_json::to_string_pretty(&v).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_api::shape::Shape;
    use nexo_api::spec::{Param, Payload};

    fn param(name: &str) -> Param {
        Param { name: name.to_string(), seen: 1, shape: Shape::default() }
    }

    fn ep(method: &str, path: &str, recorded: Value) -> Endpoint {
        let payload = Payload { mime: "application/json".to_string(), shape: Shape::of(&recorded), non_json: 0 };
        Endpoint {
            id: format!("{}_x", method.to_lowercase()),
            method: method.to_string(),
            origin: "https://app.test".to_string(),
            path: path.to_string(),
            gql: None,
            calls: 1,
            path_params: Vec::new(),
            query: vec![param("q")],
            headers: Vec::new(),
            body: None,
            responses: [(200u16, payload)].into_iter().collect(),
            paging: None,
            auth: Vec::new(),
            example: String::new(),
        }
    }

    #[test]
    fn the_answer_of_call_is_read_back() {
        let t = r#"{"status":200,"content_type":"application/json; charset=utf-8","body":{"a":1},"bytes_not_read":0}"#;
        let (s, ct, b) = parse_answer(t).unwrap();
        assert_eq!((s, ct.as_str(), b), (200, "application/json; charset=utf-8", Some(json!({"a": 1}))));
        let html = r#"{"status":200,"content_type":"text/html","body":"<html>"}"#;
        assert_eq!(parse_answer(html).unwrap().2, None);
        let missing = r#"{"content_type":"x"}"#;
        assert!(parse_answer(missing).is_none());
        assert!(parse_answer("not json").is_none());
    }

    #[test]
    fn what_call_says_about_missing_things_becomes_a_skip_reason() {
        let p = "missing required parameters: item_id (path, string); give each with -p name=value";
        assert_eq!(skip_reason(p), "needs item_id (path, string)");
        let a = "set APP_BEARER (or name another variable with --auth NAME=ENVVAR, or use --login); `nexo api describe` lists them";
        assert_eq!(skip_reason(a), "needs APP_BEARER set");
        assert_eq!(skip_reason("something else\nmore"), "something else");
    }

    #[test]
    fn a_param_goes_only_to_the_endpoints_that_have_it() {
        let mut e = ep("GET", "/items/{item_id}", json!({}));
        e.path_params.push(param("item_id"));
        assert!(takes(&e, "item_id") && takes(&e, "q") && takes(&e, "query.q"));
        assert!(!takes(&e, "other") && !takes(&e, "query.other") && !takes(&e, "body.q"));
        let mut post = ep("POST", "/items", json!({}));
        post.body = Some(Payload { mime: String::new(), shape: Shape::of(&json!({"title": "a"})), non_json: 0 });
        assert!(takes(&post, "title") && takes(&post, "body.title"));
        // a GET takes no body field
        let mut get = ep("GET", "/items", json!({}));
        get.body = post.body.clone();
        assert!(!takes(&get, "title"));
    }

    #[test]
    fn only_get_and_head_are_safe() {
        assert!(is_safe("GET") && is_safe("head"));
        assert!(!is_safe("POST") && !is_safe("DELETE") && !is_safe("OPTIONS"));
    }

    fn rows() -> Vec<Row> {
        let e = ep("GET", "/a", json!({"id": 1, "name": "x"}));
        vec![
            answered(&e, 200, "application/json".to_string(), Some(json!({"id": 2, "name": "y"}))),
            answered(&e, 200, "application/json".to_string(), Some(json!({"id": "2", "extra": 1}))),
            answered(&e, 401, "application/json".to_string(), None),
            skipped(&ep("POST", "/b", json!({})), "POST is not GET or HEAD (--unsafe calls it)".to_string()),
        ]
    }

    #[test]
    fn the_exit_code_is_the_worst_state() {
        let r = rows();
        assert_eq!(exit_code(&r), EXIT_BROKEN);
        assert_eq!(exit_code(&r[..2]), EXIT_DRIFT);
        assert_eq!(exit_code(&r[..1]), 0);
        assert_eq!(exit_code(&r[3..]), 0);
        assert_eq!(exit_code(&[]), 0);
    }

    #[test]
    fn the_text_has_one_line_per_endpoint_then_the_details() {
        let text = render_text(&rows());
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("ok") && lines[0].contains("GET /a") && lines[0].contains("200 (recorded 200)"));
        assert!(lines[1].starts_with("drift 2"), "{text}");
        assert!(lines[2].starts_with("broken") && lines[2].contains("401 (recorded 200)"), "{text}");
        assert!(lines[3].starts_with("skipped") && lines[3].contains("--unsafe"), "{text}");
        assert!(text.contains("  drift: type changed at $.id: number -> string"), "{text}");
        assert!(text.contains("  drift: missing field $.name (was string)"), "{text}");
        assert!(text.contains("  info: new field $.extra (number)"), "{text}");
        assert!(text.contains("  broken: auth failed: status 401, the map recorded 200"), "{text}");
        assert!(text.ends_with("4 endpoints: 1 ok, 1 drift, 1 broken, 1 skipped\n"), "{text}");
    }

    #[test]
    fn the_details_are_capped() {
        let e = ep("GET", "/a", json!({}));
        let mut live = serde_json::Map::new();
        for i in 0..40 {
            live.insert(format!("f{i}"), json!(1));
        }
        let row = answered(&e, 200, "application/json".to_string(), Some(Value::Object(live)));
        let rows = [row];
        let text = render_text(&rows);
        assert_eq!(text.lines().filter(|l| l.starts_with("  info:")).count(), MAX_PER_ENDPOINT, "{text}");
        assert!(text.contains("... 28 more (--json has all)"), "{text}");
        // the JSON holds all of them
        let v: Value = serde_json::from_str(&render_json(&rows, 0)).unwrap();
        assert_eq!(v["endpoints"][0]["findings"].as_array().map(Vec::len), Some(40));
    }

    #[test]
    fn the_json_has_a_summary_and_the_same_states() {
        let r = rows();
        let v: Value = serde_json::from_str(&render_json(&r, exit_code(&r))).unwrap();
        assert_eq!(v["summary"]["exit"], 2);
        assert_eq!(v["summary"]["drift"], 1);
        assert_eq!(v["summary"]["skipped"], 1);
        assert_eq!(v["endpoints"][1]["state"], "drift");
        assert_eq!(v["endpoints"][1]["drift"], 2);
        assert_eq!(v["endpoints"][1]["info"], 1);
        assert_eq!(v["endpoints"][1]["findings"][0]["severity"], "drift");
        assert_eq!(v["endpoints"][2]["status"], 401);
        assert!(v["endpoints"][3]["status"].is_null());
    }

    #[test]
    fn a_value_never_reaches_the_report() {
        let e = ep("GET", "/a", json!({"name": "x"}));
        let r = answered(
            &e,
            200,
            "application/json".to_string(),
            Some(json!({"name": 5, "token": "tkAAAA1111bbbbCCCC2222"})),
        );
        let all = format!("{}{}", render_text(std::slice::from_ref(&r)), render_json(std::slice::from_ref(&r), 1));
        assert!(!all.contains("tkAAAA1111bbbbCCCC2222"), "{all}");
    }
}
