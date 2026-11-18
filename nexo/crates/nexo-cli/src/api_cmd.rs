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
    },
    /// Call an endpoint on a schedule and print only what changed since the last answer.
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
    },
}

/// Which endpoint to call and with what.
#[derive(Args, Debug)]
pub struct Target {
    /// The map file written by `nexo api analyze --save`.
    pub map: PathBuf,
    /// An endpoint id, or "METHOD /path".
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
        ApiCmd::Describe { map, endpoint } => crate::api_call::describe(map, endpoint.as_deref()),
        ApiCmd::Watch { target, every, times, state } => {
            crate::api_call::watch(target, every.as_deref(), *times, state.as_deref()).await
        }
        ApiCmd::Analyze { har, host, openapi, json, budget, save } => {
            let text = match std::fs::read_to_string(har) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("nexo api: cannot read {}: {e}", har.display());
                    return crate::headless::EXIT_USAGE;
                }
            };
            let opts = nexo_api::spec::Options { hosts: host.clone() };
            let api = match nexo_api::analyze_har(&text, &opts) {
                Ok(api) => api,
                Err(e) => {
                    eprintln!("nexo api: {e}");
                    return crate::headless::EXIT_USAGE;
                }
            };
            let doc = if *json || openapi.is_some() {
                let v = nexo_api::openapi::to_openapi(&api, &title(&api));
                Some(serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string()))
            } else {
                None
            };
            if let (Some(path), Some(doc)) = (openapi, &doc) {
                if let Err(e) = std::fs::write(path, format!("{doc}\n")) {
                    eprintln!("nexo api: cannot write {}: {e}", path.display());
                    return crate::headless::EXIT_USAGE;
                }
            }
            if let Some(path) = save {
                if let Err(e) = std::fs::write(path, nexo_api::store::to_text(&api)) {
                    eprintln!("nexo api: cannot write {}: {e}", path.display());
                    return crate::headless::EXIT_USAGE;
                }
            }
            match (*json, &doc) {
                (true, Some(doc)) => println!("{doc}"),
                _ => print!("{}", nexo_api::summary::render(&api, *budget)),
            }
            0
        }
    }
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
            Some(crate::cli::Cmd::Api(ApiArgs { what: ApiCmd::Watch { target, every, times, state } })) => {
                assert_eq!(target.endpoint, "GET /api/items");
                assert_eq!(every.as_deref(), Some("30s"));
                assert_eq!(times, Some(3));
                assert_eq!(state, Some(PathBuf::from("s.json")));
            }
            other => panic!("not parsed as nexo api watch: {other:?}"),
        }
    }
}
