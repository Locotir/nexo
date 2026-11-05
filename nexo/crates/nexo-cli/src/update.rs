//! Updates: each time the interface starts, nexo asks GitHub (in the background) how many commits
//! `main` has past the one it was built from, and says so; `nexo update` builds the latest with cargo.

use std::path::PathBuf;
use std::time::Duration;

pub const REPO: &str = "https://github.com/Locotir/nexo";
const API: &str = "https://api.github.com/repos/Locotir/nexo";
/// The Windows installer: installs what is missing (Rust, the C++ build tools), then builds nexo.
const INSTALLER: &str = "https://raw.githubusercontent.com/Locotir/nexo/main/install.ps1";

/// The commit this binary was built from (`unknown` outside a git checkout).
pub fn commit() -> &'static str {
    nexo_bench::run::COMMIT
}

/// What the interface shows: nothing when up to date.
fn notice(behind: u32) -> Option<String> {
    (behind > 0).then(|| {
        let s = if behind == 1 { "" } else { "s" };
        format!("New nexo update: {behind} new commit{s} on main. Run `nexo update`, then restart nexo.")
    })
}

/// How many commits `main` is ahead of `commit`. When GitHub does not know the commit (a build of
/// changes pushed elsewhere, or not yet), the commits on `main` made after it count instead. `None`
/// when GitHub cannot be reached.
async fn ask(commit: &str) -> Option<u32> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent(concat!("nexo/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    let get = |url: String| client.get(url).header("Accept", "application/vnd.github+json").send();
    let r = get(format!("{API}/compare/{commit}...main")).await.ok()?;
    if r.status().is_success() {
        let v: serde_json::Value = r.json().await.ok()?;
        return v.get("ahead_by")?.as_u64().map(|n| n as u32);
    }
    let when = nexo_bench::run::COMMIT_TIME;
    if r.status() != reqwest::StatusCode::NOT_FOUND || when == "unknown" {
        return None;
    }
    let r = get(format!("{API}/commits?sha=main&per_page=100&since={when}")).await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    let v: serde_json::Value = r.json().await.ok()?;
    Some(newer(&v, when))
}

/// The commits in a GitHub commit list made strictly after `when` (`since` also returns equal ones).
fn newer(list: &serde_json::Value, when: &str) -> u32 {
    let dates = list.as_array().into_iter().flatten().filter_map(|c| c.pointer("/commit/committer/date")?.as_str());
    // both are `YYYY-MM-DDTHH:MM:SSZ`, so text order is time order
    dates.filter(|d| *d > when).count() as u32
}

/// Whether the check is turned off (`[ui] update_check = false`, `NEXO_NO_UPDATE_CHECK`) or cannot
/// work (a build that does not know its commit).
pub fn off(config: &nexo_core::Config) -> bool {
    !config.ui.update_check || std::env::var_os("NEXO_NO_UPDATE_CHECK").is_some() || commit() == "unknown"
}

/// The notice for the interface: none when up to date.
pub async fn check() -> Result<Option<String>, String> {
    Ok(behind().await?.and_then(notice))
}

/// How many commits behind `main` this build is: none when up to date.
pub async fn behind() -> Result<Option<u32>, String> {
    let c = commit();
    if c == "unknown" {
        return Err("this build does not know its commit: run `nexo update` to rebuild from GitHub".into());
    }
    let behind = ask(c).await.ok_or_else(|| format!("cannot ask GitHub about commit {c}"))?;
    Ok((behind > 0).then_some(behind))
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
    let built = std::process::Command::new("cargo").args(["install", "--locked", "--git", REPO, "nexo-cli"]).status();
    // On Windows a missing Rust or missing C++ build tools is the usual reason a build cannot start or
    // link: the installer script installs what is missing and builds, so nobody has to look it up.
    let status = match built {
        Ok(s) if s.success() || !cfg!(windows) => s,
        _ if cfg!(windows) => {
            println!("nexo: installing what the build needs (Rust, the Visual Studio C++ build tools), then building");
            let run = format!("irm {INSTALLER} | iex");
            std::process::Command::new("powershell")
                .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &run])
                .status()
                .map_err(|e| anyhow::anyhow!("cannot run PowerShell ({e}); run: {run}"))?
        }
        Err(e) => anyhow::bail!("cannot run cargo ({e}); install Rust from https://rustup.rs"),
        Ok(s) => s,
    };
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
        assert!(notice(4).unwrap().contains("restart nexo"));
    }

    #[test]
    fn a_build_github_does_not_know_counts_the_commits_made_after_it() {
        let list = serde_json::json!([
            {"commit": {"committer": {"date": "2026-11-04T20:27:00Z"}}},
            {"commit": {"committer": {"date": "2026-11-04T18:05:00Z"}}},
            {"commit": {"committer": {"date": "2026-11-04T19:00:00Z"}}},
        ]);
        assert_eq!(newer(&list, "2026-11-04T18:05:00Z"), 2);
        assert_eq!(newer(&list, "2026-11-04T21:00:00Z"), 0);
        assert_eq!(newer(&serde_json::json!({"message": "x"}), "2026-11-04T18:05:00Z"), 0);
    }
}
