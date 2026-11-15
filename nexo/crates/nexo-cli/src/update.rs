//! Updates: when the interface starts and every [`EVERY`] after, nexo asks GitHub (in the background) how many commits
//! `main` has past the one it was built from, and says so; `nexo update` downloads the latest release (cargo only as a fallback).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod build;
mod fetch;
mod release;
mod ui;
mod verify;

use build::*;
use fetch::*;
use release::*;
pub use release::{check, newer_release, off};
use ui::*;
use verify::*;

/// How often a running interface asks again whether a newer nexo is out.
pub const EVERY: Duration = Duration::from_secs(60 * 60);

pub const REPO: &str = "https://github.com/Locotir/nexo";
const API: &str = "https://api.github.com/repos/Locotir/nexo";

/// Where releases are asked for: the web address (a plain redirect and downloads, no request quota) and the
/// API address (the second try, and the commit count). Only a debug build lets `NEXO_UPDATE_WEB` and
/// `NEXO_UPDATE_API` replace them (tests against a local server): a released nexo updates from the
/// project's own releases and nowhere else.
struct Sources {
    web: String,
    api: String,
}

impl Sources {
    fn live() -> Self {
        #[allow(unused_mut)]
        let mut s = Sources { web: REPO.to_string(), api: API.to_string() };
        #[cfg(debug_assertions)]
        {
            if let Some(v) = std::env::var("NEXO_UPDATE_API").ok().filter(|v| !v.is_empty()) {
                s.api = v;
            }
            if let Some(v) = std::env::var("NEXO_UPDATE_WEB").ok().filter(|v| !v.is_empty()) {
                s.web = v;
            }
        }
        s
    }
}

/// The installers: they install what is missing (Rust, a C compiler or the C++ build tools), then build nexo.
const INSTALLER_PS: &str = "https://raw.githubusercontent.com/Locotir/nexo/main/install.ps1";
const INSTALLER_SH: &str = "https://raw.githubusercontent.com/Locotir/nexo/main/install.sh";

/// The commit this binary was built from (`unknown` outside a git checkout).
pub fn commit() -> &'static str {
    env!("NEXO_COMMIT")
}

/// What the interface shows: nothing when up to date.
fn notice(behind: u32) -> Option<String> {
    (behind > 0).then(|| {
        let s = if behind == 1 { "" } else { "s" };
        format!("New nexo release: {behind} new commit{s}, ready-built. Run `nexo update`, then restart nexo.")
    })
}

/// The `--check` line when a newer release is out but its commit count is not known.
const NEWER_NO_COUNT: &str = "A newer nexo release is out. Run `nexo update`, then restart nexo.";

/// The ready-built file for this machine, as the release workflow names it; `None` where none is published.
const ASSET: Option<&str> = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some("nexo-linux-x86_64")
} else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
    Some("nexo-linux-aarch64")
} else if cfg!(all(windows, target_arch = "x86_64")) {
    Some("nexo-windows-x86_64.exe")
} else if cfg!(all(windows, target_arch = "aarch64")) {
    Some("nexo-windows-aarch64.exe")
} else {
    None
};
/// The release file listing each file's SHA-256, as `sha256sum` writes it.
const SUMS: &str = "SHA256SUMS";

fn client(timeout: u64) -> Option<reqwest::Client> {
    nexo_providers::build_client(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout))
            .user_agent(concat!("nexo/", env!("CARGO_PKG_VERSION")))
    })
    .ok()
}

/// A client that does not follow redirects: `releases/latest` on the web answers with one, and where it
/// points is the answer.
fn probe(timeout: u64) -> Option<reqwest::Client> {
    nexo_providers::build_client(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("nexo/", env!("CARGO_PKG_VERSION")))
    })
    .ok()
}

/// `nexo update`: `--check` only says; otherwise builds the latest `main` and puts it where this nexo is.
pub async fn run(check_only: bool) -> anyhow::Result<i32> {
    let ui = Ui::detect();
    if check_only {
        ui.begin("checking for a new release", false);
        match check().await {
            Ok(Some(n)) => ui.end(Kind::Notice, &n),
            Ok(None) => ui.end(Kind::Final, &format!("nexo is up to date ({}).", short(commit()))),
            Err(e) => {
                ui.end(Kind::Fail, &e);
                return Ok(1);
            }
        }
        return Ok(0);
    }
    let exe = std::env::current_exe().ok().filter(|e| !needs_installer(e));
    // From cargo's folder or a build folder, the installer moves nexo to its own place and fixes PATH.
    let Some(exe) = exe else { return installer(None) };
    if commit() != "unknown" {
        ui.begin("checking for a new release", false);
        if status(commit(), &Sources::live()).await == Some(None) {
            ui.end(Kind::Final, &format!("nexo already up to date ({})", short(commit())));
            return Ok(0);
        }
        ui.end(Kind::Ok, "a newer release is out");
    }
    // the ready-built release first: seconds, and no compiler needed
    let stage_dir = match private_stage() {
        Ok(d) => d,
        Err(e) => anyhow::bail!(
            "cannot make a private work folder ({e}); check that the temp folder is writable and has free space"
        ),
    };
    let stage = stage_dir.path().to_path_buf();
    ui.on_interrupt(&stage);
    let got = match ASSET {
        Some(name) => download(&ui, &Sources::live(), name, &stage, Caps::LIVE).await,
        None => Err("none is published for this system".to_string()),
    };
    match got {
        Ok((file, tag)) => {
            ui.begin("installing", false);
            let placed = place(&file, &exe);
            let _ = std::fs::remove_dir_all(&stage);
            if let Err(e) = placed {
                ui.abort();
                anyhow::bail!("downloaded, but cannot replace {} ({e})", exe.display());
            }
            ui.end(Kind::Ok, &format!("installed {}", exe.display()));
            ui.end(Kind::Final, &update_text(tag.strip_prefix("build-"), commit()));
            return Ok(0);
        }
        Err(e) => {
            let t = format!("ready-built release not used: {e}");
            ui.end(Kind::Warn, &t);
        }
    }
    ui.begin("building nexo from source", true);
    let built = build(&ui, &stage);
    let ok = matches!(built, Ok((ref s, _)) if s.success());
    let placed = ok.then(|| place(&stage.join("bin").join(exe.file_name().unwrap_or_default()), &exe));
    let _ = std::fs::remove_dir_all(&stage);
    match (built, placed) {
        (Ok((_, rev)), Some(Ok(()))) => {
            ui.end(Kind::Ok, "built the latest nexo");
            ui.end(Kind::Final, &update_text(rev.as_deref(), commit()));
            Ok(0)
        }
        (_, Some(Err(e))) => {
            ui.abort();
            anyhow::bail!("built, but cannot replace {} ({e})", exe.display())
        }
        // On Windows a missing Rust or missing C++ build tools is the usual reason a build cannot start
        // or link: the installer installs what is missing and builds, so nobody has to look it up.
        _ if cfg!(windows) => {
            ui.abort();
            installer(exe.parent())
        }
        (Err(e), _) => {
            ui.abort();
            anyhow::bail!("cannot run cargo ({e}); install Rust from https://rustup.rs")
        }
        (Ok((s, _)), _) => {
            let code = s.code().unwrap_or(1);
            ui.end(Kind::Fail, &format!("the build failed (cargo exited with {code})"));
            Ok(code)
        }
    }
}
fn short(c: &str) -> &str {
    &c[..c.len().min(7)]
}

#[cfg(test)]
mod tests;
