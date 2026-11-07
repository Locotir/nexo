//! `nexo`: the terminal coding harness. Wires configuration, providers, tools
//! and the agent together and hands them to the TUI or runs one request
//! headlessly.

mod account;
mod acp;
mod cli;
mod headless;
mod mcp_cmd;
mod schema;
mod setup;
mod supervise;
mod update;

fn main() {
    nexo_core::version::set(nexo_bench::run::VERSION);
    // The local UTC offset can only be read while the process has one thread.
    nexo_agent::budget::init_local_offset();
    // A closed pipe (`nexo providers | head`) ends the process quietly instead of panicking.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    // As root, settings and sessions live under root's home even when sudo kept the caller's.
    nexo_core::paths::adopt_root_home();
    for old in nexo_core::paths::move_old_dirs() {
        eprintln!("nexo: moved {} into {}", old.display(), nexo_core::paths::home_dir().display());
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    let code = rt.block_on(cli::main());
    std::process::exit(code);
}
