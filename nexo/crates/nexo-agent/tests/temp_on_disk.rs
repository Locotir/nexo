//! Guard: test scratch dirs live on disk under the Cargo target dir, not in the system temp dir.

fn test_tmpdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new().prefix("nexo-test-").tempdir_in(env!("CARGO_TARGET_TMPDIR"))
}

#[test]
fn scratch_dir_is_under_target_not_system_temp() {
    let dir = test_tmpdir().unwrap();
    let p = dir.path().canonicalize().unwrap();
    let target = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).canonicalize().unwrap();
    assert!(p.starts_with(&target));
    let sys = std::env::temp_dir().canonicalize().unwrap();
    if !target.starts_with(&sys) {
        assert!(!p.starts_with(&sys));
    }
}
