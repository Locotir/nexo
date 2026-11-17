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
    },
}

/// The name of the API in the OpenAPI title: its first host.
fn title(api: &nexo_api::spec::Api) -> String {
    match api.hosts.first() {
        Some(h) => format!("{} API", h.split("://").nth(1).unwrap_or(h)),
        None => "API".to_string(),
    }
}

pub fn run(a: &ApiArgs) -> i32 {
    match &a.what {
        ApiCmd::Analyze { har, host, openapi, json, budget } => {
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
            Some(crate::cli::Cmd::Api(ApiArgs { what: ApiCmd::Analyze { har, host, json, openapi, budget } })) => {
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
}
