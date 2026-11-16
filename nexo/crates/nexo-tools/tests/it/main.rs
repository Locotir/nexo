//! One test binary for the crate: one `mod` per former `tests/*.rs` file. `mcp_stdio.rs` (its own harness, `harness =
//! false`) and `sandbox_bwrap.rs` (it forces `NEXO_SANDBOX_BACKEND=bwrap` for the whole process) stay binaries of
//! their own.

mod debug_windows;
mod diagnostics_kinds;
mod guarded;
mod mcp_http;
mod mcp_robust;
mod sandbox;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "../../../nexo-agent/tests/scratch/mod.rs"]
mod scratch;
