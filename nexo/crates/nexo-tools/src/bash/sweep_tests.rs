use super::*;
use std::fs;

fn mk(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    let d = root.join(name);
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("f.txt"), "x").unwrap();
    d
}

/// No least age: the tests make their folders just before the sweep.
const NOW: std::time::Duration = std::time::Duration::ZERO;

#[test]
fn a_folder_touched_lately_stays_though_its_pid_is_gone() {
    let t = tempfile::tempdir().unwrap();
    let fresh = mk(t.path(), "nexo-tmp-4000005-main");
    sweep_tmp_in(t.path(), &|_| false, std::time::Duration::from_secs(3600));
    assert!(fresh.join("f.txt").is_file());
    sweep_tmp_in(t.path(), &|_| false, NOW);
    assert!(!fresh.exists());
}

// the sweep is by pid on Unix; elsewhere by age, so these run on Unix only
#[test]
fn dead_goes_live_and_own_stay() {
    let t = tempfile::tempdir().unwrap();
    let dead = mk(t.path(), "nexo-tmp-4000001-main");
    let live = mk(t.path(), "nexo-tmp-4000002-main");
    let own = mk(t.path(), &format!("nexo-tmp-{}-main", std::process::id()));
    sweep_tmp_in(t.path(), &|p| p == 4000002 || p == std::process::id() as i32, NOW);
    assert!(!dead.exists());
    assert!(live.join("f.txt").is_file());
    assert!(own.join("f.txt").is_file());
    // even when the probe says the own pid is dead
    sweep_tmp_in(t.path(), &|_| false, NOW);
    assert!(own.join("f.txt").is_file());
    assert!(!live.exists());
}

#[test]
fn other_names_links_and_files_stay() {
    let t = tempfile::tempdir().unwrap();
    let keep: Vec<_> =
        ["nexo-tmp-abc-x", "nexo-tmpfoo", "nexo-build-123", "nexo-detached", "nexo-tmp-5-", "nexo-tmp-0-x"]
            .iter()
            .map(|n| mk(t.path(), n))
            .collect();
    let target = mk(t.path(), "target-dir");
    std::os::unix::fs::symlink(&target, t.path().join("nexo-tmp-4000003-x")).unwrap();
    fs::write(t.path().join("nexo-tmp-4000004-x"), "file").unwrap();
    sweep_tmp_in(t.path(), &|_| false, NOW);
    for k in keep {
        assert!(k.join("f.txt").is_file(), "{k:?}");
    }
    assert!(target.join("f.txt").is_file());
    assert!(fs::symlink_metadata(t.path().join("nexo-tmp-4000003-x")).unwrap().is_symlink());
    assert!(t.path().join("nexo-tmp-4000004-x").is_file());
}

#[test]
fn at_most_200_folders_are_removed() {
    let t = tempfile::tempdir().unwrap();
    for i in 0..230 {
        mk(t.path(), &format!("nexo-tmp-{}-x", 4100000 + i));
    }
    sweep_tmp_in(t.path(), &|_| false, NOW);
    let left = fs::read_dir(t.path()).unwrap().count();
    assert_eq!(left, 30);
}

#[test]
fn real_probe_sees_this_process() {
    assert!(pid_alive(std::process::id() as i32));
}
