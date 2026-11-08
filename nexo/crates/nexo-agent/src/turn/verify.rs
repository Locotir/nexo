//! Part of the agent loop; see the parent module.
//!
//! `agent.close_on_verified`: when are the calls of a reply only a confirmation of finished work.

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

fn is_check_segment(seg: &str) -> bool {
    let mut words = seg.split_whitespace().peekable();
    // leading `VAR=value` assignments
    while words.peek().is_some_and(|w| w.contains('=') && !w.starts_with('-') && !w.starts_with('=')) {
        words.next();
    }
    let Some(prog) = words.next() else { return false };
    let prog = prog.rsplit('/').next().unwrap_or(prog);
    let rest: Vec<&str> = words.collect();
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
            matches!(sub, "test" | "check" | "clippy" | "nextest" | "doc" | "tree" | "metadata")
                || (sub == "fmt" && has("--check"))
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
        "make" | "just" | "task" => is_test_script(sub),
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

/// Whether the calls of one reply are all verification: reads and searches, shell runs of checks, a write
/// carrying `check`. `kind` gives the kind of a call by its name and arguments (None: unknown tool).
pub(super) fn all_verification(
    calls: &[(String, String, Value)],
    kind: impl Fn(&str, &Value) -> Option<ToolKind>,
) -> bool {
    !calls.is_empty()
        && calls.iter().all(|(_, name, args)| match kind(name, args) {
            Some(ToolKind::Read | ToolKind::Search) => true,
            Some(ToolKind::Exec) if name == "bash" => {
                !args.get("background").and_then(Value::as_bool).unwrap_or(false)
                    && args.get("command").and_then(Value::as_str).is_some_and(is_check_command)
            }
            Some(ToolKind::Write) => args.get("check").and_then(Value::as_str).is_some_and(|c| !c.trim().is_empty()),
            _ => false,
        })
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
            "cargo build",
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
    fn a_step_is_verification_when_every_call_is() {
        let kind = |n: &str, _: &Value| match n {
            "read" => Some(ToolKind::Read),
            "grep" => Some(ToolKind::Search),
            "bash" => Some(ToolKind::Exec),
            "edit" | "write" => Some(ToolKind::Write),
            _ => None,
        };
        let c = |n: &str, a: Value| ("id".to_string(), n.to_string(), a);
        assert!(all_verification(&[c("read", json!({})), c("bash", json!({"command": "cargo test"}))], kind));
        assert!(!all_verification(&[], kind));
        assert!(!all_verification(&[c("bash", json!({"command": "cargo test", "background": true}))], kind));
        assert!(!all_verification(&[c("edit", json!({}))], kind));
        assert!(!all_verification(&[c("edit", json!({"check": "  "}))], kind));
        assert!(all_verification(&[c("edit", json!({"check": "cargo test"}))], kind));
        assert!(!all_verification(&[c("todo", json!({}))], kind));
        assert!(!all_verification(&[c("read", json!({})), c("web", json!({}))], kind));
    }
}
