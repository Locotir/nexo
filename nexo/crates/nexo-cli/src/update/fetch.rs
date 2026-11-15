use super::*;

/// A new work folder with a random name, made exclusively and readable by this user alone (0700 on Unix),
/// so no other local user can guess it or plant something in it. It is removed when dropped.
pub(super) fn private_stage() -> std::io::Result<tempfile::TempDir> {
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
pub(super) struct Caps {
    pub(super) file: u64,
    pub(super) sums: u64,
}

impl Caps {
    pub(super) const LIVE: Caps = Caps { file: 256 << 20, sums: 1 << 20 };
}

/// Downloads `name` from the latest release into `stage` and checks it against the
/// release's SHA-256 list: the file and the release tag, or why not.
pub(super) async fn download(
    ui: &Ui,
    src: &Sources,
    name: &str,
    stage: &Path,
    caps: Caps,
) -> Result<(PathBuf, String), String> {
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
pub(super) fn write_new(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(file)?.write_all(bytes)
}

/// A failed download: what to tell, and whether trying again can help.
pub(super) struct FetchError {
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
pub(super) const TRIES: usize = 3;
pub(super) const BACKOFF: [Duration; TRIES - 1] = [Duration::from_secs(1), Duration::from_secs(3)];

/// Whether a failed download is worth another try: a server error (5xx), a timeout or a connection
/// failure. Client errors (4xx) and everything else (such as a wrong checksum) will not change.
pub(super) fn retryable(status: Option<u16>, timeout: bool, connect: bool) -> bool {
    match status {
        Some(s) => (500..600).contains(&s),
        None => timeout || connect,
    }
}

/// Runs `op`, again after a short wait while it fails in a way that can pass; the last error otherwise.
pub(super) async fn with_retry<T, F, Fut>(ui: &Ui, mut op: F) -> Result<T, String>
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
pub(super) async fn fetch(
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
