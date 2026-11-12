//! `nexo`: the terminal coding harness. Wires configuration, providers, tools
//! and the agent together and hands them to the TUI or runs one request
//! headlessly.

mod account;
mod acp;
mod audit_cmd;
#[cfg(feature = "bench")]
mod bench_cmd;
mod bin_cmd;
mod cli;
mod dbg_cmd;
mod export_cmd;
mod first_use;
mod headless;
mod mcp_cmd;
mod plugin_cmd;
mod schema;
mod setup;
mod supervise;
mod update;
mod worktree;

fn main() {
    nexo_core::version::set(env!("NEXO_VERSION"));
    // The local UTC offset can only be read while the process has one thread.
    nexo_agent::budget::init_local_offset();
    // A closed stdout (`nexo providers | head`) ends the process quietly instead of panicking.
    // SIGPIPE stays ignored: a hook, MCP server or tool that exits before reading its stdin must
    // not kill nexo, so only the print that hit the closed stdout ends it.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or("");
        if msg.starts_with("failed printing to stdout") && msg.contains("Broken pipe") {
            std::process::exit(141);
        }
        default_hook(info);
    }));
    // As root, settings and sessions live under root's home even when sudo kept the caller's.
    nexo_core::paths::adopt_root_home();
    for old in nexo_core::paths::move_old_dirs() {
        eprintln!("nexo: moved {} into {}", old.display(), nexo_core::paths::home_dir().display());
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    let code = rt.block_on(cli::main());
    // `exit` runs no destructors: the audit log writes what it has queued first
    nexo_agent::audit::close_all();
    std::process::exit(code);
}
