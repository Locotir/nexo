//! The test program (examples/dbg_target.rs) is a plain executable. A program linked by MSVC carries
//! its function names in a separate PDB, which the debugger does not read yet, so the two names the tests
//! look for are exported: the export table of the file itself names them.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "msvc");
    if msvc {
        for symbol in ["dbg_add", "dbg_crash", "DBG_COUNTER,DATA"] {
            println!("cargo:rustc-link-arg-examples=/EXPORT:{symbol}");
        }
    }
}
