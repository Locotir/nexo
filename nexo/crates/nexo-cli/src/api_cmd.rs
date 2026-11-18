//! `nexo api`: turns the traffic of a website (a HAR file saved from the browser's network panel) into the API behind
//! it: endpoints, shapes, auth, paging, OpenAPI. It prints a short summary, or the OpenAPI JSON with `--json`. No
//! secret value is read into memory past the capture, and none is printed.

use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct ApiArgs {
    #[command(subcommand)]
    pub what: ApiCmd,
}

#[derive(Subcommand, Debug)]
pub enum ApiCmd {
    /// Read a HAR file and print the API behind it: endpoints, shapes, auth, paging.
    Analyze {
        /// The HAR file (Chrome and Edge: Network panel, "Save all as HAR").
        har: PathBuf,
        /// Keep only this host and the hosts under it (repeat for more).
        #[arg(long = "host", value_name = "HOST")]
        host: Vec<String>,
        /// Also write the OpenAPI 3.1 description to this file.
        #[arg(long, value_name = "FILE")]
        openapi: Option<PathBuf>,
        /// Print the OpenAPI JSON instead of the summary.
        #[arg(long)]
        json: bool,
        /// About how many characters the summary may take.
        #[arg(long, default_value_t = 6000, value_name = "N")]
        budget: usize,
        /// Also write the map (endpoints, shapes, auth, no secret values) to this file, for `call`, `describe`, `watch`.
        #[arg(long, value_name = "FILE")]
        save: Option<PathBuf>,
    },
    /// Call one endpoint of a saved map. Secrets come only from environment variables.
    Call {
        #[command(flatten)]
        target: Target,
        /// Print the request with `<secret>` for each auth value, and send nothing.
        #[arg(long)]
        dry_run: bool,
        /// Print the answer as JSON: status, content type, body.
        #[arg(long)]
        json: bool,
        /// Print the answer as it came, secrets included (by default they show as `<secret>`).
        #[arg(long)]
        reveal: bool,
    },
    /// List the endpoints of a saved map, or one endpoint's params, body, auth and variable names.
    Describe {
        /// The map file written by `nexo api analyze --save`.
        map: PathBuf,
        /// An endpoint id, or "METHOD /path".
        endpoint: Option<String>,
        /// About how many characters the summary of the whole map may take (ignored with an endpoint).
        #[arg(long, default_value_t = 6000, value_name = "N")]
        budget: usize,
        /// Print the chains instead: login to token to calls, list to item, page to next page, where each value comes from.
        #[arg(long)]
        flows: bool,
    },
    /// Write a small client for a mapped API in TypeScript, Python or shell, with no dependency and no nexo needed.
    ///
    /// One function per endpoint (named as `nexo api call` names it), plus a generator that follows the paging in
    /// TypeScript and Python. Secrets are read from environment variables when a call runs (the ones `nexo api call`
    /// suggests, such as NEXO_API_BEARER) and are never written into the file. A session cookie is sent only when its
    /// variable is set. It prints to standard output unless --out is given, and never replaces a file without --force.
    Client {
        /// The map file written by `nexo api analyze --save`.
        map: PathBuf,
        /// The language: ts (Node 18+), py (Python 3.9+) or sh (POSIX sh with curl).
        #[arg(long, value_name = "ts|py|sh")]
        lang: String,
        /// Write the client to this file instead of printing it.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Only these endpoints (an id, or "METHOD /path"). Repeat for more; the default is all of them.
        #[arg(long, value_name = "EP")]
        only: Vec<String>,
        /// Replace the file --out names when it exists.
        #[arg(long)]
        force: bool,
        /// The date written in the header (YYYY-MM-DD); the default is today.
        #[arg(long, value_name = "DATE")]
        date: Option<String>,
    },
    /// Call the safe endpoints of a saved map and say whether each live answer still has the recorded shape.
    ///
    /// Calls every GET/HEAD endpoint that needs no parameter you did not give with -p (the others are listed as
    /// skipped), or only the endpoints you name (an id, or "METHOD /path"). Other methods are never called unless
    /// --unsafe is given; a login is never run. Per endpoint it prints the status (live against recorded), the
    /// content type and the shape: a field that is gone, a changed type, a value that can be null now, a changed
    /// status or a failed auth is drift or broken; a new field is only information. No secret value is printed.
    ///
    /// Exit code: 0 all ok (or skipped), 1 some drift, 2 some endpoint broken, 3 the map or the arguments are
    /// unusable.
    Check {
        /// The map file written by `nexo api analyze --save`.
        map: PathBuf,
        /// Only these endpoints: an id, or "METHOD /path". Default: every safe one.
        endpoints: Vec<String>,
        /// A param given to each endpoint that has it: name=value (or name=@env:VAR). Repeat for more.
        #[arg(short = 'p', long = "param", value_name = "NAME=VALUE")]
        params: Vec<String>,
        /// Read an auth scheme's value from this environment variable. Repeat for more.
        #[arg(long, value_name = "NAME=ENVVAR")]
        auth: Vec<String>,
        /// Also call endpoints that are not GET or HEAD (they may change data).
        #[arg(long = "unsafe")]
        allow_unsafe: bool,
        /// Print the result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Call an endpoint on a schedule and print only what changed since the last answer.
    ///
    /// Exit code: 0; 1 if the last call failed; 2 for bad arguments; 3 if a stream broke; 4 if --when came true (or,
    /// without --when, the selected values changed) and the watch stopped on that alert.
    Watch {
        #[command(flatten)]
        target: Target,
        /// Time between calls, like 30s, 5m, 1h (at least 10s).
        #[arg(long, value_name = "DURATION")]
        every: Option<String>,
        /// Stop after this many calls.
        #[arg(long, value_name = "N")]
        times: Option<u32>,
        /// Keep the last answer here between runs (secrets scrubbed before it is saved).
        #[arg(long, value_name = "FILE")]
        state: Option<PathBuf>,
        /// Follow a live channel of the map (an event stream) for this long, like 60s, and say which message types
        /// came and what differs from the map. The endpoint argument is then the channel's id.
        #[arg(long = "for", value_name = "DURATION")]
        follow: Option<String>,
        /// Alert when this is true of the selected values: `PATH OP VALUE` (OP is < <= > >= == !=, VALUE a number, a
        /// quoted string or `prev`, the last poll's value) or `PATH changed`; join clauses with `and` / `or`, read left
        /// to right. Prints one `alert:` line and exits 4. Without it, a change in the selected values is the alert.
        #[arg(long, value_name = "EXPR")]
        when: Option<String>,
        /// Run this shell command on an alert. The values reach it only as the environment variables NEXO_API_ALERT
        /// (JSON) and NEXO_API_PATHS (JSON), and on stdin; it is killed after 60s and its exit code is printed.
        #[arg(long = "on-change", value_name = "CMD")]
        on_change: Option<String>,
    },
}

/// Which endpoint to call and with what.
#[derive(Args, Debug)]
pub struct Target {
    /// The map file written by `nexo api analyze --save`.
    pub map: PathBuf,
    /// An endpoint id, or "METHOD /path" (for `watch --for`, a channel id).
    pub endpoint: String,
    /// A param: name=value (or name=@env:VAR to read it from the environment). Repeat for more.
    #[arg(short = 'p', long = "param", value_name = "NAME=VALUE")]
    pub params: Vec<String>,
    /// The whole body: JSON text, or @file.
    #[arg(long, value_name = "JSON|@FILE")]
    pub body: Option<String>,
    /// Read an auth scheme's value from this environment variable. Repeat for more.
    #[arg(long, value_name = "NAME=ENVVAR")]
    pub auth: Vec<String>,
    /// Run the login endpoint the map records first and use the token it returns (kept in memory only).
    #[arg(long)]
    pub login: bool,
    /// A param of the login call: name=value (or name=@env:VAR).
    #[arg(long = "login-param", value_name = "NAME=VALUE")]
    pub login_params: Vec<String>,
    /// The whole body of the login call: JSON text, or @file.
    #[arg(long = "login-body", value_name = "JSON|@FILE")]
    pub login_body: Option<String>,
    /// Send the call through your own signed-in browser (the nexo extension, you click Allow): the page on the API's
    /// origin does the fetch with your session, and no cookie or token leaves the browser. `--via-browser=ID` picks
    /// one of several browsers.
    #[arg(long = "via-browser", value_name = "ID", num_args = 0..=1, require_equals = true, default_missing_value = "")]
    pub via_browser: Option<String>,
    /// Print only these values of a JSON answer, as `path<TAB>value` lines: $.a.b, $.items[0].price, $.items[*].id,
    /// $['odd key']. Separate with commas or repeat. A path that matches nothing prints `(none)`. On `watch` only the
    /// selected values are compared and kept.
    #[arg(long, value_name = "PATH[,PATH...]")]
    pub select: Vec<String>,
}

/// The name of the API in the OpenAPI title: its first host.
fn title(api: &nexo_api::spec::Api) -> String {
    match api.hosts.first() {
        Some(h) => format!("{} API", h.split("://").nth(1).unwrap_or(h)),
        None => "API".to_string(),
    }
}

pub async fn run(a: &ApiArgs) -> i32 {
    match &a.what {
        ApiCmd::Call { target, dry_run, json, reveal } => crate::api_call::call(target, *dry_run, *json, *reveal).await,
        ApiCmd::Describe { map, endpoint, budget, flows } => {
            crate::api_call::describe(map, endpoint.as_deref(), *budget, *flows)
        }
        ApiCmd::Client { map, lang, out, only, force, date } => {
            match client_text(map, lang, out.as_deref(), only, *force, date.as_deref()) {
                Ok(text) => {
                    print!("{text}");
                    0
                }
                Err(e) => {
                    eprintln!("nexo api: {e}");
                    crate::headless::EXIT_USAGE
                }
            }
        }
        ApiCmd::Check { map, endpoints, params, auth, allow_unsafe, json } => {
            crate::api_check::check(map, endpoints, params, auth, *allow_unsafe, *json).await
        }
        ApiCmd::Watch { target, every, times, state, follow, when, on_change } => {
            let sel =
                crate::api_watch::Sel::build(&target.select, when.as_deref(), on_change.as_deref(), state.as_deref());
            crate::api_call::watch(target, every.as_deref(), *times, state.as_deref(), follow.as_deref(), sel).await
        }
        ApiCmd::Analyze { har, host, openapi, json, budget, save } => {
            match analyze_text(har, host, openapi.as_deref(), *json, *budget, save.as_deref()) {
                Ok(text) => {
                    print!("{text}");
                    0
                }
                Err(e) => {
                    eprintln!("nexo api: {e}");
                    crate::headless::EXIT_USAGE
                }
            }
        }
    }
}

/// What `client` prints: the client itself, or a line that says where it was written. It refuses to replace a file
/// unless `force`. The agent's `api` tool calls this too.
pub fn client_text(
    map: &std::path::Path,
    lang: &str,
    out: Option<&std::path::Path>,
    only: &[String],
    force: bool,
    date: Option<&str>,
) -> Result<String, String> {
    let lang = nexo_api::client::Lang::parse(lang)?;
    let text = std::fs::read_to_string(map).map_err(|e| format!("cannot read {}: {e}", map.display()))?;
    let api = nexo_api::store::from_text(&text).map_err(|e| format!("{}: {e}", map.display()))?;
    let today;
    let date = match date {
        Some(d) => d,
        None => {
            today = nexo_core::clock::today();
            today.as_str()
        }
    };
    let code = nexo_api::client::render(&api, lang, only, date)?;
    let Some(path) = out else { return Ok(code) };
    if path.exists() && !force {
        return Err(format!("{} exists; use --force to replace it", path.display()));
    }
    std::fs::write(path, &code).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(format!("wrote {} ({} bytes, {})\n", path.display(), code.len(), lang.ext()))
}

/// What `analyze` prints, after it has written the files it was asked for (`openapi`, `save`). The agent's `api`
/// tool calls this too, so both print the same text.
pub fn analyze_text(
    har: &std::path::Path,
    host: &[String],
    openapi: Option<&std::path::Path>,
    json: bool,
    budget: usize,
    save: Option<&std::path::Path>,
) -> Result<String, String> {
    let text = std::fs::read_to_string(har).map_err(|e| format!("cannot read {}: {e}", har.display()))?;
    let opts = nexo_api::spec::Options { hosts: host.to_vec() };
    let api = nexo_api::analyze_har(&text, &opts)?;
    let doc = if json || openapi.is_some() {
        let v = nexo_api::openapi::to_openapi(&api, &title(&api));
        Some(serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string()))
    } else {
        None
    };
    if let (Some(path), Some(doc)) = (openapi, &doc) {
        std::fs::write(path, format!("{doc}\n")).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    if let Some(path) = save {
        std::fs::write(path, nexo_api::store::to_text(&api))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    Ok(match (json, &doc) {
        (true, Some(doc)) => format!("{doc}\n"),
        _ => nexo_api::summary::render(&api, budget),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn analyze_takes_a_file_repeated_hosts_and_json() {
        let parsed = crate::cli::Args::try_parse_from([
            "nexo", "api", "analyze", "x.har", "--host", "a", "--host", "b", "--json",
        ])
        .unwrap();
        match parsed.cmd {
            Some(crate::cli::Cmd::Api(ApiArgs { what: ApiCmd::Analyze { har, host, json, openapi, budget, .. } })) => {
                assert_eq!(har, PathBuf::from("x.har"));
                assert_eq!(host, vec!["a".to_string(), "b".to_string()]);
                assert!(json);
                assert!(openapi.is_none());
                assert_eq!(budget, 6000);
            }
            other => panic!("not parsed as nexo api analyze: {other:?}"),
        }
    }

    #[test]
    fn analyze_takes_openapi_and_budget() {
        let parsed = crate::cli::Args::try_parse_from([
            "nexo",
            "api",
            "analyze",
            "x.har",
            "--openapi",
            "o.json",
            "--budget",
            "900",
        ])
        .unwrap();
        match parsed.cmd {
            Some(crate::cli::Cmd::Api(ApiArgs { what: ApiCmd::Analyze { openapi, budget, json, .. } })) => {
                assert_eq!(openapi, Some(PathBuf::from("o.json")));
                assert_eq!(budget, 900);
                assert!(!json);
            }
            other => panic!("not parsed as nexo api analyze: {other:?}"),
        }
    }

    #[test]
    fn analyze_takes_save() {
        let parsed = crate::cli::Args::try_parse_from(["nexo", "api", "analyze", "x.har", "--save", "m.json"]).unwrap();
        match parsed.cmd {
            Some(crate::cli::Cmd::Api(ApiArgs { what: ApiCmd::Analyze { save, .. } })) => {
                assert_eq!(save, Some(PathBuf::from("m.json")));
            }
            other => panic!("not parsed as nexo api analyze: {other:?}"),
        }
    }

    #[test]
    fn call_takes_params_auth_body_and_dry_run() {
        let parsed = crate::cli::Args::try_parse_from([
            "nexo",
            "api",
            "call",
            "m.json",
            "get_api_items",
            "-p",
            "id=3",
            "-p",
            "query.q=a b",
            "--auth",
            "bearer=MY_TOKEN",
            "--body",
            "{\"a\":1}",
            "--dry-run",
            "--login",
            "--login-param",
            "user=me",
        ])
        .unwrap();
        match parsed.cmd {
            Some(crate::cli::Cmd::Api(ApiArgs { what: ApiCmd::Call { target, dry_run, json, .. } })) => {
                assert_eq!(target.map, PathBuf::from("m.json"));
                assert_eq!(target.endpoint, "get_api_items");
                assert_eq!(target.params, vec!["id=3".to_string(), "query.q=a b".to_string()]);
                assert_eq!(target.auth, vec!["bearer=MY_TOKEN".to_string()]);
                assert_eq!(target.body.as_deref(), Some("{\"a\":1}"));
                assert!(dry_run && target.login && !json);
                assert_eq!(target.login_params, vec!["user=me".to_string()]);
            }
            other => panic!("not parsed as nexo api call: {other:?}"),
        }
    }

    #[test]
    fn client_takes_a_language_and_a_file() {
        let d = crate::cli::Args::try_parse_from([
            "nexo", "api", "client", "m.json", "--lang", "py", "--out", "c.py", "--only", "a", "--only", "b", "--force",
        ])
        .unwrap();
        match d.cmd {
            Some(crate::cli::Cmd::Api(ApiArgs { what: ApiCmd::Client { lang, out, only, force, date, .. } })) => {
                assert_eq!(lang, "py");
                assert_eq!(out.as_deref(), Some(std::path::Path::new("c.py")));
                assert_eq!(only, vec!["a".to_string(), "b".to_string()]);
                assert!(force && date.is_none());
            }
            other => panic!("not parsed as nexo api client: {other:?}"),
        }
        assert!(crate::cli::Args::try_parse_from(["nexo", "api", "client", "m.json"]).is_err());
    }

    #[test]
    fn client_refuses_to_replace_a_file_without_force() {
        let dir = std::env::temp_dir().join(format!("nexo-client-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let map = dir.join("map.json");
        let har = serde_json::json!({ "log": { "entries": [{
            "_resourceType": "xhr",
            "request": { "method": "GET", "url": "https://app.test/api/ping", "headers": [] },
            "response": { "status": 200, "headers": [], "content": { "mimeType": "application/json", "text": "{\"ok\":true}" } },
        }] } });
        let har_path = dir.join("a.har");
        std::fs::write(&har_path, har.to_string()).unwrap();
        analyze_text(&har_path, &[], None, false, 500, Some(&map)).unwrap();
        let out = dir.join("c.sh");
        let first = client_text(&map, "sh", Some(&out), &[], false, Some("2026-11-18")).unwrap();
        assert!(first.starts_with("wrote "), "{first}");
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("on 2026-11-18."));
        let again = client_text(&map, "sh", Some(&out), &[], false, None);
        assert!(again.unwrap_err().contains("--force"));
        assert!(client_text(&map, "sh", Some(&out), &[], true, None).is_ok());
        assert!(client_text(&map, "go", None, &[], false, None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn describe_takes_a_budget_like_analyze() {
        let d = crate::cli::Args::try_parse_from(["nexo", "api", "describe", "m.json", "--budget", "900"]).unwrap();
        match d.cmd {
            Some(crate::cli::Cmd::Api(ApiArgs { what: ApiCmd::Describe { budget, .. } })) => assert_eq!(budget, 900),
            other => panic!("not parsed as nexo api describe: {other:?}"),
        }
        let d = crate::cli::Args::try_parse_from(["nexo", "api", "describe", "m.json"]).unwrap();
        match d.cmd {
            Some(crate::cli::Cmd::Api(ApiArgs { what: ApiCmd::Describe { budget, .. } })) => assert_eq!(budget, 6000),
            other => panic!("not parsed as nexo api describe: {other:?}"),
        }
    }

    #[test]
    fn describe_and_watch_parse() {
        let d = crate::cli::Args::try_parse_from(["nexo", "api", "describe", "m.json"]).unwrap();
        assert!(matches!(d.cmd, Some(crate::cli::Cmd::Api(ApiArgs { what: ApiCmd::Describe { endpoint: None, .. } }))));
        let w = crate::cli::Args::try_parse_from([
            "nexo",
            "api",
            "watch",
            "m.json",
            "GET /api/items",
            "--every",
            "30s",
            "--times",
            "3",
            "--state",
            "s.json",
        ])
        .unwrap();
        match w.cmd {
            Some(crate::cli::Cmd::Api(ApiArgs { what: ApiCmd::Watch { target, every, times, state, .. } })) => {
                assert_eq!(target.endpoint, "GET /api/items");
                assert_eq!(every.as_deref(), Some("30s"));
                assert_eq!(times, Some(3));
                assert_eq!(state, Some(PathBuf::from("s.json")));
            }
            other => panic!("not parsed as nexo api watch: {other:?}"),
        }
    }

    #[test]
    fn select_when_and_on_change_are_parsed() {
        let w = crate::cli::Args::try_parse_from([
            "nexo",
            "api",
            "watch",
            "m.json",
            "GET /p",
            "--select",
            "$.price,$.name",
            "--select",
            "$.a[*]",
            "--when",
            "$.price < prev",
            "--on-change",
            "echo hi",
            "--times",
            "2",
        ])
        .unwrap();
        match w.cmd {
            Some(crate::cli::Cmd::Api(ApiArgs { what: ApiCmd::Watch { target, when, on_change, .. } })) => {
                assert_eq!(target.select, ["$.price,$.name", "$.a[*]"]);
                assert_eq!(when.as_deref(), Some("$.price < prev"));
                assert_eq!(on_change.as_deref(), Some("echo hi"));
            }
            other => panic!("not parsed as nexo api watch: {other:?}"),
        }
        let c =
            crate::cli::Args::try_parse_from(["nexo", "api", "call", "m.json", "GET /p", "--select", "$.x"]).unwrap();
        match c.cmd {
            Some(crate::cli::Cmd::Api(ApiArgs { what: ApiCmd::Call { target, .. } })) => {
                assert_eq!(target.select, ["$.x"])
            }
            other => panic!("not parsed as nexo api call: {other:?}"),
        }
    }

    #[test]
    fn watch_takes_for_to_follow_a_channel() {
        let w =
            crate::cli::Args::try_parse_from(["nexo", "api", "watch", "m.json", "sse_events", "--for", "60s"]).unwrap();
        match w.cmd {
            Some(crate::cli::Cmd::Api(ApiArgs { what: ApiCmd::Watch { target, follow, every, .. } })) => {
                assert_eq!(target.endpoint, "sse_events");
                assert_eq!(follow.as_deref(), Some("60s"));
                assert_eq!(every, None);
            }
            other => panic!("not parsed as nexo api watch: {other:?}"),
        }
    }

    #[test]
    fn check_takes_endpoints_params_auth_unsafe_and_json() {
        let c = crate::cli::Args::try_parse_from([
            "nexo",
            "api",
            "check",
            "m.json",
            "get_api_items",
            "GET /api/items/{item_id}",
            "-p",
            "item_id=3",
            "--auth",
            "bearer=MY_TOKEN",
            "--unsafe",
            "--json",
        ])
        .unwrap();
        match c.cmd {
            Some(crate::cli::Cmd::Api(ApiArgs {
                what: ApiCmd::Check { map, endpoints, params, auth, allow_unsafe, json },
            })) => {
                assert_eq!(map, PathBuf::from("m.json"));
                assert_eq!(endpoints, vec!["get_api_items".to_string(), "GET /api/items/{item_id}".to_string()]);
                assert_eq!(params, vec!["item_id=3".to_string()]);
                assert_eq!(auth, vec!["bearer=MY_TOKEN".to_string()]);
                assert!(allow_unsafe && json);
            }
            other => panic!("not parsed as nexo api check: {other:?}"),
        }
        let bare = crate::cli::Args::try_parse_from(["nexo", "api", "check", "m.json"]).unwrap();
        match bare.cmd {
            Some(crate::cli::Cmd::Api(ApiArgs { what: ApiCmd::Check { endpoints, allow_unsafe, json, .. } })) => {
                assert!(endpoints.is_empty() && !allow_unsafe && !json);
            }
            other => panic!("not parsed as nexo api check: {other:?}"),
        }
        assert!(crate::cli::Args::try_parse_from(["nexo", "api", "check"]).is_err());
    }
}
