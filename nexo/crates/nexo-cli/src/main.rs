//! `nexo`: the terminal coding harness. Wires configuration, providers, tools
//! and the agent together and hands them to the TUI or runs one request
//! headlessly.

mod account;
mod acp;
mod audit_cmd;
#[cfg(feature = "bench")]
mod bench_cmd;
mod bin_cmd;
mod bin_managed;
mod bin_tool;
mod chrome_setup_cmd;
mod cli;
mod dbg_cmd;
mod export_cmd;
mod first_use;
mod headless;
mod mcp_cmd;
mod plugin_cmd;
mod re_cmd;
mod re_mcp;
mod schema;
mod setup;
mod supervise;
mod update;
mod worktree;

/// The stack of the thread the CLI runs on: what Linux gives a main thread.
const STACK: usize = 8 << 20;

/// The browser starts nexo as the native-messaging host of the nexo extension with the extension's origin as its
/// argument (`nexo chrome-extension://<id>/`, plus `--parent-window=N` on Windows); `nexo browser-host` does the same
/// by hand. Stdout belongs to the protocol, so this runs before anything else can print.
fn browser_host_mode() -> bool {
    let first = std::env::args().nth(1).unwrap_or_default();
    first == "browser-host" || first.starts_with("chrome-extension://")
}

fn run_browser_host() -> i32 {
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("nexo browser-host: {e}");
            return 1;
        }
    };
    match rt.block_on(nexo_browser_host::run_stdio()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("nexo browser-host: {e}");
            1
        }
    }
}

fn main() {
    if browser_host_mode() {
        std::process::exit(run_browser_host());
    }
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
    // The CLI runs on a thread of its own with the stack Linux gives a main thread: Windows gives its main
    // thread 1 MiB, less than an unoptimised build of the CLI takes before it reads its arguments.
    let code = std::thread::Builder::new()
        .name("main".into())
        .stack_size(STACK)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
            rt.block_on(cli::main())
        })
        .expect("main thread")
        .join()
        .unwrap_or(101);
    // `exit` runs no destructors: the audit log writes what it has queued first
    nexo_agent::audit::close_all();
    std::process::exit(code);
}
