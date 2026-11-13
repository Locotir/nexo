//! The same failure again: a call that fails on the same thing (a path, a program) for the same clear
//! cause as a call of an earlier round that also failed, with no success on it in between, gets one line
//! saying so after its output. Recorded sessions show most such repeats come with the first failure still in
//! the model's view: it does not forget the failure, it does not act on it. The call always runs (the state
//! may have changed) and its output is kept whole; the line is only added when the failure did come back,
//! so it is never stale. Build and test output is left alone: a fix-and-rebuild loop is the right loop.

use std::collections::HashMap;
use std::sync::OnceLock;

use nexo_core::message::Part;
use regex::{Regex, RegexBuilder};
use serde_json::Value;

/// A failure kept for one thing.
struct Fail {
    kind: &'static str,
    /// The call it first happened at, the round it last happened in, and how many times in a row it has.
    first: usize,
    round: usize,
    times: u32,
    text: String,
}

#[derive(Default)]
pub struct Repeats {
    calls: usize,
    rounds: usize,
    last: HashMap<String, Fail>,
}

fn kinds() -> &'static [(&'static str, Regex)] {
    static KINDS: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    KINDS.get_or_init(|| {
        [
            ("blocked", r"^(permission denied: |blocked by a hook)|denied by|not allowed|was rejected"),
            (
                "missing program",
                r"command not found|is not recognized as|: not found\b|no such command|unknown command",
            ),
            ("no permission", r"permission denied|read-only file system|operation not permitted|access is denied"),
            ("missing", r"no such file|does not exist|cannot find|can't open|not a directory|file not found"),
            ("timeout", r"timed out"),
        ]
        .into_iter()
        .map(|(k, p)| (k, RegexBuilder::new(p).case_insensitive(true).multi_line(true).build().unwrap()))
        .collect()
    })
}

/// Output of a compiler or a test run: its "cannot find" is about the code, which changes between runs.
fn build_output(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?m)error\[E\d+\]|error TS\d+|: error: |test result:|FAILED|Traceback \(most recent|panicked at")
            .unwrap()
    })
    .is_match(text)
}

/// Where a cause is looked for: the whole text, or its start and its end when it is long.
fn ends(text: &str) -> String {
    let n = text.chars().count();
    if n <= 800 {
        text.to_string()
    } else {
        format!("{}\n{}", text.chars().take(400).collect::<String>(), text.chars().skip(n - 400).collect::<String>())
    }
}

/// The clear cause of a failed output, if it says one.
pub fn cause(text: &str) -> Option<&'static str> {
    let ends = ends(text);
    kinds().iter().find(|(_, r)| r.is_match(&ends)).map(|(k, _)| *k)
}

/// What a call acts on: the path it names; for a command, the path on the line of its error, the program it
/// says is missing, or the command itself (the bool: known by the command only).
pub fn target(name: &str, args: &Value, text: &str, kind: &str) -> (String, bool) {
    if let Some(p) = ["path", "file_path"].iter().find_map(|k| args.get(*k).and_then(Value::as_str)) {
        return (p.to_string(), false);
    }
    let Some(cmd) = args.get("command").and_then(Value::as_str) else {
        return (format!("{name} {}", one_line(&args.to_string(), 80)), false);
    };
    static PATH: OnceLock<Regex> = OnceLock::new();
    static TOOL: OnceLock<Vec<Regex>> = OnceLock::new();
    if matches!(kind, "missing" | "no permission") {
        let re = PATH.get_or_init(|| {
            Regex::new(r#"(?:^|[\s'"`:(])((?:~|\.{1,2})?/[^\s'"`:,)]+|[\w.-]+/[^\s'"`:,)]+)"#).unwrap()
        });
        let said = kinds().iter().find(|(k, _)| *k == kind).map(|(_, r)| r);
        // the path on the line that says the cause, not one from a line of progress before it
        let line = said.and_then(|r| ends(text).lines().find(|l| r.is_match(l)).map(str::to_string));
        // and only a path the command named: `find /` warns about `/root` without trying to reach it
        if let Some(c) = line.as_deref().and_then(|l| re.captures(l)).filter(|c| cmd.contains(&c[1])) {
            return (c[1].to_string(), false);
        }
    }
    if kind == "missing program" {
        // "zsh: command not found: fd" names the shell first: the program after the colon wins
        let res = TOOL.get_or_init(|| {
            [
                r"command not found: (\S+)",
                r"(\S+): command not found",
                r"(\S+): not found",
                r"'(\S+)' is not recognized",
            ]
            .into_iter()
            .map(|p| Regex::new(p).unwrap())
            .collect()
        });
        if let Some(c) = res.iter().find_map(|re| re.captures(text)).filter(|c| cmd.contains(&c[1])) {
            return (format!("the program `{}`", &c[1]), false);
        }
    }
    (format!("`{}`", one_line(cmd, 80)), true)
}

/// Whether two outputs of one command say the same thing. A timeout prints the pid, port or seconds it
/// reached, which change on every run: there the numbers do not count.
fn same(kind: &str, a: &str, b: &str) -> bool {
    if kind != "timeout" {
        return a == b;
    }
    let plain = |s: &str| s.chars().filter(|c| !c.is_ascii_digit()).collect::<String>();
    plain(a) == plain(b)
}

fn one_line(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        s
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

impl Repeats {
    /// The results of one round of calls (`(id, name, args)`), each with the line of a repeated failure when
    /// it is one. Calls of the same round are not repeats of each other: none of them saw another's output.
    pub fn round(&mut self, calls: &[(String, String, Value)], results: Vec<Part>) -> Vec<Part> {
        self.rounds += 1;
        results
            .into_iter()
            .map(|p| {
                let call = match &p {
                    Part::ToolResult { call_id, .. } => calls.iter().find(|(id, _, _)| id == call_id),
                    _ => None,
                };
                match call {
                    Some((_, name, args)) => self.tag(name, args, p),
                    None => p,
                }
            })
            .collect()
    }

    fn tag(&mut self, name: &str, args: &Value, part: Part) -> Part {
        self.calls += 1;
        let Part::ToolResult { call_id, name: n, content, is_error } = part else { return part };
        let text: String = content
            .iter()
            .filter_map(|p| if let Part::Text { text } = p { Some(text.as_str()) } else { None })
            .collect();
        // a command that ended with a non-zero code is not marked as an error, but its failure is tracked all the same
        let shown_error = is_error;
        let is_error = is_error || nexo_core::shell::exit_code_in(&text).is_some_and(|c| c != 0);
        let cmd = args.get("command").and_then(Value::as_str);
        let kind = if cmd.is_some() && build_output(&text) {
            None
        } else if is_error {
            cause(&text)
        } else if cmd.is_some() && text.chars().count() <= 800 {
            // `cat x 2>&1 | head` or `cat x; echo $?` ends at 0 though cat failed: a short output whose error
            // line names a path or a program still says the command failed on it
            cause(&text).filter(|k| !target(name, args, &text, k).1)
        } else {
            None
        };
        let Some(kind) = kind else {
            // a success clears what was kept for its thing; a failure without a clear cause leaves it
            if !is_error {
                self.cleared(name, args, &text, cmd);
            }
            return Part::ToolResult { call_id, name: n, content, is_error: shown_error };
        };
        let (key, by_command) = target(name, args, &text, kind);
        let (now, round) = (self.calls, self.rounds);
        let fresh = Fail { kind, first: now, round, times: 0, text: String::new() };
        let fail = self.last.entry(key.clone()).or_insert(fresh);
        // a command known only by its text is the same failure only when it says the same thing
        let other = fail.kind != kind || (by_command && fail.times > 0 && !same(kind, &fail.text, &text));
        if other {
            *fail = Fail { kind, first: now, round, times: 0, text: String::new() };
        }
        if fail.times > 0 && fail.round == round {
            // the same thing twice in one round: one failure, not a retry
            return Part::ToolResult { call_id, name: n, content, is_error: shown_error };
        }
        fail.times += 1;
        fail.round = round;
        fail.text = text.clone();
        if fail.times < 2 {
            return Part::ToolResult { call_id, name: n, content, is_error: shown_error };
        }
        let line = format!(
            "[the same failure on {key} ({kind}) for the {} time, first {} calls ago: as it stands it fails again, so change the approach rather than retry it]",
            ordinal(fail.times),
            now - fail.first
        );
        Part::ToolResult {
            call_id,
            name: n,
            content: vec![Part::text(format!("{text}\n\n{line}"))],
            is_error: shown_error,
        }
    }

    /// A success forgets the failures of what it touched: its path, the paths its command names, the
    /// programs its command runs, and the command itself.
    fn cleared(&mut self, name: &str, args: &Value, text: &str, cmd: Option<&str>) {
        let Some(cmd) = cmd else {
            self.last.remove(&target(name, args, text, "").0);
            return;
        };
        let runs: Vec<&str> = cmd
            .split([';', '|', '&', '\n', '(', ')'])
            .filter_map(|s| s.split_whitespace().find(|w| !w.contains('=')))
            .collect();
        let itself = format!("`{}`", one_line(cmd, 80));
        self.last.retain(|k, _| {
            if let Some(p) = k.strip_prefix("the program `").and_then(|s| s.strip_suffix('`')) {
                !runs.contains(&p)
            } else if k.starts_with('`') {
                *k != itself
            } else {
                !cmd.contains(k.as_str())
            }
        });
    }
}

fn ordinal(n: u32) -> String {
    match n {
        2 => "second".into(),
        3 => "third".into(),
        n => format!("{n}th"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn res(text: &str, is_error: bool) -> Part {
        Part::ToolResult { call_id: "c".into(), name: Some("bash".into()), content: vec![Part::text(text)], is_error }
    }

    fn text(p: &Part) -> String {
        let Part::ToolResult { content, .. } = p else { panic!() };
        content.iter().map(|c| if let Part::Text { text } = c { text.clone() } else { String::new() }).collect()
    }

    /// One call alone in its round.
    fn one(r: &mut Repeats, name: &str, args: Value, out: &str, is_error: bool) -> String {
        let calls = [("c".to_string(), name.to_string(), args)];
        text(&r.round(&calls, vec![res(out, is_error)])[0])
    }

    fn tagged(t: &str) -> bool {
        t.contains("[the same failure")
    }

    #[test]
    fn a_path_the_command_did_not_name_is_not_its_failure() {
        let mut r = Repeats::default();
        let find = |n: &str| json!({"command": format!("find / -name {n} 2>&1 | head")});
        one(&mut r, "bash", find("foo"), "find: '/root': Permission denied", false);
        let t = one(&mut r, "bash", find("bar"), "find: '/root': Permission denied", false);
        assert!(!tagged(&t), "{t}");
    }

    #[test]
    fn a_timeout_that_prints_a_new_pid_is_the_same_timeout() {
        let mut r = Repeats::default();
        let cmd = json!({"command": "npm run dev"});
        one(&mut r, "bash", cmd.clone(), "listening on :5173 (pid 4121)\ncommand timed out after 120s", true);
        let t = one(&mut r, "bash", cmd, "listening on :5174 (pid 4388)\ncommand timed out after 120s", true);
        assert!(tagged(&t), "{t}");
    }

    #[test]
    fn a_relative_path_links_a_shell_failure_with_a_read() {
        let mut r = Repeats::default();
        one(&mut r, "read", json!({"path": "src/x.rs"}), "src/x.rs: No such file or directory", true);
        let t =
            one(&mut r, "bash", json!({"command": "cat src/x.rs"}), "cat: src/x.rs: No such file or directory", true);
        assert!(tagged(&t), "{t}");
    }

    #[test]
    fn a_failure_that_comes_back_on_the_same_thing_says_so_and_keeps_its_output() {
        let mut r = Repeats::default();
        let w = json!({"path": "/etc/hosts", "content": "x"});
        let out = "cannot write /etc/hosts: Permission denied (os error 13)";
        assert!(!tagged(&one(&mut r, "write", w.clone(), out, true)));
        one(&mut r, "read", json!({"path": "a.rs"}), "fn main() {}", false);
        let t = one(&mut r, "write", w, out, true);
        assert!(t.starts_with(out), "the output stays whole: {t}");
        assert!(t.contains("on /etc/hosts (no permission) for the second time, first 2 calls ago"), "{t}");
    }

    #[test]
    fn a_success_in_between_or_a_new_cause_starts_over() {
        let mut r = Repeats::default();
        let rd = json!({"path": "src/x.rs"});
        one(&mut r, "read", rd.clone(), "No such file or directory: src/x.rs", true);
        one(&mut r, "read", rd.clone(), "now it is there", false);
        assert!(!tagged(&one(&mut r, "read", rd.clone(), "No such file or directory: src/x.rs", true)));
        assert!(!tagged(&one(&mut r, "read", rd, "Permission denied", true)));
    }

    #[test]
    fn a_failure_with_no_clear_cause_is_left_alone() {
        let mut r = Repeats::default();
        let t = json!({"command": "cargo test"});
        one(&mut r, "bash", t.clone(), "test a ... FAILED\n\n[exit code 101]", true);
        assert!(!tagged(&one(&mut r, "bash", t, "test a ... FAILED\n\n[exit code 101]", true)));
    }

    #[test]
    fn a_fix_and_rebuild_loop_is_not_a_repeat() {
        let mut r = Repeats::default();
        let b = json!({"command": "cargo build"});
        let err = |id: &str| {
            format!("   Compiling app v0.1.0 (/work/app)\nerror[E0425]: cannot find value `{id}` in this scope\n --> src/main.rs:2:5\n\n[exit code 101]")
        };
        one(&mut r, "bash", b.clone(), &err("foo"), true);
        assert!(!tagged(&one(&mut r, "bash", b.clone(), &err("bar"), true)));
        assert!(!tagged(&one(&mut r, "bash", b, &err("bar"), true)), "nor the same error from a build");
    }

    #[test]
    fn commands_are_known_by_the_path_on_the_error_line_or_the_program_it_names() {
        let mut r = Repeats::default();
        let out = "listing /home/x first\ncat: /root/notes.txt: Permission denied\n\n[exit code 1]";
        one(&mut r, "bash", json!({"command": "ls /home/x; cat /root/notes.txt"}), out, true);
        let t = one(&mut r, "bash", json!({"command": "cat /root/notes.txt"}), out, true);
        assert!(t.contains("on /root/notes.txt (no permission)"), "{t}");
        one(&mut r, "bash", json!({"command": "fd x"}), "zsh: command not found: fd\n\n[exit code 127]", true);
        let t =
            one(&mut r, "bash", json!({"command": "cd src && fd y"}), "sh: 1: fd: not found\n\n[exit code 127]", true);
        assert!(t.contains("on the program `fd` (missing program)"), "{t}");
    }

    #[test]
    fn a_command_known_only_by_its_text_repeats_only_when_it_says_the_same() {
        let mut r = Repeats::default();
        let c = json!({"command": "ls $DIR"});
        one(&mut r, "bash", c.clone(), "ls: cannot access 'one': No such file or directory", true);
        assert!(!tagged(&one(&mut r, "bash", c.clone(), "ls: cannot access 'two': No such file or directory", true)));
        assert!(tagged(&one(&mut r, "bash", c, "ls: cannot access 'two': No such file or directory", true)));
    }

    #[test]
    fn two_calls_in_one_round_are_not_a_retry() {
        let mut r = Repeats::default();
        let calls: Vec<_> =
            ["a", "b"].iter().map(|id| (id.to_string(), "read".to_string(), json!({"path": "/no/such"}))).collect();
        let out = |id: &str| Part::ToolResult {
            call_id: id.into(),
            name: Some("read".into()),
            content: vec![Part::text("No such file or directory")],
            is_error: true,
        };
        let got = r.round(&calls, vec![out("a"), out("b")]);
        assert!(got.iter().all(|p| !tagged(&text(p))));
        let got = r.round(&calls[..1], vec![out("a")]);
        assert!(text(&got[0]).contains("for the second time"), "{}", text(&got[0]));
    }

    #[test]
    fn a_failure_a_pipe_or_an_echo_ends_at_zero_is_still_one() {
        let mut r = Repeats::default();
        let c = json!({"command": "cat /root/secret.txt 2>&1 | head -1; echo \"exit=$?\""});
        let out = "cat: /root/secret.txt: Permission denied\nexit=0\n";
        one(
            &mut r,
            "bash",
            json!({"command": "head -1 /root/secret.txt"}),
            "head: cannot open '/root/secret.txt' for reading: Permission denied",
            true,
        );
        assert!(one(&mut r, "bash", c.clone(), out, false)
            .contains("on /root/secret.txt (no permission) for the second time"));
        assert!(one(&mut r, "bash", c, out, false).contains("for the third time"));
        let ls = json!({"command": "ls /etc"});
        one(&mut r, "bash", ls.clone(), "ls: cannot access '/etc/nope': No such file or directory", true);
        let long = format!("{}\nls: cannot access '/etc/nope': No such file or directory", "hosts ".repeat(200));
        assert!(!tagged(&one(&mut r, "bash", ls, &long, false)), "a long output that worked is a success");
    }

    #[test]
    fn a_command_that_works_on_the_path_or_runs_the_program_clears_it() {
        let mut r = Repeats::default();
        let cat = json!({"command": "cat /tmp/o"});
        let missing = "cat: /tmp/o: No such file or directory\n\n[exit code 1]";
        one(&mut r, "bash", cat.clone(), missing, true);
        one(&mut r, "bash", json!({"command": "make > /tmp/o && cat /tmp/o"}), "built", false);
        assert!(!tagged(&one(&mut r, "bash", cat, missing, true)));
        one(&mut r, "bash", json!({"command": "fd x"}), "sh: 1: fd: not found", true);
        one(&mut r, "bash", json!({"command": "PATH=~/bin:$PATH fd --version"}), "fd 10.2.0", false);
        assert!(!tagged(&one(&mut r, "bash", json!({"command": "fd y"}), "sh: 1: fd: not found", true)));
    }
}
