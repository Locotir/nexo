//! Updates: when the interface starts and every [`EVERY`] after, nexo asks GitHub (in the background) how many commits
//! `main` has past the one it was built from, and says so; `nexo update` downloads the latest release (cargo only as a fallback).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

/// Whether `t` is a tag that cannot change where an address built from it points: plain characters, and not a
/// path step (`.` and `..`).
fn plain_tag(t: &str) -> bool {
    let plain = t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    !t.is_empty() && plain && t != "." && t != ".."
}

/// The release tag in the address `releases/latest` redirects to (`.../releases/tag/<tag>`). Only plain
/// tag characters pass, so the address built from it stays inside the release downloads.
fn tag_of(location: &str) -> Option<String> {
    let t = location.split("/releases/tag/").nth(1)?.split(['?', '#', '/']).next()?;
    plain_tag(t).then(|| t.to_string())
}

/// The latest release's tag from the web, with no API request: `releases/latest` is a redirect.
async fn tag_via_web(probe: &reqwest::Client, src: &Sources) -> Option<String> {
    let r = probe.get(format!("{}/releases/latest", src.web)).send().await.ok()?;
    if !r.status().is_redirection() {
        return None;
    }
    tag_of(r.headers().get(reqwest::header::LOCATION)?.to_str().ok()?)
}

/// The release `tag` with the files named, at their download addresses.
fn release_from_tag(src: &Sources, tag: &str, names: &[&str]) -> Release {
    let files = names.iter().map(|n| (n.to_string(), format!("{}/releases/download/{tag}/{n}", src.web)));
    Release { tag: tag.to_string(), files: files.collect() }
}

const NO_RELEASE: &str = "GitHub did not answer with a release";

/// The reason in words when GitHub refused for its hourly limit (403 or 429 with no requests left), with
/// the minutes until `reset` (an epoch second) counted from `now`.
fn limit_text(status: u16, remaining: Option<&str>, reset: Option<&str>, now: u64) -> Option<String> {
    if !matches!(status, 403 | 429) || remaining.map(str::trim) != Some("0") {
        return None;
    }
    let when = match reset.and_then(|r| r.trim().parse::<u64>().ok()) {
        Some(r) => format!("it resets in {} min", r.saturating_sub(now).div_ceil(60).max(1)),
        None => "try again later".to_string(),
    };
    Some(format!("GitHub's hourly limit for this network is used up; {when}"))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The latest release from the API (the second try): why not when it cannot be had.
async fn api_release(client: &reqwest::Client, src: &Sources) -> Result<Release, String> {
    let r = client
        .get(format!("{}/releases/latest", src.api))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| NO_RELEASE.to_string())?;
    if !r.status().is_success() {
        let h = |k: &str| r.headers().get(k).and_then(|v| v.to_str().ok());
        let why = limit_text(r.status().as_u16(), h("x-ratelimit-remaining"), h("x-ratelimit-reset"), now_secs());
        return Err(why.unwrap_or_else(|| NO_RELEASE.to_string()));
    }
    let rel =
        parse_release(&r.json().await.map_err(|_| NO_RELEASE.to_string())?).ok_or_else(|| NO_RELEASE.to_string())?;
    // the response names the tag and the files, never where to download them from
    if !plain_tag(&rel.tag) {
        return Err(NO_RELEASE.to_string());
    }
    let files = rel.files.iter().map(|(n, _)| (n.clone(), format!("{}/releases/download/{}/{n}", src.web, rel.tag)));
    Ok(Release { files: files.filter(|(n, _)| plain_tag(n)).collect(), tag: rel.tag })
}

/// The latest release: from the web redirect first (no API quota), then from the API.
async fn latest_release(
    client: &reqwest::Client,
    probe: &reqwest::Client,
    src: &Sources,
    names: &[&str],
) -> Result<Release, String> {
    match tag_via_web(probe, src).await {
        Some(tag) => Ok(release_from_tag(src, &tag, names)),
        None => api_release(client, src).await,
    }
}

/// Whether a release tag (`build-<short commit>`) is the commit this build was made from.
fn tag_is_commit(tag: &str, commit: &str) -> bool {
    let Some(sha) = tag.strip_prefix("build-") else { return false };
    sha.len() >= 7 && commit.len() >= 7 && (commit.starts_with(sha) || sha.starts_with(commit))
}

/// What a check found: a newer release, its tag and how many commits ahead when the API could say.
#[derive(Debug, PartialEq)]
pub struct Newer {
    pub tag: String,
    pub count: Option<u32>,
}

/// Up to date (`Some(None)`), a newer release (`Some(Some(..))`), or `None` when nothing could be asked.
/// The release tag comes from the web redirect; when it is the running commit nothing else is asked. The
/// API only adds the count of commits (never guessed: without it, none is shown).
async fn status(commit: &str, src: &Sources) -> Option<Option<Newer>> {
    let (client, probe) = (client(8)?, probe(8)?);
    let web_tag = tag_via_web(&probe, src).await;
    if web_tag.as_deref().is_some_and(|t| tag_is_commit(t, commit)) {
        return Some(None);
    }
    let target = match &web_tag {
        Some(t) => t.clone(),
        None => api_release(&client, src).await.map_or_else(|_| "main".to_string(), |r| r.tag),
    };
    let count = ahead(&client, src, commit, &target).await;
    match (web_tag, count) {
        (_, Some(0)) => Some(None),
        (Some(tag), count) => Some(Some(Newer { tag, count })),
        (None, Some(n)) => Some(Some(Newer { tag: target, count: Some(n) })),
        (None, None) => None,
    }
}

/// How many commits `target` (a release tag, or `main`) is ahead of `commit`.
/// When GitHub does not know the commit (a build of changes pushed elsewhere, or not yet), the commits
/// on `main` made after it count instead. `None` when the API cannot say.
async fn ahead(client: &reqwest::Client, src: &Sources, commit: &str, target: &str) -> Option<u32> {
    let get = |url: String| client.get(url).header("Accept", "application/vnd.github+json").send();
    let r = get(format!("{}/compare/{commit}...{target}", src.api)).await.ok()?;
    if r.status().is_success() {
        let v: serde_json::Value = r.json().await.ok()?;
        return v.get("ahead_by")?.as_u64().map(|n| n as u32);
    }
    let when = env!("NEXO_COMMIT_TIME");
    if r.status() != reqwest::StatusCode::NOT_FOUND || when == "unknown" {
        return None;
    }
    let r = get(format!("{}/commits?sha=main&per_page=100&since={when}", src.api)).await.ok()?;
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
    Ok(newer_release().await?.map(|n| n.count.and_then(notice).unwrap_or_else(|| NEWER_NO_COUNT.to_string())))
}

/// The newer release this build lacks: none when up to date.
pub async fn newer_release() -> Result<Option<Newer>, String> {
    let c = commit();
    if c == "unknown" {
        return Err("this build does not know its commit: run `nexo update` to rebuild from GitHub".into());
    }
    status(c, &Sources::live()).await.ok_or_else(|| format!("cannot ask GitHub about commit {c}"))
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

/// A new work folder with a random name, made exclusively and readable by this user alone (0700 on Unix),
/// so no other local user can guess it or plant something in it. It is removed when dropped.
fn private_stage() -> std::io::Result<tempfile::TempDir> {
    let d = tempfile::Builder::new().prefix("nexo-build-").tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(d)
}

/// The most bytes read for each download: the release file (about 35 MB today, so several times that) and
/// the SHA-256 list (a few lines).
#[derive(Clone, Copy)]
struct Caps {
    file: u64,
    sums: u64,
}

impl Caps {
    const LIVE: Caps = Caps { file: 256 << 20, sums: 1 << 20 };
}

/// Downloads `name` from the latest release into `stage` and checks it against the
/// release's SHA-256 list: the file and the release tag, or why not.
async fn download(ui: &Ui, src: &Sources, name: &str, stage: &Path, caps: Caps) -> Result<(PathBuf, String), String> {
    let client = client(300).ok_or("no HTTP client")?;
    let probe = probe(30).ok_or("no HTTP client")?;
    let rel = latest_release(&client, &probe, src, &[name, SUMS]).await?;
    let url = |n: &str| rel.files.iter().find(|(f, _)| f == n).map(|(_, u)| u.clone());
    let (bin_url, sums_url) =
        (url(name).ok_or(format!("{} has no {name}", rel.tag))?, url(SUMS).ok_or("no SHA256SUMS")?);
    ui.begin(&format!("downloading {name}"), false);
    let sums = with_retry(ui, || fetch(&client, &sums_url, None, caps.sums, SUMS)).await?;
    let sums = String::from_utf8_lossy(&sums).into_owned();
    let t = Instant::now();
    let bytes = with_retry(ui, || fetch(&client, &bin_url, Some(ui), caps.file, name)).await?;
    let t =
        format!("downloaded {name} ({}) from {} in {}", megabytes(bytes.len() as u64), rel.tag, secs_text(t.elapsed()));
    ui.end(Kind::Ok, &t);
    ui.begin("verifying SHA-256", false);
    let want = expected_sum(&sums, name).ok_or(format!("{SUMS} does not list {name}"))?;
    if sha256_hex(&bytes) != want {
        return Err(format!("{name} does not match its SHA-256: not used"));
    }
    let file = stage.join(name);
    write_new(&file, &bytes)
        .map_err(|e| format!("cannot write {}: {e}; check free space in the temp folder", file.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    ui.end(Kind::Ok, "verified SHA-256");
    Ok((file, rel.tag))
}

/// Writes `bytes` to a file that must not exist yet (never follows a planted link), private to this user.
fn write_new(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(file)?.write_all(bytes)
}

/// A failed download: what to tell, and whether trying again can help.
struct FetchError {
    text: String,
    retry: bool,
}

impl From<reqwest::Error> for FetchError {
    fn from(e: reqwest::Error) -> Self {
        let retry = retryable(e.status().map(|s| s.as_u16()), e.is_timeout(), e.is_connect());
        let text = short_error(&e.to_string());
        let text = if e.is_timeout() {
            format!("timed out: {text}")
        } else if e.is_connect() {
            format!("cannot connect: {text}")
        } else {
            text
        };
        FetchError { text, retry }
    }
}

/// Tries per file (the first plus two more) and the wait before each of the later ones.
const TRIES: usize = 3;
const BACKOFF: [Duration; TRIES - 1] = [Duration::from_secs(1), Duration::from_secs(3)];

/// Whether a failed download is worth another try: a server error (5xx), a timeout or a connection
/// failure. Client errors (4xx) and everything else (such as a wrong checksum) will not change.
fn retryable(status: Option<u16>, timeout: bool, connect: bool) -> bool {
    match status {
        Some(s) => (500..600).contains(&s),
        None => timeout || connect,
    }
}

/// Runs `op`, again after a short wait while it fails in a way that can pass; the last error otherwise.
async fn with_retry<T, F, Fut>(ui: &Ui, mut op: F) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, FetchError>>,
{
    let mut attempt = 0;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) if e.retry && attempt < BACKOFF.len() => {
                ui.note(Kind::Dim, &format!("retrying ({}/{TRIES}): {}", attempt + 2, e.text));
                tokio::time::sleep(BACKOFF[attempt]).await;
                attempt += 1;
            }
            Err(e) => return Err(e.text),
        }
    }
}

/// Gets one file; with a `ui`, redraws the progress line as the body arrives.
/// Stops with an error as soon as more than `cap` bytes are announced or have arrived, whatever the server says.
async fn fetch(
    client: &reqwest::Client,
    url: &str,
    ui: Option<&Ui>,
    cap: u64,
    what: &str,
) -> Result<Vec<u8>, FetchError> {
    let too_big = || {
        FetchError {
        text: format!(
            "{what} is larger than {} and was not downloaded; try again later, or build from source if it keeps happening",
            megabytes(cap)
        ),
        retry: false,
    }
    };
    let mut r = client.get(url).send().await.and_then(|r| r.error_for_status())?;
    let total = r.content_length();
    if total.is_some_and(|t| t > cap) {
        return Err(too_big());
    }
    let mut body = Vec::with_capacity(total.unwrap_or(0).min(cap).min(64 << 20) as usize);
    if let Some(ui) = ui {
        ui.progress(0, total);
    }
    while let Some(chunk) = r.chunk().await? {
        if body.len() as u64 + chunk.len() as u64 > cap {
            return Err(too_big());
        }
        body.extend_from_slice(&chunk);
        if let Some(ui) = ui {
            ui.progress(body.len() as u64, total);
        }
    }
    Ok(body)
}

/// Runs `cargo install` from the repository into `stage`, keeping cargo's output out of the way: one live
/// line shows the crate it is on. Its messages are shown only if it fails. Gives cargo's exit status and
/// the commit it built (when it said which).
fn build(ui: &Ui, stage: &Path) -> std::io::Result<(std::process::ExitStatus, Option<String>)> {
    use std::io::BufRead;
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};
    let mut child = std::process::Command::new("cargo")
        .args(["install", "--locked", "--git", REPO, "nexo-cli", "--root"])
        .arg(stage)
        .stderr(Stdio::piped())
        .spawn()?;
    let seen = Arc::new(Mutex::new(Seen::default()));
    let reader = child.stderr.take().map(|err| {
        let seen = Arc::clone(&seen);
        std::thread::spawn(move || {
            let mut err = std::io::BufReader::new(err);
            let mut raw = Vec::new();
            // bytes, not `lines()`: a line that is not UTF-8 must not stop the reading and fill the pipe
            while err.read_until(b'\n', &mut raw).is_ok_and(|n| n > 0) {
                seen.lock().unwrap_or_else(|e| e.into_inner()).take(&String::from_utf8_lossy(&raw));
                raw.clear();
            }
        })
    });
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        ui.detail(&seen.lock().unwrap_or_else(|e| e.into_inner()).krate);
        std::thread::sleep(Duration::from_millis(100));
    };
    if let Some(r) = reader {
        let _ = r.join();
    }
    let seen = seen.lock().unwrap_or_else(|e| e.into_inner());
    if !status.success() {
        ui.abort();
        for l in &seen.tail {
            eprintln!("{l}");
        }
    }
    Ok((status, seen.rev.clone()))
}

/// What is read from cargo's messages: the crate it is on, the commit it builds, and the last lines (shown on failure).
#[derive(Default)]
struct Seen {
    krate: String,
    rev: Option<String>,
    tail: std::collections::VecDeque<String>,
}

impl Seen {
    const TAIL: usize = 60;

    fn take(&mut self, line: &str) {
        let line = line.trim_end();
        if let Some(k) = compiling(line) {
            self.krate = k.to_string();
        }
        if let Some(r) = git_rev(line) {
            self.rev = Some(r.to_string());
        }
        if self.tail.len() == Self::TAIL {
            self.tail.pop_front();
        }
        self.tail.push_back(line.to_string());
    }
}

/// The crate in a cargo `Compiling <name> v<version> ...` line.
fn compiling(line: &str) -> Option<&str> {
    line.trim_start().strip_prefix("Compiling ")?.split_whitespace().next()
}

/// The short commit in a cargo line that names the repository (`... (https://github.com/Locotir/nexo#4b4f255a)`).
fn git_rev(line: &str) -> Option<&str> {
    let rest = &line[line.find(REPO)? + REPO.len()..];
    let hex = rest.strip_prefix('#')?;
    let end = hex.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(hex.len());
    (end >= 7).then(|| &hex[..7])
}

/// Where the output goes and how it looks. On a terminal each step is a spinner line that a background
/// ticker redraws about 12 times a second (and only while a step runs) and that turns into a `✓` line when
/// the step ends; anywhere else (a pipe, `NO_COLOR`, `TERM=dumb`) it is plain lines with no control codes.
struct Ui {
    fancy: bool,
    shared: std::sync::Arc<Shared>,
    ticker: Option<std::thread::JoinHandle<()>>,
}

struct Shared {
    live: std::sync::Mutex<Option<Live>>,
    stop: std::sync::atomic::AtomicBool,
}

/// The step on screen: its label, when it began, an optional detail (the crate cargo is on) and download progress.
struct Live {
    label: String,
    since: Instant,
    detail: String,
    bytes: Option<(u64, Option<u64>)>,
}

/// One frame of the ticker: about 12 per second, the one rate nexo moves at.
const FRAME: Duration = Duration::from_millis(83);

impl Ui {
    /// Fancy when stdout and stderr are terminals that take escape codes, `NO_COLOR` is not set and `TERM` is not `dumb`.
    fn detect() -> Self {
        use std::io::IsTerminal;
        let dumb = std::env::var("TERM").is_ok_and(|t| t == "dumb");
        let tty = std::io::stderr().is_terminal()
            && std::io::stdout().is_terminal()
            && std::env::var_os("NO_COLOR").is_none()
            && !dumb;
        #[cfg(windows)]
        let tty = tty && crossterm::ansi_support::supports_ansi();
        Self::new(tty)
    }

    fn new(fancy: bool) -> Self {
        let shared = std::sync::Arc::new(Shared { live: std::sync::Mutex::new(None), stop: false.into() });
        let ticker = fancy.then(|| {
            eprint!("{HIDE_CURSOR}");
            let shared = std::sync::Arc::clone(&shared);
            std::thread::spawn(move || {
                while !shared.stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(FRAME);
                    shared.draw();
                }
            })
        });
        Ui { fancy, shared, ticker }
    }

    /// Starts a step. Plain mode says nothing unless `announce` (a step that takes minutes).
    fn begin(&self, label: &str, announce: bool) {
        if self.fancy {
            let live = Live { label: label.into(), since: Instant::now(), detail: String::new(), bytes: None };
            *self.shared.lock() = Some(live);
            self.shared.draw();
        } else if announce {
            println!("{}", styled(false, Kind::Step, label));
        }
    }

    /// Ends the step: the spinner line becomes `msg` as a finished line of `kind`.
    fn end(&self, kind: Kind, msg: &str) {
        self.note(kind, msg);
        *self.shared.lock() = None;
    }

    /// Prints a line above the running step.
    fn note(&self, kind: Kind, msg: &str) {
        if self.fancy {
            let live = self.shared.lock();
            let text = styled(true, kind, msg);
            let mut e = std::io::stderr();
            use std::io::Write;
            let _ = write!(e, "\r{text}{CLEAR_REST}\n").and_then(|()| e.flush());
            drop(live);
            self.shared.draw();
        } else if matches!(kind, Kind::Fail) {
            eprintln!("{}", styled(false, kind, msg));
        } else {
            println!("{}", styled(false, kind, msg));
        }
    }

    /// Wipes the live line without a result (the caller is about to fail or hand over the terminal).
    fn abort(&self) {
        *self.shared.lock() = None;
        if self.fancy {
            eprint!("\r{CLEAR_LINE}");
        }
    }

    fn progress(&self, done: u64, total: Option<u64>) {
        if let Some(l) = self.shared.lock().as_mut() {
            l.bytes = Some((done, total));
        }
    }

    fn detail(&self, text: &str) {
        if let Some(l) = self.shared.lock().as_mut() {
            if l.detail != text {
                l.detail = text.into();
            }
        }
    }

    /// On Ctrl+C: wipes the live line, brings the cursor back, removes `stage` and leaves with 130.
    fn on_interrupt(&self, stage: &Path) {
        if !self.fancy {
            return;
        }
        let stage = stage.to_path_buf();
        std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
            if rt.block_on(tokio::signal::ctrl_c()).is_ok() {
                eprint!("\r{CLEAR_LINE}{SHOW_CURSOR}");
                eprintln!("{}", styled(true, Kind::Warn, "update cancelled"));
                let _ = std::fs::remove_dir_all(stage);
                std::process::exit(130);
            }
        });
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        self.shared.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.ticker.take() {
            let _ = t.join();
            eprint!("\r{CLEAR_LINE}{SHOW_CURSOR}");
        }
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Live>> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Redraws the live line if a step is running (one write, so a frame never shows half drawn).
    fn draw(&self) {
        let live = self.lock();
        let Some(l) = live.as_ref() else { return };
        let cols = crossterm::terminal::size().map_or(80, |(c, _)| c as usize);
        let frame = (l.since.elapsed().as_millis() / FRAME.as_millis()) as usize;
        let text = live_line(frame, &l.label, &l.detail, l.since.elapsed().as_secs_f64(), l.bytes, cols);
        use std::io::Write;
        let mut e = std::io::stderr();
        let _ = write!(e, "\r{CYAN}{text}{RESET}{CLEAR_REST}").and_then(|()| e.flush());
    }
}

const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";
const CLEAR_REST: &str = "\x1b[K";
const CLEAR_LINE: &str = "\x1b[2K";
const HIDE_CURSOR: &str = "\x1b[?25l";
const SHOW_CURSOR: &str = "\x1b[?25h";

/// The kinds of line `nexo update` prints.
#[derive(Clone, Copy)]
enum Kind {
    /// A step that begins (plain mode only).
    Step,
    /// A step that ended well.
    Ok,
    /// Something to know, not an error.
    Warn,
    /// A side remark.
    Dim,
    /// A step that failed.
    Fail,
    /// An update is waiting (`--check`): a warning with a symbol, a bare line in plain mode.
    Notice,
    /// The one closing line: where nexo is now.
    Final,
}

/// A line as shown: with colors and symbols when `fancy`, else plain `nexo: <message>` with no escape codes
/// (the closing line has no prefix).
fn styled(fancy: bool, kind: Kind, msg: &str) -> String {
    if !fancy {
        return if matches!(kind, Kind::Final | Kind::Notice) { msg.to_string() } else { format!("nexo: {msg}") };
    }
    match kind {
        Kind::Step => format!("nexo {CYAN}▸{RESET} {msg}"),
        Kind::Ok => format!("{GREEN}✓{RESET} {msg}"),
        Kind::Final => format!("{GREEN}✓{RESET} {msg}"),
        Kind::Warn | Kind::Notice => format!("{YELLOW}⚠{RESET} {msg}"),
        Kind::Fail => format!("{RED}✗{RESET} {msg}"),
        Kind::Dim => format!("{DIM}  {msg}{RESET}"),
    }
}

/// The last line: where nexo was and where it is now (each left out when unknown).
fn update_text(new: Option<&str>, old: &str) -> String {
    let old = if old == "unknown" { String::new() } else { format!(" ({})", short(old)) };
    let new = new.map_or("the latest build".to_string(), |n| short(n).to_string());
    format!("nexo {}{old} → {new}. Restart nexo.", env!("CARGO_PKG_VERSION"))
}

fn secs_text(d: Duration) -> String {
    format!("{} s", d.as_secs_f64().round() as u64)
}

fn megabytes(n: u64) -> String {
    format!("{:.1} MB", n as f64 / 1e6)
}

/// The live line: a spinner frame, the step, then the elapsed seconds (fixed width, so the line never shifts);
/// for a download a bar, percent, done of total and speed, which drops its bar and then its tail as the
/// terminal narrows; without a known length, bytes and speed. Never longer than `cols - 1`.
fn live_line(
    frame: usize,
    label: &str,
    detail: &str,
    secs: f64,
    bytes: Option<(u64, Option<u64>)>,
    cols: usize,
) -> String {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let spin = FRAMES[frame % FRAMES.len()];
    let tail = match bytes {
        None => {
            let d = if detail.is_empty() { String::new() } else { format!("  {detail}") };
            format!("{:>3} s{d}", secs as u64)
        }
        Some((done, total)) => {
            let speed = format!("{:>8}/s", megabytes((done as f64 / secs.max(0.001)) as u64));
            match total.filter(|t| *t > 0) {
                Some(t) => {
                    let frac = (done as f64 / t as f64).clamp(0.0, 1.0);
                    let w = megabytes(t).len();
                    let stats =
                        format!("{:>3}%  {:>w$} / {}  {speed}", (frac * 100.0) as u32, megabytes(done), megabytes(t));
                    let room = cols.saturating_sub(1 + 2 + label.chars().count() + 2 + stats.chars().count() + 2);
                    let width = room.min(24);
                    if width >= 8 {
                        let full = (frac * width as f64) as usize;
                        format!("{}{}  {stats}", "█".repeat(full), "░".repeat(width - full))
                    } else {
                        stats
                    }
                }
                None => format!("{:>8}  {speed}", megabytes(done)),
            }
        }
    };
    let line = format!("{spin} {label}  {tail}");
    line.chars().take(cols.saturating_sub(1).max(1)).collect()
}

/// A reqwest error without the long address: `... for url (https://host/a/b/FILE)` becomes `... (FILE)`, and
/// `HTTP status server error (504 Gateway Timeout)` becomes `HTTP 504 Gateway Timeout`.
fn short_error(e: &str) -> String {
    let Some((what, rest)) = e.split_once(" for url (") else { return e.to_string() };
    let url = rest.strip_suffix(')').unwrap_or(rest);
    let file = url.split(['?', '#']).next().unwrap_or(url).rsplit('/').next().unwrap_or(url);
    let what = match (what.starts_with("HTTP status "), what.split_once('(')) {
        (true, Some((_, code))) => format!("HTTP {}", code.trim_end_matches(')')),
        _ => what.to_string(),
    };
    format!("{what} ({file})")
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
    place_with(new, exe, cfg!(windows), |from, to| std::fs::rename(from, to))
}

/// [`place`] with the operating system and the renaming made explicit. When the new file cannot take
/// the place (a virus scanner holding it, on Windows), the old program is put back: nexo is never left missing.
fn place_with(
    new: &Path,
    exe: &Path,
    windows: bool,
    rename: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let next = exe.with_extension("new");
    std::fs::copy(new, &next)?;
    let old = exe.with_extension("old");
    let moved = windows && exe.exists();
    if moved {
        let _ = std::fs::remove_file(&old);
        if let Err(e) = rename(exe, &old) {
            let _ = std::fs::remove_file(&next);
            return Err(e);
        }
    }
    if let Err(e) = rename(&next, exe) {
        if moved {
            let _ = rename(&old, exe);
        }
        let _ = std::fs::remove_file(&next);
        return Err(e);
    }
    Ok(())
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
    fn a_failed_replacement_on_windows_puts_the_old_program_back() {
        let d = tempfile::tempdir().unwrap();
        let (exe, new) = (d.path().join("nexo.exe"), d.path().join("fresh"));
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        // the last step fails, as when a virus scanner holds the new file
        let refuse_new = |from: &Path, to: &Path| {
            if from.extension().is_some_and(|e| e == "new") {
                Err(std::io::Error::other("held by another program"))
            } else {
                std::fs::rename(from, to)
            }
        };
        assert!(place_with(&new, &exe, true, refuse_new).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"old", "nexo is still there");
        assert!(!exe.with_extension("new").exists(), "no half-placed file is left");
        // and when it works, the new program is in place with the old one beside it
        place_with(&new, &exe, true, |a, b| std::fs::rename(a, b)).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(exe.with_extension("old")).unwrap(), b"old");
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

    #[test]
    fn only_server_errors_timeouts_and_connection_failures_are_tried_again() {
        assert!(retryable(Some(500), false, false));
        assert!(retryable(Some(504), false, false));
        assert!(retryable(None, true, false));
        assert!(retryable(None, false, true));
        assert!(!retryable(Some(404), false, false));
        assert!(!retryable(Some(403), true, true));
        assert!(!retryable(Some(302), false, false));
        assert!(!retryable(None, false, false));
    }

    #[test]
    fn the_live_line_fits_the_terminal_and_never_shifts() {
        let bar = |cols| live_line(0, "downloading x", "", 2.5, Some((12_500_000, Some(25_000_000))), cols);
        let l = bar(100);
        assert!(l.contains(" 50%") && l.contains("12.5 MB / 25.0 MB") && l.contains("5.0 MB/s"), "{l}");
        assert_eq!(l.matches('█').count(), 12);
        assert_eq!(l.matches('░').count(), 12);
        // narrower terminals lose the bar first, then are cut, and never reach the last column
        assert!(!bar(50).contains('░'));
        for cols in [10, 30, 50, 80, 120] {
            assert!(bar(cols).chars().count() < cols, "{cols}");
        }
        // unknown length: bytes and speed
        let l = live_line(0, "downloading x", "", 1.0, Some((3_000_000, None)), 80);
        assert!(l.contains("3.0 MB") && l.contains("3.0 MB/s") && !l.contains('%'), "{l}");
        // zero length or an instant start must not divide by zero
        assert!(live_line(0, "x", "", 0.0, Some((10, Some(0))), 80).contains("MB/s"));
        assert!(live_line(0, "x", "", 0.0, Some((60, Some(50))), 80).contains("100%"));
        // every spinner frame has the same width and the seconds field has a fixed width
        let widths: Vec<_> = (0..10).map(|f| live_line(f, "x", "", 9.0, None, 80).chars().count()).collect();
        assert!(widths.windows(2).all(|w| w[0] == w[1]));
        assert_eq!(
            live_line(0, "x", "", 9.0, None, 80).chars().count(),
            live_line(0, "x", "", 10.0, None, 80).chars().count()
        );
        assert_eq!(live_line(0, "building", "tokio", 7.0, None, 80), "⠋ building    7 s  tokio");
    }

    #[test]
    fn cargo_lines_name_the_crate_and_the_commit() {
        assert_eq!(compiling("   Compiling tokio v1.54.0"), Some("tokio"));
        assert_eq!(compiling("Compiling nexo-cli v0.1.0 (https://github.com/Locotir/nexo#4b4f255a)"), Some("nexo-cli"));
        assert_eq!(compiling("Downloaded tokio v1.54.0"), None);
        assert_eq!(
            git_rev("Installing nexo-cli v0.1.0 (https://github.com/Locotir/nexo#4b4f255a1c2d)"),
            Some("4b4f255")
        );
        assert_eq!(git_rev("Updating git repository `https://github.com/Locotir/nexo`"), None);
    }

    #[test]
    fn plain_lines_carry_no_escape_codes_and_fancy_ones_do() {
        for kind in [Kind::Step, Kind::Ok, Kind::Warn, Kind::Dim, Kind::Fail] {
            let plain = styled(false, kind, "downloading x");
            assert_eq!(plain, "nexo: downloading x");
            assert!(!plain.contains('\x1b') && !plain.contains('\r'));
            assert!(styled(true, kind, "downloading x").contains('\x1b'));
        }
        assert!(styled(true, Kind::Step, "a").contains("\x1b[36m▸"));
        assert!(styled(true, Kind::Ok, "a").starts_with("\x1b[32m✓"));
        assert!(styled(true, Kind::Fail, "a").starts_with("\x1b[31m✗"));
        assert!(styled(true, Kind::Warn, "a").starts_with("\x1b[33m⚠"));
    }

    #[test]
    fn the_last_line_says_where_nexo_was_and_is_and_leaves_out_what_is_unknown() {
        let v = env!("CARGO_PKG_VERSION");
        assert_eq!(
            update_text(Some("4b4f255a"), "153ab70ffff"),
            format!("nexo {v} (153ab70) → 4b4f255. Restart nexo.")
        );
        assert_eq!(update_text(None, "unknown"), format!("nexo {v} → the latest build. Restart nexo."));
        assert_eq!(styled(false, Kind::Final, "x"), "x");
    }

    #[test]
    fn an_error_is_told_with_its_status_and_file_not_the_whole_address() {
        let long = "HTTP status server error (504 Gateway Timeout) for url \
                    (https://github.com/Locotir/nexo/releases/download/build-6553da6/SHA256SUMS)";
        assert_eq!(short_error(long), "HTTP 504 Gateway Timeout (SHA256SUMS)");
        let sent = "error sending request for url (https://github.com/x/y/nexo-linux-x86_64?a=b)";
        assert_eq!(short_error(sent), "error sending request (nexo-linux-x86_64)");
        assert_eq!(
            short_error("nexo-linux-x86_64 does not match its SHA-256"),
            "nexo-linux-x86_64 does not match its SHA-256"
        );
    }

    /// A local web server: each request path gets the raw response registered for it (404 otherwise); every
    /// requested path is recorded. `routes` is given the server's own address, for redirects.
    struct Mock {
        base: String,
        hits: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Mock {
        async fn start(routes: impl FnOnce(&str) -> Vec<(String, String)>) -> Mock {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let routes = routes(&base);
            let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = hits.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((mut sock, _)) = listener.accept().await else { return };
                    let (routes, seen) = (routes.clone(), seen.clone());
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 4096];
                        let n = sock.read(&mut buf).await.unwrap_or(0);
                        let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                        let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                        let reply = routes.iter().find(|(p, _)| *p == path).map(|(_, r)| r.clone());
                        seen.lock().unwrap().push(path);
                        let reply = reply.unwrap_or_else(|| reply_with("404 Not Found", &[], ""));
                        let _ = sock.write_all(reply.as_bytes()).await;
                        let _ = sock.shutdown().await;
                    });
                }
            });
            Mock { base, hits }
        }

        fn sources(&self) -> Sources {
            Sources { web: self.base.clone(), api: format!("{}/api", self.base) }
        }

        fn api_calls(&self) -> usize {
            self.hits.lock().unwrap().iter().filter(|p| p.starts_with("/api")).count()
        }
    }

    fn reply_with(status: &str, headers: &[(&str, String)], body: &str) -> String {
        let h: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
        format!("HTTP/1.1 {status}\r\n{h}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    fn redirect(base: &str, tag: &str) -> (String, String) {
        let loc = format!("{base}/releases/tag/{tag}");
        ("/releases/latest".to_string(), reply_with("302 Found", &[("Location", loc)], ""))
    }

    fn rate_limited(reset: u64) -> String {
        let h = [("x-ratelimit-remaining", "0".to_string()), ("x-ratelimit-reset", reset.to_string())];
        reply_with("403 Forbidden", &h, "{\"message\":\"API rate limit exceeded\"}")
    }

    #[tokio::test]
    async fn the_release_is_found_from_the_web_redirect_with_no_api_call() {
        let m = Mock::start(|b| vec![redirect(b, "build-abc1234")]).await;
        let (c, p) = (client(5).unwrap(), probe(5).unwrap());
        let rel = latest_release(&c, &p, &m.sources(), &["nexo-linux-x86_64", SUMS]).await.unwrap();
        assert_eq!(rel.tag, "build-abc1234");
        let want = format!("{}/releases/download/build-abc1234/SHA256SUMS", m.base);
        assert!(rel.files.contains(&(SUMS.to_string(), want)), "{:?}", rel.files);
        assert_eq!(rel.files.len(), 2);
        assert_eq!(m.api_calls(), 0);
    }

    #[tokio::test]
    async fn when_the_redirect_gives_nothing_the_api_is_the_second_try() {
        let body = r#"{"tag_name":"build-def5678","assets":[{"name":"SHA256SUMS","browser_download_url":"https://x/SHA256SUMS"}]}"#;
        let ok = reply_with("200 OK", &[("Content-Type", "application/json".into())], body);
        let m = Mock::start(|_| vec![("/api/releases/latest".to_string(), ok)]).await;
        let (c, p) = (client(5).unwrap(), probe(5).unwrap());
        let rel = latest_release(&c, &p, &m.sources(), &["nexo-linux-x86_64", SUMS]).await.unwrap();
        assert_eq!(rel.tag, "build-def5678");
        assert_eq!(
            rel.files,
            vec![(SUMS.to_string(), format!("{}/releases/download/build-def5678/SHA256SUMS", m.base))]
        );
        assert_eq!(m.api_calls(), 1);
    }

    #[tokio::test]
    async fn the_api_never_chooses_the_download_host_or_path() {
        let body = r#"{"tag_name":"build-def5678","assets":[{"name":"SHA256SUMS","browser_download_url":"https://evil.example/SHA256SUMS"}]}"#;
        let ok = reply_with("200 OK", &[("Content-Type", "application/json".into())], body);
        let bad = r#"{"tag_name":"..","assets":[{"name":"SHA256SUMS","browser_download_url":"https://x/a"}]}"#;
        let bad = reply_with("200 OK", &[("Content-Type", "application/json".into())], bad);
        let m = Mock::start(|_| vec![("/api/releases/latest".to_string(), ok)]).await;
        let (c, p) = (client(5).unwrap(), probe(5).unwrap());
        let rel = latest_release(&c, &p, &m.sources(), &[SUMS]).await.unwrap();
        assert!(rel.files.iter().all(|(_, u)| u.starts_with(&m.base)), "{:?}", rel.files);
        let m = Mock::start(|_| vec![("/api/releases/latest".to_string(), bad)]).await;
        assert!(latest_release(&c, &p, &m.sources(), &[SUMS]).await.is_err());
    }

    #[tokio::test]
    async fn a_used_up_hourly_limit_is_named_with_the_minutes_left() {
        let reset = now_secs() + 590;
        let m = Mock::start(|_| vec![("/api/releases/latest".to_string(), rate_limited(reset))]).await;
        let (c, p) = (client(5).unwrap(), probe(5).unwrap());
        let e = latest_release(&c, &p, &m.sources(), &[SUMS]).await.err().unwrap();
        assert_eq!(e, "GitHub's hourly limit for this network is used up; it resets in 10 min");
        // a 403 that is not the limit keeps the plain reason
        let plain =
            Mock::start(|_| vec![("/api/releases/latest".to_string(), reply_with("403 Forbidden", &[], ""))]).await;
        let e = latest_release(&c, &p, &plain.sources(), &[SUMS]).await.err().unwrap();
        assert_eq!(e, NO_RELEASE);
    }

    #[tokio::test]
    async fn a_release_tag_equal_to_the_running_commit_is_up_to_date_without_the_api() {
        let m = Mock::start(|b| vec![redirect(b, "build-abc1234")]).await;
        assert_eq!(status("abc1234deadbeefabc1234deadbeefabc1234dea", &m.sources()).await, Some(None));
        assert_eq!(m.api_calls(), 0);
    }

    #[tokio::test]
    async fn a_newer_release_is_told_without_a_count_when_the_api_cannot_give_one() {
        let m = Mock::start(|b| {
            vec![redirect(b, "build-def5678"), ("/api/compare/abc1234...build-def5678".into(), rate_limited(0))]
        })
        .await;
        let got = status("abc1234", &m.sources()).await;
        assert_eq!(got, Some(Some(Newer { tag: "build-def5678".into(), count: None })));
        // and with the count when the API answers
        let ok = reply_with("200 OK", &[], r#"{"ahead_by":3}"#);
        let m =
            Mock::start(|b| vec![redirect(b, "build-def5678"), ("/api/compare/abc1234...build-def5678".into(), ok)])
                .await;
        let got = status("abc1234", &m.sources()).await;
        assert_eq!(got, Some(Some(Newer { tag: "build-def5678".into(), count: Some(3) })));
    }

    fn route(tag: &str, file: &str, reply: String) -> (String, String) {
        (format!("/releases/download/{tag}/{file}"), reply)
    }

    /// A release with `bin` as the file `nexo-test` and a SHA-256 list that is right for it.
    fn release(bin_reply: impl Fn(&str) -> String, bin: &str) -> impl FnOnce(&str) -> Vec<(String, String)> {
        let sums = format!("{}  nexo-test\n", sha256_hex(bin.as_bytes()));
        let bin_reply = bin_reply(bin);
        move |b| {
            vec![
                redirect(b, "build-t1"),
                route("build-t1", SUMS, reply_with("200 OK", &[], &sums)),
                route("build-t1", "nexo-test", bin_reply),
            ]
        }
    }

    const SMALL: Caps = Caps { file: 1000, sums: 1000 };

    fn left_in(d: &Path) -> usize {
        std::fs::read_dir(d).unwrap().count()
    }

    #[tokio::test]
    async fn a_normal_download_is_verified_and_kept_in_a_private_folder() {
        let m = Mock::start(release(|b| reply_with("200 OK", &[], b), "binary")).await;
        let stage = private_stage().unwrap();
        let (file, tag) = download(&Ui::new(false), &m.sources(), "nexo-test", stage.path(), SMALL).await.unwrap();
        assert_eq!(tag, "build-t1");
        assert_eq!(std::fs::read(&file).unwrap(), b"binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(stage.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
    }

    #[tokio::test]
    async fn a_file_over_the_cap_is_refused_and_leaves_nothing() {
        let big = "x".repeat(2000);
        let m = Mock::start(release(|b| reply_with("200 OK", &[], b), &big)).await;
        let stage = private_stage().unwrap();
        let e = download(&Ui::new(false), &m.sources(), "nexo-test", stage.path(), SMALL).await.unwrap_err();
        assert!(e.contains("nexo-test is larger than") && !e.contains('\n'), "{e}");
        assert_eq!(left_in(stage.path()), 0);
    }

    #[tokio::test]
    async fn the_cap_holds_with_no_length_or_a_false_one() {
        let big = "x".repeat(5000);
        let none = |b: &str| format!("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{b}");
        let lie = |b: &str| format!("HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n{b}");
        for reply in [&none as &dyn Fn(&str) -> String, &lie] {
            let m = Mock::start(release(|b| reply(b), &big)).await;
            let stage = private_stage().unwrap();
            let r = download(&Ui::new(false), &m.sources(), "nexo-test", stage.path(), SMALL).await;
            assert!(r.is_err(), "{r:?}");
            assert_eq!(left_in(stage.path()), 0);
        }
    }

    #[tokio::test]
    async fn a_checksum_list_over_its_cap_is_refused() {
        let m = Mock::start(|b| {
            vec![redirect(b, "build-t1"), route("build-t1", SUMS, reply_with("200 OK", &[], &"y".repeat(3000)))]
        })
        .await;
        let stage = private_stage().unwrap();
        let e = download(&Ui::new(false), &m.sources(), "nexo-test", stage.path(), SMALL).await.unwrap_err();
        assert!(e.starts_with("SHA256SUMS is larger than"), "{e}");
    }

    #[test]
    fn the_work_folder_name_is_different_each_run() {
        let (a, b) = (private_stage().unwrap(), private_stage().unwrap());
        assert_ne!(a.path(), b.path());
        let name = a.path().file_name().unwrap().to_string_lossy().into_owned();
        assert_ne!(name, format!("nexo-build-{}", std::process::id()));
    }

    #[test]
    fn tags_and_limits_are_read_strictly() {
        assert_eq!(tag_of("https://github.com/Locotir/nexo/releases/tag/build-abc1234"), Some("build-abc1234".into()));
        assert_eq!(tag_of("/Locotir/nexo/releases/tag/v1.2?x=1"), Some("v1.2".into()));
        assert_eq!(tag_of("https://github.com/Locotir/nexo/releases"), None);
        assert_eq!(tag_of("https://github.com/Locotir/nexo/releases/tag/.."), None);
        assert_eq!(tag_of("/Locotir/nexo/releases/tag/."), None);
        assert_eq!(tag_of("https://github.com/x/releases/tag/a%2F..%2Fb"), None);
        assert!(tag_is_commit("build-abc1234", "abc1234ffff") && !tag_is_commit("build-abc1234", "abc1235ffff"));
        assert!(!tag_is_commit("v1", "v1") && !tag_is_commit("build-abc", "abc"));
        assert_eq!(
            limit_text(429, Some("0"), Some("1060"), 1000).unwrap(),
            "GitHub's hourly limit for this network is used up; it resets in 1 min"
        );
        assert_eq!(
            limit_text(403, Some("0"), Some("1000"), 1000).unwrap(),
            "GitHub's hourly limit for this network is used up; it resets in 1 min"
        );
        assert!(limit_text(403, Some("12"), Some("1"), 0).is_none() && limit_text(404, Some("0"), None, 0).is_none());
        assert!(limit_text(403, Some("0"), None, 0).unwrap().ends_with("try again later"));
    }
}
