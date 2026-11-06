//! The same failure again: a call that fails on the same thing (a path, a program) for the same clear
//! cause as an earlier call of the session that also failed, with no success on it in between, gets one
//! line saying so; when its output is the earlier one word for word, that line takes the output's place.
//! Recorded sessions show most such repeats come with the first failure still in the model's view: it does
//! not forget the failure, it does not act on it. The call always runs (the state may have changed); the
//! line is only added when the failure did come back, so it is never stale.

use std::collections::HashMap;
use std::sync::OnceLock;

use nexo_core::message::Part;
use regex::{Regex, RegexBuilder};
use serde_json::Value;

/// A failure kept for one thing.
struct Fail {
    kind: &'static str,
    /// The call it first happened at, and how many times in a row it has.
    first: usize,
    times: u32,
    text: String,
}

#[derive(Default)]
pub struct Repeats {
    calls: usize,
    last: HashMap<String, Fail>,
}

fn kinds() -> &'static [(&'static str, Regex)] {
    static KINDS: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    KINDS.get_or_init(|| {
        [
            ("blocked", r"^(permission denied: |blocked by a hook)|denied by|not allowed|was rejected"),
            ("shell glob", r"no matches found"),
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

/// The clear cause of a failed output (its start and its end), if it says one.
pub fn cause(text: &str) -> Option<&'static str> {
    let n = text.chars().count();
    let ends: String = if n <= 800 {
        text.to_string()
    } else {
        format!("{}\n{}", text.chars().take(400).collect::<String>(), text.chars().skip(n - 400).collect::<String>())
    };
    kinds().iter().find(|(_, r)| r.is_match(&ends)).map(|(k, _)| *k)
}

/// What a call acts on: the path it names; for a command, the path its error names, the program it says is
/// missing, or the command itself.
pub fn target(name: &str, args: &Value, text: &str, kind: &str) -> String {
    if let Some(p) = ["path", "file_path"].iter().find_map(|k| args.get(*k).and_then(Value::as_str)) {
        return p.to_string();
    }
    let Some(cmd) = args.get("command").and_then(Value::as_str) else {
        return format!("{name} {}", one_line(&args.to_string(), 80));
    };
    static PATH: OnceLock<Regex> = OnceLock::new();
    static TOOL: OnceLock<Vec<Regex>> = OnceLock::new();
    if matches!(kind, "missing" | "no permission") {
        let re = PATH.get_or_init(|| Regex::new(r#"(?:^|[\s'"`:(])((?:~|\.{1,2})?/[^\s'"`:,)]+)"#).unwrap());
        if let Some(c) = re.captures(text) {
            return c[1].to_string();
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
        if let Some(c) = res.iter().find_map(|re| re.captures(text)) {
            return format!("the program `{}`", &c[1]);
        }
    }
    if kind == "shell glob" {
        return "the shell's glob".into();
    }
    format!("`{}`", one_line(cmd, 80))
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
    /// The result of call `name(args)`, with the line of a repeated failure when it is one.
    pub fn tag(&mut self, name: &str, args: &Value, part: Part) -> Part {
        self.calls += 1;
        let Part::ToolResult { call_id, name: n, content, is_error } = part else { return part };
        let text: String = content
            .iter()
            .filter_map(|p| if let Part::Text { text } = p { Some(text.as_str()) } else { None })
            .collect();
        let kind = if is_error { cause(&text) } else { None };
        let Some(kind) = kind else {
            // a success clears what was kept for its thing; a failure without a clear cause leaves it
            if !is_error {
                let key = target(name, args, &text, "");
                self.last.remove(&key);
            }
            return Part::ToolResult { call_id, name: n, content, is_error };
        };
        let key = target(name, args, &text, kind);
        let now = self.calls;
        let fail = self.last.entry(key.clone()).or_insert(Fail { kind, first: now, times: 0, text: String::new() });
        if fail.kind != kind {
            *fail = Fail { kind, first: now, times: 0, text: String::new() };
        }
        fail.times += 1;
        if fail.times < 2 {
            fail.text = text;
            return Part::ToolResult { call_id, name: n, content, is_error };
        }
        let line = format!(
            "[the same failure on {key} ({kind}) for the {} time, first {} calls ago: as it stands it fails again, so change the approach rather than retry it]",
            ordinal(fail.times),
            now - fail.first
        );
        let body =
            if fail.text == text { "(the same output as the last time it failed)".to_string() } else { text.clone() };
        fail.text = text;
        Part::ToolResult { call_id, name: n, content: vec![Part::text(format!("{body}\n\n{line}"))], is_error }
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

    fn fail(text: &str) -> Part {
        Part::ToolResult {
            call_id: "c".into(),
            name: Some("bash".into()),
            content: vec![Part::text(text)],
            is_error: true,
        }
    }

    fn ok(text: &str) -> Part {
        Part::ToolResult {
            call_id: "c".into(),
            name: Some("bash".into()),
            content: vec![Part::text(text)],
            is_error: false,
        }
    }

    fn text(p: &Part) -> String {
        let Part::ToolResult { content, .. } = p else { panic!() };
        content.iter().map(|c| if let Part::Text { text } = c { text.clone() } else { String::new() }).collect()
    }

    #[test]
    fn a_failure_that_comes_back_on_the_same_thing_says_so_once_it_does() {
        let mut r = Repeats::default();
        let w = json!({"path": "/etc/hosts", "content": "x"});
        let first = r.tag("write", &w, fail("cannot write /etc/hosts: Permission denied (os error 13)"));
        assert!(!text(&first).contains("same failure"));
        r.tag("read", &json!({"path": "a.rs"}), ok("fn main() {}"));
        let again = r.tag("write", &w, fail("cannot write /etc/hosts: Permission denied (os error 13)"));
        let t = text(&again);
        assert!(t.starts_with("(the same output as the last time it failed)"), "{t}");
        assert!(t.contains("on /etc/hosts (no permission) for the second time, first 2 calls ago"), "{t}");
    }

    #[test]
    fn a_success_in_between_or_a_new_cause_starts_over() {
        let mut r = Repeats::default();
        let rd = json!({"path": "src/x.rs"});
        r.tag("read", &rd, fail("No such file or directory: src/x.rs"));
        r.tag("read", &rd, ok("now it is there"));
        assert!(!text(&r.tag("read", &rd, fail("No such file or directory: src/x.rs"))).contains("same failure"));
        assert!(!text(&r.tag("read", &rd, fail("Permission denied"))).contains("same failure"));
    }

    #[test]
    fn a_failure_with_no_clear_cause_is_left_alone() {
        let mut r = Repeats::default();
        let t = json!({"command": "cargo test"});
        r.tag("bash", &t, fail("test a ... FAILED\n\n[exit code 101]"));
        assert!(!text(&r.tag("bash", &t, fail("test a ... FAILED\n\n[exit code 101]"))).contains("same failure"));
    }

    #[test]
    fn commands_are_known_by_the_path_or_program_their_error_names() {
        let mut r = Repeats::default();
        r.tag(
            "bash",
            &json!({"command": "grep -rn x --include=*.rs ."}),
            fail("zsh: no matches found: --include=*.rs"),
        );
        let t = text(&r.tag(
            "bash",
            &json!({"command": "ugrep y --include=*.md"}),
            fail("zsh: no matches found: --include=*.md"),
        ));
        assert!(
            t.contains("on the shell's glob (shell glob)") && t.contains("zsh: no matches found: --include=*.md"),
            "{t}"
        );
        r.tag("bash", &json!({"command": "fd x"}), fail("zsh: command not found: fd\n\n[exit code 127]"));
        let t = text(&r.tag(
            "bash",
            &json!({"command": "cd src && fd y"}),
            fail("zsh: command not found: fd\n\n[exit code 127]"),
        ));
        assert!(t.contains("on the program `fd` (missing program)"), "{t}");
    }
}
