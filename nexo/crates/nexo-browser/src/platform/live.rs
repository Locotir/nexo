//! The platform helpers against a real browser. Skipped when none is installed.

use crate::{find_browser, Browser, Error, LaunchOptions};

fn skip() -> bool {
    let none = find_browser().is_none();
    if none {
        eprintln!("skipped: no chrome");
    }
    none
}

#[tokio::test]
async fn closing_deletes_the_throwaway_profile_and_navigate_takes_a_plain_path() {
    if skip() {
        return;
    }
    let dir = tempfile::Builder::new().prefix("nexo path ").tempdir().unwrap();
    let page = dir.path().join("a page.html");
    std::fs::write(&page, "<title>plain path</title><body>hello</body>").unwrap();

    let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
    let profile = browser.profile.as_ref().expect("ephemeral profile").path().to_path_buf();
    assert!(profile.join("Local State").exists() || profile.is_dir());
    // No file:// URL: the path as the model has it.
    browser.navigate(&page.to_string_lossy()).await.unwrap();
    assert_eq!(browser.read_text(None).await.unwrap().trim(), "hello");
    browser.close().await.unwrap();
    assert!(!profile.exists(), "profile {} was left behind", profile.display());
}

#[tokio::test]
async fn an_upload_that_cannot_be_read_names_the_path() {
    if skip() {
        return;
    }
    let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
    let err = browser.upload("input", r"C:\no\such\cv.pdf").await.unwrap_err();
    assert!(matches!(err, Error::Io(_)), "{err}");
    assert!(err.to_string().contains("cv.pdf"), "{err}");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn tab_new_takes_a_plain_path_like_navigate() {
    if skip() {
        return;
    }
    let dir = tempfile::Builder::new().prefix("nexo tab ").tempdir().unwrap();
    let page = dir.path().join("second page.html");
    std::fs::write(&page, "<title>second</title><body>on the second tab</body>").unwrap();

    let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
    let index = browser.tab_new(&page.to_string_lossy()).await.unwrap();
    assert_eq!(index, 1);
    assert_eq!(browser.read_text(None).await.unwrap().trim(), "on the second tab");
    let tabs = browser.tabs().await.unwrap();
    assert!(tabs[1].url.starts_with("file://") && tabs[1].url.contains("second%20page.html"), "{:?}", tabs[1]);
    browser.close().await.unwrap();
}
