//! One test binary for the crate: one `mod` per former `tests/*.rs` file.

use std::path::PathBuf;
use std::sync::OnceLock;

mod agent_loop;
mod chat_run;
mod close_on_verified;
mod padded;
mod q2_replay;
mod run_timeout;
#[path = "../../../nexo-agent/tests/scratch/mod.rs"]
mod scratch;
mod stop_reason;

/// The folder the agents of these tests keep their sessions in, never the user's own. The environment is process-wide
/// and the files now run in one process, so it is set once, here, and every file uses this one.
fn home() -> &'static PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        // the scratch folder a bash call without a session gets lives until the process exits too
        scratch::at_exit(|| nexo_tools::bash::wipe_tmp(""));
        let tmp = tempfile::Builder::new()
            .prefix("nexo-test-")
            .tempdir_in(nexo_core::compat::scratch_base(std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))))
            .unwrap();
        let d = scratch::until_exit(tmp);
        for var in ["NEXO_CONFIG_DIR", "NEXO_DATA_DIR", "NEXO_CACHE_DIR"] {
            std::env::set_var(var, d.join(var));
        }
        // the start check runs a real cargo (each test project has its own package name, as the target dir is shared): keep it off the outer cargo's target dir and build lock
        std::env::set_var("CARGO_TARGET_DIR", d.join("target"));
        d
    })
}
