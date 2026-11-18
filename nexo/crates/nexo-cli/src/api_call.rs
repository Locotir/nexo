//! `nexo api call|describe|watch`: runs an endpoint of a saved map (`nexo api analyze HAR --save FILE`).
//!
//! How a secret moves here: it is read from an environment variable the person named (`--auth NAME=ENVVAR`) or the
//! default one `describe` prints, or it is a token a login call returned (`--login`). It goes straight into a
//! `nexo_api::call::Val`, which prints as `<secret>`; the one place that reads it back is [`send`], which puts it in the
//! request. It is never written to disk and never printed. `--dry-run` sends nothing. A `watch --state` file holds the
//! last answer after `exchange::scrub_json`, so a token in an answer is saved as `<secret>`.

use crate::api_cmd::Target;
use crate::headless::EXIT_USAGE;
use nexo_api::spec::{Api, Endpoint};
use nexo_api::{call, diff, exchange, store, summary};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

/// How many bytes of an answer are read; the rest is counted.
const READ_CAP: usize = 8 << 20;
/// How long one call may take.
const TIMEOUT: Duration = Duration::from_secs(30);
/// How many changes a `watch` tick prints.
const MAX_CHANGES: usize = 100;

/// An exit code and what to say.
type Fail = (i32, String);

fn usage(e: String) -> Fail {
    (EXIT_USAGE, e)
}

fn failed(e: String) -> Fail {
    (1, e)
}

fn lookup(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn load_map(path: &Path) -> Result<Api, Fail> {
    let text = std::fs::read_to_string(path).map_err(|e| usage(format!("cannot read {}: {e}", path.display())))?;
    store::from_text(&text).map_err(|e| usage(format!("{}: {e}", path.display())))
}

/// A JSON value from the command line: the text itself, or `@file`.
fn json_arg(arg: &str) -> Result<Value, String> {
    let text = match arg.strip_prefix('@') {
        Some(file) => std::fs::read_to_string(file).map_err(|e| format!("cannot read {file}: {e}"))?,
        None => arg.to_string(),
    };
    serde_json::from_str(&text).map_err(|e| format!("not valid JSON ({e})"))
}

fn inputs(params: &[String], body: Option<&str>, what: &str) -> Result<call::Inputs, Fail> {
    let mut list = Vec::new();
    for p in params {
        list.push(call::parse_param(p, &lookup).map_err(usage)?);
    }
    let body = match body {
        Some(b) => Some(json_arg(b).map_err(|e| usage(format!("{what}: {e}")))?),
        None => None,
    };
    Ok(call::Inputs { params: list, body })
}

/// What a call returns.
struct Answer {
    status: u16,
    content_type: String,
    location: Option<String>,
    text: String,
    /// Bytes past the read cap.
    cut: usize,
}

/// A transport error without the URL, which may hold a secret query value.
fn net_error(e: reqwest::Error) -> String {
    if e.is_timeout() {
        "the call timed out".to_string()
    } else if e.is_connect() {
        "could not connect".to_string()
    } else {
        e.without_url().to_string()
    }
}

/// Opens a request. This is the only place a secret value is read back out of a `Val`.
async fn open(client: &reqwest::Client, req: &call::Request) -> Result<reqwest::Response, String> {
    let mut url = url::Url::parse(&req.url).map_err(|e| format!("bad url: {e}"))?;
    if !req.query.is_empty() {
        let mut q = url.query_pairs_mut();
        for (k, v) in &req.query {
            q.append_pair(k, v.reveal());
        }
    }
    let method =
        reqwest::Method::from_bytes(req.method.as_bytes()).map_err(|_| format!("bad method {}", req.method))?;
    let mut b = client.request(method, url);
    for (k, v) in &req.headers {
        b = b.header(k.as_str(), v.reveal());
    }
    if let Some(body) = &req.body {
        b = b.body(body.wire());
    }
    b.send().await.map_err(net_error)
}

/// Sends a request and reads the whole answer.
async fn send(client: &reqwest::Client, req: &call::Request) -> Result<Answer, String> {
    let mut resp = open(client, req).await?;
    let status = resp.status().as_u16();
    let header =
        |name: reqwest::header::HeaderName| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
    let content_type = header(reqwest::header::CONTENT_TYPE).unwrap_or_default();
    let location = header(reqwest::header::LOCATION);
    let mut buf: Vec<u8> = Vec::new();
    let mut cut = 0usize;
    while let Some(chunk) = resp.chunk().await.map_err(net_error)? {
        let room = READ_CAP.saturating_sub(buf.len());
        buf.extend_from_slice(&chunk[..chunk.len().min(room)]);
        cut += chunk.len().saturating_sub(room);
    }
    Ok(Answer { status, content_type, location, text: String::from_utf8_lossy(&buf).into_owned(), cut })
}

fn client() -> Result<reqwest::Client, Fail> {
    nexo_providers::build_client(|| {
        reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("nexo/", env!("CARGO_PKG_VERSION")))
    })
    .map_err(failed)
}

/// What one run needs: the map, the endpoint, what the person gave, and how auth is found.
struct Prepared {
    api: Api,
    ep: usize,
    inputs: call::Inputs,
    explicit: Vec<(usize, String)>,
    login_inputs: call::Inputs,
}

fn prepare(t: &Target) -> Result<Prepared, Fail> {
    let api = load_map(&t.map)?;
    let ep = {
        let e = call::find(&api, &t.endpoint).map_err(usage)?;
        api.endpoints.iter().position(|x| x.id == e.id).unwrap_or(0)
    };
    let explicit = call::auth_map(&api, &t.auth).map_err(usage)?;
    let inputs = inputs(&t.params, t.body.as_deref(), "--body")?;
    let login_inputs = inputs_for_login(t)?;
    Ok(Prepared { api, ep, inputs, explicit, login_inputs })
}

fn inputs_for_login(t: &Target) -> Result<call::Inputs, Fail> {
    inputs(&t.login_params, t.login_body.as_deref(), "--login-body")
}

/// The calls of one run: finds each scheme's value, running the login first when `--login` is given.
struct Session<'a> {
    api: &'a Api,
    client: reqwest::Client,
    explicit: Vec<(usize, String)>,
    login_inputs: call::Inputs,
    use_login: bool,
    /// The answers of the login calls, by endpoint id. In memory only.
    logins: BTreeMap<String, Value>,
    /// The tokens read from them, by scheme index. In memory only.
    logged: BTreeMap<usize, String>,
}

impl<'a> Session<'a> {
    async fn run_login(&self, lep: &Endpoint) -> Result<Value, Fail> {
        let (vals, missing) = call::auth_values(self.api, lep, &self.explicit, &BTreeMap::new(), &lookup);
        if !missing.is_empty() {
            return Err(usage(format!("the login {} needs {} set", lep.id, missing.join(", "))));
        }
        let req = call::build(self.api, lep, &self.login_inputs, &vals)
            .map_err(|e| usage(format!("login {}: {e}", lep.id)))?;
        let ans = send(&self.client, &req).await.map_err(failed)?;
        if !(200..300).contains(&ans.status) {
            return Err(failed(format!("the login {} answered {}", lep.id, ans.status)));
        }
        serde_json::from_str(&ans.text).map_err(|_| failed(format!("the login {} did not answer with JSON", lep.id)))
    }

    /// The value of each auth scheme of an endpoint, and the variables that are needed and not set.
    async fn values(&mut self, ep: &Endpoint) -> Result<(Vec<(usize, String)>, Vec<String>), Fail> {
        let api = self.api;
        if self.use_login {
            for need in call::needs(api, ep) {
                if self.logged.contains_key(&need.index) || self.explicit.iter().any(|(i, _)| *i == need.index) {
                    continue;
                }
                let Some(l) = &need.login else { continue };
                let lep = call::login_endpoint(api, l)
                    .ok_or_else(|| usage(format!("the map has no endpoint for the login {} {}", l.method, l.path)))?;
                if !self.logins.contains_key(&lep.id) {
                    let answer = self.run_login(lep).await?;
                    self.logins.insert(lep.id.clone(), answer);
                }
                let token = call::json_get(&self.logins[&lep.id], &l.json_path)
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| failed(format!("the login {} has no text at {}", lep.id, l.json_path)))?;
                self.logged.insert(need.index, token);
            }
        }
        Ok(call::auth_values(api, ep, &self.explicit, &self.logged, &lookup))
    }

    /// One call; after a 401 or 403 with `--login`, it logs in again and tries once more.
    async fn once(&mut self, ep: &Endpoint, inp: &call::Inputs) -> Result<Answer, Fail> {
        for attempt in 0..2 {
            let (vals, missing) = self.values(ep).await?;
            if !missing.is_empty() {
                return Err(usage(format!(
                    "set {} (or name another variable with --auth NAME=ENVVAR, or use --login); `nexo api describe` lists them",
                    missing.join(", ")
                )));
            }
            let req = call::build(self.api, ep, inp, &vals).map_err(usage)?;
            let ans = send(&self.client, &req).await.map_err(failed)?;
            if self.use_login && attempt == 0 && matches!(ans.status, 401 | 403) {
                self.logged.clear();
                self.logins.clear();
                continue;
            }
            return Ok(ans);
        }
        Err(failed("the call was refused after a new login".to_string()))
    }
}

fn session<'a>(p: &'a Prepared, t: &Target) -> Result<Session<'a>, Fail> {
    Ok(Session {
        api: &p.api,
        client: client()?,
        explicit: p.explicit.clone(),
        login_inputs: p.login_inputs.clone(),
        use_login: t.login,
        logins: BTreeMap::new(),
        logged: BTreeMap::new(),
    })
}

/// A text answer with every token-looking word (see `exchange::opaque`) replaced by `<secret>`.
fn scrub_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if exchange::opaque(word) {
            out.push_str(exchange::SECRET);
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+' | '/' | '=') {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// A `Location` URL made safe: `clean_url`, and also the value of an OAuth `code` param, which `clean_url` keeps.
fn scrub_location(l: &str) -> String {
    let clean = exchange::clean_url(l);
    let Some((head, query)) = clean.split_once('?') else { return clean };
    let pairs: Vec<String> = query
        .split('&')
        .map(|p| match p.split_once('=') {
            Some((k, v)) if !v.is_empty() && k.eq_ignore_ascii_case("code") => format!("{k}={}", exchange::SECRET),
            _ => p.to_string(),
        })
        .collect();
    format!("{head}?{}", pairs.join("&"))
}

/// What `call` prints. Unless `reveal`, secrets are scrubbed as `watch` does: JSON through `scrub_json`, text through
/// `scrub_text`, the `Location` URL through `clean_url` (secret params and the fragment go).
fn render_answer(a: &Answer, as_json: bool, reveal: bool) -> String {
    let parsed: Option<Value> = serde_json::from_str(&a.text).ok().map(|mut v: Value| {
        if !reveal {
            exchange::scrub_json(&mut v);
        }
        v
    });
    let location = a.location.as_ref().map(|l| if reveal { l.clone() } else { scrub_location(l) });
    let text = |t: &str| {
        let t = if reveal { t.to_string() } else { scrub_text(t) };
        call::show_text(&t)
    };
    let mut out = String::new();
    if as_json {
        let body = match &parsed {
            Some(v) => v.clone(),
            None => Value::String(text(&a.text)),
        };
        let v = json!({ "status": a.status, "content_type": a.content_type, "location": location, "body": body, "bytes_not_read": a.cut });
        out.push_str(&serde_json::to_string_pretty(&v).unwrap_or_default());
        out.push('\n');
        return out;
    }
    out.push_str(&format!("HTTP {} {}\n", a.status, a.content_type));
    if let Some(l) = &location {
        out.push_str(&format!("location: {l}\n"));
    }
    match parsed {
        Some(v) => out.push_str(&format!("{}\n", serde_json::to_string_pretty(&v).unwrap_or_default())),
        None => {
            let t = text(&a.text);
            out.push_str(&t);
            if !t.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    out
}

/// What a run says: the text for stdout, the notes for stderr (each printed as `nexo api: NOTE`), and the exit code.
/// The CLI prints them; the agent's `api` tool joins them.
pub struct Out {
    pub text: String,
    pub notes: Vec<String>,
    pub code: i32,
}

fn answer_out(a: &Answer, as_json: bool, reveal: bool) -> Out {
    let mut notes = Vec::new();
    if reveal {
        notes.push("--reveal: secrets in the answer are shown".to_string());
    }
    if a.cut > 0 {
        notes.push(format!("{} bytes of the answer were not read (cap {} MiB)", a.cut, READ_CAP >> 20));
    }
    Out { text: render_answer(a, as_json, reveal), notes, code: if a.status >= 400 { 1 } else { 0 } }
}

/// Prints what a run says and gives its exit code (an error prints as a note).
fn exit_of(r: Result<Out, Fail>) -> i32 {
    match r {
        Ok(o) => {
            print!("{}", o.text);
            for n in &o.notes {
                eprintln!("nexo api: {n}");
            }
            o.code
        }
        Err((code, msg)) => {
            eprintln!("nexo api: {msg}");
            code
        }
    }
}

/// `nexo api call`.
pub async fn call(t: &Target, dry_run: bool, as_json: bool, reveal: bool) -> i32 {
    exit_of(call_out(t, dry_run, as_json, reveal).await)
}

/// The method and the origin (`https://host`, no path, no query, no secret) of the endpoint a call names: what the
/// agent's `api` tool decides its permission by. Reads the map only.
pub fn target_info(map: &Path, endpoint: &str) -> Result<(String, String), Fail> {
    let api = load_map(map)?;
    let ep = call::find(&api, endpoint).map_err(usage)?;
    Ok((ep.method.clone(), ep.origin.clone()))
}

/// `nexo api call`, as text. An error carries its exit code.
pub async fn call_out(t: &Target, dry_run: bool, as_json: bool, reveal: bool) -> Result<Out, Fail> {
    let p = prepare(t)?;
    let ep = &p.api.endpoints[p.ep];
    if dry_run {
        let (mut vals, missing) = call::auth_values(&p.api, ep, &p.explicit, &BTreeMap::new(), &lookup);
        for n in call::needs(&p.api, ep) {
            if !vals.iter().any(|(i, _)| *i == n.index) {
                vals.push((n.index, String::new()));
            }
        }
        let req = call::build(&p.api, ep, &p.inputs, &vals).map_err(usage)?;
        let mut notes = Vec::new();
        if !missing.is_empty() && !t.login {
            notes.push(format!("not set: {} (a real call needs them)", missing.join(", ")));
        }
        if t.login {
            notes.push("with --login the token would come from the login call first".to_string());
        }
        return Ok(Out { text: req.render(), notes, code: 0 });
    }
    let mut s = session(&p, t)?;
    let ans = s.once(ep, &p.inputs).await?;
    if !t.select.is_empty() {
        let mut out = answer_out(&ans, as_json, reveal);
        out.text = crate::api_watch::select_text(&ans.text, &t.select, as_json, reveal).map_err(usage)?;
        if ans.status >= 400 {
            out.notes.push(format!("the answer was HTTP {}", ans.status));
        }
        return Ok(out);
    }
    let mut out = answer_out(&ans, as_json, reveal);
    crate::api_next::add(&mut out, &p.api, &ep.id, &t.map, &ans.text, ans.location.as_deref(), as_json);
    Ok(out)
}

/// `nexo api describe`.
pub fn describe(map: &Path, endpoint: Option<&str>, budget: usize, flows: bool) -> i32 {
    exit_of(describe_out(map, endpoint, budget, flows))
}

/// `nexo api describe`, as text. Without an endpoint the summary of the map takes about `budget` characters.
pub fn describe_out(map: &Path, endpoint: Option<&str>, budget: usize, flows: bool) -> Result<Out, Fail> {
    let api = load_map(map)?;
    if flows {
        return Ok(Out { text: nexo_api::links::flows(&api), notes: Vec::new(), code: 0 });
    }
    let mut text = String::new();
    match endpoint {
        Some(key) => {
            let ep = call::find(&api, key).map_err(usage)?;
            text.push_str(&call::describe(&api, ep));
        }
        None => {
            text.push_str(&summary::render(&api, budget));
            if !api.auth.is_empty() {
                text.push_str("variables `call` reads (--auth NAME=ENVVAR names another):\n");
                for l in call::labels(&api) {
                    text.push_str(&format!("  {l}: ${}\n", call::env_name(&l)));
                }
            }
        }
    }
    Ok(Out { text, notes: Vec::new(), code: 0 })
}

// ---------------------------------------------------------------- watch

/// The answer as it is compared and saved: the status and the body, secrets scrubbed. A body that is not JSON is
/// kept as its size and a fingerprint, so no text of it reaches the disk.
fn answer_value(a: &Answer) -> Value {
    let body = match serde_json::from_str::<Value>(&a.text) {
        Ok(mut v) => {
            exchange::scrub_json(&mut v);
            v
        }
        Err(_) => {
            let fp: String = exchange::fingerprint(&a.text).iter().map(|b| format!("{b:02x}")).collect();
            json!({ "text_bytes": a.text.len(), "text_fingerprint": fp })
        }
    };
    json!({ "status": a.status, "body": body })
}

/// What one tick prints: nothing when the answer is the same, the changes otherwise.
fn report(prev: Option<&Value>, now: &Value) -> String {
    match prev {
        None => format!("first answer: status {}\n", now["status"]),
        Some(old) => {
            let changes = diff::diff(old, now);
            if changes.is_empty() {
                String::new()
            } else {
                format!("# {} changes\n{}", changes.len(), diff::render_all(&changes, MAX_CHANGES))
            }
        }
    }
}

fn read_state(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Writes a file only the owner can read, through a temporary file, so a cron never reads half of it.
pub(crate) fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = format!("{}.tmp", path.display());
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(text.as_bytes())?;
    }
    std::fs::rename(&tmp, path)
}

/// Where a watch writes: straight to the terminal, or into buffers for the agent tool.
struct Sink {
    live: bool,
    text: String,
    notes: Vec<String>,
}

impl Sink {
    fn new(live: bool) -> Sink {
        Sink { live, text: String::new(), notes: Vec::new() }
    }

    fn out(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if self.live {
            print!("{s}");
            let _ = std::io::stdout().flush();
        } else {
            self.text.push_str(s);
        }
    }

    fn note(&mut self, s: &str) {
        if self.live {
            eprintln!("nexo api: {s}");
        } else {
            self.notes.push(s.to_string());
        }
    }
}

/// `nexo api watch`. `sel` is what `--select`, `--when` and `--on-change` asked for (or why they cannot run).
pub async fn watch(
    t: &Target,
    every: Option<&str>,
    times: Option<u32>,
    state: Option<&Path>,
    follow: Option<&str>,
    sel: Result<Option<crate::api_watch::Sel>, String>,
) -> i32 {
    let sel = match sel {
        Ok(s) => s,
        Err(e) => return exit_of(Err(usage(e))),
    };
    let mut sink = Sink::new(true);
    let r = watch_inner(t, every, times, state, follow, sel, false, &mut sink).await;
    exit_of(r.map(|code| Out { text: String::new(), notes: Vec::new(), code }))
}

/// `nexo api watch` as the agent's tool runs it: a bounded poll of a plain endpoint (never a channel), its text and
/// notes returned instead of printed. The caller has bounded `times` and `every` already.
pub async fn watch_out(
    t: &Target,
    every: Option<&str>,
    times: Option<u32>,
    state: Option<&Path>,
    sel: Option<crate::api_watch::Sel>,
) -> Result<Out, Fail> {
    let mut sink = Sink::new(false);
    let code = watch_inner(t, every, times, state, None, sel, true, &mut sink).await?;
    Ok(Out { text: sink.text, notes: sink.notes, code })
}

#[allow(clippy::too_many_arguments)]
async fn watch_inner(
    t: &Target,
    every: Option<&str>,
    times: Option<u32>,
    state: Option<&Path>,
    follow: Option<&str>,
    sel: Option<crate::api_watch::Sel>,
    tool: bool,
    sink: &mut Sink,
) -> Result<i32, Fail> {
    {
        let api = load_map(&t.map)?;
        let key = t.endpoint.trim();
        if let Some(c) = api.channels.iter().find(|c| c.id == key) {
            if tool {
                return Err(usage(
                    "a live channel cannot be watched from the tool: use `nexo api watch --for` yourself".to_string(),
                ));
            }
            return follow_channel(&api, c, t, follow.unwrap_or("60s"), sel.as_ref()).await;
        }
        if follow.is_some() {
            let ids: Vec<&str> = api.channels.iter().take(8).map(|c| c.id.as_str()).collect();
            let list = if ids.is_empty() {
                "the map has no channel".to_string()
            } else {
                format!("channels: {}", ids.join(", "))
            };
            return Err(usage(format!("--for follows a channel of the map, and {key:?} is not one ({list})")));
        }
    }
    let wait = match (every, times) {
        (Some(e), _) => {
            let d = call::duration(e).map_err(usage)?;
            if d < call::MIN_EVERY {
                return Err(usage(format!("--every {e} is too short: the least is {}s", call::MIN_EVERY.as_secs())));
            }
            d
        }
        (None, Some(1)) => Duration::ZERO,
        (None, _) => return Err(usage("--every DURATION is needed (or --times 1 for one call per run)".to_string())),
    };
    if times == Some(0) {
        return Err(usage("--times must be 1 or more".to_string()));
    }
    let p = prepare(t)?;
    let ep = &p.api.endpoints[p.ep];
    let mut s = session(&p, t)?;
    let mut prev = if sel.is_some() { None } else { state.and_then(read_state) };
    let mut prev_pick = sel.as_ref().and_then(|x| x.load());
    let mut n: u32 = 0;
    let mut last_failed = false;
    loop {
        n += 1;
        match s.once(ep, &p.inputs).await {
            Ok(ans) => {
                if let Some(sl) = &sel {
                    let doc = if ans.status >= 400 {
                        Err(format!("HTTP {}", ans.status))
                    } else {
                        serde_json::from_str::<Value>(&ans.text).map_err(|_| "the answer is not JSON".to_string())
                    };
                    match doc {
                        Err(why) => {
                            last_failed = true;
                            sink.note(&format!("call {n} failed: {why}"));
                        }
                        Ok(mut doc) => {
                            last_failed = false;
                            exchange::scrub_json(&mut doc);
                            let step = sl.step(&doc, prev_pick.as_ref());
                            sink.out(&step.text);
                            for x in &step.notes {
                                sink.note(x);
                            }
                            if let Err(e) = sl.save(&step.picked) {
                                sink.note(&e);
                            }
                            if let Some(a) = &step.alert {
                                sink.out(&format!("{}\n", a.line));
                                if let Some(cmd) = &sl.on_change {
                                    let (cmd, aj, pk) = (cmd.clone(), a.json.clone(), step.picked.clone());
                                    let r = tokio::task::spawn_blocking(move || {
                                        crate::api_watch::run_on_change(&cmd, &aj, &pk)
                                    })
                                    .await
                                    .unwrap_or_else(|e| format!("on-change: {e}"));
                                    sink.out(&format!("{r}\n"));
                                }
                                return Ok(crate::api_watch::EXIT_ALERT);
                            }
                            prev_pick = Some(step.picked);
                        }
                    }
                } else {
                    last_failed = false;
                    let now = answer_value(&ans);
                    sink.out(&report(prev.as_ref(), &now));
                    if let Some(path) = state {
                        let text = serde_json::to_string_pretty(&now).unwrap_or_default();
                        if let Err(e) = write_private(path, &text) {
                            sink.note(&format!("cannot write {}: {e}", path.display()));
                        }
                    }
                    prev = Some(now);
                }
            }
            Err((code, msg)) if code == EXIT_USAGE => return Err((code, msg)),
            Err((_, msg)) => {
                last_failed = true;
                sink.note(&format!("call {n} failed: {msg}"));
            }
        }
        if times.is_some_and(|m| n >= m) {
            break;
        }
        tokio::time::sleep(wait).await;
    }
    Ok(if last_failed { 1 } else { 0 })
}

// ---------------------------------------------------------------- live channels

/// How many bytes of a stream are read in one follow.
const FOLLOW_CAP: usize = 8 << 20;
/// How many difference lines a follow prints.
const MAX_FOLLOW_LINES: usize = 30;
/// Exit code of a follow that could not read the stream (exit 1 is only drift).
const EXIT_STREAM: i32 = 3;

pub(crate) fn broken(e: String) -> Fail {
    (EXIT_STREAM, e)
}

/// A client for a long answer: no total timeout, the follow keeps its own clock.
fn stream_client() -> Result<reqwest::Client, Fail> {
    nexo_providers::build_client(|| {
        reqwest::Client::builder()
            .connect_timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("nexo/", env!("CARGO_PKG_VERSION")))
    })
    .map_err(broken)
}

/// `nexo api watch MAP CHANNEL --for 60s`: follows one channel of the map for a while and says which message types
/// came, how often, and what differs from the map. An event stream is followed with a plain streaming GET; a
/// WebSocket by `api_ws`, with the same auth, report and exit code.
async fn follow_channel(
    api: &Api,
    c: &nexo_api::channel::Channel,
    t: &Target,
    window: &str,
    sel: Option<&crate::api_watch::Sel>,
) -> Result<i32, Fail> {
    use nexo_api::channel::{self, Kind};
    let span = call::duration(window).map_err(usage)?;
    if span.is_zero() || span > Duration::from_secs(3600) {
        return Err(usage(format!("--for {window} must be between 1s and 1h")));
    }
    let ep = Endpoint {
        id: c.id.clone(),
        method: "GET".to_string(),
        origin: c.origin.clone(),
        path: c.path.clone(),
        gql: None,
        calls: 0,
        path_params: Vec::new(),
        query: Vec::new(),
        headers: Vec::new(),
        body: None,
        responses: Default::default(),
        paging: None,
        auth: c.auth.clone(),
        example: String::new(),
    };
    if c.path.contains('{') {
        return Err(usage(format!(
            "{} has a path with parameters ({}); this follow cannot fill them in",
            c.id, c.path
        )));
    }
    let explicit = call::auth_map(api, &t.auth).map_err(usage)?;
    let (vals, missing) = call::auth_values(api, &ep, &explicit, &BTreeMap::new(), &lookup);
    if !missing.is_empty() {
        return Err(usage(format!(
            "set {} (or name another variable with --auth NAME=ENVVAR); `nexo api describe` lists them",
            missing.join(", ")
        )));
    }
    let mut req = call::build(api, &ep, &call::Inputs::default(), &vals).map_err(usage)?;
    let known: std::collections::HashSet<exchange::Fp> = vals.iter().map(|(_, v)| exchange::fingerprint(v)).collect();
    if c.kind == Kind::Ws {
        return crate::api_ws::follow(api, c, &req, &vals, &known, span, sel).await;
    }
    req.headers.push(("accept".to_string(), call::Val::plain("text/event-stream")));
    let client = stream_client()?;
    if sel.is_none() {
        println!("following {} (SSE {}{}) for {}s", c.id, c.origin, c.path, span.as_secs());
    }
    let started = std::time::Instant::now();
    let mut resp = match tokio::time::timeout(span, open(&client, &req)).await {
        Ok(r) => r.map_err(broken)?,
        Err(_) => return Err(broken("the server did not answer within the --for window".to_string())),
    };
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(broken(format!("the stream answered {status}")));
    }
    let ctype = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !ctype.to_ascii_lowercase().starts_with("text/event-stream") {
        let shown: String = ctype.chars().take(60).collect();
        return Err(broken(format!("the answer is {shown:?}, not an event stream (text/event-stream)")));
    }
    let mut parser = channel::SseParser::new();
    let mut msgs: Vec<channel::Msg> = Vec::new();
    let mut read = 0usize;
    let mut ended = false;
    loop {
        let left = span.saturating_sub(started.elapsed());
        if left.is_zero() || read >= FOLLOW_CAP || msgs.len() >= channel::MAX_FRAMES {
            break;
        }
        match tokio::time::timeout(left, resp.chunk()).await {
            Err(_) => break,
            Ok(Err(e)) => return Err(broken(net_error(e))),
            Ok(Ok(None)) => {
                ended = true;
                break;
            }
            Ok(Ok(Some(chunk))) => {
                read += chunk.len();
                msgs.extend(parser.feed(&String::from_utf8_lossy(&chunk)));
            }
        }
    }
    msgs.extend(parser.finish());
    msgs.truncate(channel::MAX_FRAMES);
    for m in &mut msgs {
        m.text = channel::redact_text(&m.text, &known);
    }
    let secs = started.elapsed().as_secs_f64();
    let live = channel::observe(Kind::Sse, &c.origin, &c.path, &msgs, Some(secs));
    if let Some(sel) = sel {
        let (text, code) = crate::api_watch::channel_report(sel, &msgs);
        print!("{text}");
        let _ = std::io::stdout().flush();
        return Ok(code);
    }
    print!("{}", follow_report(c, &live, secs, ended));
    let _ = std::io::stdout().flush();
    let drift = nexo_api::drift::channel(c, &live).iter().any(|f| f.drift);
    Ok(if drift { 1 } else { 0 })
}

/// What a follow prints: the messages seen, one line per type, then what differs from the recorded channel.
pub(crate) fn follow_report(
    rec: &nexo_api::channel::Channel,
    live: &nexo_api::channel::Channel,
    secs: f64,
    ended: bool,
) -> String {
    use nexo_api::channel::rate_text;
    let mut out = format!(
        "{} messages in {:.0}s{}\n",
        live.messages,
        secs,
        if ended { " (the server closed the stream)" } else { "" }
    );
    for t in live.types.iter().take(20) {
        let rate = live.per_min(t.total()).map(|r| format!(" [{}]", rate_text(r))).unwrap_or_default();
        out.push_str(&format!("  {} x{}{}\n", t.name, t.total(), rate));
    }
    if live.types.len() > 20 {
        out.push_str(&format!("  ... {} more types\n", live.types.len() - 20));
    }
    if let Some(b) = &live.beat {
        out.push_str(&format!("  heartbeat: {} x{}\n", b.name, b.got + b.sent));
    }
    let found = nexo_api::drift::channel(rec, live);
    if found.is_empty() {
        out.push_str("same message types and shapes as the map\n");
    }
    for f in found.iter().take(MAX_FOLLOW_LINES) {
        out.push_str(&format!("{} {}\n", if f.drift { "drift:" } else { "new:  " }, f.text));
    }
    if found.len() > MAX_FOLLOW_LINES {
        out.push_str(&format!("... {} more differences\n", found.len() - MAX_FOLLOW_LINES));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(status: u16, text: &str) -> Answer {
        Answer { status, content_type: "application/json".to_string(), location: None, text: text.to_string(), cut: 0 }
    }

    #[test]
    fn the_saved_answer_holds_no_token() {
        let a = answer(200, r#"{"access_token":"tkAAAA1111bbbbCCCC","items":[{"id":1,"echo":"tkAAAA1111bbbbCCCC"}]}"#);
        let v = answer_value(&a);
        let text = v.to_string();
        assert!(!text.contains("tkAAAA1111bbbbCCCC"), "{text}");
        assert_eq!(v["body"]["access_token"], "<secret>");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_private(&path, &serde_json::to_string_pretty(&v).unwrap()).unwrap();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(!on_disk.contains("tkAAAA1111bbbbCCCC") && on_disk.contains("<secret>"), "{on_disk}");
        assert_eq!(read_state(&path), Some(v));
        assert!(!std::path::Path::new(&format!("{}.tmp", path.display())).exists());
    }

    #[test]
    fn a_text_answer_is_saved_as_a_fingerprint_not_as_text() {
        let v = answer_value(&answer(200, "hello secret-ish text"));
        assert!(!v.to_string().contains("secret-ish"), "{v}");
        assert_eq!(v["body"]["text_bytes"], 21);
    }

    #[test]
    fn a_changed_token_is_not_a_change_but_a_changed_field_is() {
        let old = answer_value(&answer(200, r#"{"access_token":"aaaaaaaa1111","count":1}"#));
        let same = answer_value(&answer(200, r#"{"access_token":"bbbbbbbb2222","count":1}"#));
        assert_eq!(report(Some(&old), &same), "");
        let new = answer_value(&answer(200, r#"{"access_token":"cccccccc3333","count":2}"#));
        let r = report(Some(&old), &new);
        assert!(r.contains("~ $.body.count: 1 -> 2") && !r.contains("cccccccc3333"), "{r}");
        assert!(report(None, &new).starts_with("first answer: status 200"));
        let gone = answer_value(&answer(500, "{}"));
        assert!(report(Some(&new), &gone).contains("~ $.status: 200 -> 500"));
    }

    #[test]
    fn the_printed_answer_is_scrubbed_unless_revealed() {
        let a = Answer {
            status: 302,
            content_type: "application/json".to_string(),
            location: Some("https://x.test/cb?code=abcdef123456&page=2#access_token=zzzz".to_string()),
            text: r#"{"access_token":"tkAAAA1111bbbbCCCC","n":1}"#.to_string(),
            cut: 0,
        };
        for as_json in [false, true] {
            let safe = render_answer(&a, as_json, false);
            assert!(
                !safe.contains("tkAAAA1111bbbbCCCC") && !safe.contains("abcdef123456") && !safe.contains("zzzz"),
                "{safe}"
            );
            assert!(safe.contains("<secret>") && safe.contains("page=2"), "{safe}");
            let shown = render_answer(&a, as_json, true);
            assert!(shown.contains("tkAAAA1111bbbbCCCC") && shown.contains("abcdef123456"), "{shown}");
        }
        let t = scrub_text("hello AAAAbbbbCCCCddddEEEEffffGGGG1234 world");
        assert_eq!(t, "hello <secret> world");
    }

    #[test]
    fn a_json_argument_is_text_or_a_file() {
        assert_eq!(json_arg(r#"{"a":1}"#).unwrap(), json!({"a": 1}));
        assert!(json_arg("{nope").is_err());
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("b.json");
        std::fs::write(&f, r#"[1,2]"#).unwrap();
        assert_eq!(json_arg(&format!("@{}", f.display())).unwrap(), json!([1, 2]));
        assert!(json_arg("@/no/such/file.json").is_err());
    }
}
