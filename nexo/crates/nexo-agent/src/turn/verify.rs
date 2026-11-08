//! Part of the agent loop; see the parent module.
//!
//! `agent.close_on_verified`: when a reply proves finished work: the turn changed something and a check succeeded
//! after the last change. Reads and listings never prove anything.

use super::*;

/// The shortest text that can be an answer.
const ANSWER_MIN: usize = 20;

/// Text that announces more work (a lead-in) instead of giving the answer: too short (the plan said 40, but its own
/// example answer, "Fixed the parser; tests pass.", has 29), or ending in `:` or `...`.
pub(super) fn is_lead_in(text: &str) -> bool {
    let t = text.trim();
    t.chars().count() < ANSWER_MIN || t.ends_with(':') || t.ends_with("...") || t.ends_with('…')
}

/// Whether a shell command only looks at things or runs a check: every part of the line (split at `&&`, `||`, `;`
/// and `|`) is a read-only command or a build/test/lint run, with no redirection to a file, no substitution and
/// no heredoc. Conservative: anything unknown is not a check.
pub(super) fn is_check_command(cmd: &str) -> bool {
    let cmd = cmd.trim();
    if cmd.is_empty()
        || cmd.contains('`')
        || cmd.contains("$(")
        || cmd.contains("<<")
        || cmd.contains('\n') && cmd.contains('>')
    {
        return false;
    }
    if writes_to_file(cmd) {
        return false;
    }
    let flat = cmd.replace("&&", "\n").replace("||", "\n").replace(['|', ';', '\n'], "\n");
    let mut any = false;
    for seg in flat.lines().map(str::trim).filter(|s| !s.is_empty()) {
        if !is_check_segment(seg) {
            return false;
        }
        any = true;
    }
    any
}

/// A `>` or `>>` that is not a descriptor copy (`2>&1`) or a send to /dev/null.
fn writes_to_file(cmd: &str) -> bool {
    let b = cmd.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        if c != b'>' {
            continue;
        }
        if i > 0 && b[i - 1] == b'>' {
            continue;
        }
        let rest = cmd[i..].trim_start_matches('>').trim_start();
        if rest.starts_with('&') || rest.starts_with("/dev/null") {
            continue;
        }
        return true;
    }
    false
}

/// A `VAR=value` word.
fn is_assignment(w: &str) -> bool {
    w.contains('=') && !w.starts_with('-') && !w.starts_with('=')
}

/// Index of the first word after `i` that is not an option; `with_value` lists the options that take the next
/// word as their value. `--` ends the options.
fn skip_opts(w: &[&str], mut i: usize, with_value: &[&str]) -> usize {
    while let Some(&o) = w.get(i) {
        if o == "--" {
            return i + 1;
        }
        if !o.starts_with('-') || o == "-" {
            break;
        }
        i += if with_value.contains(&o) { 2 } else { 1 };
    }
    i
}

/// Index of the program a segment runs, past leading `VAR=value` words and prefixes that only run another command
/// (`nice`, `timeout`, `env`, `ionice`, `time`, `command`, `stdbuf`) with their own options, nested at will.
/// None: no program, or a prefix this does not understand.
fn program_index(w: &[&str]) -> Option<usize> {
    let mut i = 0;
    loop {
        while w.get(i).is_some_and(|x| is_assignment(x)) {
            i += 1;
        }
        let name = w.get(i)?.rsplit('/').next()?;
        let after = i + 1;
        i = match name {
            "nice" => skip_opts(w, after, &["-n", "--adjustment"]),
            "ionice" => skip_opts(w, after, &["-c", "-n", "--class", "--classdata"]),
            "timeout" => skip_opts(w, after, &["-s", "-k", "--signal", "--kill-after"]) + 1,
            "env" => skip_opts(w, after, &["-u", "--unset", "-C", "--chdir"]),
            "time" => skip_opts(w, after, &[]),
            "stdbuf" => skip_opts(w, after, &["-i", "-o", "-e", "--input", "--output", "--error"]),
            "command" => {
                let mut j = after;
                while w.get(j) == Some(&"-p") {
                    j += 1;
                }
                if w.get(j) == Some(&"--") {
                    j += 1;
                }
                j
            }
            _ => return Some(i),
        };
    }
}

/// The targets of a `make` line: words that are neither options (with the values of `-C`, `-f`, `-j N`) nor `VAR=x`.
fn make_targets<'a>(rest: &[&'a str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut it = rest.iter().peekable();
    while let Some(&w) = it.next() {
        if matches!(w, "-C" | "-f" | "-I" | "-o" | "-W") {
            it.next();
        } else if matches!(w, "-j" | "-l") {
            // the count is optional: `make -j install` has a target
            it.next_if(|n| (**n).bytes().all(|b| b.is_ascii_digit() || b == b'.'));
        } else if !w.starts_with('-') && !w.contains('=') {
            out.push(w);
        }
    }
    out
}

fn is_check_segment(seg: &str) -> bool {
    let words: Vec<&str> = seg.split_whitespace().collect();
    let Some(at) = program_index(&words) else { return false };
    let prog = words[at];
    let prog = prog.rsplit('/').next().unwrap_or(prog);
    let rest = &words[at + 1..];
    let sub = rest.iter().find(|w| !w.starts_with('-') && !w.starts_with('+')).copied().unwrap_or("");
    let has = |f: &str| rest.contains(&f);
    match prog {
        "true" | "test" | "[" | "cat" | "ls" | "head" | "tail" | "wc" | "grep" | "egrep" | "fgrep" | "rg" | "diff"
        | "cmp" | "stat" | "file" | "pwd" | "which" | "cd" | "nl" | "sort" | "uniq" | "cut" | "tr" | "tree" | "du"
        | "ctest" | "pytest" | "jest" | "vitest" | "mypy" | "shellcheck" | "tsc" | "phpunit" | "rspec" | "tox"
        | "nox" | "sha256sum" | "md5sum" | "jq" | "xxd" | "od" => prog != "tsc" || has("--noEmit"),
        "find" => !rest.iter().any(|w| matches!(*w, "-delete" | "-exec" | "-execdir" | "-ok" | "-fprint")),
        "sed" => !rest.iter().any(|w| w.starts_with("-i") || w.starts_with("--in-place")),
        "awk" => !seg.contains("system("),
        "cargo" => {
            matches!(
                sub,
                "test" | "t" | "check" | "c" | "build" | "b" | "clippy" | "nextest" | "doc" | "tree" | "metadata"
            ) || (sub == "fmt" && has("--check"))
        }
        "npm" | "pnpm" | "yarn" | "bun" => {
            sub == "test"
                || (sub == "run" && rest.iter().skip_while(|w| **w != "run").nth(1).is_some_and(|s| is_test_script(s)))
        }
        "npx" => matches!(sub, "jest" | "vitest" | "tsc" | "eslint" | "playwright") && !has("--fix"),
        "eslint" | "ruff" | "flake8" | "pylint" | "golangci-lint" => !has("--fix") && !has("--write"),
        "python" | "python3" => {
            rest.windows(2).any(|w| w[0] == "-m" && matches!(w[1], "pytest" | "unittest" | "mypy" | "doctest"))
        }
        "go" => matches!(sub, "test" | "vet" | "build" | "list"),
        "make" => make_targets(rest).iter().all(|t| is_test_script(t) || matches!(*t, "all" | "build")),
        "just" | "task" => is_test_script(sub),
        "cmake" => {
            has("--build") && !rest.iter().any(|w| matches!(*w, "install" | "clean" | "--install" | "uninstall"))
        }
        "ninja" => !rest.iter().any(|w| matches!(*w, "install" | "clean" | "uninstall" | "-t")),
        "node" => rest.iter().take_while(|w| w.starts_with('-')).any(|w| *w == "--test" || w.starts_with("--test-")),
        "bash" | "sh" | "dash" | "zsh" => rest.first() == Some(&"-n"),
        "mvn" | "gradle" | "gradlew" | "./gradlew" | "dotnet" | "swift" | "zig" | "deno" | "mix" | "bundle" => {
            matches!(sub, "test" | "check" | "verify")
        }
        "git" => matches!(sub, "diff" | "status" | "log" | "show" | "ls-files" | "blame" | "rev-parse" | "grep"),
        _ => false,
    }
}

fn is_test_script(s: &str) -> bool {
    matches!(s, "test" | "tests" | "check" | "lint" | "verify" | "typecheck" | "type-check")
}

/// What the calls of one reply did to the work, in order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Judged {
    /// An edit or write ran, or a shell command that is neither read-only nor a plain check.
    pub changed: bool,
    /// A check succeeded after the last change of this reply (or with nothing changed in it): a build, test or
    /// lint command, or the `check` of an edit or write.
    pub checked: bool,
}

/// Sorts the calls of one reply: reads and searches (and read-only shell commands, which `call_kind` already
/// tells apart) count for nothing; a write changes the work and is proved only by its own `check`; a shell
/// command that is a build, test or lint run and nothing else is a check; any other command changes the work.
/// `ok` tells for each call whether it succeeded. `kind` gives the kind of a call by its name and arguments
/// (None: unknown tool).
pub(super) fn judge(
    calls: &[(String, String, Value)],
    ok: &[bool],
    kind: impl Fn(&str, &Value) -> Option<ToolKind>,
) -> Judged {
    let mut out = Judged::default();
    for (i, (_, name, args)) in calls.iter().enumerate() {
        let done = ok.get(i).copied().unwrap_or(false);
        match kind(name, args) {
            Some(ToolKind::Write) => {
                out.changed = true;
                out.checked = done && args.get("check").and_then(Value::as_str).is_some_and(|c| !c.trim().is_empty());
            }
            Some(ToolKind::Exec) => {
                let plain = name == "bash" && !args.get("background").and_then(Value::as_bool).unwrap_or(false);
                let cmd = args.get("command").and_then(Value::as_str).unwrap_or("");
                if plain && nexo_tools::project::is_check_command(cmd) && is_check_command(cmd) {
                    out.checked = done;
                } else {
                    out.changed = true;
                    out.checked = false;
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lead_ins_are_short_or_announce_more() {
        assert!(is_lead_in("Let me check:"));
        assert!(is_lead_in("Running tests now"));
        assert!(is_lead_in("I will now run the whole test suite to be sure..."));
        assert!(is_lead_in("I will now run the whole test suite to be sure:"));
        assert!(!is_lead_in("Fixed the parser; the three tests in parse.rs pass."));
    }

    #[test]
    fn checks_and_reads_are_verification_and_the_rest_is_not() {
        for ok in [
            "true",
            "cargo test -q -p nexo-agent",
            "cd crates/x && cargo test 2>&1 | tail -20",
            "RUST_LOG=debug cargo clippy --all-targets -- -D warnings",
            "npm test -s",
            "npm run lint",
            "pytest -q -x",
            "python3 -m pytest tests",
            "go test ./...",
            "make test",
            "git diff --stat",
            "grep -rn foo src | head",
            "sed -n 1,20p src/lib.rs",
            "cargo fmt --check",
            "cargo test > /dev/null 2>&1",
        ] {
            assert!(is_check_command(ok), "{ok}");
        }
        for bad in [
            "",
            "cargo run",
            "cargo install foo",
            "cargo test > out.txt",
            "echo hi >> log",
            "sed -i s/a/b/ f",
            "rm -rf target && cargo test",
            "cargo test && git commit -am x",
            "cat <<'EOF' > t.rs\nEOF",
            "find . -name '*.o' -delete",
            "npm install",
            "cargo fmt",
            "make deploy",
            "echo $(rm x)",
            "false || deploy.sh",
        ] {
            assert!(!is_check_command(bad), "{bad}");
        }
    }

    #[test]
    fn prefixes_that_only_run_another_command_do_not_hide_a_check() {
        for ok in [
            "nice cargo test",
            "nice -n 10 cargo test -j 2",
            "nice -10 cargo test",
            "nice --adjustment=5 cargo test",
            "timeout 300 cargo test",
            "timeout -s KILL -k 5 300 cargo test",
            "timeout --signal=INT 1m cargo clippy",
            "env RUST_LOG=x cargo test",
            "env -i PATH=/usr/bin cargo test",
            "env -u HOME FOO=1 cargo check",
            "time cargo test",
            "time -p cargo test",
            "command cargo test",
            "command -p cargo test",
            "stdbuf -oL cargo test",
            "stdbuf -o L -e0 cargo test",
            "ionice -c3 nice cargo clippy",
            "ionice -c 2 -n 7 cargo test",
            "RUST_LOG=x nice -n 5 timeout 600 env A=1 time cargo test -q 2>&1 | tail",
            "/usr/bin/time -p /usr/bin/nice cargo test",
            "cd x && nice cargo test",
        ] {
            assert!(is_check_command(ok), "{ok}");
        }
        for bad in [
            "nice",
            "nice -n 10",
            "timeout 5",
            "env",
            "env X=1",
            "command -v cargo",
            "nice rm -rf x",
            "timeout 5 cargo install foo",
            "env X=1 sed -i s/a/b/ f",
            "ionice -c3 nice cargo run",
            "time make install",
            "stdbuf -oL cargo fmt",
            "nice cargo test > out.txt",
            "nice cargo test && nice git commit -am x",
        ] {
            assert!(!is_check_command(bad), "{bad}");
        }
    }

    #[test]
    fn build_runs_are_checks_and_installs_and_cleans_are_not() {
        for ok in [
            "cargo build",
            "cargo build --release -p nexo-agent",
            "cargo b",
            "cargo t -q",
            "cargo c",
            "cargo +nightly build",
            "make",
            "make all",
            "make build",
            "make -j 8",
            "make -j",
            "make -C build -j4 all",
            "make test",
            "cmake --build build",
            "cmake --build . --config Release -j 4",
            "ninja",
            "ninja -C build",
            "ninja test",
            "go vet ./...",
            "go build ./...",
            "node --test",
            "node --test tests/",
            "node --test-reporter=spec --test",
            "bash -n run.sh",
            "sh -n run.sh",
            "nice make -j 4",
            "timeout 600 cmake --build build",
        ] {
            assert!(is_check_command(ok), "{ok}");
        }
        for bad in [
            "cargo run",
            "cargo run --release",
            "cargo install foo",
            "cargo fmt",
            "make install",
            "make clean",
            "make -j install",
            "make -C build clean",
            "make all install",
            "make deploy",
            "cmake --build build --target install",
            "cmake --install build",
            "cmake -S . -B build",
            "ninja install",
            "ninja clean",
            "ninja -t clean",
            "node server.js",
            "node script.js --test",
            "bash run.sh",
            "bash -c 'rm x'",
            "sh install.sh",
        ] {
            assert!(!is_check_command(bad), "{bad}");
        }
    }

    fn kind(n: &str, a: &Value) -> Option<ToolKind> {
        match n {
            "read" => Some(ToolKind::Read),
            "grep" => Some(ToolKind::Search),
            // as the shell tool tells them apart: a command that only looks is a read
            "bash" if a["command"].as_str().is_some_and(|c| c.starts_with("grep ") || c.starts_with("ls")) => {
                Some(ToolKind::Read)
            }
            "bash" => Some(ToolKind::Exec),
            "edit" | "write" => Some(ToolKind::Write),
            _ => None,
        }
    }

    fn c(n: &str, a: Value) -> (String, String, Value) {
        ("id".to_string(), n.to_string(), a)
    }

    #[test]
    fn short_text_with_reads_and_no_change_is_not_a_proof() {
        // the vuln-hunt shape: reads and listings only
        let calls = [c("read", json!({})), c("grep", json!({})), c("bash", json!({"command": "ls -la src"}))];
        assert_eq!(judge(&calls, &[true; 3], kind), Judged::default());
        assert_eq!(judge(&[], &[], kind), Judged::default());
    }

    #[test]
    fn an_edit_and_a_passing_test_in_one_reply_is_a_proof() {
        let calls = [c("edit", json!({})), c("bash", json!({"command": "cargo test -q"}))];
        assert_eq!(judge(&calls, &[true, true], kind), Judged { changed: true, checked: true });
        // the test failed
        assert_eq!(judge(&calls, &[true, false], kind), Judged { changed: true, checked: false });
        // the test ran before the edit
        let calls = [c("bash", json!({"command": "cargo test -q"})), c("edit", json!({}))];
        assert_eq!(judge(&calls, &[true, true], kind), Judged { changed: true, checked: false });
    }

    #[test]
    fn an_edit_and_then_reads_only_is_not_a_proof() {
        let calls = [c("edit", json!({})), c("read", json!({})), c("bash", json!({"command": "git diff"}))];
        assert_eq!(judge(&calls, &[true; 3], kind), Judged { changed: true, checked: false });
    }

    #[test]
    fn a_write_is_proved_by_its_own_check_and_a_plain_command_is_a_change() {
        assert_eq!(
            judge(&[c("write", json!({"check": "cargo check"}))], &[true], kind),
            Judged { changed: true, checked: true }
        );
        assert!(!judge(&[c("edit", json!({"check": "  "}))], &[true], kind).checked);
        assert!(!judge(&[c("edit", json!({"check": "cargo check"}))], &[false], kind).checked);
        let sh = |cmd: &str, bg: bool| judge(&[c("bash", json!({"command": cmd, "background": bg}))], &[true], kind);
        assert_eq!(sh("rm -rf build", false), Judged { changed: true, checked: false });
        assert_eq!(sh("cargo test", true), Judged { changed: true, checked: false });
        assert_eq!(sh("cargo test && git commit -am x", false), Judged { changed: true, checked: false });
        assert_eq!(sh("cargo test", false), Judged { changed: false, checked: true });
        assert_eq!(judge(&[c("todo", json!({}))], &[true], kind), Judged::default());
    }
}
