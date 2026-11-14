//! One test binary for the crate: one `mod` per former `tests/*.rs` file. `traces.rs` stays a binary of its own: its
//! `cover` test points `NEXO_TMP` (where the debugger keeps its session files) at a folder that goes away with the
//! test, which other tests opening sessions in the same process must not see.

mod addr_forms;
mod break_boundary;
mod call_targets;
mod calls;
mod calls_sites;
// shared by the files that run the example program; unused where none of them is built
#[allow(dead_code)]
mod common;
mod core_file;
mod dbg_linux;
mod dbg_windows;
mod gdb_script;
mod range_page;
mod rsp;
mod source_level;
