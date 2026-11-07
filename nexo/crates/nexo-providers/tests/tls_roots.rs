//! A client builds, and does not panic, when the system CA store is empty
//! (a container image without ca-certificates). One test per binary: it
//! sets process-wide environment variables.

#[test]
fn builds_with_an_empty_system_store() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("empty.pem");
    std::fs::write(&file, "").unwrap();
    let certs = dir.path().join("certs");
    std::fs::create_dir(&certs).unwrap();
    std::env::set_var("SSL_CERT_FILE", &file);
    std::env::set_var("SSL_CERT_DIR", &certs);

    // the platform verifier alone refuses to start on this store (Linux, where it reads the files)
    #[cfg(all(unix, not(target_os = "android"), not(target_vendor = "apple")))]
    assert!(reqwest::Client::builder().build().is_err(), "the premise: an empty system store fails to build");

    nexo_providers::build_client(reqwest::Client::builder).expect("the bundled roots stand in");
}
