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
pub(crate) use config::load_config_for;
use config::{load_config, trust_asked};
use dispatch::run_in;
use modes::{headless_collect, interactive};

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
                    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
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
            Some(SuperviseCmd::Install { print: true }) => crate::supervise::install_print(),
            Some(SuperviseCmd::Install { print: false }) => {
                eprintln!("nexo: nothing is written; use `nexo supervise install --print` and add the text yourself");
                2
            }
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
        return crate::re_cmd::run(r);
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
        Some(d) => std::fs::canonicalize(d)?,
        None => std::env::current_dir()?,
    };
    let made = crate::worktree::enter(&args, &cwd)?;
    let code = run_in(args, made.as_ref().map_or(cwd, |w| w.path.clone())).await;
    if let Some(w) = &made {
        crate::worktree::leave(w);
    }
    code
}
