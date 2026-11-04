//! Records the nexo commit the bench was built from (`NEXO_COMMIT`), so results name the code
//! they measured. `unknown` outside a git checkout.

use std::path::PathBuf;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// The commit without the `git` program (a `cargo install --git` on a machine without git): cargo's
/// checkout keeps a `.git` folder, so `HEAD` is read from it.
fn head_from_files() -> Option<String> {
    let start = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR")?);
    let dir = start.ancestors().map(|a| a.join(".git")).find(|g| g.is_dir())?;
    let head = std::fs::read_to_string(dir.join("HEAD")).ok()?;
    let head = head.trim();
    let sha = match head.strip_prefix("ref: ") {
        None => head.to_string(),
        Some(name) => match std::fs::read_to_string(dir.join(name)) {
            Ok(s) => s.trim().to_string(),
            Err(_) => std::fs::read_to_string(dir.join("packed-refs"))
                .ok()?
                .lines()
                .find_map(|l| l.strip_suffix(name).map(|s| s.trim().to_string()))?,
        },
    };
    (sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit())).then_some(sha)
}

fn main() {
    let commit = git(&["rev-parse", "HEAD"]).or_else(head_from_files).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=NEXO_COMMIT={commit}");
    // when that commit was made, in UTC: an update check can compare dates when GitHub does not know it
    let when = Command::new("git")
        .args(["log", "-1", "--format=%cd", "--date=format-local:%Y-%m-%dT%H:%M:%SZ"])
        .env("TZ", "UTC")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=NEXO_COMMIT_TIME={when}");
    // rebuilt when HEAD moves: a checkout, or a commit on the branch it names
    if let Some(dir) = git(&["rev-parse", "--git-dir"]) {
        println!("cargo:rerun-if-changed={}", PathBuf::from(&dir).join("HEAD").display());
    }
    if let (Some(common), Some(head)) = (git(&["rev-parse", "--git-common-dir"]), git(&["symbolic-ref", "-q", "HEAD"]))
    {
        let common = PathBuf::from(common);
        println!("cargo:rerun-if-changed={}", common.join(&head).display());
        println!("cargo:rerun-if-changed={}", common.join("packed-refs").display());
    }
    println!("cargo:rerun-if-changed=build.rs");
}
