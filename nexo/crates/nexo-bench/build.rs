//! Records the nexo commit the bench was built from (`NEXO_COMMIT`), so results name the code
//! they measured. `unknown` outside a git checkout.

use std::path::PathBuf;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

fn main() {
    let commit = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=NEXO_COMMIT={commit}");
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
