//! Windows gives a program's main thread 1 MiB of stack, and an unoptimised build of the CLI once took more
//! before it read its arguments. The check runs on Linux too, with the main thread's stack cut to 1 MiB.

#[cfg(target_os = "linux")]
#[test]
fn nexo_starts_with_the_one_mebibyte_main_stack_windows_gives() {
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg("ulimit -s 1024 && exec \"$0\" --version")
        .arg(env!("CARGO_BIN_EXE_nexo"))
        .env("NEXO_CONFIG_DIR", home.path())
        .env("NEXO_DATA_DIR", home.path().join("data"))
        .env("NEXO_CACHE_DIR", home.path().join("cache"))
        .env("NEXO_COMPAT_HOME", "")
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}\n{}", out.status, String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("nexo "), "{}", String::from_utf8_lossy(&out.stdout));
}
