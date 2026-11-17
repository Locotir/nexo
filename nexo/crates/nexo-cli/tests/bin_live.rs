#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! `nexo bin --module` and `--live` against a program a `nexo dbg` session runs.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn nexo(tmp: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nexo"))
        .args(args)
        .env("NEXO_TMP", tmp)
        .env("NEXO_CONFIG_DIR", tmp)
        .env("NEXO_DATA_DIR", tmp.join("data"))
        .env("NEXO_CACHE_DIR", tmp.join("cache"))
        .output()
        .expect("nexo runs")
}

fn out(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn sleep_bin() -> PathBuf {
    ["/usr/bin/sleep", "/bin/sleep"].iter().map(PathBuf::from).find(|p| p.is_file()).expect("a sleep program")
}

/// The load base of the module whose path ends with `name`, from `nexo dbg modules`.
fn base_of(tmp: &Path, name: &str) -> u64 {
    let text = out(&nexo(tmp, &["dbg", "modules"]));
    let line = text.lines().find(|l| l.trim_end().ends_with(name)).unwrap_or_else(|| panic!("{text}"));
    let lo = line.trim().split('-').next().expect("a range");
    u64::from_str_radix(lo.trim_start_matches("0x"), 16).unwrap_or_else(|_| panic!("{line}"))
}

#[test]
fn a_module_of_a_running_program_is_read_at_its_live_base() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join(format!("nexo-tmp-{}-live", std::process::id()));
    let sleep = sleep_bin();
    let started = nexo(&tmp, &["dbg", "run", sleep.to_str().unwrap(), "60"]);
    assert!(started.status.success(), "{}", out(&started));
    let name = sleep.file_name().unwrap().to_str().unwrap();
    let base = base_of(&tmp, name);
    let want = format!("base        {base:#x}");

    let file = nexo(&tmp, &["bin", "info", "-", "--module", name]);
    assert!(file.status.success(), "{}", out(&file));
    assert!(out(&file).contains(&want), "{}", out(&file));

    let live = nexo(&tmp, &["bin", "info", "-", "--module", name, "--live"]);
    assert!(live.status.success(), "{}", out(&live));
    assert!(out(&live).contains(&want), "{}", out(&live));

    let unknown = nexo(&tmp, &["bin", "info", "-", "--module", "nosuchmodule"]);
    assert_eq!(unknown.status.code(), Some(2), "{}", out(&unknown));
    assert!(out(&unknown).contains("no loaded module is named"), "{}", out(&unknown));

    let patch = nexo(&tmp, &["bin", "patch", "-", "--module", name, "--in-place", "--at", "0x0", "--nop", "1"]);
    assert!(!patch.status.success());
    assert!(out(&patch).contains("--module cannot be used here"), "{}", out(&patch));

    let ended = nexo(&tmp, &["dbg", "end"]);
    assert!(out(&ended).contains("ended"), "{}", out(&ended));
}

#[test]
fn no_session_and_live_without_module_are_usage_errors() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join(format!("nexo-tmp-{}-none", std::process::id()));
    let none = nexo(&tmp, &["bin", "info", "-", "--module", "libc.so.6"]);
    assert!(!none.status.success());
    assert!(out(&none).contains("needs a running `nexo dbg` session"), "{}", out(&none));
    let live = nexo(&tmp, &["bin", "info", "-", "--live"]);
    assert!(!live.status.success());
    assert!(out(&live).contains("--live reads a module of a running program"), "{}", out(&live));
}
