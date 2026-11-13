//! Real headless Chrome on temporary profile folders: what a kept profile keeps, what an ephemeral one
//! leaves behind, and what happens when a folder is busy or holds a dead browser's lock.

use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

/// A page server on 127.0.0.1 (cookies and localStorage need a real origin, not `file://`). `/set` stores a
/// cookie and a localStorage value; every other path is a page that only reads them.
struct Site {
    origin: String,
}

impl Site {
    fn start() -> Site {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]);
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (cookie, script) = if path == "/set" {
                    (
                        "Set-Cookie: nexo_sid=kept-123; Max-Age=3600; Path=/\r\n",
                        "localStorage.setItem('nexo_ls', 'kept-456');",
                    )
                } else {
                    ("", "")
                };
                let body = format!("<!doctype html><title>t</title><body>page<script>{script}</script></body>");
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n{cookie}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(reply.as_bytes());
            }
        });
        Site { origin }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }
}

fn kept(dir: &Path) -> LaunchOptions {
    LaunchOptions { profile: Profile::Persistent(dir.to_path_buf()), ..LaunchOptions::default() }
}

#[tokio::test]
async fn a_kept_profile_keeps_cookies_and_local_storage_between_two_launches() {
    if find_browser().is_none() {
        eprintln!("skipped: no chrome");
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let profile = data.path().join("browser").join("profile");
    let site = Site::start();

    let first = Browser::launch(kept(&profile)).await.unwrap();
    assert!(first.is_persistent() && first.is_headless());
    assert_eq!(first.profile_dir(), profile);
    first.navigate(&site.url("/set")).await.unwrap();
    assert_eq!(first.eval("document.cookie").await.unwrap(), "\"nexo_sid=kept-123\"");
    assert_eq!(first.eval("localStorage.getItem('nexo_ls')").await.unwrap(), "\"kept-456\"");
    first.close().await.unwrap();
    assert!(profile.is_dir(), "a kept profile stays after close");

    let second = Browser::launch(kept(&profile)).await.unwrap();
    second.navigate(&site.url("/read")).await.unwrap();
    assert_eq!(second.eval("document.cookie").await.unwrap(), "\"nexo_sid=kept-123\"", "the cookie came back");
    assert_eq!(second.eval("localStorage.getItem('nexo_ls')").await.unwrap(), "\"kept-456\"");
    second.close().await.unwrap();

    // The same page on a clean profile sees neither.
    let clean = Browser::launch(LaunchOptions::default()).await.unwrap();
    clean.navigate(&site.url("/read")).await.unwrap();
    assert_eq!(clean.eval("document.cookie").await.unwrap(), "\"\"");
    assert_eq!(clean.eval("localStorage.getItem('nexo_ls') === null").await.unwrap(), "true");
    clean.close().await.unwrap();
}

#[tokio::test]
async fn an_ephemeral_profile_leaves_no_folder_behind() {
    if find_browser().is_none() {
        eprintln!("skipped: no chrome");
        return;
    }
    let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
    assert!(!browser.is_persistent());
    let dir = browser.profile_dir().to_path_buf();
    assert!(dir.is_dir(), "the throwaway profile exists while the browser runs");
    browser.close().await.unwrap();
    assert!(!dir.exists(), "{} should be gone", dir.display());
}

#[tokio::test]
async fn a_profile_another_browser_holds_is_refused_with_the_process_named() {
    if find_browser().is_none() {
        eprintln!("skipped: no chrome");
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let profile = data.path().join("profile");
    let holder = Browser::launch(kept(&profile)).await.unwrap();
    assert!(holder.is_alive().await);

    let err = match Browser::launch(kept(&profile)).await {
        Ok(_) => panic!("two browsers cannot share a profile"),
        Err(e) => e,
    };
    let Error::ProfileInUse { dir, pid } = &err else { panic!("{err:?}") };
    assert_eq!(dir, &profile);
    #[cfg(unix)]
    assert!(pid.is_some(), "{err}");
    let _ = pid;
    assert!(err.to_string().contains("already in use"), "{err}");
    assert!(holder.is_alive().await, "the refused launch did not disturb the running browser");
    holder.close().await.unwrap();

    Browser::launch(kept(&profile)).await.unwrap().close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn a_lock_left_by_a_dead_browser_is_cleared_and_the_launch_works() {
    if find_browser().is_none() {
        eprintln!("skipped: no chrome");
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let profile = data.path().join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    // A process id no browser has: far above pid_max.
    let host =
        std::process::Command::new("hostname").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let host = host.unwrap_or_default();
    std::os::unix::fs::symlink(format!("{host}-4194999"), profile.join("SingletonLock")).unwrap();

    let browser = Browser::launch(kept(&profile)).await.unwrap();
    browser.navigate("data:text/html,<title>up</title>").await.unwrap();
    assert!(browser.is_alive().await);
    browser.close().await.unwrap();
}

#[tokio::test]
async fn the_window_is_1280_by_800_unless_told_otherwise() {
    if find_browser().is_none() {
        eprintln!("skipped: no chrome");
        return;
    }
    let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
    browser.navigate("data:text/html,<title>w</title>").await.unwrap();
    assert_eq!(browser.eval("[outerWidth, outerHeight].join('x')").await.unwrap(), "\"1280x800\"");
    browser.close().await.unwrap();

    let opts = LaunchOptions { window: Some((900, 700)), ..LaunchOptions::default() };
    let browser = Browser::launch(opts).await.unwrap();
    browser.navigate("data:text/html,<title>w</title>").await.unwrap();
    assert_eq!(browser.eval("[outerWidth, outerHeight].join('x')").await.unwrap(), "\"900x700\"");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn a_missing_executable_is_a_clear_error_before_any_profile_is_touched() {
    let data = tempfile::tempdir().unwrap();
    let profile = data.path().join("profile");
    let opts = LaunchOptions { executable: Some(data.path().join("no-such-chrome")), ..kept(&profile) };
    assert!(matches!(Browser::launch(opts).await, Err(Error::BadExecutable(_))));
    assert!(!profile.exists());
}
