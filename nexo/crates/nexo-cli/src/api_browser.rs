//! `nexo api call --via-browser [ID]`: the same call, sent by the page of the person's own browser, where they are
//! already signed in.
//!
//! What moves where: Rust builds the request from the map (no secret in it: a secret param or `--auth` is refused),
//! turns it into a script, and the script runs inside a page on the API's origin (`AnyBrowser::connect_user`, then
//! `eval`). Cookies ride along with `credentials: "include"`. A header token (bearer or API key) is looked up inside
//! the page: every `localStorage` and `sessionStorage` value, and every string up to depth 2 of the JSON stored there,
//! is hashed with `crypto.subtle.digest("SHA-256")`; the one whose first 8 bytes equal the fingerprint the map holds
//! for that token is put in the header by the script itself. The script never returns it: only the status, the
//! headers (without `set-cookie`) and the body come back, and those pass the capture's redaction before anything is
//! printed.

use nexo_api::exchange;
use nexo_api::spec::{Api, Endpoint};
use nexo_api::{auth::Scheme, call};
use nexo_browser::AnyBrowser;
use serde_json::{json, Value};
use std::time::Duration;

/// An exit code and what to say (the one `api_call` uses).
type Fail = (i32, String);

fn failed(e: String) -> Fail {
    (1, e)
}

/// What stands in for a token while the request is built; the script never carries it.
const PLACEHOLDER: &str = "NEXO-PAGE-TOKEN";

/// A header token the page must hold.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Token {
    /// The scheme's name in `describe` (`bearer`, `header:x-api-key`).
    pub label: String,
    /// Lower-case header name.
    pub header: String,
    /// What goes before the token (`Bearer `).
    pub prefix: String,
    /// The fingerprint from the map, as 16 hex digits.
    pub fp: String,
}

/// A call ready for the page: what to send, and which tokens the page has to supply.
#[derive(Debug)]
pub(crate) struct Plan {
    /// `https://host` of the endpoint: the origin the page must be on.
    pub origin: String,
    pub method: String,
    /// The URL with its query.
    pub url: String,
    /// Plain headers only; the token headers are set in the page.
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub tokens: Vec<Token>,
    /// What `call::build` made, for `--dry-run` (every secret as `<secret>`).
    pub shown: String,
}

/// A name that says its value is a secret, whole or by its last dotted part (`body.password`).
fn secret_named(name: &str) -> bool {
    let last = name.rsplit('.').next().unwrap_or(name);
    exchange::secret_param(name) || exchange::secret_param(last)
}

fn secret_field(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.iter().any(|(k, x)| {
            (matches!(x, Value::String(s) if !s.is_empty()) && exchange::secret_name(k)) || secret_field(x)
        }),
        Value::Array(a) => a.iter().any(secret_field),
        _ => false,
    }
}

/// The refusals that need no map: flags that do not go with `--via-browser`.
pub(crate) fn refuse_flags(t: &crate::api_cmd::Target, reveal: bool) -> Result<(), String> {
    if t.login || !t.login_params.is_empty() || t.login_body.is_some() {
        return Err("--via-browser cannot be used with --login: the page's own session is the login".into());
    }
    if !t.auth.is_empty() {
        return Err("--via-browser cannot be used with --auth: the token is read inside the page, never given".into());
    }
    if reveal {
        return Err("--via-browser cannot be used with --reveal: nothing of the session is shown".into());
    }
    for p in &t.params {
        let (name, value) = p.split_once('=').unwrap_or((p.as_str(), ""));
        if value.trim().starts_with("@env:") || secret_named(name.trim()) {
            return Err(format!(
                "--via-browser takes no secret param: -p {} looks like one (the session is the page's, not yours)",
                name.trim()
            ));
        }
    }
    Ok(())
}

/// The request for the page, or the reason it is refused. Nothing is sent and no browser is touched.
pub(crate) fn plan(api: &Api, ep: &Endpoint, inp: &call::Inputs, t: &crate::api_cmd::Target) -> Result<Plan, String> {
    refuse_flags(t, false)?;
    if inp.body.as_ref().is_some_and(secret_field) {
        return Err("--via-browser takes no secret in the body: a field of it looks like one".into());
    }
    let mut vals: Vec<(usize, String)> = Vec::new();
    let mut tokens = Vec::new();
    for n in call::needs(api, ep) {
        let Some(a) = api.auth.get(n.index) else { continue };
        let (header, prefix) = match &a.scheme {
            Scheme::Cookie { .. } => continue,
            Scheme::Bearer { header } => (header.clone(), "Bearer "),
            Scheme::ApiKeyHeader { name } => (name.clone(), ""),
            _ => {
                return Err(format!(
                    "--via-browser cannot send the {} auth of this endpoint (only cookies, bearer and API-key headers)",
                    n.label
                ))
            }
        };
        let Some(fp) = a.fp.as_ref() else {
            return Err(format!(
                "the map has no fingerprint for {} (it was saved by an older nexo): run `nexo api analyze HAR --save MAP` again",
                n.label
            ));
        };
        tokens.push(Token { label: n.label.clone(), header, prefix: prefix.to_string(), fp: exchange::fp_hex(fp) });
        vals.push((n.index, PLACEHOLDER.to_string()));
    }
    let req = call::build(api, ep, inp, &vals)?;
    let mut url = url::Url::parse(&req.url).map_err(|e| format!("bad url: {e}"))?;
    // the page sends it, so it must stay on the origin the page is on (a path param cannot move the host)
    let origin =
        origin_of(&ep.origin).ok_or_else(|| format!("the endpoint's origin {:?} is not http(s)", ep.origin))?;
    if origin_of(url.as_str()).as_deref() != Some(origin.as_str()) {
        return Err("--via-browser sends only to the endpoint's own origin".to_string());
    }
    if !req.query.is_empty() {
        let mut q = url.query_pairs_mut();
        for (k, v) in &req.query {
            if v.reveal() != v.shown() {
                return Err(format!("--via-browser takes no secret param: {k} looks like one"));
            }
            q.append_pair(k, v.reveal());
        }
    }
    let mut headers = Vec::new();
    for (k, v) in &req.headers {
        if k == "cookie" || tokens.iter().any(|t| t.header == *k) {
            continue;
        }
        if v.reveal() != v.shown() {
            return Err(format!("--via-browser takes no secret header: {k} looks like one"));
        }
        headers.push((k.clone(), v.reveal().to_string()));
    }
    Ok(Plan {
        origin,
        method: req.method.clone(),
        url: url.to_string(),
        headers,
        body: req.body.as_ref().map(|b| b.wire()),
        tokens,
        shown: req.render(),
    })
}

/// The script's body, with `__SPEC__` for the request. It returns an object: `{refused}`, `{missing}`, `{redirect}`,
/// `{error}` or `{status, headers, text, cut}`. The token value lives only in local variables of this function.
const SCRIPT: &str = r#"(async () => {
  const spec = __SPEC__;
  if (location.origin !== spec.origin) return { refused: "origin", page: location.origin };
  if (new URL(spec.url, location.href).origin !== spec.origin) return { refused: "origin", page: location.origin };
  const headers = new Headers();
  for (const [k, v] of spec.headers) headers.set(k, v);
  if (spec.tokens.length > 0) {
    if (!(window.crypto && crypto.subtle)) return { refused: "crypto" };
    const seen = [];
    const walk = (v, d) => {
      if (typeof v === "string") seen.push(v);
      else if (v && typeof v === "object" && d < 2) for (const x of Object.values(v)) walk(x, d + 1);
    };
    for (const store of [window.localStorage, window.sessionStorage]) {
      try {
        for (let i = 0; i < store.length && seen.length < 5000; i++) {
          const raw = store.getItem(store.key(i));
          if (typeof raw !== "string") continue;
          seen.push(raw);
          try { walk(JSON.parse(raw), 0); } catch (e) {}
        }
      } catch (e) {}
    }
    const found = new Map();
    for (const s of seen) {
      if (s.length < 8 || s.length > 16384) continue;
      const forms = [s];
      const m = /^[A-Za-z]+\s+(\S+)$/.exec(s);
      if (m) forms.push(m[1]);
      for (const f of forms) {
        const d = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(f)));
        let h = "";
        for (let i = 0; i < 8; i++) h += d[i].toString(16).padStart(2, "0");
        if (!found.has(h)) found.set(h, f);
      }
    }
    const missing = [];
    for (const t of spec.tokens) {
      const v = found.get(t.fp);
      if (v === undefined) missing.push(t.label);
      else headers.set(t.header, t.prefix + v);
    }
    if (missing.length > 0) return { missing };
  }
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), spec.timeout_ms);
  try {
    const init = { method: spec.method, headers, credentials: "include", redirect: "manual", signal: ctl.signal };
    if (spec.body !== null) init.body = spec.body;
    const r = await fetch(spec.url, init);
    if (r.type === "opaqueredirect") return { redirect: true };
    const hs = [];
    r.headers.forEach((v, k) => { if (k.toLowerCase() !== "set-cookie") hs.push([k, v]); });
    const chunks = [];
    let kept = 0, cut = 0;
    if (r.body) {
      const rd = r.body.getReader();
      for (;;) {
        const { done, value } = await rd.read();
        if (done) break;
        const room = Math.max(0, spec.cap - kept);
        if (room > 0) { chunks.push(value.subarray(0, room)); kept += Math.min(room, value.length); }
        cut += Math.max(0, value.length - room);
      }
    }
    const all = new Uint8Array(kept);
    let at = 0;
    for (const c of chunks) { all.set(c, at); at += c.length; }
    return { status: r.status, headers: hs, text: new TextDecoder().decode(all), cut };
  } catch (e) {
    return { error: e && e.name === "AbortError" ? "the call timed out" : "the page's fetch failed (CORS, or the host is unreachable)" };
  } finally {
    clearTimeout(timer);
  }
})()"#;

/// The script for a plan. `cap` is the most body bytes kept, `timeout_ms` how long the call may take.
pub(crate) fn script(plan: &Plan, cap: usize, timeout_ms: u64) -> String {
    let tokens: Vec<Value> = plan
        .tokens
        .iter()
        .map(|t| json!({ "label": t.label, "header": t.header, "prefix": t.prefix, "fp": t.fp }))
        .collect();
    let spec = json!({
        "origin": plan.origin,
        "method": plan.method,
        "url": plan.url,
        "headers": plan.headers,
        "body": plan.body,
        "tokens": tokens,
        "cap": cap,
        "timeout_ms": timeout_ms,
    });
    // U+2028 and U+2029 are legal in JSON but not in every JavaScript string literal
    let text = spec.to_string().replace('\u{2028}', "\\u2028").replace('\u{2029}', "\\u2029");
    SCRIPT.replacen("__SPEC__", &text, 1)
}

/// What the page answered, redacted.
#[derive(Debug, PartialEq)]
pub(crate) struct Page {
    pub status: u16,
    pub content_type: String,
    pub text: String,
    pub cut: usize,
}

/// Reads what the script returned. A refusal or a missing token is an error that says what to do.
pub(crate) fn interpret(plan: &Plan, reply: &str) -> Result<Page, Fail> {
    let v: Value = serde_json::from_str(reply).map_err(|_| failed("the page did not answer with JSON".to_string()))?;
    if let Some(r) = v.get("refused").and_then(Value::as_str) {
        return Err(match r {
            "origin" => {
                let page = v
                    .get("page")
                    .and_then(Value::as_str)
                    .and_then(|p| url::Url::parse(p).ok())
                    .map(|u| u.origin().ascii_serialization())
                    .unwrap_or_else(|| "an unknown origin".to_string());
                failed(format!(
                    "the page is on {page}, not on {} (the API's origin), so nothing was sent: sign in in that browser first",
                    plan.origin
                ))
            }
            _ => {
                failed("the page cannot hash its storage (it is not a secure context), so nothing was sent".to_string())
            }
        });
    }
    if let Some(list) = v.get("missing").and_then(Value::as_array) {
        let names: Vec<&str> = plan
            .tokens
            .iter()
            .filter(|t| list.iter().any(|m| m.as_str() == Some(t.label.as_str())))
            .map(|t| t.label.as_str())
            .collect();
        return Err(failed(format!(
            "the {} token is not in the page's localStorage or sessionStorage (no value matches the map's fingerprint), so nothing was sent: sign in in that browser first",
            names.join(", ")
        )));
    }
    if v.get("redirect").is_some() {
        return Err(failed(
            "the API answered with a redirect, and a page does not say where to; the session is probably gone: sign in in that browser first"
                .to_string(),
        ));
    }
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        return Err(failed(e.chars().take(200).collect()));
    }
    let status = v
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|s| u16::try_from(s).ok())
        .ok_or_else(|| failed("the page's answer has no status".to_string()))?;
    let headers: Vec<(String, String)> = v
        .get("headers")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| Some((p.get(0)?.as_str()?.to_string(), p.get(1)?.as_str()?.to_string())))
                .filter(|(k, _)| !k.eq_ignore_ascii_case("set-cookie"))
                .map(|(k, val)| {
                    let shown = nexo_browser::header_value_marked(&k, &val);
                    (k, shown)
                })
                .collect()
        })
        .unwrap_or_default();
    let content_type = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let raw = v.get("text").and_then(Value::as_str).unwrap_or("");
    let text = nexo_browser::body_marked(raw, &content_type);
    let cut = v.get("cut").and_then(Value::as_u64).unwrap_or(0) as usize;
    Ok(Page { status, content_type, text, cut })
}

/// The origin of a tab's address, when it has one.
fn origin_of(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| u.origin().ascii_serialization())
}

fn blank(url: &str) -> bool {
    url.is_empty() || url == "about:blank" || url.starts_with("chrome://newtab")
}

/// Connects to the person's browser (`id` empty: any that is allowed), runs the script in a tab on the origin, and
/// leaves the browser as it was found: a tab opened for this is closed.
pub(crate) async fn run(plan: &Plan, id: &str, cap: usize, timeout_ms: u64) -> Result<Page, Fail> {
    let js = script(plan, cap, timeout_ms);
    let id = if id.trim().is_empty() { None } else { Some(id.trim()) };
    let browser = AnyBrowser::connect_user(id, "nexo api").await.map_err(|e| {
        failed(format!("cannot reach your browser: {e} (the nexo extension must run, and you click Allow)"))
    })?;
    let reply = in_tab(&browser, &plan.origin, &js, timeout_ms).await;
    let _ = browser.close().await;
    interpret(plan, &reply?)
}

async fn in_tab(browser: &AnyBrowser, origin: &str, js: &str, timeout_ms: u64) -> Result<String, Fail> {
    let no = |e: nexo_browser::Error| failed(format!("the browser: {e}"));
    let tabs = browser.tabs().await.map_err(no)?;
    let mut opened = None;
    match tabs.iter().find(|t| origin_of(&t.url).as_deref() == Some(origin)) {
        Some(t) => {
            if !t.active {
                browser.tab_switch(t.index).await.map_err(no)?;
            }
        }
        None => {
            let url = format!("{origin}/");
            let open = browser.tab_new(&url);
            match tokio::time::timeout(Duration::from_secs(60), open).await {
                Ok(r) => opened = Some(r.map_err(no)?),
                Err(_) => return Err(failed(format!("the browser did not open {origin} in time"))),
            }
        }
    }
    // the script times its own fetch out; this bound is for a page that never answers at all
    let wait = Duration::from_millis(timeout_ms).saturating_add(Duration::from_secs(60));
    let reply = match tokio::time::timeout(wait, browser.eval(js)).await {
        Ok(r) => r.map_err(|e| failed(format!("the page could not run the call: {e}"))),
        Err(_) => Err(failed("the page did not answer in time".to_string())),
    };
    if let Some(i) = opened {
        // the tab made for this call; and, in a browser nexo launched, the empty one it started with (never a tab
        // of the person's own browser)
        let _ = browser.tab_close(i).await;
        if matches!(browser, AnyBrowser::Headless(_)) {
            for t in tabs.iter().rev().filter(|t| blank(&t.url) && t.index != i) {
                let _ = browser.tab_close(t.index).await;
            }
        }
    }
    reply
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_cmd::Target;
    use std::path::PathBuf;

    const TOKEN: &str = "tkAAAA1111bbbbCCCC2222dddd";

    fn entry(method: &str, url: &str, auth: bool) -> Value {
        let mut headers = vec![json!({"name": "Accept", "value": "application/json"})];
        if auth {
            headers.push(json!({"name": "Authorization", "value": format!("Bearer {TOKEN}")}));
        }
        json!({
            "_resourceType": "XHR",
            "request": { "method": method, "url": url, "headers": headers },
            "response": { "status": 200, "headers": [{"name": "Content-Type", "value": "application/json"}],
                "content": {"mimeType": "application/json", "text": "{\"items\":[1,2]}"} }
        })
    }

    fn api_of(entries: Vec<Value>) -> Api {
        let har = json!({"log": {"version": "1.2", "entries": entries}}).to_string();
        nexo_api::analyze_har(&har, &nexo_api::spec::Options::default()).unwrap()
    }

    fn api() -> Api {
        api_of(vec![
            entry("GET", "https://api.test/v1/items?page=1", true),
            entry("GET", "https://api.test/v1/open", false),
        ])
    }

    fn target() -> Target {
        Target {
            map: PathBuf::from("m.json"),
            endpoint: "GET /v1/items".into(),
            params: Vec::new(),
            body: None,
            auth: Vec::new(),
            login: false,
            login_params: Vec::new(),
            login_body: None,
            via_browser: Some(String::new()),
            select: Vec::new(),
        }
    }

    fn plan_of(api: &Api, key: &str, t: &Target) -> Result<Plan, String> {
        let ep = call::find(api, key).unwrap();
        plan(api, ep, &call::Inputs::default(), t)
    }

    #[test]
    fn the_plan_names_the_token_by_fingerprint_and_keeps_its_header_out() {
        let a = api();
        let p = plan_of(&a, "GET /v1/items", &target()).unwrap();
        assert_eq!(p.origin, "https://api.test");
        assert_eq!(p.tokens.len(), 1);
        let t = &p.tokens[0];
        assert_eq!((t.label.as_str(), t.header.as_str(), t.prefix.as_str()), ("bearer", "authorization", "Bearer "));
        assert_eq!(t.fp, exchange::fp_hex(&exchange::fingerprint(TOKEN)));
        assert!(p.headers.iter().all(|(k, _)| k != "authorization"), "{:?}", p.headers);
        assert!(p.shown.contains("<secret>") && !p.shown.contains(PLACEHOLDER), "{}", p.shown);
        let open = plan_of(&a, "GET /v1/open", &target()).unwrap();
        assert!(open.tokens.is_empty());
    }

    #[test]
    fn the_script_holds_the_fingerprint_and_no_secret() {
        let a = api();
        let p = plan_of(&a, "GET /v1/items", &target()).unwrap();
        let js = script(&p, 1024, 5000);
        assert!(js.contains(&p.tokens[0].fp), "the fingerprint is how the page finds the token");
        assert!(js.contains(r#"crypto.subtle.digest("SHA-256""#) && js.contains(r#"credentials: "include""#));
        assert!(js.contains("localStorage") && js.contains("sessionStorage") && js.contains("set-cookie"));
        assert!(!js.contains(TOKEN) && !js.contains(PLACEHOLDER), "no token value is in the script");
        assert!(!js.contains("__SPEC__"));
        // the script never hands the value back: nothing returned holds `v` or the header
        assert!(!js.contains("return { token") && !js.contains("return v;"));
        // the request travels as data: a quote or a line break in a param cannot end the literal
        let mut t = target();
        t.params = vec!["q=a\"; alert(1); \"\u{2028}".to_string()];
        let inp = call::Inputs {
            params: vec![("q".to_string(), "a\"; alert(1); \"\u{2028}".to_string())],
            ..call::Inputs::default()
        };
        let ep = call::find(&a, "GET /v1/items").unwrap();
        if let Ok(p2) = plan(&a, ep, &inp, &t) {
            let js2 = script(&p2, 1024, 5000);
            assert!(!js2.contains('\u{2028}'));
        }
    }

    #[test]
    fn flags_that_do_not_go_with_the_session_are_refused() {
        let a = api();
        let mut t = target();
        t.login = true;
        assert!(plan_of(&a, "GET /v1/items", &t).unwrap_err().contains("--login"));
        let mut t = target();
        t.auth = vec!["bearer=MY_TOKEN".into()];
        assert!(plan_of(&a, "GET /v1/items", &t).unwrap_err().contains("--auth"));
        let mut t = target();
        t.params = vec!["password=@env:PW".into()];
        assert!(plan_of(&a, "GET /v1/items", &t).unwrap_err().contains("secret param"));
        let mut t = target();
        t.params = vec!["api_key=abc".into()];
        assert!(plan_of(&a, "GET /v1/items", &t).unwrap_err().contains("secret param"));
        assert!(refuse_flags(&target(), true).unwrap_err().contains("--reveal"));
        assert!(refuse_flags(&target(), false).is_ok());
    }

    #[test]
    fn a_secret_in_the_body_is_refused() {
        let a = api();
        let ep = call::find(&a, "GET /v1/items").unwrap();
        let inp = call::Inputs { params: Vec::new(), body: Some(json!({"user": {"password": "hunter2hunter2"}})) };
        assert!(plan(&a, ep, &inp, &target()).unwrap_err().contains("secret in the body"));
    }

    #[test]
    fn a_map_without_the_fingerprint_asks_for_a_new_capture() {
        let mut a = api();
        for x in &mut a.auth {
            x.fp = None;
        }
        let e = plan_of(&a, "GET /v1/items", &target()).unwrap_err();
        assert!(e.contains("no fingerprint") && e.contains("analyze"), "{e}");
    }

    #[test]
    fn the_answers_the_page_can_give() {
        let a = api();
        let p = plan_of(&a, "GET /v1/items", &target()).unwrap();
        let err = |r: Value| interpret(&p, &r.to_string()).unwrap_err();
        let (code, msg) = err(json!({"refused": "origin", "page": "https://login.other.test/x?token=abc"}));
        assert_eq!(code, 1);
        assert!(msg.contains("https://login.other.test") && msg.contains("sign in in that browser first"), "{msg}");
        assert!(!msg.contains("token=abc"));
        let (_, msg) = err(json!({"missing": ["bearer"]}));
        assert!(msg.contains("bearer") && msg.contains("sign in in that browser first"), "{msg}");
        assert!(!msg.contains(TOKEN));
        assert!(err(json!({"redirect": true})).1.contains("redirect"));
        assert!(err(json!({"error": "the call timed out"})).1.contains("timed out"));
        assert!(err(json!({"refused": "crypto"})).1.contains("secure context"));
    }

    #[test]
    fn the_body_is_redacted_and_set_cookie_is_dropped() {
        let a = api();
        let p = plan_of(&a, "GET /v1/items", &target()).unwrap();
        let reply = json!({
            "status": 200,
            "headers": [["content-type", "application/json"], ["set-cookie", "sid=abcdef0123456789abcdef"]],
            "text": format!("{{\"access_token\":\"{TOKEN}\",\"n\":1}}"),
            "cut": 7,
        });
        let page = interpret(&p, &reply.to_string()).unwrap();
        assert_eq!((page.status, page.cut), (200, 7));
        assert_eq!(page.content_type, "application/json");
        assert!(!page.text.contains(TOKEN), "{}", page.text);
        assert!(page.text.contains("\"n\":1"), "{}", page.text);
    }

    #[test]
    fn origins_of_tabs_are_compared_as_origins() {
        assert_eq!(origin_of("https://api.test/app?x=1#y").as_deref(), Some("https://api.test"));
        assert_eq!(origin_of("http://127.0.0.1:8080/a").as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(origin_of("about:blank"), None);
        assert_eq!(origin_of("chrome://newtab/"), None);
        assert!(blank("about:blank") && blank("") && !blank("https://api.test/"));
    }
}
