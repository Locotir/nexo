#![cfg(all(any(target_os = "linux", target_os = "macos"), target_arch = "x86_64"))]
//! How a program starts: `run --env K=V --unset K --cwd DIR --aslr --argv0 NAME`, and the gdb forms `set environment`,
//! `unset environment`, `set cwd`, `set disable-randomization`. The program prints one environment variable, its
//! folder, `argv[0]`, the address of a local and those of a global and of `main`. Skipped without `cc`.

use nexo_dbg::{execute, gdb_plan, parse, script_units, split_words, Host};

const SRC: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int global = 1;
int main(int argc, char **argv) {
    int local = 0;
    char cwd[1024];
    const char *t = getenv("NX_LAUNCH");
    const char *p = getenv("PATH");
    if (!getcwd(cwd, sizeof cwd)) cwd[0] = 0;
    printf("env=%s\n", t ? t : "(unset)");
    printf("path=%s\n", p ? "set" : "(unset)");
    printf("cwd=%s\n", cwd);
    printf("argv0=%s argc=%d\n", argv[0], argc);
    printf("local=%p\n", (void *)&local);
    printf("global=%p code=%p\n", (void *)&global, (void *)&main);
    return 0;
}
"#;

fn build(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let src = dir.join("launch.c");
    std::fs::write(&src, SRC).unwrap();
    let exe = dir.join("launch");
    let ok = std::process::Command::new("cc").arg("-o").arg(&exe).arg(&src).status().is_ok_and(|s| s.success());
    if !ok {
        eprintln!("no C compiler: skipped");
        return None;
    }
    Some(exe)
}

fn call(state: &std::path::Path, words: &[&str]) -> Result<String, nexo_dbg::Error> {
    let w: Vec<String> = words.iter().map(|s| s.to_string()).collect();
    execute(&parse(&w)?, state, &Host::Thread, "nexo dbg")
}

/// Start with `flags`, run to the end and give the start answer and what the program printed.
fn printed(exe: &std::path::Path, flags: &[&str]) -> (String, String) {
    let state = tempfile::tempdir().unwrap();
    let mut w = vec!["run"];
    w.extend_from_slice(flags);
    w.push(exe.to_str().unwrap());
    let first = call(state.path(), &w).unwrap();
    call(state.path(), &["cont", "--wait", "20"]).unwrap();
    let out = call(state.path(), &["out", "--all"]).unwrap();
    let _ = call(state.path(), &["end"]);
    (first, out)
}

fn line<'a>(out: &'a str, key: &str) -> &'a str {
    out.lines().find_map(|l| l.trim().strip_prefix(key)).unwrap_or_else(|| panic!("no {key} in {out}"))
}

#[test]
fn an_environment_variable_set_at_launch_reaches_the_program_and_the_answer_names_it_without_its_value() {
    let work = tempfile::tempdir().unwrap();
    let Some(exe) = build(work.path()) else { return };
    let (first, out) = printed(&exe, &["--env", "NX_LAUNCH=hunter two"]);
    assert_eq!(line(&out, "env="), "hunter two", "{out}");
    assert!(first.contains("launch: environment set: NX_LAUNCH"), "{first}");
    assert!(!first.contains("hunter"), "a value is never shown: {first}");
    // the later of two settings of one name is the one the program sees
    let (_, out) = printed(&exe, &["--env", "NX_LAUNCH=a", "--env", "NX_LAUNCH=b"]);
    assert_eq!(line(&out, "env="), "b", "{out}");
    let (_, out) = printed(&exe, &[]);
    assert_eq!(line(&out, "env="), "(unset)", "{out}");
}

#[test]
fn an_inherited_variable_can_be_taken_out_and_the_program_sees_it_missing() {
    let work = tempfile::tempdir().unwrap();
    let Some(exe) = build(work.path()) else { return };
    let (_, out) = printed(&exe, &[]);
    assert_eq!(line(&out, "path="), "set", "{out}");
    let (first, out) = printed(&exe, &["--unset", "PATH"]);
    assert_eq!(line(&out, "path="), "(unset)", "{out}");
    assert!(first.contains("environment unset: PATH"), "{first}");
    // an unset after a set of the same name leaves it unset, and the other way round leaves it set
    let (_, out) = printed(&exe, &["--env", "NX_LAUNCH=1", "--unset", "NX_LAUNCH"]);
    assert_eq!(line(&out, "env="), "(unset)", "{out}");
    let (_, out) = printed(&exe, &["--unset", "NX_LAUNCH", "--env", "NX_LAUNCH=2"]);
    assert_eq!(line(&out, "env="), "2", "{out}");
}

#[test]
fn the_working_folder_and_argv0_are_the_ones_asked_for() {
    let work = tempfile::tempdir().unwrap();
    let Some(exe) = build(work.path()) else { return };
    let (_, out) = printed(&exe, &[]);
    assert_eq!(line(&out, "argv0=").split(' ').next(), exe.to_str(), "the path it was started from: {out}");
    let other = tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(other.path()).unwrap();
    let (first, out) = printed(&exe, &["--cwd", real.to_str().unwrap(), "--argv0", "renamed"]);
    assert_eq!(line(&out, "cwd="), real.to_str().unwrap(), "{out}");
    assert!(line(&out, "argv0=").starts_with("renamed argc=1"), "{out}");
    assert!(first.contains("working folder") && first.contains("argv[0] renamed"), "{first}");
}

#[test]
fn a_relative_program_path_still_starts_when_the_folder_changes() {
    let opts = nexo_dbg::LaunchOpts { cwd: Some(std::env::temp_dir()), ..Default::default() };
    let l = opts.launch(std::path::Path::new("./some/prog"), &[]);
    assert!(l.exe.is_absolute(), "{:?}", l.exe);
    assert_eq!(l.argv0.as_deref(), Some("./some/prog"), "the program keeps the name it was given");
    let l = nexo_dbg::LaunchOpts::default().launch(std::path::Path::new("./some/prog"), &[]);
    assert!(l.exe.is_relative() && l.argv0.is_none());
}

#[test]
fn addresses_repeat_with_randomisation_off_and_change_with_it_on() {
    let work = tempfile::tempdir().unwrap();
    let Some(exe) = build(work.path()) else { return };
    let off: Vec<(String, String, String)> = (0..2)
        .map(|_| {
            let (first, out) = printed(&exe, &[]);
            (first, line(&out, "global=").to_string(), line(&out, "local=").to_string())
        })
        .collect();
    assert_eq!(off[0].1, off[1].1, "the default start repeats code and global addresses");
    let stack = |s: &str| u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap();
    let apart = stack(&off[0].2).abs_diff(stack(&off[1].2));
    if cfg!(target_os = "macos") {
        // macOS writes random values of varying length above the stack at each start: now and then the stack moves
        // by 16 bytes, and the start answer says so
        assert!(apart <= 0x20 && apart % 16 == 0, "the stack moved by {apart:#x}: {off:?}");
        assert!(off[0].0.contains("stack addresses can still differ by 16 bytes"), "{}", off[0].0);
    } else {
        assert_eq!(apart, 0, "the default start repeats stack addresses: {off:?}");
    }
    let on: Vec<String> = (0..4).map(|_| line(&printed(&exe, &["--aslr"]).1, "local=").to_string()).collect();
    assert!(on.iter().any(|a| *a != on[0]), "four randomised runs gave one address: {on:?}");
    let (first, _) = printed(&exe, &["--aslr"]);
    assert!(first.contains("address randomisation on"), "{first}");
    assert!(!first.contains("differ by 16 bytes"), "said only with randomisation off: {first}");
}

#[test]
fn a_gdb_script_sets_the_launch_before_run() {
    let work = tempfile::tempdir().unwrap();
    let Some(exe) = build(work.path()) else { return };
    let other = tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(other.path()).unwrap();
    let text = format!(
        "set environment NX_LAUNCH=from gdb\nunset environment PATH\nset cwd {}\nset disable-randomization off\nfile {}\nbreak main\nrun",
        real.display(),
        exe.display()
    );
    let plan = gdb_plan(&script_units(&text)).unwrap().expect("a plan");
    assert_eq!(
        split_words(&plan.start),
        [
            "run",
            "--env",
            "NX_LAUNCH=from gdb",
            "--unset",
            "PATH",
            "--cwd",
            real.to_str().unwrap(),
            "--aslr",
            exe.to_str().unwrap()
        ],
        "{}",
        plan.start
    );
    assert_eq!(plan.dropped, 0, "none of these is dropped as noise");
    assert!(plan.rest.iter().any(|r| r == "break main"), "{:?}", plan.rest);
    let state = tempfile::tempdir().unwrap();
    let w: Vec<String> = split_words(&plan.start);
    let w: Vec<&str> = w.iter().map(String::as_str).collect();
    call(state.path(), &w).unwrap();
    call(state.path(), &["cont", "--wait", "20"]).unwrap();
    let out = call(state.path(), &["out", "--all"]).unwrap();
    assert_eq!(line(&out, "env="), "from gdb", "{out}");
    assert_eq!(line(&out, "path="), "(unset)", "{out}");
    assert_eq!(line(&out, "cwd="), real.to_str().unwrap(), "{out}");
    // `set disable-randomization on` is the default and removes an earlier `off`
    let on = format!("set disable-randomization off\nset disable-randomization on\nfile {}\nrun", exe.display());
    let plan = gdb_plan(&script_units(&on)).unwrap().expect("a plan");
    assert!(!plan.start.contains("--aslr"), "{}", plan.start);
    // `run` may carry its own options after the script's
    let both = format!("set environment A=1\nfile {}\nrun --env B=2", exe.display());
    let plan = gdb_plan(&script_units(&both)).unwrap().expect("a plan");
    assert!(plan.start.starts_with("run --env A=1 --env B=2 "), "{}", plan.start);
    let _ = call(state.path(), &["end"]);
}

#[test]
fn in_a_live_session_the_gdb_settings_say_when_they_apply() {
    let work = tempfile::tempdir().unwrap();
    let Some(exe) = build(work.path()) else { return };
    let state = tempfile::tempdir().unwrap();
    call(state.path(), &["run", exe.to_str().unwrap()]).unwrap();
    for cmd in [&["set", "environment", "X=1"][..], &["unset", "environment", "X"][..], &["set", "cwd", "/"][..]] {
        let e = call(state.path(), cmd).expect_err("not applied to a running program").to_string();
        assert!(e.contains("fixed when the program starts") && e.contains("run [--env K=V]"), "{cmd:?}: {e}");
    }
    // randomisation is already off by default: a gdb preamble must not stop at it, and a script goes on
    for v in ["on", "off"] {
        let ok = call(state.path(), &["set", "disable-randomization", v]).expect("answered, not refused");
        assert!(ok.contains("chosen when the program starts") && ok.contains("run --aslr"), "{v}: {ok}");
    }
    let script =
        call(state.path(), &["do", "set disable-randomization on; regs"]).expect("the script goes past the setting");
    assert!(script.contains("rip"), "{script}");
    let _ = call(state.path(), &["end"]);
}

#[test]
fn a_bad_launch_option_is_refused_before_anything_starts() {
    let state = tempfile::tempdir().unwrap();
    let say = |w: &[&str]| call(state.path(), w).expect_err("refused").to_string();
    assert!(say(&["run", "--env", "NOEQUALS", "./p"]).contains("--env takes NAME=VALUE"));
    assert!(say(&["run", "--env", "=x", "./p"]).contains("no name before an ="));
    assert!(say(&["run", "--unset", "A=B", "./p"]).contains("not one"));
    assert!(say(&["run", "--cwd", "/no/such/folder", "./p"]).contains("is not a folder"));
    assert!(say(&["run", "--argv0", "", "./p"]).contains("--argv0 takes"));
    assert!(say(&["run", "--env"]).contains("--env needs a value"));
}
