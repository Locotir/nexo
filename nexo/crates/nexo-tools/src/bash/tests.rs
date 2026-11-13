#[cfg(unix)]
#[tokio::test]
async fn nexo_tmp_is_kept_between_commands_until_it_is_wiped() {
    let d = tempfile::tempdir().unwrap();
    let mut c = ToolContext::new(d.path().to_path_buf());
    c.session_id = "tmp-test-session".into();
    let out = Bash.run(json!({"command": "echo probe > \"$NEXO_TMP/probe.txt\" && echo \"$NEXO_TMP\""}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    let dir = std::path::PathBuf::from(out.content.trim());
    assert!(dir.join("probe.txt").is_file(), "{dir:?}");
    assert!(!dir.starts_with(d.path()), "the folder is not in the project");
    // a later command (the next turn) still finds the file
    let out = Bash.run(json!({"command": "cat \"$NEXO_TMP/probe.txt\"; echo again > \"$NEXO_TMP/p2\""}), &c).await;
    assert_eq!(out.content.trim(), "probe");
    wipe_tmp("tmp-test-session");
    assert!(!dir.exists(), "{dir:?} is gone once the session ends");
    // a later command gets a fresh one
    let out = Bash.run(json!({"command": "ls \"$NEXO_TMP\"; echo x > \"$NEXO_TMP/p3\"; ls \"$NEXO_TMP\""}), &c).await;
    assert_eq!(out.content.trim(), "p3");
    wipe_tmp("tmp-test-session");
}

use super::*;

#[cfg(unix)]
#[tokio::test]
async fn own_checks_see_nexo_tmp() {
    let d = tempfile::tempdir().unwrap();
    let mut c = ToolContext::new(d.path().to_path_buf());
    c.session_id = "tmp-own-session".into();
    let own = crate::postcheck::run_own("echo \"[$NEXO_TMP]\"", &c).await;
    assert!(own.contains("nexo-tmp-"), "{own}");
    wipe_tmp("tmp-own-session");
}

#[cfg(unix)]
#[tokio::test]
async fn background_jobs_see_nexo_tmp() {
    let d = tempfile::tempdir().unwrap();
    let mut c = ToolContext::new(d.path().to_path_buf());
    c.session_id = "tmp-bg2-session".into();
    let marker = d.path().join("seen.txt");
    let cmd = format!("printf %s \"$NEXO_TMP\" > '{}'", marker.display());
    let out = Bash.run(json!({"command": cmd, "background": true}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    for _ in 0..100 {
        if marker.is_file() && std::fs::metadata(&marker).map(|m| m.len() > 0).unwrap_or(false) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let seen = std::fs::read_to_string(&marker).unwrap_or_default();
    assert!(seen.contains("nexo-tmp-"), "NEXO_TMP in a background job: {seen:?}");
    wipe_tmp("tmp-bg2-session");
}

#[test]
fn read_only_commands_are_read_kind() {
    let kind = |c: &str| Bash.call_kind(&json!({"command": c}));
    for (c, want) in [
        ("cat x", ToolKind::Read),
        ("sed -n '1,20p' src/a.rs", ToolKind::Read),
        ("sed -i 's/a/b/' x", ToolKind::Exec),
        ("sed -n '1w out' x", ToolKind::Exec),
        ("cargo test", ToolKind::Exec),
        ("cat a | grep b", ToolKind::Read),
        ("grep -rn 'a|b' src 2>&1 | head -5", ToolKind::Read),
        ("cat a > b", ToolKind::Exec),
        ("cat a >> b", ToolKind::Exec),
        ("ls -la && git log --oneline -5", ToolKind::Read),
        ("git status", ToolKind::Read),
        ("git checkout main", ToolKind::Exec),
        ("find . -name '*.rs' -delete", ToolKind::Exec),
        ("find . -name '*.rs'", ToolKind::Read),
        ("cat a | tee b", ToolKind::Exec),
        ("rm x", ToolKind::Exec),
        ("cat $(echo x)", ToolKind::Exec),
        ("tail -f log &", ToolKind::Exec),
        ("", ToolKind::Exec),
    ] {
        assert_eq!(kind(c), want, "{c}");
    }
    assert_eq!(Bash.call_kind(&json!({"command": "cat x", "background": true})), ToolKind::Exec);
}

#[test]
fn nexo_dbg_steering_is_a_read_and_starting_or_writing_is_not() {
    let kind = |c: &str| Bash.call_kind(&json!({"command": c}));
    for c in [
        "nexo dbg regs",
        "nexo dbg -s ab12 bt 5 --lines 20",
        "nexo dbg mem rsp 64 --as u64",
        "nexo dbg break main if rdi == 3",
        "nexo dbg cont --wait 5",
        "nexo dbg step 3",
        "nexo dbg kill",
        "nexo dbg sessions",
        "nexo re",
        "nexo re overview ./a.out",
        "nexo re regs",
        "nexo dbg do \"break f; cont; args; bt\"",
    ] {
        assert_eq!(kind(c), ToolKind::Read, "{c}");
    }
    for c in [
        "nexo dbg run ./a.out",
        "nexo dbg attach 1234",
        "nexo dbg write 0x1000 90 90",
        "nexo re write 0x1000 90 90",
        "nexo re run ./a.out",
        "nexo dbg setreg rax 0",
        "nexo dbg do \"break f; cont; setreg rax 1\"",
        "nexo dbg do \"run x\"",
        "nexo dbg nosuch",
        "nexo dbg regs > out.txt",
    ] {
        assert_eq!(kind(c), ToolKind::Exec, "{c}");
    }
    for (c, changes) in [
        ("nexo dbg attach 1234", true),
        ("nexo dbg -s ab write 0x10 90", true),
        ("nexo dbg do \"cont; setreg rax 1\"", true),
        ("nexo re attach 1234", true),
        ("nexo re write 0x10 90", true),
        ("nexo re regs", false),
        ("nexo dbg run ./a.out", false),
        ("nexo dbg regs", false),
        ("echo nexo dbg attach", false),
    ] {
        assert_eq!(dbg_changes(c), changes, "{c}");
    }
}

#[test]
fn nexo_bin_reads_are_read_kind_and_what_saves_or_follows_is_judged_as_before() {
    let kind = |c: &str| Bash.call_kind(&json!({"command": c}));
    for c in [
        "nexo bin asm 'mov eax, 7; ret' --at 0x401000",
        "nexo re asm 'nop'",
        "nexo bin info a.out",
        "nexo bin funcs a.out --sort size --lines 20",
        "nexo bin fn a.out main --bytes",
        "nexo bin xref a.out 0x401a30 --json",
        "nexo bin calls a.out main",
        "nexo bin strings a.out --grep 'http' --sort refs",
        "nexo bin find a.out 48 8d 05",
        "nexo bin names a.out",
        "nexo bin bytes a.out 0x402000 64 --as u32",
        "nexo bin diff old.out new.out get_item --all",
        "nexo bin fn a.out main 2>&1 | head -40",
        "cd src && nexo bin info a.out",
    ] {
        let want = if c.starts_with("cd ") { ToolKind::Exec } else { ToolKind::Read };
        assert_eq!(kind(c), want, "{c}");
    }
    for c in [
        // saves into nexo's data folder: asks, as a command that writes does
        "nexo bin patch a.out --out patched --at 0x401000 --nop 4",
        "nexo re patch a.out --out patched --fn main+1 --asm 'nop'",
        "nexo bin name a.out main entry",
        "nexo bin note a.out main starts here",
        "nexo bin name a.out main --clear",
        // not a read, or not a form that is known
        "nexo bin",
        "nexo bin --help",
        "nexo bin nope a.out",
        "nexo info a.out",
        "nexo bin fn a.out main | tee out.txt",
        "nexo bin fn a.out main > out.txt",
        "nexo bin fn a.out main; rm x",
        "nexo bin fn a.out main && touch x",
        "nexo bin fn $(touch x) main",
        "nexo bin fn `touch x` main",
        "nexo bin fn a.out main &",
    ] {
        assert_eq!(kind(c), ToolKind::Exec, "{c}");
    }
    // a name with a quote or a dash is only text: the judgment reads it and runs nothing
    assert_eq!(kind("nexo bin info '$(touch x)'"), ToolKind::Read);
    assert_eq!(kind("nexo bin info -- -weird"), ToolKind::Read);
    assert_eq!(kind("nexo bin info \"$(touch x)\""), ToolKind::Exec);
    assert_eq!(kind("nexo bin info a.out\ntouch x"), ToolKind::Exec);
    assert_eq!(Bash.call_kind(&json!({"command": "nexo bin info a", "background": true})), ToolKind::Exec);
}

#[test]
fn command_words_splits_without_running_or_expanding() {
    let w = |c: &str| command_words(c);
    assert_eq!(
        w("objdump -d 'my file' | head -5; strings x"),
        Some(vec![
            vec!["objdump".into(), "-d".into(), "my file".into()],
            vec!["head".into(), "-5".into()],
            vec!["strings".into(), "x".into()],
        ])
    );
    assert_eq!(
        w("objdump -d a 2>&1 | head"),
        Some(vec![vec!["objdump".into(), "-d".into(), "a".into()], vec!["head".into()]])
    );
    assert_eq!(w("'$(x)' a"), Some(vec![vec!["$(x)".into(), "a".into()]]));
    for c in ["objdump -d $(x)", "objdump -d `x`", "objdump -d a > b", "objdump -d 'a", "objdump (a)", "x &"] {
        assert_eq!(w(c), None, "{c}");
    }
    assert_eq!(w(""), Some(vec![]));
}

#[cfg(unix)]
#[test]
fn a_folder_others_can_write_in_is_never_added_to_the_path() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let (bin, other) = (home.path().join("shared"), home.path().join("other"));
    for d in [&bin, &other] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(bin.join("nexo"), "x").unwrap();
    let path = std::env::join_paths([&other]).unwrap();
    for mode in [0o777, 0o1777, 0o775, 0o757] {
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(nexo_on_path(Some(&path), Some(&bin.join("nexo"))), None, "{mode:o}");
    }
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(nexo_on_path(Some(&path), Some(&bin.join("nexo"))).is_some());
    // a folder that is gone cannot be asked, so it is not added either
    assert_eq!(nexo_on_path(Some(&path), Some(&home.path().join("gone").join("nexo"))), None);
}

#[test]
fn nexo_is_added_at_the_end_of_the_path_only_when_none_is_found() {
    use std::ffi::OsString;
    let name = if cfg!(windows) { "nexo.exe" } else { "nexo" };
    let home = tempfile::tempdir().unwrap();
    let (bin, other, with) = (home.path().join("app"), home.path().join("other"), home.path().join("with"));
    for d in [&bin, &other, &with] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(bin.join(name), "x").unwrap();
    std::fs::write(with.join(name), "x").unwrap();
    // whatever the umask of the machine: this folder is the person's alone
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let exe = bin.join(name);
    let join = |ds: &[&std::path::Path]| std::env::join_paths(ds).unwrap();
    // absent: the folder of the running nexo comes last, what was there stays in order
    let path = join(&[home.path(), &other]);
    let got = nexo_on_path(Some(&path), Some(&exe)).expect("added");
    let parts: Vec<_> = std::env::split_paths(&got).collect();
    assert_eq!(parts, [home.path().to_path_buf(), other.clone(), bin.clone()]);
    // present anywhere on it: untouched
    for ds in [vec![&with as &std::path::Path, &other], vec![&other, &with]] {
        assert_eq!(nexo_on_path(Some(&join(&ds)), Some(&exe)), None);
    }
    // the folder of the running one is already there: untouched
    assert_eq!(nexo_on_path(Some(&join(&[&bin, &other])), Some(&exe)), None);
    // a program not called nexo, no path to add to, no known program: untouched
    assert_eq!(nexo_on_path(Some(&path), Some(&bin.join("nexo-test-binary"))), None);
    assert_eq!(nexo_on_path(None, Some(&exe)), None);
    assert_eq!(nexo_on_path(Some(&OsString::new()), Some(&exe)), None);
    assert_eq!(nexo_on_path(Some(&path), None), None);
    // and the command that is built carries it, or carries nothing of its own
    let env_of = |p: Option<&std::ffi::OsStr>| {
        let mut c = tokio::process::Command::new("x");
        add_nexo_path(&mut c, p, Some(&exe));
        c.as_std().get_envs().find(|(k, _)| *k == "PATH").map(|(_, v)| v.map(OsString::from))
    };
    assert_eq!(env_of(Some(&path)), Some(Some(got)));
    assert_eq!(env_of(Some(&join(&[&with]))), None);
}

#[tokio::test]
async fn runs_and_reports_output_and_exit_code() {
    if nexo_core::shell::shell().kind == "powershell" {
        eprintln!("the test writes POSIX shell; this system runs PowerShell: skipped");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    let out = Bash.run(json!({"command": "echo hi; echo err >&2; exit 3"}), &c).await;
    assert!(!out.is_error, "a command that ran to the end is no failure");
    assert!(out.content.contains("hi\n") && out.content.contains("err\n"), "{}", out.content);
    assert!(out.content.ends_with("[exit code 3]"));
    match out.display {
        ToolDisplay::Shell { exit_code, .. } => assert_eq!(exit_code, Some(3)),
        _ => panic!(),
    }
    let out = Bash.run(json!({"command": "pwd"}), &c).await;
    assert!(!out.is_error);
    // Git Bash prints its own form of the path (/tmp/x, not C:\...\Temp\x): compare the folder
    let name = d.path().file_name().unwrap().to_string_lossy().into_owned();
    assert!(out.content.trim().ends_with(&format!("/{name}")), "{}", out.content);
    let out = Bash.run(json!({"command": "true"}), &c).await;
    assert_eq!(out.content, "(no output)");
    assert!(Bash.run(json!({}), &c).await.is_error);
}

#[tokio::test]
async fn streams_lines_and_kills_on_timeout_and_cancel() {
    if nexo_core::shell::shell().kind == "powershell" {
        eprintln!("the test writes POSIX shell; this system runs PowerShell: skipped");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut c = ToolContext::new(d.path().to_path_buf());
    c.progress = Some(tx);
    let t = std::time::Instant::now();
    let out = Bash.run(json!({"command": "echo start; sleep 30; echo never", "timeout": 1}), &c).await;
    assert!(t.elapsed() < Duration::from_secs(5));
    assert!(out.is_error && out.content.contains("timed out"), "{}", out.content);
    assert!(out.content.contains("start"));
    assert_eq!(rx.recv().await.unwrap().line, "start");

    let c2 = ToolContext::new(d.path().to_path_buf());
    let cancel = c2.cancel.clone();
    let t = std::time::Instant::now();
    let run = Bash.run(json!({"command": "echo go; sleep 30", "timeout": 60}), &c2);
    let out = tokio::join!(run, async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
    })
    .0;
    assert!(t.elapsed() < Duration::from_secs(5));
    assert!(out.content.contains("[cancelled]"), "{}", out.content);
}

fn posix() -> bool {
    if nexo_core::shell::shell().kind == "powershell" {
        eprintln!("the test writes POSIX shell; this system runs PowerShell: skipped");
        return false;
    }
    true
}

#[tokio::test]
async fn a_leading_fixed_sleep_is_refused_with_a_poll_rewrite() {
    if !posix() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    for (cmd, has) in [
        ("sleep 45; R=/tmp/x/out; ls -la $R; find $R -type f | head", "[ -e /tmp/x/out ]"),
        ("sleep 25 && python3 - <<'PY'\nprint(1)\nPY", "wait_until"),
        ("sleep 3 && curl -s http://127.0.0.1:8080/health", "curl -sf http://127.0.0.1:8080/health >/dev/null"),
        ("sleep 0.5m", "timeout 60 "),
        ("  sleep 5s\necho hi", "timeout 10 "),
        ("sleep 45", "timeout 90 "),
    ] {
        let t = std::time::Instant::now();
        let out = Bash.run(json!({"command": cmd}), &c).await;
        assert!(t.elapsed() < Duration::from_secs(1), "{cmd} ran");
        assert!(
            out.is_error && out.content.contains("until") && out.content.contains("wait_until"),
            "{cmd}: {}",
            out.content
        );
        assert!(out.content.contains(has), "{cmd}: {}", out.content);
    }
}

#[tokio::test]
async fn short_sleeps_and_sleeps_that_are_not_first_still_run() {
    if !posix() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    for cmd in [
        "sleep 1; echo ok",
        "sleep 0.5 && echo ok",
        "sleep 1.9; echo ok",
        "echo a; sleep 2; echo ok",
        "timeout 5 sh -c 'until [ -e nothing ] || true; do sleep 2; done'; echo ok",
        "i=0; while [ $i -lt 1 ]; do sleep 2; i=1; done; echo ok",
        "sh -c 'sleep 2; echo ok'",
        "cat <<'EOF' >/dev/null\nsleep 5\nEOF\necho ok",
        "sleep 20 & echo ok",
    ] {
        let out = Bash.run(json!({"command": cmd, "timeout": 10}), &c).await;
        assert!(!out.is_error && out.content.contains("ok"), "{cmd}: {}", out.content);
    }
    let out = Bash.run(json!({"command": "sleep 2; echo bg", "background": true}), &c).await;
    assert!(out.content.contains("Started background job"), "{}", out.content);
}

#[tokio::test]
async fn wait_until_returns_when_a_file_appears() {
    if !posix() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    Bash.run(json!({"command": "sleep 1; echo done > flag.txt", "background": true}), &c).await;
    let t = std::time::Instant::now();
    let out = Bash
        .run(json!({"command": "cat flag.txt", "wait_until": "[ -e flag.txt ] && [ -s flag.txt ]", "timeout": 30}), &c)
        .await;
    assert!(!out.is_error && out.content.contains("done"), "{}", out.content);
    assert!(t.elapsed() >= Duration::from_millis(900) && t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
}

#[tokio::test]
async fn a_condition_that_never_holds_is_reported_and_the_command_does_not_run() {
    if !posix() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    let t = std::time::Instant::now();
    let out =
        Bash.run(json!({"command": "touch ran.txt", "wait_until": "echo not yet; false", "timeout": 1}), &c).await;
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert!(out.is_error && out.content.contains("condition never held"), "{}", out.content);
    assert!(out.content.starts_with("[waited 1 s") && out.content.contains("not yet"), "{}", out.content);
    assert!(!d.path().join("ran.txt").exists());
}

#[tokio::test]
async fn cancel_stops_the_wait() {
    if !posix() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    let cancel = c.cancel.clone();
    let t = std::time::Instant::now();
    let run = Bash.run(json!({"command": "touch ran.txt", "wait_until": "false", "timeout": 60}), &c);
    let out = tokio::join!(run, async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel();
    })
    .0;
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    assert!(out.is_error && out.content.contains("cancelled"), "{}", out.content);
    assert!(!d.path().join("ran.txt").exists());
}

#[test]
fn reads_the_shells_not_found_forms() {
    for (line, want) in [
        ("sh: 1: python3: not found", Some("python3")),
        ("/bin/sh: 1: foo: not found", Some("foo")),
        ("bash: foo: command not found", Some("foo")),
        ("bash: line 3: foo: command not found", Some("foo")),
        ("zsh: command not found: foo", Some("foo")),
        ("foo: command not found", Some("foo")),
        ("'foo' is not recognized as an internal or external command,", Some("foo")),
        ("foo : The term 'foo' is not recognized as the name of a cmdlet, function, script file,", Some("foo")),
        ("The term 'foo' is not recognized as a name of a cmdlet, function,", Some("foo")),
        ("Error: file: not found", None),
        ("all fine", None),
    ] {
        assert_eq!(not_found_name(line).as_deref(), want, "{line}");
    }
}

#[tokio::test]
async fn a_command_not_found_in_a_script_that_exits_0_is_noted() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    if nexo_core::shell::shell().kind == "powershell" {
        eprintln!("PowerShell words a missing command its own way: skipped");
        return;
    }
    let (missing, ok) = ("false_cmd_xyz; true", "echo fine; true");
    let out = Bash.run(json!({"command": missing}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(
        out.content.ends_with("\n[note: a command in this script was not found: false_cmd_xyz]"),
        "{}",
        out.content
    );
    let out = Bash.run(json!({"command": ok}), &c).await;
    assert!(!out.is_error && !out.content.contains("[note:"), "{}", out.content);
    // a non-zero exit shows the error itself: no note
    let out = Bash.run(json!({"command": "false_cmd_xyz"}), &c).await;
    assert!(out.is_error && !out.content.contains("[note:"), "{}", out.content);
    let out = Bash.run(json!({"command": "a1x; b2x; a1x; c3x; d4x; true"}), &c).await;
    assert!(out.content.ends_with("a1x, b2x, c3x]"), "{}", out.content);
}

#[test]
fn a_missing_path_probe_in_an_and_chain_is_named() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("src")).unwrap();
    std::fs::write(d.path().join("Cargo.toml"), "").unwrap();
    let p = d.path();
    let note = |c: &str| skipped_by_probe(c, p);
    let n = note("ls tests 2>/dev/null && cat src/lib.rs").expect("probe of a missing path");
    assert!(n.contains("tests") && n.contains(';'), "{n}");
    assert!(note("cat Cargo.toml && ls -R src tests 2>/dev/null && echo done").unwrap().contains("tests"));
    // the probe sits after a cd: the path is looked for there
    assert!(note("cd src && ls nope && echo x").unwrap().contains("nope"));
    assert!(note("cd src && ls ../src && echo x").is_none());
    // the path exists, is the last link, is a glob or a variable, or the chain uses ; or ||
    assert!(note("ls src && cat missing.rs").is_none());
    assert!(note("ls tests*/ && echo x").is_none());
    assert!(note("ls $HOME/zz && echo x").is_none());
    assert!(note("ls tests || echo none && echo x").is_none());
    assert!(note("ls tests; cat src/lib.rs").is_none());
    assert!(note("grep -n x tests && echo x").is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_probe_in_a_chain_adds_a_note_to_the_result() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    let out = Bash.run(json!({"command": "ls tests 2>/dev/null && echo after"}), &c).await;
    assert!(!out.is_error && out.content.contains("[exit code"), "{}", out.content);
    assert!(
        out.content.contains("skipped the commands after it") && out.content.contains("`tests`"),
        "{}",
        out.content
    );
    let out = Bash.run(json!({"command": "ls tests 2>/dev/null; echo after"}), &c).await;
    assert!(!out.is_error && !out.content.contains("skipped the commands"), "{}", out.content);
}

#[tokio::test]
async fn caps_long_output() {
    if nexo_core::shell::shell().kind == "powershell" {
        eprintln!("the test writes POSIX shell; this system runs PowerShell: skipped");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    let out = Bash.run(json!({"command": "seq 1 20000"}), &c).await;
    assert!(!out.is_error);
    assert!(out.content.chars().count() < MAX_OUTPUT_CHARS + 200);
    assert!(out.content.starts_with("1\n2\n"));
    assert!(out.content.trim_end().ends_with("20000"));
    assert!(out.content.contains("characters omitted"));
    // a million lines: bounded while it runs, head and tail kept, quick
    let t = std::time::Instant::now();
    // awk, not seq: macOS's seq writes 1000000 as 1e+06
    let out = Bash.run(json!({"command": "awk 'BEGIN { for (i = 1; i <= 1000000; i++) print i }'"}), &c).await;
    assert!(t.elapsed() < Duration::from_secs(20));
    assert!(
        out.content.starts_with("1\n2\n") && out.content.trim_end().ends_with("1000000"),
        "{}",
        &out.content[out.content.len() - 40..]
    );
    assert!(out.content.chars().count() < MAX_OUTPUT_CHARS + 200);
    match &out.display {
        ToolDisplay::Shell { stdout, .. } => {
            assert!(stdout.chars().count() < MAX_OUTPUT_CHARS + 200, "the display is bounded too")
        }
        _ => panic!(),
    }
    assert_eq!(Bash.summary(&json!({"command": "cargo test\n# second"})), "bash: cargo test …");
}

#[tokio::test]
async fn lines_are_decoded_lossily_and_cut_to_a_bound() {
    let mut input = b"caf\xe9\nok\r\n".to_vec();
    input.extend(std::iter::repeat_n(b'y', LINE_BYTES * 4));
    input.extend_from_slice(b"\nlast");
    let (tx, mut rx) = mpsc::unbounded_channel();
    read_lines(&input[..], tx).await;
    assert_eq!(rx.recv().await.unwrap(), "caf\u{fffd}");
    assert_eq!(rx.recv().await.unwrap(), "ok");
    let long = rx.recv().await.unwrap();
    assert!(long.starts_with(&"y".repeat(LINE_BYTES)), "the head is kept");
    assert!(long.ends_with(&format!(" … [{} bytes of this line cut]", LINE_BYTES * 3)), "{}", &long[LINE_BYTES..]);
    assert_eq!(rx.recv().await.unwrap(), "last");
    assert!(rx.recv().await.is_none());
    // a character split by the cut is not half kept
    let mut s = "a".repeat(LINE_BYTES - 1).into_bytes();
    s.extend_from_slice("é tail\n".as_bytes());
    let (tx, mut rx) = mpsc::unbounded_channel();
    read_lines(&s[..], tx).await;
    let l = rx.recv().await.unwrap();
    assert!(!l.contains('\u{fffd}') && l.contains("bytes of this line cut"), "{}", &l[LINE_BYTES - 4..]);
}

#[cfg(unix)]
#[tokio::test]
async fn output_that_is_not_utf8_or_one_huge_line_is_kept_bounded() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    let out = Bash.run(json!({"command": "printf 'caf\\351\\n'; echo after"}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("caf\u{fffd}\nafter"), "{}", out.content);
    // 8 MB with no line end
    let out = Bash.run(json!({"command": "head -c 8000000 /dev/zero | tr '\\0' y; echo; echo end"}), &c).await;
    assert!(!out.is_error, "{}", &out.content[out.content.len().saturating_sub(200)..]);
    assert!(out.content.contains("bytes of this line cut") && out.content.trim_end().ends_with("end"));
    assert!(out.content.chars().count() < MAX_OUTPUT_CHARS + 200);
}

#[cfg(unix)]
#[tokio::test]
async fn the_call_ends_when_the_shell_exits_even_if_a_child_holds_the_pipes() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    let t = std::time::Instant::now();
    let out = Bash.run(json!({"command": "sleep 20 & echo $!", "timeout": 30}), &c).await;
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert!(!out.is_error, "{}", out.content);
    let pid: i32 = out.content.trim().parse().unwrap();
    // SAFETY: a signal to the child this test started
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

/// Whether process `pid` still runs (a zombie counts as gone).
#[cfg(target_os = "linux")]
fn alive(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(s) => !s.rsplit(')').next().unwrap_or("").trim_start().starts_with('Z'),
        Err(_) => false,
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn dropping_the_call_kills_what_the_command_started() {
    let d = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut c = ToolContext::new(d.path().to_path_buf());
    c.progress = Some(tx);
    let run = Bash.run(json!({"command": "sleep 300 & echo $!; wait", "timeout": 600}), &c);
    // the turn's select drops the call when the person interrupts it
    let pid: i32 = tokio::select! {
        _ = run => panic!("the command ended by itself"),
        p = rx.recv() => p.unwrap().line.trim().parse().unwrap(),
    };
    let t = std::time::Instant::now();
    while alive(pid) && t.elapsed() < Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!alive(pid), "the background child of the dropped command still runs");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_detached_command_names_its_log_and_pid_and_outlives_the_registry() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    let out = Bash.run(json!({"command": "echo up; exec sleep 30", "background": true, "detach": true}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    let pid: i32 = out.content.split("detached process ").nth(1).unwrap().split(':').next().unwrap().parse().unwrap();
    let log = out.content.split("output is in ").nth(1).unwrap().split(". Stop").next().unwrap().to_string();
    assert!(out.content.contains("outlives nexo"), "{}", out.content);
    assert!(c.jobs.list().is_empty(), "a detached command must not be a job of the session");
    let t = std::time::Instant::now();
    while std::fs::read_to_string(&log).unwrap_or_default().trim() != "up" && t.elapsed() < Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "up");
    // its own session: it leads its group, and the session's end does not reach it
    assert_eq!(unsafe { libc::getsid(pid) }, pid);
    c.jobs.kill_all();
    drop(c);
    assert!(unsafe { libc::kill(pid, 0) } == 0 && alive(pid), "the detached command died with the registry");
    // SAFETY: a signal to the process this test started
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = std::fs::remove_file(&log);
}

#[tokio::test]
async fn detach_needs_background() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    let out = Bash.run(json!({"command": "true", "detach": true}), &c).await;
    assert!(out.is_error && out.content.contains("background"), "{}", out.content);
}

#[tokio::test]
async fn only_a_command_that_did_not_run_to_the_end_is_a_failure() {
    if nexo_core::shell::shell().kind == "powershell" {
        eprintln!("the test writes POSIX shell; this system runs PowerShell: skipped");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a"), "1").unwrap();
    std::fs::write(d.path().join("b"), "2").unwrap();
    std::fs::write(d.path().join("empty"), "").unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    // ran to the end with a code of its own: shown as information, never as a failure
    for (cmd, code) in [
        ("cmp a b && echo iguales || echo distintos", None),
        ("cmp a b", Some(1)),
        ("grep x empty", Some(1)),
        ("false", Some(1)),
        ("test -e /no/such/file", Some(1)),
        ("diff a b > /dev/null", Some(1)),
        ("exit 42", Some(42)),
    ] {
        let out = Bash.run(json!({"command": cmd}), &c).await;
        assert!(!out.is_error, "{cmd}: {}", out.content);
        match (&out.display, code) {
            (ToolDisplay::Shell { exit_code, .. }, Some(c)) => {
                assert_eq!(*exit_code, Some(c), "{cmd}");
                assert!(out.content.ends_with(&format!("[exit code {c}]")), "{cmd}: {}", out.content);
            }
            (ToolDisplay::Shell { exit_code, .. }, None) => assert_eq!(*exit_code, Some(0), "{cmd}"),
            _ => panic!("{cmd}"),
        }
    }
    // not found (127), not runnable (126) and a signal are failures: unix exec semantics,
    // so the Windows shell (which maps these differently) does not run them
    #[cfg(unix)]
    {
        let out = Bash.run(json!({"command": "no_such_program_xyz"}), &c).await;
        assert!(out.is_error && out.content.ends_with("[exit code 127]"), "{}", out.content);
        std::fs::write(d.path().join("noexec"), "echo hi").unwrap();
        let out = Bash.run(json!({"command": "./noexec"}), &c).await;
        assert!(out.is_error && out.content.ends_with("[exit code 126]"), "{}", out.content);
        let out = Bash.run(json!({"command": "sh -c 'kill -SEGV $$'"}), &c).await;
        assert!(out.is_error, "{}", out.content);
    }
    // a timeout is a failure on every platform
    let out = Bash.run(json!({"command": "echo start; sleep 30; echo never", "timeout": 1}), &c).await;
    assert!(out.is_error && out.content.contains("timed out"), "{}", out.content);
}
