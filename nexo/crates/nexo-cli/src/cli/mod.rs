//! The command line.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use nexo_core::agent::{channels, AgentCommand, AgentEvent};
use nexo_core::permission::PermissionMode;

use crate::setup::{self, Picked};

mod args;
mod config;
mod dispatch;
mod modes;

pub use args::*;
pub(crate) use config::{apply_audit_flag, load_config_for, point_to_the_files};
use config::{load_config, trust_asked, way_out};
use dispatch::run_in;
use modes::{headless_collect, interactive};

/// Libraries that handle requests with their auth headers and log them at `trace` (tungstenite prints the whole
/// handshake request). Their `trace` output is never wanted, whatever `RUST_LOG` says.
const REQUEST_LIBS: [&str; 7] = ["tungstenite", "async_tungstenite", "hyper", "hyper_util", "reqwest", "rustls", "h2"];

/// `RUST_LOG` with the libraries of [`REQUEST_LIBS`] held at `debug` when something asks for `trace` (a bare level, a
/// target, or the number 5); anything else is left as written.
fn capped_spec(spec: &str) -> String {
    let wants_trace = spec.split(',').any(|d| {
        let level = d.rsplit('=').next().unwrap_or("").trim();
        level.eq_ignore_ascii_case("trace") || level == "5"
    });
    if !wants_trace {
        return spec.to_string();
    }
    let mut out = spec.to_string();
    for lib in REQUEST_LIBS {
        out.push_str(&format!(",{lib}=debug"));
    }
    out
}

/// The log filter of `--log`: `RUST_LOG` (errors only when unset), without `trace` from the request libraries.
fn log_filter(spec: &str) -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing_subscriber::filter::LevelFilter::ERROR.into())
        .parse_lossy(capped_spec(spec))
}

pub async fn main() -> i32 {
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(e) => {
            // `nexo bin <op>` says what the model's `bin` tool says; any other error is clap's own
            let argv: Vec<String> = std::env::args().skip(1).collect();
            let hint =
                argv.iter().position(|w| w == "bin").and_then(|i| crate::bin_tool::bin_error(&e, &argv[i + 1..]));
            match hint {
                Some(text) => {
                    eprintln!("error: {text}");
                    return e.exit_code();
                }
                None => e.exit(),
            }
        }
    };
    if let Some(f) = &args.log {
        match std::fs::File::create(f) {
            Ok(file) => {
                tracing_subscriber::fmt()
                    .with_env_filter(log_filter(
                        &std::env::var(tracing_subscriber::EnvFilter::DEFAULT_ENV).unwrap_or_default(),
                    ))
                    .with_writer(std::sync::Mutex::new(file))
                    .with_ansi(false)
                    .init();
            }
            Err(e) => eprintln!("nexo: cannot open log file {}: {e}", f.display()),
        }
    }
    if let Some(Cmd::Supervise { what }) = &args.cmd {
        return match what {
            Some(SuperviseCmd::Hook { kind }) => crate::supervise::hook(kind),
            // printing is the only mode, so it is the default; `--print` stays accepted
            Some(SuperviseCmd::Install { .. }) => crate::supervise::install_print(),
            None => match crate::supervise::daemon().await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("nexo: {e:#}");
                    1
                }
            },
        };
    }
    // the lanes read their own folders' configuration, not this one's
    if let Some(Cmd::Team { what }) = &args.cmd {
        return crate::team::run(what).await;
    }
    if let Some(Cmd::Bin(b)) = &args.cmd {
        return crate::bin_cmd::run(b);
    }
    if let Some(Cmd::Dbg(d)) = &args.cmd {
        return crate::dbg_cmd::run(d);
    }
    if let Some(Cmd::ChromeSetup(c)) = &args.cmd {
        return crate::chrome_setup_cmd::run(c);
    }
    if let Some(Cmd::Re(r)) = &args.cmd {
        return crate::re_cmd::run(r).await;
    }
    if let Some(Cmd::Api(a)) = &args.cmd {
        return crate::api_cmd::run(a).await;
    }
    if let (None, Some(word)) = (&args.cmd, &args.first) {
        use clap::CommandFactory;
        let names: Vec<String> =
            Args::command().get_subcommands().filter(|c| !c.is_hide_set()).map(|c| c.get_name().to_string()).collect();
        if let Some(why) = crate::first_use::mistyped_command(word, &names) {
            eprintln!("nexo: {why}");
            return crate::headless::EXIT_USAGE;
        }
    }
    let print_fmt = args.prompt.is_some().then(|| crate::headless::format_asked(&args.headless, args.json));
    match run(args).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("nexo: {e:#}");
            if let Some(fmt) = print_fmt {
                crate::headless::print_startup_error(fmt, &format!("{e:#}"));
            }
            1
        }
    }
}

async fn run(args: Args) -> anyhow::Result<i32> {
    let cwd = match &args.cwd {
        Some(d) => {
            nexo_core::paths::canonical(d).map_err(|e| anyhow::anyhow!("cannot use --cwd {}: {e}", d.display()))?
        }
        None => std::env::current_dir()?,
    };
    let made = crate::worktree::enter(&args, &cwd)?;
    let code = run_in(args, made.as_ref().map_or(cwd, |w| w.path.clone())).await;
    if let Some(w) = &made {
        crate::worktree::leave(w);
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_flag_enables_loaded_config_with_yolo() {
        let args = Args::try_parse_from(["nexo", "--yolo", "--audit"]).unwrap();
        assert!(args.yolo);
        let dir = tempfile::tempdir().unwrap();
        let config = load_config(&args, dir.path()).unwrap();
        assert!(config.audit.enabled);
    }

    #[test]
    fn trace_is_held_back_for_the_request_libraries() {
        for spec in ["trace", "info,tungstenite=trace", "TRACE", "warn,rustls=5", "nexo=trace"] {
            let out = capped_spec(spec);
            assert!(out.starts_with(spec), "{out}");
            for lib in REQUEST_LIBS {
                assert!(
                    out.ends_with(&format!("{lib}=debug")) || out.contains(&format!(",{lib}=debug")),
                    "{lib} in {out}"
                );
            }
            let _ = log_filter(spec);
        }
        // nothing asks for trace: the text is left as written
        for spec in ["", "info", "debug,hyper=warn", "nexo=debug"] {
            assert_eq!(capped_spec(spec), spec);
        }
    }
}
