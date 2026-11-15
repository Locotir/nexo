use super::detect::{plain_cmd, register};
use super::translate::examine;
use super::*;
use nexo_core::message::{Message, Part};
use serde_json::json;

const SEGV: &str = "Segmentation fault (core dumped)\n\n[exit code 139]";

fn dir_with_prog() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::copy(std::env::current_exe().unwrap(), d.path().join("paint")).unwrap();
    d
}

/// A path as a command and a call write it: `/` for the separator on Windows too.
fn slashed(p: impl AsRef<Path>) -> String {
    hint_cmd::slash(&p.as_ref().display().to_string())
}

fn fresh(command: &str, result: &str, cwd: &Path) -> Option<(Kind, String)> {
    line(command, result, cwd, &Told::default())
}

/// The call inside the first `debug {"command": "..."}` of a line, JSON escapes undone.
fn call_of(l: &str) -> String {
    let at = l.find("debug {\"command\": \"").unwrap() + "debug {\"command\": \"".len();
    let rest = &l[at..];
    let end = rest.find("\"}").unwrap();
    rest[..end].replace("\\\"", "\"").replace("\\\\", "\\")
}

#[test]
fn a_segfault_of_a_project_program_names_the_debug_call_with_its_input() {
    let d = dir_with_prog();
    let (kind, l) = fresh("echo '1 -1 #' | ./paint", SEGV, d.path()).unwrap();
    assert_eq!(kind, Kind::Fault);
    assert_eq!(
        l,
        r#"[nexo: the debug tool runs this to the fault in one call: debug {"command": "run ./paint <<< \"1 -1 #\"; cont"} stops at the fault (exit 139) with file:line, cause and locals]"#
    );
    assert!(fresh("./paint big < in.txt", SEGV, d.path()).unwrap().1.contains(r#""run ./paint big < in.txt; cont""#));
    let l = fresh("./paint <<< '3 4 x'", "[exit code 134]", d.path()).unwrap().1;
    assert!(l.contains(r#"run ./paint <<< \"3 4 x\"; cont"#), "{l}");
    // a heredoc of one line, an output redirected away, a folder changed, an absolute path, a wrapper
    let l = fresh("./paint <<EOF\n3 4\nEOF", SEGV, d.path()).unwrap().1;
    assert!(l.contains(r#"run ./paint <<< \"3 4\"; cont"#), "{l}");
    assert!(fresh("./paint big > out.txt 2>&1", SEGV, d.path()).unwrap().1.contains(r#""run ./paint big; cont""#));
    let abs = d.path().join("paint");
    let l = fresh(&format!("timeout 5 {}", slashed(&abs)), SEGV, d.path()).unwrap().1;
    assert!(
        l.contains(&hint_cmd::tool_call("debug", &format!("run {}; cont", hint_cmd::quoted(&slashed(&abs))))),
        "{l}"
    );
    let sub = d.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::copy(abs, sub.join("tool")).unwrap();
    let l = fresh("cd sub && ./tool", SEGV, d.path()).unwrap().1;
    assert!(
        l.contains(&hint_cmd::tool_call(
            "debug",
            &format!("run {}; cont", hint_cmd::quoted(&format!("{}/sub/./tool", slashed(d.path()))))
        )),
        "{l}"
    );
}

#[test]
fn the_shapes_of_the_paste_one_script_in_a_heredoc_and_gdb_on_an_absolute_path() {
    let d = dir_with_prog();
    let abs = slashed(d.path().join("paint"));
    let shown = hint_cmd::quoted(&abs);
    let command = format!(
        "cat > g1.gdb <<'EOF'\nset pagination off\nbreak *(_ZN6LCRYPT7encryptERKNSt7__cxx1112basic_stringIcSt11char_traitsIcESaIcEEEi+0x170)\ncommands\nsilent\nx/s $rdi\ninfo registers rax\ncontinue\nend\nrun -e t.bin -p 0 -P test123\nEOF\ngdb -q -batch -x g1.gdb {abs} 2>&1 | tail -40"
    );
    let (kind, l) = fresh(&command, "out", d.path()).unwrap();
    assert_eq!(kind, Kind::Gdb);
    assert!(l.starts_with(HEAD_GDB), "{l}");
    assert_eq!(
        call_of(&l),
        format!(
            "run {shown} -e t.bin -p 0 -P test123; break *_ZN6LCRYPT7encryptERKNSt7__cxx1112basic_stringIcSt11char_traitsIcESaIcEEEi+0x170; cont; mem $rdi --as str; regs rax"
        )
    );
    // the same script from a file written by an earlier command
    std::fs::write(d.path().join("g2.gdb"), "break main\nrun 1 2\nbt\ninfo locals\n").unwrap();
    let l = fresh(&format!("gdb -batch -x g2.gdb {abs}"), "", d.path()).unwrap().1;
    assert_eq!(call_of(&l), format!("run {shown} 1 2; break main; cont; bt; locals"));
}

#[test]
fn gdb_in_each_of_its_forms() {
    let d = dir_with_prog();
    let call = |c: &str| call_of(&fresh(c, "", d.path()).unwrap_or_else(|| panic!("no line for {c}")).1);
    assert_eq!(call("gdb -batch -ex run -ex bt ./paint"), "run ./paint; cont; bt");
    assert_eq!(call("gdb -q -batch -ex r --args ./paint 1 2"), "run ./paint 1 2; cont");
    assert_eq!(call("gdb ./paint"), "run ./paint; cont");
    assert_eq!(
        call("gdb -batch -ex 'break parse if n > 3' -ex 'tbreak *0x401000' -ex run -ex 'x/4gx $rsp' -ex 'p $rax' ./paint"),
        "run ./paint; break parse if n > 3; tbreak *0x401000; cont; mem $rsp 32; print $rax"
    );
    assert_eq!(call("gdb -batch -ex 'run < in.txt' ./paint"), "run ./paint < in.txt; cont");
    assert_eq!(
        call("gdb -batch -ex 'set args a \"b c\"' -ex start ./paint"),
        "run ./paint a \"b c\"; tbreak main; cont"
    );
    assert_eq!(
        call("gdb -batch -ex 'x/5i $pc' -ex 'watch counter' -ex 'catch syscall write' ./paint"),
        "run ./paint; dis $rip 5; watch counter; catch syscall write; cont"
    );
    // lines the debugger has no word for are left out; nothing runs
    assert_eq!(
        call("gdb -batch -ex 'python print(1)' -ex 'shell touch touched' -ex 'call f()' -ex bt ./paint"),
        "run ./paint; cont; bt"
    );
    assert!(!d.path().join("touched").exists());
    // a script on stdin: a heredoc, a here-string, a pipe from echo or cat
    assert_eq!(call("gdb -batch ./paint <<EOF\nbreak main\nrun\nbt\nEOF"), "run ./paint; break main; cont; bt");
    assert_eq!(call("echo 'run' | gdb -batch ./paint"), "run ./paint; cont");
    assert_eq!(call("cat <<EOF | gdb -batch ./paint\nbreak main\nrun\nEOF"), "run ./paint; break main; cont");
    // the options that take a value, the wrappers, the folder
    assert_eq!(call("gdb -q -nx -iex 'set a b' -ex run ./paint"), "run ./paint; cont");
    assert_eq!(call("sudo -n gdb -batch -ex bt ./paint"), "run ./paint; cont; bt");
    let sub = d.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::copy(d.path().join("paint"), sub.join("paint")).unwrap();
    assert_eq!(
        call("cd sub; gdb -batch ./paint"),
        format!("run {}; cont", hint_cmd::quoted(&format!("{}/sub/./paint", slashed(d.path()))))
    );
}

#[test]
fn registers_stay_and_gdb_variables_the_debugger_does_not_have_are_left_out() {
    let d = dir_with_prog();
    let call = |c: &str| call_of(&fresh(c, "", d.path()).unwrap_or_else(|| panic!("no line for {c}")).1);
    // every register spelling of the debugger passes
    assert_eq!(
        call("gdb -batch -ex 'break main' -ex run -ex 'p $rax + $r8d' -ex 'x/2gx $sp' -ex 'p $eflags' -ex 'x/3i $pc' ./paint"),
        "run ./paint; break main; cont; print $rax + $r8d; mem $rsp 16; print $eflags; dis $rip 3"
    );
    // a convenience variable, gdb's own `$_`, a history value: no word for them there, the line is dropped
    assert_eq!(
        call("gdb -batch -ex 'break main' -ex 'set $n = 3' -ex run -ex 'p $n' -ex 'p $_exitcode' -ex 'p $1' -ex 'p $rdi' ./paint"),
        "run ./paint; break main; cont; print $rdi"
    );
    for r in ["rax", "RDI", "r8", "r15d", "r9w", "r10b", "sil", "eflags", "fs_base", "ax", "pc"] {
        assert!(register(r), "{r}");
    }
    for r in ["", "n", "_", "_exitcode", "1", "r16", "r7", "r8dd", "rxx", "eip", "xmm0"] {
        assert!(!register(r), "{r}");
    }
    assert!(plain_cmd("print $rax") && !plain_cmd("print $") && !plain_cmd("print \"a\"") && !plain_cmd("p $n"));
}

#[test]
fn a_script_whose_every_stop_was_left_out_gets_no_line() {
    let d = dir_with_prog();
    // the only breakpoint is on something the debugger cannot place: `run; cont` would stop nowhere it did
    assert_eq!(fresh("gdb -batch -ex 'break foo thread 2' -ex run -ex bt ./paint", "", d.path()), None);
    assert_eq!(fresh("gdb -batch -ex 'catch throw' -ex run ./paint", "", d.path()), None);
    assert_eq!(fresh("gdb -batch -ex 'watch $n' -ex run ./paint", "", d.path()), None);
    // one kept stop is enough, and a script with none asked for none
    let call = |c: &str| call_of(&fresh(c, "", d.path()).unwrap_or_else(|| panic!("no line for {c}")).1);
    assert_eq!(call("gdb -batch -ex 'catch throw' -ex 'break main' -ex run ./paint"), "run ./paint; break main; cont");
    assert_eq!(call("gdb -batch -ex run -ex bt ./paint"), "run ./paint; cont; bt");
}

#[test]
fn a_long_script_is_cut_but_never_loses_the_cont_and_two_steps_stay_two() {
    let d = dir_with_prog();
    let call = |c: &str| call_of(&fresh(c, "", d.path()).unwrap_or_else(|| panic!("no line for {c}")).1);
    let breaks: String = (0..14).map(|i| format!("-ex 'break f{i}' ")).collect();
    let c = call(&format!("gdb -batch {breaks}-ex run -ex bt ./paint"));
    let words: Vec<&str> = c.split("; ").collect();
    assert!(words.len() <= MAX_CMDS && words.contains(&"cont") && words[0] == "run ./paint", "{c}");
    assert!(c.contains("break f0") && !c.contains("break f13"), "{c}");
    // long places: the character bound cuts the same way
    let long = "x".repeat(150);
    let breaks: String = (0..6).map(|i| format!("-ex 'break {long}{i}' ")).collect();
    let c = call(&format!("gdb -batch {breaks}-ex run -ex bt ./paint"));
    assert!(c.len() <= MAX_CALL && c.contains("; cont"), "{c}");
    assert_eq!(
        call("gdb -batch -ex 'break main' -ex run -ex next -ex next -ex bt ./paint"),
        "run ./paint; break main; cont; next; next; bt"
    );
}

#[test]
fn x_with_a_huge_count_does_not_overflow() {
    assert_eq!(examine("18446744073709551615g", "$rsp"), Some("mem $rsp 4096".to_string()));
    assert_eq!(examine("99999999999999999999g", "$rsp"), None);
    assert_eq!(examine("2w", "$rsp"), Some("mem $rsp 8".to_string()));
}

#[test]
fn gdb_with_a_core_file_asks_for_the_core_command() {
    let d = dir_with_prog();
    std::fs::write(d.path().join("core.123"), b"core").unwrap();
    let l = fresh("gdb -batch -ex bt ./paint core.123", "", d.path()).unwrap().1;
    assert_eq!(call_of(&l), "core core.123 --exe ./paint; bt");
    // a process id is not a core file
    let l = fresh("gdb -batch ./paint 1234", "", d.path()).unwrap().1;
    assert_eq!(call_of(&l), "run ./paint; cont");
    // attaching is not a program of the project
    assert_eq!(fresh("gdb -p 1234", "", d.path()), None);
}

#[test]
fn strace_and_ltrace_on_a_project_program_name_the_debugger_traces() {
    let d = dir_with_prog();
    let call = |c: &str| {
        let (k, l) = fresh(c, "", d.path()).unwrap_or_else(|| panic!("no line for {c}"));
        assert_eq!(k, Kind::Trace);
        assert!(l.starts_with(HEAD_TRACE), "{l}");
        call_of(&l)
    };
    assert_eq!(call("strace ./paint a b"), "run ./paint a b; strace; cont; strace show");
    assert_eq!(call("strace -f -o t.txt ./paint"), "run ./paint; strace; cont; strace show");
    assert_eq!(call("strace -e trace=openat,read ./paint"), "run ./paint; strace openat,read; cont; strace show");
    assert_eq!(call("strace -etrace=write ./paint x 2>&1 | head"), "run ./paint x; strace write; cont; strace show");
    assert_eq!(call("strace -e trace=%file ./paint"), "run ./paint; strace; cont; strace show");
    assert_eq!(call("ltrace -e malloc+free ./paint"), "run ./paint; ltrace malloc,free; cont; ltrace show");
    assert_eq!(call("ltrace ./paint < in.txt"), "run ./paint < in.txt; ltrace; cont; ltrace show");
    // a process, a program that is not ours
    assert_eq!(fresh("strace -p 1234", "", d.path()), None);
    assert_eq!(fresh("strace -fp 1234", "", d.path()), None);
    assert_eq!(fresh("strace ls", "", d.path()), None);
}

#[test]
fn no_line_for_clean_exits_other_programs_or_a_pipe_after_the_program() {
    let d = dir_with_prog();
    for (c, r) in [
        ("./paint", "ok"),
        ("./paint", "[exit code 1]"),
        ("./paint | head", "[exit code 139]"),
        ("./paint; echo done", "[exit code 139]"),
        ("python3 x.py", "[exit code 139]"),
        ("./missing", "[exit code 139]"),
        ("gdb ./missing", ""),
        ("gdb --version", ""),
        ("objdump -d ./paint", "ok"),
        ("ls $(touch touched)", "[exit code 139]"),
    ] {
        assert_eq!(fresh(c, r, d.path()), None, "{c} {r}");
    }
    assert!(!d.path().join("touched").exists());
}

#[test]
fn once_per_session_per_kind_and_found_again_in_a_kept_history() {
    let d = dir_with_prog();
    let bash = |id: &str, c: &str| (id.to_string(), "bash".to_string(), json!({"command": c}));
    let calls = vec![
        bash("a", "./paint"),
        bash("b", "gdb -batch -ex run ./paint"),
        bash("c", "strace ./paint"),
        bash("d", "gdb ./paint"),
    ];
    let res = vec![
        Part::tool_result("a", "boom\n\n[exit code 139]", true),
        Part::tool_result("b", "gdb out", false),
        Part::tool_result("c", "trace", false),
        Part::tool_result("d", "gdb again", false),
    ];
    let mut now = Told::default();
    let out = round(&mut now, &calls, res.clone(), d.path());
    assert_eq!(now, Told { gdb: true, trace: true, fault: true });
    let text = |p: &Part| match p {
        Part::ToolResult { content, .. } => content.iter().filter_map(Part::as_text).collect::<String>(),
        _ => String::new(),
    };
    assert!(
        text(&out[0]).contains(HEAD_FAULT) && text(&out[1]).contains(HEAD_GDB) && text(&out[2]).contains(HEAD_TRACE)
    );
    assert_eq!(text(&out[3]), "gdb again", "one line per kind");
    assert_eq!(told(&[Message::tool_results(out)]), Told { gdb: true, trace: true, fault: true });
    let again = round(&mut now, &calls, res, d.path());
    assert_eq!(text(&again[1]), "gdb out");
    assert_eq!(told(&[Message::tool_results(again)]), Told::default());
    // a fault line is a fault and nothing else: a later gdb still earns its line
    let fault = Message::tool_results(vec![Part::tool_result("x", format!("out\n\n{HEAD_FAULT} debug {{}}]"), false)]);
    assert_eq!(told(&[fault]), Told { gdb: false, trace: false, fault: true });
    // a line of a session saved before the kinds covers a fault and a gdb
    let old = Message::tool_results(vec![Part::tool_result("x", format!("out\n\n{HEAD} debug {{}}]"), false)]);
    assert_eq!(told(&[old]), Told { gdb: true, trace: false, fault: true });
}

#[test]
fn a_refused_command_spends_nothing_and_a_failed_one_does() {
    let d = dir_with_prog();
    let calls = vec![("a".to_string(), "bash".to_string(), json!({"command": "gdb -batch ./paint"}))];
    let mut now = Told::default();
    let refused = vec![Part::tool_result("a", "Permission denied: the person said no", true)];
    round(&mut now, &calls, refused, d.path());
    assert_eq!(now, Told::default());
    let failed = vec![Part::tool_result("a", "sh: gdb: command not found\n\n[exit code 127]", true)];
    round(&mut now, &calls, failed, d.path());
    assert!(now.gdb);
}

#[test]
fn background_and_detached_calls_and_other_tools_get_no_line_and_spend_nothing() {
    let d = dir_with_prog();
    let calls = vec![
        ("a".to_string(), "bash".to_string(), json!({"command": "gdb -batch ./paint", "background": true})),
        ("b".to_string(), "bash".to_string(), json!({"command": "gdb -batch ./paint", "detach": true})),
        ("c".to_string(), "read".to_string(), json!({"command": "gdb -batch ./paint"})),
    ];
    let res = vec![
        Part::tool_result("a", "started", false),
        Part::tool_result("b", "started", false),
        Part::tool_result("c", "x", false),
    ];
    let mut now = Told::default();
    let out = round(&mut now, &calls, res, d.path());
    assert_eq!(now, Told::default());
    let said: Vec<String> = out
        .iter()
        .map(|p| match p {
            Part::ToolResult { content, .. } => content.iter().filter_map(Part::as_text).collect(),
            _ => String::new(),
        })
        .collect();
    assert_eq!(said, ["started", "started", "x"]);
}

#[test]
fn the_line_follows_the_results_text_joined_by_newlines_and_a_full_set_changes_nothing() {
    let d = dir_with_prog();
    let calls = vec![("a".to_string(), "bash".to_string(), json!({"command": "gdb -batch ./paint"}))];
    let two = Part::ToolResult {
        call_id: "a".into(),
        name: Some("bash".into()),
        content: vec![Part::text("one"), Part::text("two")],
        is_error: false,
    };
    let mut now = Told::default();
    let out = round(&mut now, &calls, vec![two.clone()], d.path());
    let Part::ToolResult { content, .. } = &out[0] else { panic!("not a result") };
    assert_eq!(content.len(), 1);
    assert!(content[0].as_text().unwrap().starts_with("one\ntwo\n\n[nexo: "), "{:?}", content[0].as_text());
    assert!(now.gdb);
    let mut all = Told { gdb: true, trace: true, fault: true };
    let same = round(&mut all, &calls, vec![two.clone()], d.path());
    assert_eq!(same, vec![two]);
    assert_eq!(told(&[]), Told::default());
}
