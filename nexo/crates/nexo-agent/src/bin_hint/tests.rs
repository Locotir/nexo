use super::*;
use nexo_core::message::Message;
use serde_json::json;

const TOOL: Offer = Offer { bin: true, debug: true };
const SHELL: Offer = Offer { bin: false, debug: false };

/// This test program: an ELF or PE x86-64 file `nexo_binmagic::opens`, read as bytes only.
fn this_binary() -> std::path::PathBuf {
    std::env::current_exe().unwrap()
}

/// A path as a command and a call write it: `/` for the separator on Windows too.
fn slashed(p: impl AsRef<Path>) -> String {
    hint_cmd::slash(&p.as_ref().display().to_string())
}

fn call(id: &str, command: &str) -> (String, String, Value) {
    (id.into(), "bash".into(), json!({"command": command}))
}

fn result(id: &str, text: &str) -> Part {
    Part::tool_result(id, text, false)
}

fn texts(parts: &[Part]) -> Vec<String> {
    parts
        .iter()
        .map(|p| match p {
            Part::ToolResult { content, .. } => content.iter().filter_map(Part::as_text).collect(),
            _ => String::new(),
        })
        .collect()
}

/// The line for `command`, run in `cwd`.
fn line(command: &str, cwd: &Path, offer: Offer) -> Option<String> {
    found(command, cwd, offer).map(|(_, l)| l)
}

fn wanted(command: &str, cwd: &Path) -> bool {
    line(command, cwd, TOOL).is_some()
}

/// The op of `nexo bin` the line for `command` names, as the shell form writes it.
fn op_in(command: &str, cwd: &Path) -> String {
    let l = line(command, cwd, SHELL).unwrap_or_else(|| panic!("no line for {command}"));
    let call = l.split_once("; nexo bin ").unwrap_or_else(|| panic!("no call in {l}")).1;
    call.split_once(MARK).unwrap().0.to_string()
}

/// A small ELF64 x86-64 executable with its symbols, written as `name` in `dir`.
fn small_in(dir: &Path, name: &str) -> u64 {
    let bytes = nexo_bin::fixtures::elf64(false).bytes;
    std::fs::write(dir.join(name), &bytes).unwrap();
    bytes.len() as u64
}

#[test]
fn a_command_that_never_ran_gets_no_line_and_does_not_spend_it() {
    let cwd = tempfile::tempdir().unwrap();
    let command = format!("objdump -d '{}'", slashed(this_binary()));
    for refusal in ["Permission denied: the person said no", "Blocked by a hook: no", "interrupted by the person"] {
        let mut told = Told::default();
        let out =
            round(&mut told, &[call("c1", &command)], vec![Part::tool_result("c1", refusal, true)], cwd.path(), TOOL);
        assert_eq!(texts(&out), [refusal.to_string()], "{refusal}");
        assert!(!told.said(), "{refusal}: the line is still there for a command that runs");
    }
    // a command that ran and failed (the reader is not installed) is where the line helps most
    let mut told = Told::default();
    let failed = "sh: objdump: command not found\n\n[exit code 127]";
    let out = round(&mut told, &[call("c1", &command)], vec![Part::tool_result("c1", failed, true)], cwd.path(), TOOL);
    assert!(texts(&out)[0].contains(MARK) && told.said(), "{:?}", texts(&out));
}

#[test]
fn a_line_that_already_uses_nexo_bin_gets_none_even_with_a_reader_in_it() {
    let cwd = tempfile::tempdir().unwrap();
    let exe = slashed(this_binary());
    assert!(wanted(&format!("echo x; xxd '{exe}'"), cwd.path()), "the same line without nexo bin asks for it");
    assert!(!wanted(&format!("nexo bin fn '{exe}' main; xxd '{exe}'"), cwd.path()));
    assert!(!wanted(&format!("nexo bin info '{exe}' | strings '{exe}'"), cwd.path()));
}

/// A named pipe nobody writes to: opening it never returns, so its kind is asked first.
#[cfg(unix)]
#[test]
fn a_named_pipe_is_not_opened() {
    let dir = tempfile::tempdir().unwrap();
    let pipe = dir.path().join("pipe");
    let made = std::process::Command::new("mkfifo").arg(&pipe).status();
    if !made.is_ok_and(|s| s.success()) {
        eprintln!("no mkfifo here: not checked");
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let cwd = dir.path().to_path_buf();
    std::thread::spawn(move || tx.send(wanted("strings pipe", &cwd)));
    let got = rx.recv_timeout(std::time::Duration::from_secs(5));
    // let a reader that did block go, so no thread outlives the test; opened for both ways, which never waits
    if got.is_err() {
        let _ = std::fs::OpenOptions::new().read(true).write(true).open(&pipe);
    }
    assert_eq!(got, Ok(false), "the check must come back at once and say no");
    assert!(!nexo_binmagic::opens(dir.path()), "a folder is not a program either");
}

#[test]
fn a_webassembly_module_asks_for_the_line_too() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("m.wasm"), b"\0asm\x01\0\0\0").unwrap();
    assert!(wanted("strings m.wasm", dir.path()));
    assert!(!wanted("strings notes.txt", dir.path()));
}

#[test]
fn the_line_tells_what_the_file_is_and_the_exact_call_as_a_tool_call_or_in_the_shell() {
    let dir = tempfile::tempdir().unwrap();
    let size = nexo_binmagic::size_words(small_in(dir.path(), "prog"));
    let l = line("objdump -d prog | head -50", dir.path(), TOOL).unwrap();
    assert_eq!(
        l,
        format!(
            "[nexo: prog: ELF64 x86-64 executable, {size}, with symbols; bin {{\"command\": \"overview prog\"}} reads it \
             in pages, with names and references, then fn|funcs|xref|decomp|strings]"
        )
    );
    let l = line("objdump -d prog | head -50", dir.path(), SHELL).unwrap();
    assert_eq!(
        l,
        format!(
            "[nexo: prog: ELF64 x86-64 executable, {size}, with symbols; nexo bin overview prog reads it in pages, with \
             names and references, then fn|funcs|xref|decomp|strings]"
        )
    );
    eprintln!(
        "bytes of the line: {} as a tool call, {} in the shell",
        line("nm prog", dir.path(), TOOL).unwrap().len(),
        l.len()
    );
    // a name with a space is quoted for the language of the tool, and cannot break the JSON
    std::fs::copy(this_binary(), dir.path().join("my prog")).unwrap();
    let l = line("nm 'my prog'", dir.path(), TOOL).unwrap();
    assert!(l.contains("funcs \\\"my prog\\\""), "{l}");
}

#[test]
fn each_reader_asks_for_the_op_that_gives_the_same() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(this_binary(), dir.path().join("p")).unwrap();
    let op = |c: &str| op_in(c, dir.path());
    assert_eq!(op("objdump -d p"), "overview p");
    assert_eq!(op("objdump -d -M intel p"), "overview p");
    assert_eq!(op("objdump -dS p"), "overview p");
    assert_eq!(op("objdump --disassemble=main p"), "fn p main");
    assert_eq!(op("objdump --start-address=0x401000 -d p"), "fn p 0x401000");
    // the paste: a range of one binary, both ends, either way of writing the option
    assert_eq!(op("objdump -d --start-address=0x11950 --stop-address=0x140a0 p"), "fn p 0x11950 --to 0x140a0");
    assert_eq!(op("objdump -d --start-address 0x11950 --stop-address 0x140a0 p"), "fn p 0x11950 --to 0x140a0");
    assert_eq!(op("objdump -d --stop-address=0x140a0 --start-address=0x11950 p"), "fn p 0x11950 --to 0x140a0");
    assert_eq!(op("objdump -d --start-address=0x11950 p"), "fn p 0x11950");
    // a bare `--disassemble` has no symbol: the word after it is the file, not a name
    assert_eq!(op("objdump --disassemble p"), "overview p");
    // a symbol is one word for the language of the tool and for a shell: `<>` and `~` are quoted, `$` and a
    // leading dash never get through
    assert_eq!(op("objdump '--disassemble=foo<int>::bar' p"), "fn p \"foo<int>::bar\"");
    assert_eq!(op("objdump --disassemble=~Foo p"), "fn p \"~Foo\"");
    assert_eq!(op("objdump '--disassemble=a$b' p"), "overview p");
    assert_eq!(op("objdump --disassemble=-x p"), "overview p");
    assert_eq!(op("r2 -qc 'pdf @ sym.a<b>' p"), "fn p \"sym.a<b>\"");
    assert_eq!(op("objdump -t p"), "funcs p");
    assert_eq!(op("objdump -T p"), "funcs p");
    assert_eq!(op("objdump -x p"), "info p");
    assert_eq!(op("objdump -h p"), "info p");
    assert_eq!(op("readelf -s p"), "funcs p");
    assert_eq!(op("readelf -Ws p"), "funcs p");
    assert_eq!(op("readelf -S p"), "info p");
    assert_eq!(op("readelf -p .rodata p"), "strings p");
    assert_eq!(op("objdump -s -j .rodata p"), "bytes p .rodata");
    assert_eq!(op("objdump -s -j .data p"), "bytes p .data");
    assert_eq!(op("objdump -sj .data p"), "bytes p .data");
    assert_eq!(op("objdump -s -j.data p"), "bytes p .data");
    assert_eq!(op("objdump --full-contents --section=.rodata p"), "bytes p .rodata");
    assert_eq!(op("objdump -s --section .rodata p"), "bytes p .rodata");
    assert_eq!(op("objdump -s p"), "info p");
    assert_eq!(op("objdump -d -j .text p"), "overview p");
    assert_eq!(op("objdump -s -j .got p"), "bytes p .got --as ptr");
    assert_eq!(op("objdump -s -j '$x' p"), "info p");
    assert_eq!(op("readelf -x .data p"), "bytes p .data");
    assert_eq!(op("readelf -x.data p"), "bytes p .data");
    assert_eq!(op("readelf -Wx .rodata p"), "bytes p .rodata");
    assert_eq!(op("readelf --hex-dump=.rodata p"), "bytes p .rodata");
    assert_eq!(op("readelf -x .data.rel.ro p"), "bytes p .data.rel.ro --as ptr");
    assert_eq!(op("readelf -x .init_array p"), "bytes p .init_array --as ptr");
    assert_eq!(op("readelf -x .fini_array p"), "bytes p .fini_array --as ptr");
    assert_eq!(op("readelf -x .got.plt p"), "bytes p .got.plt --as ptr");
    assert_eq!(op("readelf -x .symtab p"), "bytes p .symtab");
    assert_eq!(op("readelf -x 16 p"), "info p");
    assert_eq!(op("readelf -p.rodata p"), "strings p");
    assert_eq!(op("nm -C p"), "funcs p");
    assert_eq!(op("strings -n 8 p"), "strings p");
    assert_eq!(op("strings p | grep -i http"), "strings p --grep \"(?i)http\"");
    assert_eq!(op("strings p | grep -v http"), "strings p");
    assert_eq!(op("strings p | grep 'a|b' | head"), "strings p --grep \"a|b\"");
    assert_eq!(op("xxd p | head"), "info p");
    assert_eq!(op("hexdump -C p"), "info p");
    assert_eq!(op("od -x p"), "info p");
    assert_eq!(op("r2 -A p"), "overview p");
    assert_eq!(op("r2 -qc 'pdf @ main' p"), "fn p main");
    assert_eq!(op("rizin -c 'aaa; afl' p"), "funcs p");
    assert_eq!(op("r2 -c iz p"), "strings p");
    // gdb is the debugger's where the debug tool is offered, and the map where it is not
    assert_eq!(line("gdb -batch p", dir.path(), TOOL), None);
    assert_eq!(op("gdb -batch p"), "overview p");
}

#[test]
fn the_shapes_of_a_real_session_are_read_absolute_paths_cd_redirects_and_wrappers() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("tests");
    std::fs::create_dir(&sub).unwrap();
    std::fs::copy(this_binary(), sub.join("c-lcrypt")).unwrap();
    let abs = slashed(sub.join("c-lcrypt"));
    // an absolute path, with the output redirected and piped
    assert!(wanted(&format!("objdump -d {abs} 2>&1 | head -100"), dir.path()));
    assert!(wanted(&format!("objdump -d {abs} > /tmp/dump.txt 2>/dev/null"), dir.path()));
    // `cd` first: the file is found from there, and the call carries the whole path
    let l = line("cd tests && objdump -d c-lcrypt | head", dir.path(), TOOL).unwrap();
    assert!(l.contains(&hint_cmd::tool_call("bin", &format!("overview {}", hint_cmd::quoted(&abs)))), "{l}");
    assert!(!wanted("objdump -d c-lcrypt", dir.path()), "from here the name is nothing");
    // wrappers, variables, a script run by the shell
    assert!(wanted(&format!("sudo -n nm {abs}"), dir.path()));
    assert!(wanted(&format!("LC_ALL=C strings {abs}"), dir.path()));
    assert!(wanted(&format!("timeout 10 readelf -h {abs}"), dir.path()));
    assert!(wanted("bash -c 'cd tests; strings c-lcrypt'", dir.path()));
}

#[test]
fn not_for_text_missing_other_commands_or_nexo_bin_itself() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "hello\nworld\n").unwrap();
    std::fs::write(dir.path().join("run.sh"), "#!/bin/sh\necho hi\n").unwrap();
    let exe = slashed(this_binary());
    for c in [
        "strings notes.txt".to_string(),
        "objdump -d run.sh".into(),
        "objdump -d missing.out".into(),
        "objdump -d .".into(),
        format!("nexo bin fn '{exe}' main"),
        format!("nexo bin strings '{exe}'; strings '{exe}'"),
        format!("cat '{exe}' > copy"),
        format!("dd if='{exe}' of=copy"),
        format!("grep main '{exe}'"),
        format!("python3 -c 'print(1)' '{exe}x'"),
        format!("ls -l '{exe}'"),
        format!("echo objdump '{exe}'"),
        format!("which objdump '{exe}'"),
        String::new(),
    ] {
        assert!(!wanted(&c, dir.path()), "{c}");
    }
}

#[test]
fn a_hostile_name_is_only_text_nothing_runs_or_expands() {
    let dir = tempfile::tempdir().unwrap();
    let exe = this_binary();
    let shown = slashed(&exe);
    // each of these would create `touched` if anything ran it; none is a file nexo bin opens
    for c in [
        format!("objdump -d $(touch touched) '{}'", shown),
        format!("objdump -d `touch touched` '{}'", shown),
        "objdump -d '$(touch touched)'".to_string(),
        "objdump -d 'a\ntouch touched'".to_string(),
        "objdump -d\ntouch touched".to_string(),
        "objdump -d -- -rf".to_string(),
        format!("objdump -d '-{}'", shown),
        "strings ; touch touched".to_string(),
        "strings <(touch touched)".to_string(),
    ] {
        assert!(!wanted(&c, dir.path()), "{c:?}");
    }
    assert!(!dir.path().join("touched").exists());
    // a dash or a newline inside a real name is just a name, and a name the call cannot carry gets no line
    let odd = dir.path().join("a\nb-$(touch touched)");
    if std::fs::copy(&exe, &odd).is_ok() {
        assert!(!wanted("objdump -d 'a\nb-$(touch touched)'", dir.path()), "no newline in a call");
        assert!(!dir.path().join("touched").exists());
    }
    let quoted = dir.path().join("a\"b");
    if std::fs::copy(&exe, &quoted).is_ok() {
        assert!(!wanted("objdump -d 'a\"b'", dir.path()), "no double quote in a call");
    }
    let dashed = dir.path().join("-prog");
    std::fs::copy(&exe, &dashed).unwrap();
    assert!(!wanted("objdump -d -prog", dir.path()), "an option, not a file");
    assert!(wanted("objdump -d ./-prog", dir.path()));
}

#[cfg(unix)]
#[test]
fn a_link_to_a_binary_is_read_as_bytes_and_a_link_to_nothing_is_not() {
    let dir = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(this_binary(), dir.path().join("link")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("gone"), dir.path().join("dangling")).unwrap();
    assert!(wanted("nm link", dir.path()));
    assert!(!wanted("nm dangling", dir.path()));
}

#[test]
fn the_line_closes_the_first_result_that_earns_it_and_only_that_one() {
    let dir = tempfile::tempdir().unwrap();
    let hit = format!("objdump -d '{}'", slashed(this_binary()));
    let calls = vec![call("a", "ls"), call("b", &hit), call("c", &hit)];
    let results = vec![result("a", "files"), result("b", "disassembly"), result("c", "again")];
    let mut told = Told::default();
    let out = round(&mut told, &calls, results, dir.path(), TOOL);
    assert!(told.said());
    let said = texts(&out);
    assert_eq!((said[0].as_str(), said[2].as_str()), ("files", "again"));
    assert!(said[1].starts_with("disassembly\n\n[nexo: ") && said[1].contains(MARK), "{said:?}");
    // later rounds: nothing
    let again = round(&mut told, &calls, vec![result("b", "more")], dir.path(), TOOL);
    assert_eq!(texts(&again), ["more"]);
}

#[test]
fn a_failed_command_still_gets_the_line_and_a_background_one_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let hit = format!("objdump -d '{}'", slashed(this_binary()));
    let mut told = Told::default();
    let bg = vec![("a".to_string(), "bash".to_string(), json!({"command": hit, "background": true}))];
    let out = round(&mut told, &bg, vec![result("a", "job started")], dir.path(), TOOL);
    assert!(!told.said() && texts(&out) == ["job started"]);
    let calls = vec![call("b", &hit)];
    let err = Part::tool_result("b", "bash: objdump: command not found\n\n[exit code 127]", true);
    let out = round(&mut told, &calls, vec![err], dir.path(), TOOL);
    assert!(told.said());
    assert!(texts(&out)[0].contains(MARK));
    assert!(matches!(out[0], Part::ToolResult { is_error: true, .. }));
}

#[test]
fn only_bash_calls_are_looked_at() {
    let dir = tempfile::tempdir().unwrap();
    let hit = format!("objdump -d '{}'", slashed(this_binary()));
    let calls = vec![("a".to_string(), "read".to_string(), json!({"command": hit}))];
    let mut told = Told::default();
    let out = round(&mut told, &calls, vec![result("a", "x")], dir.path(), TOOL);
    assert!(!told.said() && texts(&out) == ["x"]);
}

#[test]
fn told_finds_either_line_and_the_old_one_in_a_kept_history_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let hit = format!("objdump -d '{}'", slashed(this_binary()));
    for offer in [TOOL, SHELL] {
        let l = line(&hit, dir.path(), offer).unwrap();
        let with = Message::tool_results(vec![result("b", &format!("out\n\n{l}"))]);
        assert!(told(&[Message::user("hi"), with]).said());
    }
    for old in told::OLD {
        let old = Message::tool_results(vec![result("b", &format!("out\n\n{old} nexo bin overview p]"))]);
        assert!(told(&[old]).said(), "a session saved with an old line has it");
    }
    let without = Message::tool_results(vec![result("b", "out")]);
    // the model quoting the line is not nexo having said it
    let l = line(&hit, dir.path(), TOOL).unwrap();
    assert!(!told(&[Message::user(&l), without, Message::assistant(&l)]).said());
    assert!(!told(&[]).said());
}

#[test]
fn the_line_follows_the_results_text_joined_by_newlines_and_a_told_session_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let hit = format!("objdump -d '{}'", slashed(this_binary()));
    let calls = vec![call("a", &hit)];
    let two = Part::ToolResult {
        call_id: "a".into(),
        name: Some("bash".into()),
        content: vec![Part::text("one"), Part::text("two")],
        is_error: false,
    };
    let mut told = Told::default();
    let out = round(&mut told, &calls, vec![two.clone()], dir.path(), TOOL);
    let Part::ToolResult { content, .. } = &out[0] else { panic!("not a result") };
    assert_eq!(content.len(), 1);
    assert!(content[0].as_text().unwrap().starts_with("one\ntwo\n\n[nexo: "), "{:?}", content[0].as_text());
    assert!(told.said());
    let mut spent = told.clone();
    assert_eq!(round(&mut spent, &calls, vec![two.clone()], dir.path(), TOOL), vec![two]);
}

#[test]
fn the_readers_a_model_reaches_for_first_are_seen_too() {
    let dir = tempfile::tempdir().unwrap();
    small_in(dir.path(), "p");
    let op = |c: &str| op_in(c, dir.path());
    assert_eq!(op("file p"), "info p");
    assert_eq!(op("ldd p"), "overview p");
    assert_eq!(op("otool -L p"), "info p");
    assert_eq!(op("llvm-objdump -d p"), "overview p");
    assert_eq!(op("x86_64-linux-gnu-readelf -S p"), "info p");
    assert_eq!(op("llvm-readobj --sections p"), "info p");
    assert_eq!(op("cat p | head"), "info p");
    assert_eq!(op("head -c 64 p | xxd"), "info p");
    assert_eq!(op("tail -c 100 p"), "info p");
    assert_eq!(op("dd if=p bs=1 count=64 2>/dev/null | od -c"), "info p");
    assert_eq!(op("od -A x -t x1z -N 128 p"), "info p");
    assert_eq!(op("hd p"), "info p");
    assert_eq!(op("xxd < p | head"), "info p");
    // a script that opens the file by a quoted name, on its command line or in a heredoc
    assert_eq!(op("python3 -c \"print(open('p', 'rb').read(64))\""), "overview p");
    assert_eq!(op("perl -e 'open(F, \"<\", \"p\"); print <F>'"), "overview p");
    assert_eq!(op("python3 - <<'EOF'\nimport struct\ndata = open(\"p\", \"rb\").read()\nEOF"), "overview p");
    std::fs::write(dir.path().join("parse.py"), "import sys\nprint(open(sys.argv[1], 'rb').read(64))\n").unwrap();
    assert_eq!(op("python3 parse.py p"), "overview p");
    // grep reads a compiled file only when told to take it as text: the pattern goes to `strings --grep`
    assert_eq!(op("grep -a helper p"), "strings p --grep helper");
    assert_eq!(op("grep -ai Helper p"), "strings p --grep \"(?i)Helper\"");
    assert_eq!(op("grep --text --ignore-case helper p"), "strings p --grep \"(?i)helper\"");
    assert_eq!(op("grep -ac helper p"), "strings p");
}

#[test]
fn a_script_that_runs_the_program_it_names_is_not_reading_it() {
    let dir = tempfile::tempdir().unwrap();
    small_in(dir.path(), "app");
    std::fs::write(dir.path().join("parse.py"), "import sys\ndata = open(sys.argv[1], 'rb').read()\n").unwrap();
    std::fs::write(dir.path().join("run_tests.py"), "import subprocess, sys\nsubprocess.run([sys.argv[1]])\n").unwrap();
    let w = |c: &str| wanted(c, dir.path());
    // a parser opens the file: from a script file, on the line, or line by line with `perl -n`
    assert!(w("python3 parse.py app"));
    assert!(w("python3 -c \"import lief; print(lief.parse('app').header)\""));
    assert!(w("perl -ne 'print if /GLIBC/' app"));
    // a test runner or a launcher starts it; a module or a script that cannot be read says nothing of what it does
    for c in [
        "python3 run_tests.py app",
        "python3 -c \"import subprocess; subprocess.run(['./app'])\"",
        "python3 -c \"import os; os.system('./app')\"",
        "python3 - <<'EOF'\nimport subprocess\nout = subprocess.check_output(['./app'])\nEOF",
        "perl -e 'system(\"./app\")'",
        "perl -e 'open(my $f, \"-|\", \"./app\"); print <$f>'",
        "python3 -m pytest app",
        "python3 missing.py app",
    ] {
        assert!(!w(c), "{c}");
    }
}

#[test]
fn managed_code_a_32_bit_program_and_a_webassembly_module_are_told_with_their_own_call() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("App.class"), [0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 52]).unwrap();
    assert_eq!(
        line("strings App.class", dir.path(), SHELL).unwrap(),
        format!(
            "[nexo: App.class: JVM class, 8 bytes; nexo bin managed App.class{MARK}, then a class or method by name]"
        )
    );
    // an ELF32 x86 executable with no section headers, as `strip` leaves one
    let mut e = vec![0u8; 52];
    e[..4].copy_from_slice(b"\x7fELF");
    (e[4], e[5], e[6], e[7]) = (1, 1, 1, 3);
    e[16..18].copy_from_slice(&2u16.to_le_bytes());
    e[18..20].copy_from_slice(&3u16.to_le_bytes());
    std::fs::write(dir.path().join("old"), &e).unwrap();
    assert_eq!(
        line("objdump -d old", dir.path(), SHELL).unwrap(),
        format!("[nexo: old: ELF32 x86 Linux executable, 52 bytes, stripped; nexo bin overview old{MARK}{THEN}]")
    );
    std::fs::write(dir.path().join("m.wasm"), b"\0asm\x01\0\0\0").unwrap();
    assert_eq!(op_in("objdump -s -j .data m.wasm", dir.path()), "overview m.wasm");
    assert!(line("xxd m.wasm", dir.path(), SHELL).unwrap().ends_with(&format!("{THEN_WASM}]")));
}

#[test]
fn another_file_is_told_again_only_after_a_long_stretch_and_a_resumed_session_counts_the_same() {
    let dir = tempfile::tempdir().unwrap();
    small_in(dir.path(), "p");
    small_in(dir.path(), "q");
    let mut told = Told::default();
    let mut history = Vec::new();
    // one round of `command`, kept in the history as the session saves it; whether it got a line
    let mut run = |told: &mut Told, command: &str| {
        let out = round(told, &[call("a", command)], vec![result("a", "out")], dir.path(), TOOL);
        let said = texts(&out)[0].contains(MARK);
        history.push(Message::tool_results(out));
        said
    };
    assert!(run(&mut told, "strings p"));
    assert!(!run(&mut told, "strings p"), "the same file is told once");
    for n in 2..told::GAP {
        assert!(!run(&mut told, "strings q"), "round {n}: too soon after the last line");
    }
    assert!(run(&mut told, "strings q"), "a new file after the stretch has facts of its own");
    for _ in 0..told::GAP {
        assert!(!run(&mut told, "strings p"), "a file already told is not told again");
    }
    assert_eq!(super::told(&history), told, "the history gives back the same count");
}
