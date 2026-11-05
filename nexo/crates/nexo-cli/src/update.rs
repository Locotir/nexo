//! Updates: each time the interface starts, nexo asks GitHub (in the background) how many commits
//! `main` has past the one it was built from, and says so; `nexo update` builds the latest with cargo.

use std::path::{Path, PathBuf};
use std::time::Duration;

pub const REPO: &str = "https://github.com/Locotir/nexo";
const API: &str = "https://api.github.com/repos/Locotir/nexo";
/// The installers: they install what is missing (Rust, a C compiler or the C++ build tools), then build nexo.
const INSTALLER_PS: &str = "https://raw.githubusercontent.com/Locotir/nexo/main/install.ps1";
const INSTALLER_SH: &str = "https://raw.githubusercontent.com/Locotir/nexo/main/install.sh";

/// The commit this binary was built from (`unknown` outside a git checkout).
pub fn commit() -> &'static str {
    nexo_bench::run::COMMIT
}

/// What the interface shows: nothing when up to date.
fn notice(behind: u32) -> Option<String> {
    (behind > 0).then(|| {
        let s = if behind == 1 { "" } else { "s" };
        format!("New nexo release: {behind} new commit{s}, ready-built. Run `nexo update`, then restart nexo.")
    })
}

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
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout))
        .user_agent(concat!("nexo/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()
}

/// The latest release: its tag and its files (name, download address).
struct Release {
    tag: String,
    files: Vec<(String, String)>,
}

fn parse_release(v: &serde_json::Value) -> Option<Release> {
    let tag = v.get("tag_name")?.as_str()?.to_string();
    let files = v.get("assets")?.as_array()?.iter();
    let files = files
        .filter_map(|a| {
            Some((a.get("name")?.as_str()?.to_string(), a.get("browser_download_url")?.as_str()?.to_string()))
        })
        .collect();
    Some(Release { tag, files })
}

async fn latest_release(client: &reqwest::Client) -> Option<Release> {
    let r = client
        .get(format!("{API}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        return None;
    }
    parse_release(&r.json().await.ok()?)
}

/// How many commits the latest release (or `main`, before the first release) is ahead of `commit`.
/// When GitHub does not know the commit (a build of changes pushed elsewhere, or not yet), the commits
/// on `main` made after it count instead. `None` when GitHub cannot be reached.
async fn ask(commit: &str) -> Option<u32> {
    let client = client(8)?;
    let target = latest_release(&client).await.map_or_else(|| "main".to_string(), |r| r.tag);
    let get = |url: String| client.get(url).header("Accept", "application/vnd.github+json").send();
    let r = get(format!("{API}/compare/{commit}...{target}")).await.ok()?;
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

/// `nexo update`: `--check` only says; otherwise builds the latest `main` and puts it where this nexo is.
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
    let exe = std::env::current_exe().ok().filter(|e| !needs_installer(e));
    // From cargo's folder or a build folder, the installer moves nexo to its own place and fixes PATH.
    let Some(exe) = exe else { return installer(None) };
    if commit() != "unknown" && ask(commit()).await == Some(0) {
        println!("nexo is up to date ({}).", short(commit()));
        return Ok(0);
    }
    // the ready-built release first: seconds, and no compiler needed
    let stage = std::env::temp_dir().join(format!("nexo-build-{}", std::process::id()));
    match download(&stage).await {
        Ok((file, tag)) => {
            let placed = place(&file, &exe);
            let _ = std::fs::remove_dir_all(&stage);
            placed.map_err(|e| anyhow::anyhow!("downloaded, but cannot replace {} ({e})", exe.display()))?;
            println!("nexo: updated {} to {tag}. Restart nexo.", exe.display());
            return Ok(0);
        }
        Err(e) => println!("nexo: no ready-built release to use ({e}); building from source instead"),
    }
    println!("nexo: building the latest nexo from {REPO}");
    let built = std::process::Command::new("cargo")
        .args(["install", "--quiet", "--locked", "--git", REPO, "nexo-cli", "--root"])
        .arg(&stage)
        .status();
    let ok = matches!(built, Ok(ref s) if s.success());
    let placed = ok.then(|| place(&stage.join("bin").join(exe.file_name().unwrap_or_default()), &exe));
    let _ = std::fs::remove_dir_all(&stage);
    match (built, placed) {
        (_, Some(Ok(()))) => {
            println!("nexo: updated {}. Restart nexo.", exe.display());
            Ok(0)
        }
        (_, Some(Err(e))) => anyhow::bail!("built, but cannot replace {} ({e})", exe.display()),
        // On Windows a missing Rust or missing C++ build tools is the usual reason a build cannot start
        // or link: the installer installs what is missing and builds, so nobody has to look it up.
        _ if cfg!(windows) => installer(exe.parent()),
        (Err(e), _) => anyhow::bail!("cannot run cargo ({e}); install Rust from https://rustup.rs"),
        (Ok(s), _) => Ok(s.code().unwrap_or(1)),
    }
}

/// Downloads this machine's file from the latest release into `stage` and checks it against the
/// release's SHA-256 list: the file and the release tag, or why not.
async fn download(stage: &Path) -> Result<(PathBuf, String), String> {
    let name = ASSET.ok_or("none is published for this system")?;
    let client = client(300).ok_or("no HTTP client")?;
    let rel = latest_release(&client).await.ok_or("GitHub did not answer with a release")?;
    let url = |n: &str| rel.files.iter().find(|(f, _)| f == n).map(|(_, u)| u.clone());
    let (bin_url, sums_url) =
        (url(name).ok_or(format!("{} has no {name}", rel.tag))?, url(SUMS).ok_or("no SHA256SUMS")?);
    let fetch = |u: String| {
        let c = client.clone();
        async move {
            let r = c.get(&u).send().await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?;
            r.bytes().await.map_err(|e| e.to_string())
        }
    };
    println!("nexo: downloading {name} from release {}", rel.tag);
    let sums = String::from_utf8_lossy(&fetch(sums_url).await?).into_owned();
    let bytes = fetch(bin_url).await?;
    let want = expected_sum(&sums, name).ok_or(format!("{SUMS} does not list {name}"))?;
    if sha256_hex(&bytes) != want {
        return Err(format!("{name} does not match its SHA-256: not used"));
    }
    std::fs::create_dir_all(stage).map_err(|e| e.to_string())?;
    let file = stage.join(name);
    std::fs::write(&file, &bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    Ok((file, rel.tag))
}

/// The SHA-256 a `sha256sum` list gives for `name` (`<hex>  <name>`, or `<hex> *<name>`).
fn expected_sum<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|l| {
        let (hex, file) = l.split_once(char::is_whitespace)?;
        (file.trim_start().trim_start_matches('*') == name).then_some(hex)
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// An install from before nexo had its own folder (cargo's `bin`), or a build in a checkout's
/// `target`: the installer puts nexo in its own place instead of writing over these.
fn needs_installer(exe: &Path) -> bool {
    let Some(dir) = exe.parent() else { return true };
    let name = |p: &Path| p.file_name().and_then(|n| n.to_str()).map(str::to_owned).unwrap_or_default();
    let cargo_home = std::env::var_os("CARGO_HOME").map(PathBuf::from);
    let in_cargo =
        name(dir) == "bin" && dir.parent().is_some_and(|p| name(p) == ".cargo" || Some(p) == cargo_home.as_deref());
    let in_target =
        matches!(name(dir).as_str(), "debug" | "release") && dir.parent().is_some_and(|p| name(p) == "target");
    in_cargo || in_target
}

/// Puts the new build at `exe`: a file renamed into place, so a running nexo keeps working; Windows,
/// which cannot replace a running program, renames the old one to `.old` first.
fn place(new: &Path, exe: &Path) -> std::io::Result<()> {
    let next = exe.with_extension("new");
    std::fs::copy(new, &next)?;
    if cfg!(windows) && exe.exists() {
        let old = exe.with_extension("old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(exe, &old)?;
    }
    std::fs::rename(&next, exe)
}

/// Runs the installer script (into `dir` when given); it installs what is missing and builds.
fn installer(dir: Option<&Path>) -> anyhow::Result<i32> {
    let (shell, flag, run) = if cfg!(windows) {
        ("powershell", "-Command", format!("irm {INSTALLER_PS} | iex"))
    } else {
        ("sh", "-c", format!("curl -fsSL {INSTALLER_SH} | sh"))
    };
    println!("nexo: running the installer: {run}");
    let mut cmd = std::process::Command::new(shell);
    if cfg!(windows) {
        cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass"]);
    }
    if let Some(d) = dir {
        cmd.env("NEXO_INSTALL_DIR", d);
    }
    let status =
        cmd.args([flag, &run]).status().map_err(|e| anyhow::anyhow!("cannot run {shell} ({e}); run: {run}"))?;
    Ok(status.code().unwrap_or(1))
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
        assert!(notice(1).unwrap().contains("1 new commit,"));
        assert!(notice(4).unwrap().contains("4 new commits"));
        assert!(notice(4).unwrap().contains("restart nexo"));
    }

    #[test]
    fn a_release_file_is_used_only_when_its_sha256_matches_the_list() {
        let bytes = b"nexo";
        let hex = sha256_hex(bytes);
        assert_eq!(hex.len(), 64);
        let sums = format!(
            "{}  nexo-linux-aarch64\n{hex}  nexo-linux-x86_64\n{hex} *nexo-windows-x86_64.exe\n",
            "0".repeat(64)
        );
        assert_eq!(expected_sum(&sums, "nexo-linux-x86_64"), Some(hex.as_str()));
        assert_eq!(expected_sum(&sums, "nexo-windows-x86_64.exe"), Some(hex.as_str()));
        assert_ne!(expected_sum(&sums, "nexo-linux-aarch64"), Some(hex.as_str()));
        assert_eq!(expected_sum(&sums, "nexo-linux"), None);
    }

    #[test]
    fn a_release_is_read_from_githubs_answer() {
        let v = serde_json::json!({"tag_name": "build-abc1234", "assets": [
            {"name": "SHA256SUMS", "browser_download_url": "https://x/SHA256SUMS"},
            {"name": "nexo-linux-x86_64", "browser_download_url": "https://x/nexo-linux-x86_64"},
            {"name": "broken"},
        ]});
        let r = parse_release(&v).unwrap();
        assert_eq!(r.tag, "build-abc1234");
        assert_eq!(r.files.len(), 2);
        assert!(parse_release(&serde_json::json!({"message": "Not Found"})).is_none());
    }

    #[test]
    fn nexo_in_cargos_folder_or_a_build_folder_goes_through_the_installer() {
        let p = |s: &str| PathBuf::from(s);
        assert!(needs_installer(&p("/home/a/.cargo/bin/nexo")));
        assert!(needs_installer(&p("/src/nexo/target/release/nexo")));
        assert!(needs_installer(&p("/src/nexo/target/debug/nexo")));
        assert!(!needs_installer(&p("/home/a/.local/bin/nexo")));
        assert!(!needs_installer(&p("/usr/local/bin/nexo")));
        assert!(!needs_installer(&p("/opt/release/nexo")));
    }

    #[test]
    fn the_new_build_takes_the_old_ones_place_and_nothing_is_left_beside_it() {
        let d = tempfile::tempdir().unwrap();
        let (new, exe) = (d.path().join("built"), d.path().join("nexo"));
        std::fs::write(&new, "v2").unwrap();
        std::fs::write(&exe, "v1").unwrap();
        place(&new, &exe).unwrap();
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "v2");
        assert!(!exe.with_extension("new").exists());
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
