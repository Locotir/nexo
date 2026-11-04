//! Updates: each time the interface starts, nexo asks GitHub (in the background) how many commits
//! `main` has past the one it was built from, and says so; `nexo update` builds the latest with cargo.

use std::path::PathBuf;
use std::time::Duration;

pub const REPO: &str = "https://github.com/Locotir/nexo";
const API: &str = "https://api.github.com/repos/Locotir/nexo/compare";

/// The commit this binary was built from (`unknown` outside a git checkout).
pub fn commit() -> &'static str {
    nexo_bench::run::COMMIT
}

/// What the interface shows: nothing when up to date.
fn notice(behind: u32) -> Option<String> {
    (behind > 0).then(|| {
        let s = if behind == 1 { "" } else { "s" };
        format!("nexo update available: {behind} new commit{s} on main: run `nexo update`.")
    })
}

/// How many commits `main` is ahead of `commit`; `None` when GitHub does not know the commit (a local
/// build of changes never pushed) or cannot be reached.
async fn ask(commit: &str) -> Option<u32> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent(concat!("nexo/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    let r = client
        .get(format!("{API}/{commit}...main"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        return None;
    }
    let v: serde_json::Value = r.json().await.ok()?;
    v.get("ahead_by")?.as_u64().map(|n| n as u32)
}

/// Whether the check is turned off (`[ui] update_check = false`, `NEXO_NO_UPDATE_CHECK`) or cannot
/// work (a build that does not know its commit).
pub fn off(config: &nexo_core::Config) -> bool {
    !config.ui.update_check || std::env::var_os("NEXO_NO_UPDATE_CHECK").is_some() || commit() == "unknown"
}

/// The notice for the interface: none when up to date.
pub async fn check() -> Result<Option<String>, String> {
    let c = commit();
    let behind = ask(c).await.ok_or_else(|| format!("GitHub does not know commit {c}, or cannot be reached"))?;
    Ok(notice(behind))
}

/// `nexo update`: `--check` only says; otherwise `cargo install` builds the latest `main`.
pub async fn run(check_only: bool) -> anyhow::Result<i32> {
    if check_only {
        match check().await {
            Ok(Some(n)) => println!("{n}"),
            Ok(None) => println!("nexo is up to date ({}).", short(commit())),
            Err(e) => {
                eprintln!("nexo: {e}");
                return Ok(1);
            }
        }
        return Ok(0);
    }
    // Windows cannot write over a running program, but it can rename one.
    let moved = if cfg!(windows) { set_aside() } else { None };
    println!("nexo: cargo install --locked --git {REPO} nexo-cli");
    let status = std::process::Command::new("cargo")
        .args(["install", "--locked", "--git", REPO, "nexo-cli"])
        .status()
        .map_err(|e| anyhow::anyhow!("cannot run cargo ({e}); install Rust from https://rustup.rs"))?;
    if !status.success() {
        if let Some((from, to)) = moved {
            let _ = std::fs::rename(to, from);
        }
        return Ok(status.code().unwrap_or(1));
    }
    Ok(0)
}

fn set_aside() -> Option<(PathBuf, PathBuf)> {
    let exe = std::env::current_exe().ok()?;
    let old = exe.with_extension("old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).ok()?;
    Some((exe, old))
}

fn short(c: &str) -> &str {
    &c[..c.len().min(7)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notice_shows_only_when_behind() {
        assert_eq!(notice(0), None);
        assert!(notice(1).unwrap().contains("1 new commit on main"));
        assert!(notice(4).unwrap().contains("4 new commits"));
    }
}
