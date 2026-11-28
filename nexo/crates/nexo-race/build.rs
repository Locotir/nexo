//! Records `NEXO_CURATOR_FORMAT`: a hash of the curator's own source, data and the workspace's lock file. A
//! curator state saved next to a session (`snapshot.rs`) carries it, and a build whose engine reads or decides
//! otherwise has another one, so it rebuilds the state from the history instead of reading fields it would take
//! for something else.

use std::path::{Path, PathBuf};

/// FNV-1a: stable across Rust versions and machines, which the standard hasher does not promise.
fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            files(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let mut all = Vec::new();
    for d in ["src", "data"] {
        files(&root.join(d), &mut all);
        println!("cargo:rerun-if-changed={d}");
    }
    all.push(root.join("Cargo.toml"));
    println!("cargo:rerun-if-changed=Cargo.toml");
    // the versions of the libraries the engine runs on (regex, serde_json): the workspace's lock, when there is one
    let lock = root.join("../../Cargo.lock");
    if lock.is_file() {
        all.push(lock);
        println!("cargo:rerun-if-changed=../../Cargo.lock");
    }
    // by the path inside the crate, so the same tree hashes the same wherever it is checked out
    let mut named: Vec<(String, PathBuf)> = all
        .into_iter()
        .map(|p| (p.strip_prefix(&root).unwrap_or(&p).to_string_lossy().replace('\\', "/"), p))
        .collect();
    named.sort();
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for (name, p) in &named {
        h = fnv(h, name.as_bytes());
        // line ends as git checks them out on Windows must not make another format
        let text = std::fs::read(p).unwrap_or_default();
        let text: Vec<u8> = text.into_iter().filter(|&b| b != b'\r').collect();
        h = fnv(h, &text);
    }
    println!("cargo:rustc-env=NEXO_CURATOR_FORMAT={h:016x}");
}
