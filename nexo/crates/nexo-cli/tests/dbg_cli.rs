#![cfg(all(any(target_os = "linux", target_os = "macos"), target_arch = "x86_64"))]
//! `nexo dbg` run through the binary: a session lives between calls and nothing is left behind.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

fn nexo(tmp: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nexo"))
        .arg("dbg")
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

/// A program that sleeps for the seconds it is given. On Linux the system's own `sleep`; on macOS one built with `cc`
/// into `dir`, because `run` there debugs a signed copy of a file the user owns, and `/bin/sleep` belongs to root.
#[cfg(target_os = "linux")]
fn sleep_bin(_dir: &Path) -> PathBuf {
    ["/usr/bin/sleep", "/bin/sleep"].iter().map(PathBuf::from).find(|p| p.is_file()).expect("a sleep program")
}

#[cfg(target_os = "macos")]
fn sleep_bin(dir: &Path) -> PathBuf {
    let exe = dir.join("sleeper");
    if exe.is_file() {
        return exe;
    }
    let src = dir.join("sleeper.c");
    std::fs::write(
        &src,
        "#include <stdlib.h>\n#include <time.h>\nint main(int c, char **v) { double s = c > 1 ? atof(v[1]) : 1; \
         struct timespec t = { (time_t)s, (long)((s - (time_t)s) * 1e9) }; nanosleep(&t, 0); return 0; }\n",
    )
    .unwrap();
    let o = Command::new("cc").arg("-o").arg(&exe).arg(&src).output().expect("cc runs");
    assert!(o.status.success(), "cc: {}", String::from_utf8_lossy(&o.stderr));
    exe
}

/// Whether `pid` is a live process (a zombie is not).
#[cfg(target_os = "linux")]
fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.contains(") Z "))
}

/// Whether `pid` is a live process (a zombie is not); macOS has no `/proc`, `ps` gives the state.
#[cfg(target_os = "macos")]
fn alive(pid: u32) -> bool {
    let o = Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().expect("ps runs");
    let state = String::from_utf8_lossy(&o.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

fn wait_gone(pid: u32) -> bool {
    for _ in 0..150 {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    false
}

fn pid_of(text: &str) -> u32 {
    let l = text.lines().find(|l| l.contains("started pid")).unwrap_or_else(|| panic!("{text}"));
    l.rsplit(' ').next().unwrap().parse().unwrap()
}

#[test]
fn a_session_lives_between_calls_and_end_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join(format!("nexo-tmp-{}-t", std::process::id()));
    let sleep = sleep_bin(dir.path());
    let first = nexo(&tmp, &["run", sleep.to_str().unwrap(), "60"]);
    let text = out(&first);
    assert!(first.status.success(), "{text}");
    assert!(
        text.starts_with("debug ") && text.contains(" stop entry ") && text.contains("stopped at the program's entry"),
        "{text}"
    );
    let pid = pid_of(&text);
    assert!(alive(pid));
    // the next calls find the session by themselves (it is the only one)
    let regs = out(&nexo(&tmp, &["regs"]));
    assert!(regs.contains("rip") && regs.contains("rsp"), "{regs}");
    let bt = out(&nexo(&tmp, &["bt"]));
    assert!(bt.contains("#0"), "{bt}");
    let maps = out(&nexo(&tmp, &["maps"]));
    assert!(maps.contains("[stack]"), "{maps}");
    let bad = nexo(&tmp, &["nosuchcommand"]);
    assert_eq!(bad.status.code(), Some(2), "{}", out(&bad));
    let list = out(&nexo(&tmp, &["sessions"]));
    assert!(list.contains(&format!("pid {pid}")), "{list}");
    let json = out(&nexo(&tmp, &["regs", "--json"]));
    assert!(json.contains("\"session\"") && json.contains("\"state\": \"stopped\""), "{json}");
    let ended = out(&nexo(&tmp, &["end"]));
    assert!(ended.contains("ended"), "{ended}");
    assert!(wait_gone(pid), "the program {pid} is still there after `end`");
    if cfg!(target_os = "macos") {
        // macOS: the session ran a signed copy beside the program, said so, and `end` took the copy away
        assert!(text.contains("signed ad hoc with get-task-allow") && text.contains("your own files only"), "{text}");
        let copies = || {
            std::fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().contains(".nexo-dbg-"))
                .count()
        };
        for _ in 0..50 {
            if copies() == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        assert_eq!(copies(), 0, "a signed copy is left beside the program after `end`");
    }
    let after = nexo(&tmp, &["regs"]);
    assert!(!after.status.success());
}

#[test]
fn a_program_that_ends_is_reported_and_kill_works() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join(format!("nexo-tmp-{}-t", std::process::id()));
    let sleep = sleep_bin(dir.path());
    let first = out(&nexo(&tmp, &["run", sleep.to_str().unwrap(), "0.2"]));
    let pid = pid_of(&first);
    let cont = out(&nexo(&tmp, &["cont", "--wait", "10"]));
    assert!(cont.contains("exited with code 0"), "{cont}");
    assert!(wait_gone(pid));
    let regs = nexo(&tmp, &["regs"]);
    assert!(out(&regs).contains("the program has ended"), "{}", out(&regs));
    nexo(&tmp, &["end"]);
    let again = out(&nexo(&tmp, &["run", sleep.to_str().unwrap(), "60"]));
    let pid2 = pid_of(&again);
    let k = out(&nexo(&tmp, &["kill"]));
    assert!(k.contains("killed"), "{k}");
    assert!(wait_gone(pid2));
    nexo(&tmp, &["end"]);
}

#[test]
fn the_program_goes_when_its_owner_goes() {
    let dir = tempfile::tempdir().unwrap();
    // the owner is the process named in the scratch folder's name
    let mut owner = Command::new(sleep_bin(dir.path())).arg("60").spawn().unwrap();
    let tmp = dir.path().join(format!("nexo-tmp-{}-t", owner.id()));
    let first = out(&nexo(&tmp, &["run", sleep_bin(dir.path()).to_str().unwrap(), "60"]));
    let pid = pid_of(&first);
    assert!(alive(pid));
    owner.kill().unwrap();
    owner.wait().unwrap();
    assert!(wait_gone(pid), "the program {pid} outlived its owner");
}

#[test]
fn a_file_that_is_not_a_program_fails_with_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join(format!("nexo-tmp-{}-t", std::process::id()));
    let f = dir.path().join("notes.txt");
    std::fs::write(&f, "hello").unwrap();
    let o = nexo(&tmp, &["run", f.to_str().unwrap()]);
    assert!(!o.status.success());
    let t = out(&o);
    assert!(t.contains("nexo dbg:"), "{t}");
    let o = nexo(&tmp, &["run", "/no/such/file"]);
    assert!(out(&o).contains("is not a file"), "{}", out(&o));
}

#[test]
fn what_a_session_keeps_comes_back_with_the_next_one_of_the_same_binary() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join(format!("nexo-tmp-{}-k", std::process::id()));
    let sleep = sleep_bin(dir.path());
    let first = out(&nexo(&tmp, &["run", sleep.to_str().unwrap(), "60"]));
    assert!(!first.contains("known about this binary"), "{first}");
    let saved = out(&nexo(&tmp, &["note", "crash: findings go here"]));
    assert_eq!(saved.lines().filter(|l| l.starts_with("saved:")).count(), 1, "{saved}");
    assert!(nexo(&tmp, &["end"]).status.success());
    let again = out(&nexo(&tmp, &["run", sleep.to_str().unwrap(), "60"]));
    assert!(again.contains("known about this binary") && again.contains("crash: findings go here"), "{again}");
    assert!(nexo(&tmp, &["end"]).status.success());
}

#[test]
fn a_core_file_that_is_not_one_is_refused_through_the_binary() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join(format!("nexo-tmp-{}-c", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let o = nexo(&tmp, &["core", sleep_bin(dir.path()).to_str().unwrap()]);
    let t = out(&o);
    // the cores nexo reads are ELF (and Windows minidumps): a macOS program is not even ELF
    let why = if cfg!(target_os = "macos") { "is not a 64-bit little-endian ELF file" } else { "not a core file" };
    assert!(!o.status.success() && t.contains(why), "{t}");
    let o = nexo(&tmp, &["core"]);
    assert!(out(&o).contains("usage: core <file>"), "{}", out(&o));
}

/// Every `__serve` process whose command line holds `needle`, and how many `__serve` processes there are.
#[cfg(target_os = "linux")]
fn serve_cmdlines(needle: &str) -> (usize, usize) {
    let (mut with, mut all) = (0, 0);
    for e in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(raw) = std::fs::read(e.path().join("cmdline")) else { continue };
        let text = String::from_utf8_lossy(&raw).replace('\0', " ");
        if text.contains("dbg __serve") {
            all += 1;
            with += usize::from(text.contains(needle));
        }
    }
    (with, all)
}

#[cfg(target_os = "linux")]
#[test]
fn an_environment_value_reaches_the_program_but_never_the_servers_command_line() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join(format!("nexo-tmp-{}-e", std::process::id()));
    let secret = format!("nx-secret-{}-value", std::process::id());
    let env = ["/usr/bin/printenv", "/bin/printenv"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .expect("a printenv program");
    let first = nexo(&tmp, &["run", "--env", &format!("NX_SECRET={secret}"), env.to_str().unwrap(), "NX_SECRET"]);
    let text = out(&first);
    assert!(first.status.success(), "{text}");
    assert!(
        text.contains("NX_SECRET") && !text.contains(&secret),
        "the start line names the variable, not its value: {text}"
    );
    let (with, all) = serve_cmdlines(&secret);
    assert!(all >= 1, "the server is running");
    assert_eq!(with, 0, "the value is on a server's command line");
    out(&nexo(&tmp, &["cont", "--wait", "20"]));
    let printed = out(&nexo(&tmp, &["out", "--all"]));
    assert!(printed.contains(&secret), "{printed}");
    let _ = nexo(&tmp, &["end"]);
}
