//! Where the test program (examples/dbg_target.rs) is. Cargo up to 1.99 put a test binary in
//! `target/debug/deps` and later ones put it under `target/debug/build/<package>/<hash>/out`; the example
//! is in `target/debug/examples` either way, so it is looked for above the test binary, and built (its path
//! taken from cargo's own report) when it is not there.
//!
//! A package that cannot have the example built for its tests by cargo (an example of another package is
//! out of reach) builds the same source under a name of its own and asks for that name with `target_as`;
//! the nested build below stays as the way out when only that one test binary was built.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// The test program under its own name, for the tests of the package that owns it.
// The agent's tests ask for their own copy with `target_as` and never call this.
#[allow(dead_code)]
pub fn target() -> PathBuf {
    target_as("dbg_target")
}

/// The test program built under ``name``, or plain `dbg_target` when no such build is next to the test binary,
/// or one built on the spot when neither is.
pub fn target_as(name: &str) -> PathBuf {
    static AT: Mutex<Option<HashMap<String, PathBuf>>> = Mutex::new(None);
    let mut at = AT.lock().unwrap();
    at.get_or_insert_with(HashMap::new)
        .entry(name.to_string())
        .or_insert_with(|| {
            let exe = std::env::current_exe().unwrap();
            [name, "dbg_target"]
                .iter()
                .find_map(|n| {
                    let file = format!("{n}{}", std::env::consts::EXE_SUFFIX);
                    exe.ancestors().skip(1).take(6).map(|d| d.join("examples").join(&file)).find(|t| t.is_file())
                })
                .unwrap_or_else(built)
        })
        .clone()
}

fn built() -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = std::process::Command::new(cargo)
        .args(["build", "-q", "-p", "nexo-dbg", "--example", "dbg_target", "--message-format=json"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo runs");
    assert!(out.status.success(), "dbg_target did not build: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["target"]["name"] == "dbg_target")
        .find_map(|v| v["executable"].as_str().map(PathBuf::from))
        .expect("cargo names the example it built")
}
