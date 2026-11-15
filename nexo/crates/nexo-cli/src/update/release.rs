use super::*;

/// The latest release: its tag and its files (name, download address).
pub(super) struct Release {
    pub(super) tag: String,
    pub(super) files: Vec<(String, String)>,
}

pub(super) fn parse_release(v: &serde_json::Value) -> Option<Release> {
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
pub(super) fn plain_tag(t: &str) -> bool {
    let plain = t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    !t.is_empty() && plain && t != "." && t != ".."
}

/// The release tag in the address `releases/latest` redirects to (`.../releases/tag/<tag>`). Only plain
/// tag characters pass, so the address built from it stays inside the release downloads.
pub(super) fn tag_of(location: &str) -> Option<String> {
    let t = location.split("/releases/tag/").nth(1)?.split(['?', '#', '/']).next()?;
    plain_tag(t).then(|| t.to_string())
}

/// The latest release's tag from the web, with no API request: `releases/latest` is a redirect.
pub(super) async fn tag_via_web(probe: &reqwest::Client, src: &Sources) -> Option<String> {
    let r = probe.get(format!("{}/releases/latest", src.web)).send().await.ok()?;
    if !r.status().is_redirection() {
        return None;
    }
    tag_of(r.headers().get(reqwest::header::LOCATION)?.to_str().ok()?)
}

/// The release `tag` with the files named, at their download addresses.
pub(super) fn release_from_tag(src: &Sources, tag: &str, names: &[&str]) -> Release {
    let files = names.iter().map(|n| (n.to_string(), format!("{}/releases/download/{tag}/{n}", src.web)));
    Release { tag: tag.to_string(), files: files.collect() }
}

pub(super) const NO_RELEASE: &str = "GitHub did not answer with a release";

/// The reason in words when GitHub refused for its hourly limit (403 or 429 with no requests left), with
/// the minutes until `reset` (an epoch second) counted from `now`.
pub(super) fn limit_text(status: u16, remaining: Option<&str>, reset: Option<&str>, now: u64) -> Option<String> {
    if !matches!(status, 403 | 429) || remaining.map(str::trim) != Some("0") {
        return None;
    }
    let when = match reset.and_then(|r| r.trim().parse::<u64>().ok()) {
        Some(r) => format!("it resets in {} min", r.saturating_sub(now).div_ceil(60).max(1)),
        None => "try again later".to_string(),
    };
    Some(format!("GitHub's hourly limit for this network is used up; {when}"))
}

/// The latest release from the API (the second try): why not when it cannot be had.
pub(super) async fn api_release(client: &reqwest::Client, src: &Sources) -> Result<Release, String> {
    let r = client
        .get(format!("{}/releases/latest", src.api))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| NO_RELEASE.to_string())?;
    if !r.status().is_success() {
        let h = |k: &str| r.headers().get(k).and_then(|v| v.to_str().ok());
        let why =
            limit_text(r.status().as_u16(), h("x-ratelimit-remaining"), h("x-ratelimit-reset"), nexo_core::auth::now());
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
pub(super) async fn latest_release(
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
pub(super) fn tag_is_commit(tag: &str, commit: &str) -> bool {
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
pub(super) async fn status(commit: &str, src: &Sources) -> Option<Option<Newer>> {
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
pub(super) async fn ahead(client: &reqwest::Client, src: &Sources, commit: &str, target: &str) -> Option<u32> {
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
pub(super) fn newer(list: &serde_json::Value, when: &str) -> u32 {
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
