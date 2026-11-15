use super::words::strip_wrappers;
use super::*;

fn at(command: &str) -> Vec<Seg> {
    segments(command, Path::new("/work")).unwrap_or_else(|| panic!("unreadable: {command:?}"))
}

fn words(command: &str) -> Vec<Vec<String>> {
    at(command).into_iter().map(|s| s.words).collect()
}

fn w(l: &[&str]) -> Vec<String> {
    l.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_pipeline_a_list_and_quotes_split_like_a_shell() {
    assert_eq!(
        words("objdump -d 'my prog' | head -5 && ls; echo \"a b\""),
        [w(&["objdump", "-d", "my prog"]), w(&["head", "-5"]), w(&["ls"]), w(&["echo", "a b"])]
    );
    assert_eq!(words("a\\ b c"), [w(&["a b", "c"])]);
    assert_eq!(words("# note, it's here\nls"), [w(&["ls"])]);
    assert_eq!(words("ls \\\n -l"), [w(&["ls", "-l"])]);
}

#[test]
fn redirections_are_set_aside_not_taken_for_arguments() {
    let s = &at("gdb -q prog 2>&1 | tail -40")[0];
    assert_eq!(s.words, w(&["gdb", "-q", "prog"]));
    let s = &at("./paint big > out.txt 2>/dev/null &")[0];
    assert_eq!((s.words.clone(), s.output.clone()), (w(&["./paint", "big"]), Some("out.txt".to_string())));
    let s = &at("./paint &> all.txt")[0];
    assert_eq!((s.words.clone(), s.output.clone()), (w(&["./paint"]), Some("all.txt".to_string())));
    let s = &at("./paint >> log < in.txt")[0];
    assert_eq!(s.output.as_deref(), Some("log"));
    assert_eq!(s.input, Some(Input::File("in.txt".into())));
    let s = &at("./paint <<< '3 4 x'")[0];
    assert_eq!((s.words.clone(), s.input.clone()), (w(&["./paint"]), Some(Input::Text("3 4 x".into()))));
    assert_eq!(words("echo 5 1>&2"), [w(&["echo", "5"])]);
}

#[test]
fn a_heredoc_body_goes_to_the_command_that_opened_it_and_is_never_read_as_commands() {
    let s = at("cat > g1.gdb <<'EOF'\nbreak *main\nrun $(rm -rf x) `y`\nend\nEOF\ngdb -x g1.gdb prog 2>&1 | tail");
    assert_eq!(s.len(), 3, "{s:?}");
    assert_eq!(s[0].words, w(&["cat"]));
    assert_eq!(s[0].output.as_deref(), Some("g1.gdb"));
    assert_eq!(s[0].input, Some(Input::Text("break *main\nrun $(rm -rf x) `y`\nend\n".into())));
    assert_eq!(s[1].words, w(&["gdb", "-x", "g1.gdb", "prog"]));
    // piped into the program, `<<-` with tabs, and a body that never ends
    let s = at("cat <<EOF | gdb -batch prog\nrun\nEOF\nls");
    assert_eq!(
        (s[0].input.clone(), s[1].words.clone(), s[2].words.clone()),
        (Some(Input::Text("run\n".into())), w(&["gdb", "-batch", "prog"]), w(&["ls"]))
    );
    let s = at("cat <<-EOF\n\tx\n\tEOF\nls");
    assert_eq!((s[0].input.clone(), s.len()), (Some(Input::Text("\tx\n".into())), 2));
    assert_eq!(at("cat <<EOF\nnever closed")[0].input, Some(Input::Text("never closed\n".into())));
}

#[test]
fn cd_and_groups_say_where_a_command_runs() {
    let s = at("cd /tmp/tests && gdb -batch prog; ls");
    assert_eq!((s[0].dir.clone(), s[1].dir.clone()), (PathBuf::from("/tmp/tests"), PathBuf::from("/tmp/tests")));
    let s = at("cd build; ./paint");
    assert_eq!(s[0].dir, PathBuf::from("/work/build"));
    // a group gives the folder back when it closes; `cd -`, `~` and `$X` say nothing
    let s = at("(cd build && ./a) ; ./b");
    assert_eq!((s[0].dir.clone(), s[1].dir.clone()), (PathBuf::from("/work/build"), PathBuf::from("/work")));
    assert_eq!(at("cd ~/x && ./a")[0].dir, PathBuf::from("/work"));
    assert_eq!(at("cd $HOME && ./a")[0].dir, PathBuf::from("/work"));
    assert_eq!(at("cd - && ./a")[0].dir, PathBuf::from("/work"));
}

#[test]
fn variables_and_wrappers_are_looked_through() {
    assert_eq!(words("FOO=1 BAR=2 gdb -batch p"), [w(&["gdb", "-batch", "p"])]);
    assert_eq!(words("sudo -u me env A=1 timeout -s KILL 10 gdb p"), [w(&["gdb", "p"])]);
    assert_eq!(words("time nice -n 5 ./p x"), [w(&["./p", "x"])]);
    assert_eq!(words("stdbuf -oL ./p"), [w(&["./p"])]);
    assert_eq!(words("sh -c 'cd /x && gdb -batch p'"), [w(&["gdb", "-batch", "p"])]);
    assert_eq!(at("bash -lc \"cd /x; ./p\"")[0].dir, PathBuf::from("/x"));
    // `command -v` and `which` are not a run
    assert_eq!(words("command -v gdb"), [w(&["command", "-v", "gdb"])]);
    assert_eq!(words("which gdb"), [w(&["which", "gdb"])]);
}

#[test]
fn a_wrapper_option_with_no_value_left_is_read_without_panicking() {
    // the line may be cut anywhere: `sudo -u` at the end has no user to skip
    for c in ["sudo -u", "sudo -u; ls", "timeout -s", "timeout -s KILL", "nice -n", "env -", "sudo -n -u -g"] {
        let _ = at(c);
    }
    assert_eq!(strip_wrappers(w(&["sudo", "-u"])), Vec::<String>::new());
    assert_eq!(strip_wrappers(w(&["timeout", "-s"])), Vec::<String>::new());
    assert_eq!(words("sudo -u me gdb p; sudo -u")[0], w(&["gdb", "p"]));
}

#[test]
fn a_substitution_or_an_open_quote_is_unreadable_and_nothing_is_run() {
    for c in [
        "objdump -d $(touch touched) prog",
        "objdump -d `touch touched` prog",
        "echo \"$(touch touched)\"",
        "echo \"`touch touched`\"",
        "cat <(echo x)",
        "tee >(cat) x",
        "objdump -d 'unclosed",
        "echo \"unclosed",
    ] {
        assert!(segments(c, Path::new("/work")).is_none(), "{c}");
    }
    assert_eq!(segments("", Path::new("/work")).unwrap(), Vec::<Seg>::new());
    assert!(segments(&"a ".repeat(MAX_COMMAND), Path::new("/")).is_none());
    // single quotes keep every character as it is
    assert_eq!(words("echo '$(x) `y`'"), [w(&["echo", "$(x) `y`"])]);
}

#[test]
fn small_helpers() {
    assert_eq!(program("/usr/bin/GDB.exe"), "gdb");
    assert_eq!(program("C:\\tools\\objdump.exe"), "objdump");
    assert_eq!(quoted("a b"), "\"a b\"");
    assert_eq!(quoted("ab"), "ab");
    assert_eq!(quoted("(?i)a|b"), "\"(?i)a|b\"");
    assert_eq!(quoted("./p-1.2"), "./p-1.2");
    assert_eq!(shell_quoted("a b"), "'a b'");
    assert_eq!(shell_quoted("./p-1.2"), "./p-1.2");
    assert_eq!(shell_quoted("it's"), "'it'\\''s'");
    assert_eq!(tool_call("debug", "run \"a b\"; cont"), r#"debug {"command": "run \"a b\"; cont"}"#);
    assert!(ran("x\n\n[exit code 1]") && ran("[exit code -1]\n") && ran("x\n\n[exit code 127]\n\n[nexo: more]"));
    assert!(
        ran("partial\n\n[killed: timed out after 120 s]") && ran("[killed by a signal]") && !ran("x\n\n[cancelled]")
    );
    assert!(!ran("Permission denied: [exit code ]") && !ran("[exit code 1] and more") && !ran("exit code 1"));
}
