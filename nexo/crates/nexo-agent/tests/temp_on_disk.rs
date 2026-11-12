//! Guard: test scratch dirs live on disk under the Cargo target dir, not in the system temp dir, unless that
//! target dir is inside a git checkout (CI builds in the repository): there they would be taken for the project.

use std::path::Path;

fn test_tmpdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("nexo-test-")
        .tempdir_in(nexo_core::compat::scratch_base(Path::new(env!("CARGO_TARGET_TMPDIR"))))
}

#[test]
fn scratch_dir_is_under_target_unless_target_is_inside_a_repository() {
    let dir = test_tmpdir().unwrap();
    let p = dir.path().canonicalize().unwrap();
    let target = Path::new(env!("CARGO_TARGET_TMPDIR")).canonicalize().unwrap();
    let in_repo = nexo_core::compat::project_root(&target).join(".git").exists();
    let base = if in_repo { std::env::temp_dir() } else { target };
    assert!(p.starts_with(base.canonicalize().unwrap()), "{}", p.display());
    assert!(!nexo_core::compat::project_root(&p).join(".git").exists(), "{} is inside a repository", p.display());
}

#[test]
fn a_folder_inside_a_repository_gets_the_system_temp_dir_instead() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir(repo.path().join(".git")).unwrap();
    let inner = repo.path().join("target/tmp");
    std::fs::create_dir_all(&inner).unwrap();
    assert_eq!(nexo_core::compat::scratch_base(&inner), std::env::temp_dir());
    let plain = tempfile::tempdir().unwrap();
    assert_eq!(nexo_core::compat::scratch_base(plain.path()), plain.path());
}
