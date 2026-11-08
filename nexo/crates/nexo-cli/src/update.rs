//! Updates: when the interface starts and every [`EVERY`] after, nexo asks GitHub (in the background) how many commits
//! `main` has past the one it was built from, and says so; `nexo update` builds the latest with cargo.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often a running interface asks again whether a newer nexo is out.
pub const EVERY: Duration = Duration::from_secs(60 * 60);

pub const REPO: &str = "https://github.com/Locotir/nexo";
const API: &str = "https://api.github.com/repos/Locotir/nexo";
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
    let when = env!("NEXO_COMMIT_TIME");
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
    let ui = Ui::detect();
    let started = Instant::now();
    // the ready-built release first: seconds, and no compiler needed
    let stage = std::env::temp_dir().join(format!("nexo-build-{}", std::process::id()));
    match download(&ui, &stage).await {
        Ok((file, tag)) => {
            let placed = place(&file, &exe);
            let _ = std::fs::remove_dir_all(&stage);
            placed.map_err(|e| anyhow::anyhow!("downloaded, but cannot replace {} ({e})", exe.display()))?;
            ui.ok(&done_text(tag.strip_prefix("build-"), commit(), started.elapsed()));
            return Ok(0);
        }
        Err(e) => ui.warn(&format!("no ready-built release to use ({e}); building from source instead")),
    }
    ui.step(&format!("building the latest nexo from {REPO}"));
    let built = build(&ui, &stage);
    let ok = matches!(built, Ok((ref s, _)) if s.success());
    let placed = ok.then(|| place(&stage.join("bin").join(exe.file_name().unwrap_or_default()), &exe));
    let _ = std::fs::remove_dir_all(&stage);
    match (built, placed) {
        (Ok((_, rev)), Some(Ok(()))) => {
            ui.ok(&done_text(rev.as_deref(), commit(), started.elapsed()));
            Ok(0)
        }
        (_, Some(Err(e))) => anyhow::bail!("built, but cannot replace {} ({e})", exe.display()),
        // On Windows a missing Rust or missing C++ build tools is the usual reason a build cannot start
        // or link: the installer installs what is missing and builds, so nobody has to look it up.
        _ if cfg!(windows) => installer(exe.parent()),
        (Err(e), _) => anyhow::bail!("cannot run cargo ({e}); install Rust from https://rustup.rs"),
        (Ok((s, _)), _) => Ok(s.code().unwrap_or(1)),
    }
}

/// Downloads this machine's file from the latest release into `stage` and checks it against the
/// release's SHA-256 list: the file and the release tag, or why not.
async fn download(ui: &Ui, stage: &Path) -> Result<(PathBuf, String), String> {
    let name = ASSET.ok_or("none is published for this system")?;
    let client = client(300).ok_or("no HTTP client")?;
    let rel = latest_release(&client).await.ok_or("GitHub did not answer with a release")?;
    let url = |n: &str| rel.files.iter().find(|(f, _)| f == n).map(|(_, u)| u.clone());
    let (bin_url, sums_url) =
        (url(name).ok_or(format!("{} has no {name}", rel.tag))?, url(SUMS).ok_or("no SHA256SUMS")?);
    ui.step(&format!("downloading {name} from release {}", rel.tag));
    let sums = with_retry(ui, || fetch(&client, &sums_url, None)).await?;
    let sums = String::from_utf8_lossy(&sums).into_owned();
    let t = Instant::now();
    let bytes = with_retry(ui, || fetch(&client, &bin_url, Some(ui))).await?;
    ui.ok(&format!("downloaded {name} ({}) in {}", megabytes(bytes.len() as u64), secs_text(t.elapsed())));
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
                ui.clear();
                ui.dim(&format!("retrying ({}/{TRIES}): {}", attempt + 2, e.text));
                tokio::time::sleep(BACKOFF[attempt]).await;
                attempt += 1;
            }
            Err(e) => return Err(e.text),
        }
    }
}

/// Gets one file; with a `ui`, redraws the progress line as the body arrives.
async fn fetch(client: &reqwest::Client, url: &str, ui: Option<&Ui>) -> Result<Vec<u8>, FetchError> {
    let mut r = client.get(url).send().await.and_then(|r| r.error_for_status())?;
    let total = r.content_length();
    let mut body = Vec::with_capacity(total.unwrap_or(0).min(256 << 20) as usize);
    let (start, mut last) = (Instant::now(), None::<Instant>);
    while let Some(chunk) = r.chunk().await? {
        body.extend_from_slice(&chunk);
        if let Some(ui) = ui {
            if last.is_none_or(|t| t.elapsed() >= Duration::from_millis(100)) {
                ui.draw(&progress_line(body.len() as u64, total, start.elapsed().as_secs_f64()));
                last = Some(Instant::now());
            }
        }
    }
    if let Some(ui) = ui {
        ui.clear();
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
    let (start, mut frame) = (Instant::now(), 0);
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        let name = seen.lock().unwrap_or_else(|e| e.into_inner()).krate.clone();
        ui.draw(&build_line(frame, start.elapsed().as_secs(), &name));
        frame += 1;
        std::thread::sleep(Duration::from_millis(100));
    };
    if let Some(r) = reader {
        let _ = r.join();
    }
    ui.clear();
    let seen = seen.lock().unwrap_or_else(|e| e.into_inner());
    if !status.success() {
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

/// Where the output goes and how it looks: colors, a redrawn line and a spinner only on a terminal.
struct Ui {
    fancy: bool,
}

impl Ui {
    /// Fancy when stderr is a terminal that takes escape codes and `NO_COLOR` is not set.
    fn detect() -> Self {
        use std::io::IsTerminal;
        let tty = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        #[cfg(windows)]
        let tty = tty && crossterm::ansi_support::supports_ansi();
        Ui { fancy: tty }
    }

    /// A finished line. Fancy ones go to stderr, with the redrawn line; plain ones to stdout, as before.
    fn line(&self, kind: Kind, msg: &str) {
        let text = styled(self.fancy, kind, msg);
        if self.fancy {
            eprintln!("{text}");
        } else {
            println!("{text}");
        }
    }

    fn step(&self, msg: &str) {
        self.line(Kind::Step, msg);
    }

    fn ok(&self, msg: &str) {
        self.line(Kind::Ok, msg);
    }

    fn warn(&self, msg: &str) {
        self.line(Kind::Warn, msg);
    }

    fn dim(&self, msg: &str) {
        self.line(Kind::Dim, msg);
    }

    /// Redraws the one live line (nothing in plain mode).
    fn draw(&self, text: &str) {
        if self.fancy {
            use std::io::Write;
            let mut e = std::io::stderr();
            let _ = write!(e, "\r\x1b[2K{CYAN}{text}{RESET}").and_then(|()| e.flush());
        }
    }

    /// Wipes the live line.
    fn clear(&self) {
        if self.fancy {
            eprint!("\r\x1b[2K");
        }
    }
}

const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

/// The kinds of line `nexo update` prints.
#[derive(Clone, Copy)]
enum Kind {
    Step,
    Ok,
    Warn,
    Dim,
}

/// A line as shown: with colors and symbols when `fancy`, else plain `nexo: <message>` with no escape codes.
fn styled(fancy: bool, kind: Kind, msg: &str) -> String {
    if !fancy {
        return format!("nexo: {msg}");
    }
    match kind {
        Kind::Step => format!("nexo {CYAN}▸{RESET} {msg}"),
        Kind::Ok => format!("{GREEN}✓{RESET} nexo {msg}"),
        Kind::Warn => format!("{YELLOW}⚠{RESET} {msg}"),
        Kind::Dim => format!("{DIM}  {msg}{RESET}"),
    }
}

/// The last line: where nexo is now, where it was (each left out when unknown), and how long it took.
fn done_text(new: Option<&str>, old: &str, took: Duration) -> String {
    let new = new.map(|n| format!(" to {}", short(n))).unwrap_or_default();
    let old = if old == "unknown" { String::new() } else { format!(" (was {})", short(old)) };
    format!("updated{new}{old} in {}. Restart nexo.", secs_text(took))
}

fn secs_text(d: Duration) -> String {
    format!("{} s", d.as_secs_f64().round() as u64)
}

fn megabytes(n: u64) -> String {
    format!("{:.1} MB", n as f64 / 1e6)
}

/// The download line: a bar, percent, MB done of total and speed; without a known length, MB and speed.
fn progress_line(done: u64, total: Option<u64>, secs: f64) -> String {
    let speed = format!("{}/s", megabytes((done as f64 / secs.max(0.001)) as u64));
    match total.filter(|t| *t > 0) {
        Some(t) => {
            const WIDTH: usize = 24;
            let frac = (done as f64 / t as f64).clamp(0.0, 1.0);
            let full = (frac * WIDTH as f64) as usize;
            let bar = format!("{}{}", "█".repeat(full), "░".repeat(WIDTH - full));
            format!("{bar} {:>3}%  {} / {}  {speed}", (frac * 100.0) as u32, megabytes(done), megabytes(t))
        }
        None => format!("{}  {speed}", megabytes(done)),
    }
}

/// The build line: a spinner frame, seconds so far and the crate cargo is on.
fn build_line(frame: usize, secs: u64, krate: &str) -> String {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let on = if krate.is_empty() { String::new() } else { format!("  {krate}") };
    format!("{} building  {secs} s{on}", FRAMES[frame % FRAMES.len()])
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
    fn the_progress_line_shows_a_bar_with_a_known_length_and_megabytes_without() {
        let l = progress_line(12_500_000, Some(25_000_000), 2.5);
        assert!(l.contains(" 50%"), "{l}");
        assert!(l.contains("12.5 MB / 25.0 MB"), "{l}");
        assert!(l.contains("5.0 MB/s"), "{l}");
        assert_eq!(l.matches('█').count(), 12);
        assert_eq!(l.matches('░').count(), 12);
        let l = progress_line(3_000_000, None, 1.0);
        assert_eq!(l, "3.0 MB  3.0 MB/s");
        // a zero length or an instant start must not divide by zero
        assert!(progress_line(10, Some(0), 0.0).contains("MB/s"));
        assert!(progress_line(60, Some(50), 0.0).contains("100%"));
    }

    #[test]
    fn the_build_line_names_the_crate_cargo_is_on() {
        assert_eq!(build_line(0, 7, "tokio"), "⠋ building  7 s  tokio");
        assert_eq!(build_line(10, 0, ""), "⠋ building  0 s");
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
        for kind in [Kind::Step, Kind::Ok, Kind::Warn, Kind::Dim] {
            let plain = styled(false, kind, "downloading x");
            assert_eq!(plain, "nexo: downloading x");
            assert!(!plain.contains('\x1b') && !plain.contains('\r'));
            assert!(styled(true, kind, "downloading x").contains('\x1b'));
        }
        assert!(styled(true, Kind::Step, "a").contains("\x1b[36m▸"));
        assert!(styled(true, Kind::Ok, "a").starts_with("\x1b[32m✓"));
        assert!(styled(true, Kind::Warn, "a").starts_with("\x1b[33m⚠"));
    }

    #[test]
    fn the_last_line_says_where_nexo_is_and_was_and_leaves_out_what_is_unknown() {
        let t = Duration::from_secs(41);
        assert_eq!(
            done_text(Some("4b4f255a"), "153ab70ffff", t),
            "updated to 4b4f255 (was 153ab70) in 41 s. Restart nexo."
        );
        assert_eq!(done_text(None, "153ab70", t), "updated (was 153ab70) in 41 s. Restart nexo.");
        assert_eq!(done_text(Some("4b4f255"), "unknown", t), "updated to 4b4f255 in 41 s. Restart nexo.");
        assert_eq!(
            styled(true, Kind::Ok, &done_text(None, "unknown", t)),
            "\x1b[32m✓\x1b[0m nexo updated in 41 s. Restart nexo."
        );
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
}
