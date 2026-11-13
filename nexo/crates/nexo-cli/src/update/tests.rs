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
    let sums =
        format!("{}  nexo-linux-aarch64\n{hex}  nexo-linux-x86_64\n{hex} *nexo-windows-x86_64.exe\n", "0".repeat(64));
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
    assert_eq!(git_rev("Installing nexo-cli v0.1.0 (https://github.com/Locotir/nexo#4b4f255a1c2d)"), Some("4b4f255"));
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
    assert_eq!(update_text(Some("4b4f255a"), "153ab70ffff"), format!("nexo {v} (153ab70) → 4b4f255. Restart nexo."));
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
    assert_eq!(rel.files, vec![(SUMS.to_string(), format!("{}/releases/download/build-def5678/SHA256SUMS", m.base))]);
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
    let plain = Mock::start(|_| vec![("/api/releases/latest".to_string(), reply_with("403 Forbidden", &[], ""))]).await;
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
        Mock::start(|b| vec![redirect(b, "build-def5678"), ("/api/compare/abc1234...build-def5678".into(), ok)]).await;
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
