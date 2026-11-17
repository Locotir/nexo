use super::*;

use std::io::Cursor;

/// Send these lines to a server and return its answers, one value each.
fn talk(allow: bool, lines: &[Value]) -> Vec<Value> {
    let input = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n") + "\n";
    let mut out = Vec::new();
    serve(Cursor::new(input), &mut out, allow).unwrap();
    String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn call(id: u64, tool: &str, args: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": args}})
}

fn result_text(v: &Value) -> String {
    v["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string()
}

/// A program of my own, built from C; the bytes of a fixture when there is no compiler.
fn fixture(dir: &std::path::Path) -> String {
    let c = dir.join("p.c");
    std::fs::write(&c, "#include <stdio.h>\nint add(int a, int b) { return a + b; }\nint main(int n, char **v) { printf(\"%d\\n\", add(n, 2)); return 0; }\n").unwrap();
    let exe = dir.join("p.out");
    let built = std::process::Command::new("cc")
        .args(if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            ["-arch", "x86_64"].as_slice()
        } else {
            [].as_slice()
        })
        .args(["-O0", "-fno-pie", "-no-pie", "-o"])
        .arg(&exe)
        .arg(&c)
        .output()
        .is_ok_and(|o| o.status.success());
    if !built {
        std::fs::write(&exe, &nexo_bin::fixtures::elf64(false).bytes).unwrap();
    }
    exe.display().to_string()
}

#[test]
fn initialize_answers_with_the_tools_capability_and_a_notification_gets_no_answer() {
    let a = talk(
        false,
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
        ],
    );
    assert_eq!(a.len(), 2, "{a:?}");
    assert_eq!(a[0]["result"]["protocolVersion"], "2024-11-05");
    assert!(a[0]["result"]["capabilities"]["tools"].is_object());
    assert_eq!(a[1]["result"], json!({}));
}

#[test]
fn tools_list_has_one_tool_per_bin_op_plus_bin_batch_and_the_three_debugger_tools() {
    let a = talk(false, &[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})]);
    let tools = a[0]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), bin_op_count() + 4);
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for n in ["bin_info", "bin_decomp", "bin_emu", "bin_batch", "dbg_start", "dbg_command", "dbg_stop"] {
        assert!(names.contains(&n), "{n} missing from {names:?}");
    }
    assert!(tools.iter().all(|t| t["inputSchema"]["type"] == "object"));
    let info = tools.iter().find(|t| t["name"] == "bin_info").unwrap();
    assert_eq!(info["inputSchema"]["required"], json!(["file"]));
    assert_eq!(info["inputSchema"]["properties"]["lines"]["type"], json!(["integer", "string"]));
}

#[test]
fn assembly_is_read_only_and_copy_patches_need_allow_changes() {
    let dir = tempfile::tempdir().unwrap();
    let f = nexo_bin::fixtures::elf64(false);
    let input = dir.path().join("input");
    let out = dir.path().join("output");
    std::fs::write(&input, &f.bytes).unwrap();
    let args = json!({"file": input, "out": out, "at": format!("{:#x}", f.b), "asm": "mov eax, 7; ret"});
    let a = talk(
        false,
        &[call(1, "bin_asm", json!({"source": "nop; ret", "at": "0x401000"})), call(2, "bin_patch", args.clone())],
    );
    assert_eq!(a[0]["result"]["isError"], false, "{a:?}");
    assert!(result_text(&a[0]).contains("90 c3"));
    assert_eq!(a[1]["result"]["isError"], true);
    assert!(result_text(&a[1]).contains("--allow-changes"));
    assert!(!out.exists());
    let a = talk(true, &[call(3, "bin_patch", args)]);
    assert_eq!(a[0]["result"]["isError"], false, "{a:?}");
    assert_eq!(nexo_bin::Image::open(&out).unwrap().read_at(f.b, 6).unwrap(), [0xb8, 7, 0, 0, 0, 0xc3]);
    assert_eq!(std::fs::read(input).unwrap(), f.bytes);
}

#[test]
fn tools_call_info_returns_the_text_nexo_bin_prints() {
    let dir = tempfile::tempdir().unwrap();
    let exe = fixture(dir.path());
    let a = talk(false, &[call(1, "bin_info", json!({"file": exe}))]);
    assert_eq!(a[0]["result"]["isError"], false, "{a:?}");
    let text = result_text(&a[0]);
    let expect = {
        let parsed = crate::cli::Args::try_parse_from(["nexo", "bin", "info", &exe]).unwrap();
        let Some(crate::cli::Cmd::Bin(b)) = parsed.cmd else { panic!("bin") };
        crate::bin_cmd::execute(&b).unwrap()
    };
    assert_eq!(text, expect);
    // the format line names what `cc` really produced: ELF on Linux, Mach-O on macOS, PE on Windows (MinGW)
    let magic = std::fs::read(&exe).unwrap();
    let want = if magic.starts_with(b"MZ") {
        "pe32"
    } else if magic.starts_with(&[0xcf, 0xfa, 0xed, 0xfe]) || magic.starts_with(&[0xca, 0xfe, 0xba, 0xbe]) {
        "mach-o"
    } else {
        "elf"
    };
    assert!(text.to_lowercase().contains(want), "expected {want} in\n{text}");
    // a positional and a flag together
    let a = talk(false, &[call(2, "bin_funcs", json!({"file": exe, "lines": 12}))]);
    assert_eq!(a[0]["result"]["isError"], false, "{a:?}");
}

#[test]
fn a_bad_call_is_an_error_result_that_names_the_next_step() {
    let a = talk(
        false,
        &[
            call(1, "bin_info", json!({"fil": "x"})),
            call(2, "bin_info", json!({"file": "/nonexistent/none"})),
            call(3, "bin_nope", json!({})),
            json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}),
        ],
    );
    assert_eq!(a[0]["result"]["isError"], true);
    assert!(result_text(&a[0]).contains("valid: file"), "{}", result_text(&a[0]));
    assert_eq!(a[1]["result"]["isError"], true);
    assert!(result_text(&a[1]).starts_with("nexo bin:"), "{}", result_text(&a[1]));
    assert_eq!(a[2]["error"]["code"], -32602);
    assert_eq!(a[3]["error"]["code"], -32601);
}

#[test]
fn the_read_only_ops_and_their_paging_args_work_without_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let exe = fixture(dir.path());
    let cases = [
        ("bin_cfg", json!({"file": exe, "target": "main"})),
        ("bin_bytes", json!({"file": exe, "addr": "main", "len": "16", "ty": "int"})),
        ("bin_bytes", json!({"file": exe, "addr": "main", "len": 8, "ty": "u8"})),
        ("bin_xref", json!({"file": exe, "target": "add"})),
        ("bin_xref", json!({"file": exe, "target": "add", "to": "main"})),
        ("bin_xref", json!({"file": exe, "target": "add..main"})),
        ("bin_fn", json!({"file": exe, "target": "main", "code": true})),
        ("bin_find", json!({"file": exe, "pattern": ["c3"], "within": "main"})),
        ("bin_funcs", json!({"file": exe, "lines": 2, "from": "0", "grep": "a"})),
        ("bin_funcs", json!({"file": exe, "lines": "all"})),
    ];
    let lines: Vec<Value> = cases.iter().enumerate().map(|(i, (t, a))| call(i as u64, t, a.clone())).collect();
    for (i, v) in talk(false, &lines).iter().enumerate() {
        let t = result_text(v);
        eprintln!("{} {}: {}", cases[i].0, cases[i].1, t.lines().next().unwrap_or(""));
        assert_eq!(v["result"]["isError"], false, "{} {}: {t}", cases[i].0, cases[i].1);
    }
}

#[test]
fn a_write_is_refused_without_the_flag_and_the_refusal_names_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let exe = fixture(dir.path());
    let a = talk(
        false,
        &[
            call(1, "bin_name", json!({"file": exe, "target": "main", "name": "entry"})),
            call(2, "bin_crypto", json!({"file": exe, "save": true})),
            call(3, "dbg_command", json!({"command": "write 0x1000 90"})),
            call(4, "dbg_command", json!({"command": "do \"break main; setreg rax 1\""})),
            call(5, "dbg_command", json!({"command": "note 0x1000 hello"})),
            call(6, "dbg_start", json!({"kind": "attach", "target": std::process::id()})),
            call(7, "dbg_start", json!({"kind": "connect", "target": "127.0.0.1:1"})),
        ],
    );
    for (i, v) in a.iter().enumerate() {
        assert_eq!(v["result"]["isError"], true, "{i}: {v}");
        let t = result_text(v);
        assert!(t.starts_with("refused:") && t.contains("--allow-changes"), "{i}: {t}");
    }
}

#[test]
fn reads_are_not_refused_and_session_commands_are_routed() {
    let a = talk(
        false,
        &[
            call(1, "dbg_command", json!({"command": "run ./x"})),
            call(2, "dbg_command", json!({"command": "end"})),
            call(3, "dbg_command", json!({"command": "unterminated \"quote"})),
            call(4, "dbg_start", json!({"kind": "attach"})),
        ],
    );
    assert!(result_text(&a[0]).contains("use dbg_start"), "{}", result_text(&a[0]));
    assert!(result_text(&a[1]).contains("dbg_stop"));
    assert!(result_text(&a[2]).contains("unclosed quote"));
    assert!(result_text(&a[3]).contains("needs \"target\""));
}

#[test]
fn attach_is_for_processes_of_the_same_user_even_with_the_flag() {
    #[cfg(unix)]
    if unsafe { libc::geteuid() } != 0 {
        assert!(same_user("1").unwrap_err().contains("another user"));
    }
    assert!(same_user("abc").is_err());
    #[cfg(unix)]
    assert!(same_user(&std::process::id().to_string()).is_ok());
}

#[test]
fn broken_input_gets_a_parse_error_and_the_server_goes_on() {
    let input = "not json\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n";
    let mut out = Vec::new();
    serve(Cursor::new(input), &mut out, false).unwrap();
    let lines: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines[0]["error"]["code"], -32700);
    assert_eq!(lines[1]["id"], 7);
}

#[test]
fn command_lines_split_like_a_shell_would() {
    assert_eq!(split_words("break main").unwrap(), ["break", "main"]);
    assert_eq!(split_words("do \"break f; cont; bt\"").unwrap(), ["do", "break f; cont; bt"]);
    assert_eq!(split_words("a 'b c' \"\"").unwrap(), ["a", "b c", ""]);
}

fn refused(a: &Value) -> bool {
    a["result"]["isError"] == true
        && result_text(a).starts_with("refused:")
        && result_text(a).contains("--allow-changes")
}

#[test]
fn a_quoted_command_word_does_not_skip_the_gate() {
    let cmds = [
        "do '\"write\" 0x1000 90'",
        "do 'h\"ook\" f set arg1 5'",
        "do 'wr\"i\"te 0x1000 90'",
        "do 'break main; \"setreg\" rax 1'",
    ];
    let calls: Vec<Value> =
        cmds.iter().enumerate().map(|(i, c)| call(i as u64, "dbg_command", json!({"command": c}))).collect();
    let a = talk(false, &calls);
    for (c, v) in cmds.iter().zip(&a) {
        assert!(refused(v), "{c}: {v}");
    }
    // with the flag the same lines pass the gate (and then fail only because no session is live)
    let a = talk(true, &calls);
    for (c, v) in cmds.iter().zip(&a) {
        assert!(!result_text(v).starts_with("refused:"), "{c}: {v}");
    }
}

#[test]
fn a_nested_do_and_unknown_words_and_calls_and_run_are_refused_without_the_flag() {
    let cmds = [
        "do 'break main; do \"write 0x1000 90\"'",
        "do 'bt; frobnicate'",
        "frobnicate",
        "calls",
        "do 'bt; calls'",
        "do '__serve'",
        "do 'run /bin/sh'",
        "stdin hello",
        "serve --gdb 1 x",
    ];
    let calls: Vec<Value> =
        cmds.iter().enumerate().map(|(i, c)| call(i as u64, "dbg_command", json!({"command": c}))).collect();
    let a = talk(false, &calls);
    for (c, v) in cmds.iter().zip(&a) {
        assert!(refused(v), "{c}: {v}");
    }
}

#[test]
fn reading_commands_and_a_read_only_script_pass_the_gate_without_the_flag() {
    let cmds = ["bt", "regs rax", "do 'break main; cont; \"bt\"; mem 0x10 8'", "hook f stop", "cover show"];
    let calls: Vec<Value> =
        cmds.iter().enumerate().map(|(i, c)| call(i as u64, "dbg_command", json!({"command": c}))).collect();
    for (c, v) in cmds.iter().zip(&talk(false, &calls)) {
        assert!(!result_text(v).starts_with("refused:"), "{c}: {v}");
    }
}

#[test]
fn running_a_program_or_attaching_needs_the_flag_and_only_a_core_is_free() {
    let a = talk(
        false,
        &[
            call(1, "dbg_start", json!({"kind": "run", "target": "/bin/sh", "args": ["-c", "id"]})),
            call(2, "dbg_start", json!({"kind": "run", "target": "/bin/cat", "args": ["<", "/etc/hostname"]})),
            call(3, "dbg_start", json!({"kind": "core", "target": "/nonexistent/core"})),
            call(4, "dbg_start", json!({"kind": "core", "target": "-s"})),
        ],
    );
    assert!(refused(&a[0]) && refused(&a[1]), "{a:?}");
    assert!(!result_text(&a[2]).starts_with("refused:"), "{}", result_text(&a[2]));
    assert!(result_text(&a[3]).contains("cannot start with '-'"), "{}", result_text(&a[3]));
    let a = talk(true, &[call(1, "dbg_start", json!({"kind": "run", "target": "/nonexistent/prog"}))]);
    assert!(!result_text(&a[0]).starts_with("refused:"), "{}", result_text(&a[0]));
}

#[test]
fn a_pid_with_a_plus_sign_or_other_junk_is_not_a_pid() {
    for bad in ["+123", "-1", " 12", "1_0", "", "0x10"] {
        assert!(same_user(bad).is_err(), "{bad:?}");
    }
    #[cfg(unix)]
    {
        let me = std::process::id();
        assert_eq!(same_user(&format!("{me:010}")).unwrap(), me);
    }
}

#[test]
fn the_initialize_text_says_what_the_flag_enables() {
    let off = talk(false, &[json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})]);
    let t = off[0]["result"]["instructions"].as_str().unwrap().to_string();
    for w in ["running programs", "attaching", "memory and register writes", "patches", "--allow-changes"] {
        assert!(t.contains(w), "{w}: {t}");
    }
}

#[test]
fn bin_batch_takes_ops_and_dbg_command_takes_grep() {
    let a = talk(false, &[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})]);
    let tools = a[0]["result"]["tools"].as_array().unwrap();
    let batch = tools.iter().find(|t| t["name"] == "bin_batch").unwrap();
    assert_eq!(batch["inputSchema"]["required"], json!(["ops"]));
    assert_eq!(batch["inputSchema"]["properties"]["ops"]["type"], "string");
    let dbg = tools.iter().find(|t| t["name"] == "dbg_command").unwrap();
    for (arg, _, _) in dbg_flags() {
        assert!(dbg["inputSchema"]["properties"].get(arg).is_some(), "{arg} missing from dbg_command");
    }
    assert_eq!(dbg["inputSchema"]["properties"]["grep"]["type"], "string");
    // --timing is a tool input now
    let info = tools.iter().find(|t| t["name"] == "bin_info").unwrap();
    assert_eq!(info["inputSchema"]["properties"]["timing"]["type"], "boolean");
}

#[test]
fn bin_batch_runs_each_op_and_keeps_the_write_gate() {
    let dir = tempfile::tempdir().unwrap();
    let exe = fixture(dir.path());
    let ops = format!("# a comment\ninfo {exe}; asm --help; info /nonexistent/none");
    let a = talk(false, &[call(1, "bin_batch", json!({"ops": ops}))]);
    assert_eq!(a[0]["result"]["isError"], false, "{a:?}");
    let t = result_text(&a[0]);
    assert!(t.contains(&format!("== info {exe} ==")) && t.contains("error: nexo bin:"), "{t}");
    // one saving op refuses the whole call before anything runs
    let out = dir.path().join("saved");
    let ops = format!("info {exe}; note {exe} 0x1000 hi; patch {exe} --out {}", out.display());
    let a = talk(
        false,
        &[call(2, "bin_batch", json!({"ops": ops})), call(3, "bin_batch", json!({"ops": "crypto p --save"}))],
    );
    assert!(refused(&a[0]) && refused(&a[1]), "{a:?}");
    assert!(!result_text(&a[0]).contains("== info"), "nothing runs when one op is refused");
    assert!(!out.exists());
    let a = talk(false, &[call(4, "bin_batch", json!({})), call(5, "bin_batch", json!({"ops": " ; # x"}))]);
    assert_eq!(a[0]["result"]["isError"], true);
    assert_eq!(a[1]["result"]["isError"], true);
}
