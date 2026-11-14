//! One test binary for the crate: one `mod` per former `tests/*.rs` file. `codex.rs` and `tls_roots.rs` stay binaries of
//! their own: they set process-wide environment variables (the credential store, the system CA store).

mod client;
mod compat;
mod replay;
mod resilience;
