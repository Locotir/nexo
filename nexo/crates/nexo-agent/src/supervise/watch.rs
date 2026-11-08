//! The mechanical watch over this program's own sessions: plain counting over the tool calls of the request in
//! progress, no model call. It sees the same call again and again, a failing command re-run unchanged, edits
//! that undo each other and a long run of reads, and at the end of a turn open todo items, a failed last test
//! run and a "done" said right after one. What it sees goes to the model as a note on the NEXT request only
//! ([`hidden_block`]), or, when the session stopped with work that looks unfinished, as the evidence of one
//! check-in ([`check_in_ask`], [`parse_answer`]). Pure: histories in, findings out.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use nexo_core::message::{Message, Part};
use serde_json::Value;

use crate::checkpoint::todo_of;

/// The same call this many times, with nothing changed between them.
pub const REPEAT_CALLS: usize = 3;
/// The same failing command run this many times, with nothing changed between the runs.
pub const REPEAT_FAILS: usize = 2;
/// Reads in a row, with no change, no run and no todo update.
pub const READ_RUN: usize = 12;

/// One thing the watch saw. `key` tells it from the same thing seen before, so it is said once.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub key: String,
    pub text: String,
}

fn finding(key: String, text: String) -> Finding {
    Finding { key, text }
}

struct Step {
    id: String,
    name: String,
    args: Value,
    /// Whether the call failed and what it printed, once its result is in.
    result: Option<(bool, String)>,
}

impl Step {
    fn command(&self) -> Option<&str> {
        (self.name == "bash").then(|| self.args.get("command").and_then(Value::as_str)).flatten()
    }

    fn failed(&self) -> bool {
        self.result.as_ref().is_some_and(|(e, _)| *e)
    }

    fn key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.name.hash(&mut h);
        self.args.to_string().hash(&mut h);
        h.finish()
    }

    fn short(&self) -> String {
        let what = self
            .command()
            .or_else(|| self.args.get("path").and_then(Value::as_str))
            .or_else(|| self.args.get("pattern").and_then(Value::as_str))
            .unwrap_or("");
        let what: String = what.lines().next().unwrap_or("").chars().take(80).collect();
        format!("{} {what}", self.name).trim().to_string()
    }
}

fn steps(history: &[Message]) -> Vec<Step> {
    let mut out: Vec<Step> = Vec::new();
    for m in history {
        for p in &m.content {
            match p {
                Part::ToolCall { id, name, args } => {
                    out.push(Step { id: id.clone(), name: name.clone(), args: args.clone(), result: None });
                }
                Part::ToolResult { call_id, content, is_error, .. } => {
                    let text: String = content
                        .iter()
                        .filter_map(|c| match c {
                            Part::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if let Some(s) = out.iter_mut().rev().find(|s| s.id == *call_id) {
                        s.result = Some((*is_error, text));
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Programs and subcommands that only look or build: running them changes no source file.
fn looks_only(segment: &str) -> bool {
    let mut w = segment.split_whitespace().skip_while(|w| w.contains('='));
    let (Some(first), second) = (w.next(), w.next()) else { return true };
    let first = first.rsplit('/').next().unwrap_or(first);
    match first {
        "ls" | "cat" | "head" | "tail" | "pwd" | "echo" | "grep" | "rg" | "find" | "which" | "wc" | "file" | "stat"
        | "tree" | "env" | "date" | "whoami" | "diff" | "sleep" | "true" | "test" | "printf" | "cd" | "ps" | "df"
        | "du" | "less" | "sort" | "uniq" | "cut" | "jq" | "tsc" | "pytest" | "jest" | "vitest" | "rspec" | "ctest"
        | "phpunit" => true,
        "git" => matches!(
            second,
            Some("status" | "diff" | "log" | "show" | "branch" | "rev-parse" | "ls-files" | "blame" | "remote")
        ),
        "cargo" | "go" | "npm" | "yarn" | "pnpm" | "dotnet" | "mvn" | "gradle" | "make" | "deno" | "bun" => {
            matches!(
                second,
                Some("test" | "build" | "check" | "clippy" | "vet" | "lint" | "tsc" | "typecheck" | "doc" | "bench")
            ) || (matches!(second, Some("run")) && w.next().is_some_and(|t| matches!(t, "test" | "build" | "lint")))
        }
        "python" | "python3" => second == Some("-m") && w.next().is_some_and(|m| m == "pytest"),
        _ => false,
    }
}

/// Whether a shell command can have changed something: anything not made only of looks and builds.
fn bash_changes(cmd: &str) -> bool {
    if cmd.contains('>') || cmd.contains("sed -i") || cmd.contains(" tee ") {
        return true;
    }
    !cmd.split(['\n', ';', '|', '&']).filter(|s| !s.trim().is_empty()).all(looks_only)
}

/// Whether a call can have changed something between two runs of the same call.
fn changes(s: &Step) -> bool {
    match s.name.as_str() {
        "edit" | "write" | "notebook_edit" | "apply_patch" => true,
        "bash" => bash_changes(s.command().unwrap_or("")),
        _ => false,
    }
}

/// A test run: the shell command names a test runner.
fn is_test(cmd: &str) -> bool {
    const RUNNERS: [&str; 16] = [
        "cargo test",
        "cargo nextest",
        "pytest",
        "npm test",
        "npm run test",
        "yarn test",
        "pnpm test",
        "go test",
        "mvn test",
        "gradle test",
        "dotnet test",
        "make test",
        "ctest",
        "jest",
        "vitest",
        "rspec",
    ];
    cmd.split(['\n', ';', '&', '|']).any(|seg| RUNNERS.iter().any(|r| seg.trim_start().starts_with(r)))
}

fn is_reading(s: &Step) -> bool {
    match s.name.as_str() {
        "read" | "grep" | "glob" | "ls" | "recall" => true,
        "bash" => {
            let c = s.command().unwrap_or("");
            !bash_changes(c) && !is_test(c)
        }
        _ => false,
    }
}

/// What the request in progress shows: `history` is the slice from the start of the request of the person.
pub fn scan_rounds(history: &[Message]) -> Vec<Finding> {
    let steps = steps(history);
    let mut out = Vec::new();
    // the same call again, nothing changed between the runs
    for (i, s) in steps.iter().enumerate() {
        if s.result.is_none() || s.name == "todo" {
            continue;
        }
        let key = s.key();
        let mut times = 1;
        for p in steps[..i].iter().rev() {
            if p.key() == key {
                times += 1;
            } else if changes(p) {
                break;
            }
        }
        let cmd = s.command().is_some();
        let fails = cmd && s.failed();
        if fails && times >= REPEAT_FAILS {
            out.push(finding(
                format!("rerun:{key}:{}", times / REPEAT_FAILS),
                format!(
                    "`{}` failed and was run again {} time(s) with nothing changed in between; the same input gives \
                     the same failure",
                    s.short(),
                    times - 1
                ),
            ));
        } else if !fails && times >= REPEAT_CALLS {
            out.push(finding(
                format!("repeat:{key}:{}", times / REPEAT_CALLS),
                format!(
                    "`{}` was called {times} times with the same arguments and nothing changed in between",
                    s.short()
                ),
            ));
        }
    }
    // an edit that puts back what an earlier edit took out
    let mut edits: Vec<(String, String, String)> = Vec::new();
    for s in steps.iter().filter(|s| s.name == "edit" && !s.failed() && s.result.is_some()) {
        let path = s.args.get("path").and_then(Value::as_str).unwrap_or("").to_string();
        let mut pairs: Vec<(String, String)> = Vec::new();
        if let (Some(o), Some(n)) =
            (s.args.get("old_string").and_then(Value::as_str), s.args.get("new_string").and_then(Value::as_str))
        {
            pairs.push((o.to_string(), n.to_string()));
        }
        for e in s.args.get("edits").and_then(Value::as_array).into_iter().flatten() {
            if let (Some(o), Some(n)) =
                (e.get("old_string").and_then(Value::as_str), e.get("new_string").and_then(Value::as_str))
            {
                pairs.push((o.to_string(), n.to_string()));
            }
        }
        for (o, n) in pairs {
            if o != n && edits.iter().any(|(p, po, pn)| *p == path && *po == n && *pn == o) {
                let mut h = DefaultHasher::new();
                (&path, &o, &n).hash(&mut h);
                out.push(finding(
                    format!("undo:{:x}", h.finish()),
                    format!("an edit of {path} puts back the text an earlier edit of it replaced; the edits undo each other"),
                ));
            }
            edits.push((path.clone(), o, n));
        }
    }
    // a long run of reads
    let run = steps.iter().rev().take_while(|s| is_reading(s)).count();
    if run >= READ_RUN {
        out.push(finding(
            format!("reads:{}", run / READ_RUN),
            format!("{run} reads in a row, with no change made, no command that builds or tests and no todo update"),
        ));
    }
    out
}

/// Whether the reply ends handing the word to the person (a question, or `GOAL WAIT`).
fn hands_over(reply: &str) -> bool {
    let last = reply.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("");
    let bare = last.trim_end_matches(['*', '_', '`', ')', '"', '\'', ' ']);
    bare.ends_with('?') || bare.ends_with('？') || last.to_ascii_uppercase().contains("GOAL WAIT")
}

/// Whether the reply says the work is finished.
fn claims_done(reply: &str) -> bool {
    let tail: String = reply.chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect();
    let t = tail.to_ascii_lowercase();
    [
        "all done",
        "is done",
        "are done",
        "i'm done",
        "i am done",
        "all set",
        "finished",
        "complete",
        "tests pass",
        "all tests pass",
        "works now",
        "is fixed",
        "are fixed",
        "implemented",
        "ready to",
    ]
    .iter()
    .any(|w| t.contains(w))
}

/// What the end of a turn shows: `history` is the slice from the start of the request, `reply` the last words.
pub fn scan_end(history: &[Message], reply: &str) -> Vec<Finding> {
    if hands_over(reply) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Some(t) = todo_of(history) {
        let open = t.pending + t.doing;
        if open > 0 {
            let doing = t.doing_text.as_deref().map(|d| format!(" (in progress: {d})")).unwrap_or_default();
            out.push(finding(
                format!("todo:{open}"),
                format!("the todo list still has {open} open item(s){doing} and the turn ended"),
            ));
        }
    }
    let steps = steps(history);
    let last_test = steps.iter().rposition(|s| s.command().is_some_and(is_test) && s.result.is_some());
    if let Some(i) = last_test.filter(|i| steps[*i].failed()) {
        let s = &steps[i];
        let wrote = steps[i + 1..].iter().any(changes);
        let tail = if wrote { ", then files were changed and the tests were not run again" } else { "" };
        if claims_done(reply) {
            out.push(finding(
                format!("claim:{}", s.key()),
                format!("the last message says the work is done, but the last test run (`{}`) failed{tail}", s.short()),
            ));
        } else {
            out.push(finding(
                format!("test:{}", s.key()),
                format!("the turn ended and the last test run (`{}`) failed{tail}", s.short()),
            ));
        }
    }
    out
}

/// The note that rides on the next request only: not a message, never saved in the conversation.
pub fn hidden_block(lines: &[String]) -> String {
    let list: Vec<String> = lines.iter().map(|l| format!("- {l}")).collect();
    format!(
        "<system-reminder>\nAn automatic check on this session noticed, by counting (it may be wrong):\n{}\nIf it \
         is true, change what you do next. Do not mention this note to the person unless it matters to them.\n\
         </system-reminder>",
        list.join("\n")
    )
}

/// The question of a check-in, which goes to the session's model as one more message after the conversation.
pub fn check_in_ask(evidence: &[String]) -> String {
    let list: Vec<String> = evidence.iter().map(|l| format!("- {l}")).collect();
    format!(
        "Automatic check-in from the supervisor, not from the person. The session stopped. Everything OK? I noticed:\n\
         {}\nIs the task finished? Answer on the first line with the single word DONE if it is, or with CONTINUE \
         followed by what to do next, in one or two sentences. Do not call tools.",
        list.join("\n")
    )
}

/// What a check-in's reply decided.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    Done,
    Continue(String),
}

/// Reads the reply of a check-in: a first word of CONTINUE resumes the session with what follows it; DONE, or
/// anything that is not clear, leaves it as it is.
pub fn parse_answer(reply: &str) -> Answer {
    let t = reply.trim_start_matches(|c: char| !c.is_alphanumeric()).trim_end();
    let head: String = t.chars().take(8).collect::<String>().to_ascii_uppercase();
    if !head.starts_with("CONTINUE") {
        return Answer::Done;
    }
    let rest = t[8..].trim_start_matches(|c: char| c == ':' || c == '-' || c == '.' || c == '*' || c.is_whitespace());
    let rest: String = rest.trim().chars().take(1500).collect();
    Answer::Continue(rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A history of rounds: each is (tool, args, failed, output).
    fn hist(rounds: &[(&str, Value, bool, &str)]) -> Vec<Message> {
        let mut out = vec![Message::user("do it")];
        for (i, (name, args, err, text)) in rounds.iter().enumerate() {
            let id = format!("c{i}");
            out.push(Message::new(
                nexo_core::message::Role::Assistant,
                vec![Part::tool_call(&id, *name, args.clone())],
            ));
            out.push(Message::tool_results(vec![Part::ToolResult {
                call_id: id,
                name: Some(name.to_string()),
                content: vec![Part::text(*text)],
                is_error: *err,
            }]));
        }
        out
    }

    fn bash(c: &str, err: bool) -> (&'static str, Value, bool, &'static str) {
        ("bash", json!({"command": c}), err, if err { "boom\n[exit code 1]" } else { "ok" })
    }

    fn edit(p: &str, o: &str, n: &str) -> (&'static str, Value, bool, &'static str) {
        ("edit", json!({"path": p, "old_string": o, "new_string": n}), false, "edited")
    }

    fn read(p: &str) -> (&'static str, Value, bool, &'static str) {
        ("read", json!({"path": p}), false, "text")
    }

    fn kinds(f: &[Finding]) -> Vec<&str> {
        f.iter().map(|f| f.key.split(':').next().unwrap()).collect()
    }

    #[test]
    fn the_same_call_three_times_is_seen_and_two_times_is_not() {
        let two = hist(&[read("a.rs"), read("a.rs")]);
        assert!(scan_rounds(&two).is_empty());
        let three = hist(&[read("a.rs"), read("a.rs"), read("a.rs")]);
        assert_eq!(kinds(&scan_rounds(&three)), ["repeat"]);
        let other = hist(&[read("a.rs"), read("b.rs"), read("a.rs"), read("c.rs"), read("a.rs")]);
        assert_eq!(kinds(&scan_rounds(&other)), ["repeat"], "other calls in between do not hide it");
    }

    #[test]
    fn a_call_run_again_after_an_edit_is_a_retry_not_a_repeat() {
        let h = hist(&[read("a.rs"), read("a.rs"), edit("a.rs", "x", "y"), read("a.rs")]);
        assert!(scan_rounds(&h).is_empty());
        let h = hist(&[read("a.rs"), read("a.rs"), bash("sed -i s/x/y/ a.rs", false), read("a.rs")]);
        assert!(scan_rounds(&h).is_empty(), "a shell command that writes counts as a change");
    }

    #[test]
    fn a_failing_command_run_again_unchanged_is_seen_but_not_after_a_change() {
        let h = hist(&[bash("cargo test", true), bash("cargo test", true)]);
        assert_eq!(kinds(&scan_rounds(&h)), ["rerun"]);
        let h = hist(&[bash("cargo test", true), edit("a.rs", "x", "y"), bash("cargo test", true)]);
        assert!(scan_rounds(&h).is_empty(), "the edit in between is the change");
        let h = hist(&[bash("cargo test", true), bash("cargo build", false), bash("cargo test", true)]);
        assert_eq!(kinds(&scan_rounds(&h)), ["rerun"], "a build changes no source");
        let h = hist(&[bash("make deploy", true), bash("rm -rf out", false), bash("make deploy", true)]);
        assert!(scan_rounds(&h).is_empty(), "a command that removes files is a change");
        let h = hist(&[bash("cargo test", false), bash("cargo test", false)]);
        assert!(scan_rounds(&h).is_empty(), "passing twice is no failure");
    }

    #[test]
    fn edits_that_undo_each_other_are_seen() {
        let h = hist(&[edit("a.rs", "foo", "bar"), edit("a.rs", "bar", "foo")]);
        assert_eq!(kinds(&scan_rounds(&h)), ["undo"]);
        let h = hist(&[edit("a.rs", "foo", "bar"), edit("b.rs", "bar", "foo")]);
        assert!(scan_rounds(&h).is_empty(), "another file");
        let h = hist(&[edit("a.rs", "foo", "bar"), edit("a.rs", "bar", "baz")]);
        assert!(scan_rounds(&h).is_empty(), "building on the first edit is no undo");
        let many = json!({"path": "a.rs", "edits": [{"old_string": "one", "new_string": "two"}]});
        let back = json!({"path": "a.rs", "edits": [{"old_string": "two", "new_string": "one"}]});
        let h = hist(&[("edit", many, false, "ok"), ("edit", back, false, "ok")]);
        assert_eq!(kinds(&scan_rounds(&h)), ["undo"]);
    }

    #[test]
    fn a_long_run_of_reads_is_seen_until_something_else_happens() {
        let reads: Vec<_> = (0..READ_RUN).map(|i| read(&format!("f{i}.rs"))).collect();
        assert_eq!(kinds(&scan_rounds(&hist(&reads))), ["reads"]);
        let short: Vec<_> = (0..READ_RUN - 1).map(|i| read(&format!("f{i}.rs"))).collect();
        assert!(scan_rounds(&hist(&short)).is_empty());
        let mut broken = reads.clone();
        broken.push(bash("cargo test", false));
        assert!(scan_rounds(&hist(&broken)).is_empty(), "a test run is progress");
        let mut edited = reads;
        edited.push(edit("a.rs", "x", "y"));
        assert!(scan_rounds(&hist(&edited)).is_empty());
    }

    fn todo(items: Value) -> (&'static str, Value, bool, &'static str) {
        ("todo", json!({"items": items}), false, "ok")
    }

    #[test]
    fn the_end_of_a_turn_with_open_todo_items_is_seen() {
        let open = todo(json!([{"text": "a", "status": "done"}, {"text": "b", "status": "in_progress"}]));
        let f = scan_end(&hist(&[open]), "I stopped here.");
        assert_eq!(kinds(&f), ["todo"]);
        assert!(f[0].text.contains("(in progress: b)"), "{}", f[0].text);
        let closed = todo(json!([{"text": "a", "status": "done"}]));
        assert!(scan_end(&hist(&[closed]), "Done.").is_empty());
        assert!(scan_end(&hist(&[read("a.rs")]), "Done.").is_empty(), "no list, no signal");
    }

    #[test]
    fn a_failed_last_test_is_seen_and_a_later_pass_clears_it() {
        let h = hist(&[bash("cargo test -q", true)]);
        assert_eq!(kinds(&scan_end(&h, "Here is where I am.")), ["test"]);
        let h = hist(&[bash("cargo test -q", true), edit("a.rs", "x", "y"), bash("cargo test -q", false)]);
        assert!(scan_end(&h, "Here is where I am.").is_empty());
        let h = hist(&[bash("cargo test -q", true), edit("a.rs", "x", "y")]);
        let f = scan_end(&h, "Here is where I am.");
        assert!(f[0].text.contains("not run again"), "{}", f[0].text);
        let h = hist(&[bash("cargo build", true)]);
        assert!(scan_end(&h, "Failed.").is_empty(), "a build is not a test run");
    }

    #[test]
    fn done_said_right_after_a_failed_test_is_one_finding_of_its_own() {
        let h = hist(&[bash("pytest -x", true)]);
        let f = scan_end(&h, "All done, the feature is implemented.");
        assert_eq!(kinds(&f), ["claim"]);
        assert!(f[0].text.contains("pytest -x"));
        let h = hist(&[bash("pytest -x", false)]);
        assert!(scan_end(&h, "All done, the feature is implemented.").is_empty());
    }

    #[test]
    fn a_turn_that_hands_the_word_to_the_person_is_not_unfinished() {
        let h = hist(&[bash("cargo test", true)]);
        assert!(scan_end(&h, "The test fails on the parser. Shall I change the grammar?").is_empty());
        assert!(scan_end(&h, "Needs your decision.\nGOAL WAIT").is_empty());
    }

    #[test]
    fn answers_are_read_by_their_first_word() {
        assert_eq!(parse_answer("DONE"), Answer::Done);
        assert_eq!(parse_answer("  done.\nAll tests pass."), Answer::Done);
        assert_eq!(
            parse_answer("CONTINUE: run the tests again\nthen fix parser.rs"),
            Answer::Continue("run the tests again\nthen fix parser.rs".into())
        );
        assert_eq!(parse_answer("**Continue** - finish the todo"), Answer::Continue("finish the todo".into()));
        assert_eq!(parse_answer("I think maybe"), Answer::Done, "unclear changes nothing");
        assert_eq!(parse_answer(""), Answer::Done);
    }
}
