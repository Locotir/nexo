//! Where the test program (examples/dbg_target.rs) is. Cargo up to 1.99 put a test binary in
//! `target/debug/deps` and later ones put it under `target/debug/build/<package>/<hash>/out`; the example
//! is in `target/debug/examples` either way, so it is looked for above the test binary, and built (its path
//! taken from cargo's own report) when it is not there.

use std::path::PathBuf;
use std::sync::OnceLock;

pub fn target() -> PathBuf {
    static AT: OnceLock<PathBuf> = OnceLock::new();
    AT.get_or_init(|| {
        let name = format!("dbg_target{}", std::env::consts::EXE_SUFFIX);
        let exe = std::env::current_exe().unwrap();
        exe.ancestors()
            .skip(1)
            .take(6)
            .map(|d| d.join("examples").join(&name))
            .find(|t| t.is_file())
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
