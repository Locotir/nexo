//! One test binary for the files below: one `mod` per former `tests/*.rs` file. `agentperf.rs`, `debug_cold.rs` and
//! `worktree_agent.rs` stay binaries of their own: each points `NEXO_DATA_DIR`, `NEXO_CONFIG_DIR` and `NEXO_CACHE_DIR`
//! at a folder of its own for the tests it runs (and drops it afterwards), which the other files must not see in the same
//! process. The four folders with a `main.rs` (`agent_loop`, `subagent_detach`, `subagent_events`, `workflow`) are
//! binaries of their own too, as before.

mod brief_golden;
mod managed_policy;
#[path = "../scratch/mod.rs"]
mod scratch;
mod temp_on_disk;
