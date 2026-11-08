//! Counters read from a run's transcript (no model call): how often the agent used `recall`,
//! asked again for something it had already read, and how much of what tools returned it had
//! already been sent before. They also rebuild the numbers of runs recorded before they existed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::run::Record;

/// A line shorter than this (trimmed) is too common (`}`, `)`, `end`) to count as a duplicate.
const MIN_LINE: usize = 4;

/// The lines `read` returns when it is given no `limit`.
const READ_DEFAULT: u64 = 2000;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Counters {
    /// Calls of the `recall` tool.
    pub recall_calls: u32,
    /// Reads of a range of a file already read in the run (ranges overlap), plus the exact same
    /// shell command run again.
    pub re_reads: u32,
    /// Lines of tool output already present in an earlier tool output of the run.
    pub dup_lines: u32,
    /// Lines of tool output (the base of the duplicate percentage).
    pub result_lines: u32,
    /// Calls that were certainly not needed: the same thing asked again while nothing that could
    /// change it had happened since. A lower bound.
    pub needless: Vec<Needless>,
}

impl Counters {
    /// Duplicate lines as a percentage of the lines tools returned.
    pub fn dup_percent(&self) -> f64 {
        if self.result_lines == 0 {
            0.0
        } else {
            f64::from(self.dup_lines) * 100.0 / f64::from(self.result_lines)
        }
    }
}

/// The files a `read` call names: `path`, then each of `paths`.
fn read_paths(input: &Value) -> Vec<String> {
    nexo_core::tool::call_paths(input)
}

/// The counters of the transcript at `path` (one JSON request per line); defaults when the file
/// cannot be read.
pub fn from_file(path: &Path) -> Counters {
    std::fs::read_to_string(path).map(|t| from_text(&t)).unwrap_or_default()
}

/// A tool call in the order the model made it.
struct Call {
    id: String,
    name: String,
    input: Value,
    /// The request (1-based line of the transcript) whose reply made it.
    at: u32,
}

/// The counters of a transcript's text.
pub fn from_text(text: &str) -> Counters {
    let mut calls: Vec<Call> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    // The first appearance of a result is the raw one: later views may shorten it.
    let mut results: HashMap<String, String> = HashMap::new();
    let mut failed: HashSet<String> = HashSet::new();
    for (n, line) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        for m in v["messages"].as_array().into_iter().flatten() {
            for b in m["content"].as_array().into_iter().flatten() {
                if b["type"] == "tool_result" {
                    if let Some(id) = b["tool_use_id"].as_str() {
                        results.entry(id.to_string()).or_insert_with(|| result_text(&b["content"]));
                        if b["is_error"] == true {
                            failed.insert(id.to_string());
                        }
                    }
                }
            }
        }
        for b in v["reply"]["content"].as_array().into_iter().flatten() {
            if b["type"] != "tool_use" {
                continue;
            }
            let id = b["id"].as_str().unwrap_or_default().to_string();
            if seen_ids.insert(id.clone()) {
                let name = b["name"].as_str().unwrap_or_default().into();
                calls.push(Call { id, name, input: b["input"].clone(), at: n as u32 + 1 });
            }
        }
    }
    let mut c = Counters::default();
    let mut reads: HashMap<String, Vec<(u64, u64)>> = HashMap::new();
    let mut commands: HashSet<String> = HashSet::new();
    let mut lines: HashSet<String> = HashSet::new();
    for call in &calls {
        match call.name.as_str() {
            "recall" => c.recall_calls += 1,
            "read" => {
                let start = call.input["offset"].as_u64().unwrap_or(1).max(1);
                let end = start + call.input["limit"].as_u64().unwrap_or(READ_DEFAULT).max(1) - 1;
                for p in read_paths(&call.input) {
                    let seen = reads.entry(p.trim_start_matches("./").to_string()).or_default();
                    if seen.iter().any(|&(s, e)| start <= e && s <= end) {
                        c.re_reads += 1;
                    }
                    seen.push((start, end));
                }
            }
            "bash" => {
                if let Some(cmd) = call.input["command"].as_str() {
                    if !commands.insert(cmd.trim().to_string()) {
                        c.re_reads += 1;
                    }
                }
            }
            _ => {}
        }
        let Some(out) = results.get(&call.id) else { continue };
        let mut mine: Vec<&str> = Vec::new();
        for l in out.lines().map(str::trim).filter(|l| l.chars().count() >= MIN_LINE) {
            c.result_lines += 1;
            if lines.contains(l) {
                c.dup_lines += 1;
            }
            mine.push(l);
        }
        lines.extend(mine.into_iter().map(String::from));
    }
    c.needless = needless(&calls, &results, &failed);
    c
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// A call that asked again for what the run already had, unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Needless {
    /// The request (1-based) whose reply made the call.
    pub at: u32,
    /// What it asked for again: `read src/a.rs:1-2000`, `ls src`, `bash: git status`.
    pub what: String,
}

/// Programs that only look: run with no redirection into a file, they change nothing.
const LOOKS: &[&str] = &[
    "ls", "cat", "head", "tail", "wc", "grep", "egrep", "fgrep", "rg", "tree", "pwd", "cd", "echo", "printf", "stat",
    "file", "du", "nl", "cut", "uniq", "diff", "cmp", "basename", "dirname", "realpath", "which", "true",
];

/// Git subcommands that only look.
const GIT_LOOKS: &[&str] = &["status", "diff", "log", "show", "ls-files", "grep", "blame"];

/// Whether shell `cmd` surely changes nothing: every program in it only looks (`find` without
/// actions, `sed` without `-i`, `sort` without `-o`, read-only git), nothing is written through a
/// redirection and nothing runs inside `$(...)` or backquotes. Anything unknown might change.
pub fn only_looks(cmd: &str) -> bool {
    let mut c = cmd.to_string();
    for harmless in ["2>/dev/null", "2> /dev/null", "&>/dev/null", ">/dev/null", "> /dev/null", "2>&1", ">&2"] {
        c = c.replace(harmless, " ");
    }
    if c.contains(['>', '`']) || c.contains("$(") || c.contains("<(") {
        return false;
    }
    c.split(['|', ';', '&', '\n']).map(str::trim).filter(|s| !s.is_empty()).all(|seg| {
        let words: Vec<&str> = seg.split_whitespace().collect();
        let (prog, rest) = (words[0], &words[1..]);
        let has = |flags: &[&str]| rest.iter().any(|w| flags.iter().any(|f| w.starts_with(f)));
        match prog {
            "find" => !has(&["-delete", "-exec", "-ok", "-fprint", "-fls"]),
            "sed" => !has(&["-i", "--in-place"]),
            "sort" => !has(&["-o", "--output"]),
            "git" => rest.iter().find(|w| !w.starts_with('-')).is_some_and(|s| GIT_LOOKS.contains(s)),
            p => LOOKS.contains(&p),
        }
    })
}

/// Whether `[s, e]` lies inside the union of `ranges`.
fn covered(ranges: &[(u64, u64)], s: u64, e: u64) -> bool {
    let mut sorted = ranges.to_vec();
    sorted.sort_unstable();
    let mut reach = s;
    for (a, b) in sorted {
        if a <= reach && b >= reach {
            reach = b + 1;
            if reach > e {
                return true;
            }
        }
    }
    false
}

/// The calls that asked again for something the run already had and that nothing could have
/// changed since: a `read` of lines already read whole (no error, not cut short) with no write or
/// edit to that file and no shell command that might change anything in between; an `ls`, `glob`,
/// `grep` or looking-only shell command identical to an earlier one with no write, edit or
/// possibly-changing command in between. A shell command that might change something (any
/// program not known to only look, `cargo` included) forgets everything seen before it, so this is
/// a lower bound: what it counts was certainly not needed.
fn needless(calls: &[Call], results: &HashMap<String, String>, failed: &HashSet<String>) -> Vec<Needless> {
    let mut reads: HashMap<String, Vec<(u64, u64)>> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for call in calls {
        let whole = !failed.contains(&call.id)
            && results.get(&call.id).is_some_and(|r| !r.contains("characters truncated] ..."));
        let path = || call.input["path"].as_str().map(|p| p.trim().trim_start_matches("./").to_string());
        // a read result that stopped at the size cap did not show every file whole
        let whole_all =
            whole && results.get(&call.id).is_none_or(|r| !r.contains("size cap") && !r.contains("(not read)"));
        match call.name.as_str() {
            "read" => {
                let s = call.input["offset"].as_u64().unwrap_or(1).max(1);
                let e = s + call.input["limit"].as_u64().unwrap_or(READ_DEFAULT).max(1) - 1;
                for p in read_paths(&call.input) {
                    let p = p.trim().trim_start_matches("./").to_string();
                    let had = reads.entry(p.clone()).or_default();
                    if covered(had, s, e) {
                        out.push(Needless { at: call.at, what: format!("read {p}:{s}-{e}") });
                    }
                    if whole_all {
                        had.push((s, e));
                    }
                }
            }
            "write" | "edit" => {
                if let Some(p) = path() {
                    reads.remove(&p);
                }
                seen.clear();
            }
            "ls" | "glob" | "grep" => {
                let key = format!("{} {}", call.name, call.input);
                if seen.contains(&key) {
                    out.push(Needless { at: call.at, what: format!("{} {}", call.name, short(&call.input)) });
                } else if whole {
                    seen.insert(key);
                }
            }
            "bash" => {
                let Some(cmd) = call.input["command"].as_str() else { continue };
                let cmd = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
                if !only_looks(&cmd) {
                    reads.clear();
                    seen.clear();
                    continue;
                }
                let key = format!("bash {cmd}");
                if seen.contains(&key) {
                    out.push(Needless { at: call.at, what: format!("bash: {}", one_line(&cmd)) });
                } else if whole {
                    seen.insert(key);
                }
            }
            _ => {}
        }
    }
    out
}

/// A call's input in a few words: its string values, space separated.
fn short(input: &Value) -> String {
    let words: Vec<&str> = input.as_object().into_iter().flatten().filter_map(|(_, v)| v.as_str()).collect();
    one_line(&words.join(" "))
}

/// `s` on one line, at most 60 characters.
fn one_line(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 60 {
        flat.chars().take(59).collect::<String>() + "…"
    } else {
        flat
    }
}

/// The records of the `results.jsonl` in `inputs` (files or folders), each with its counters
/// recomputed from the transcript next to the file.
pub fn load_with_counters(inputs: &[PathBuf]) -> Result<Vec<Record>, String> {
    let mut all = Vec::new();
    for p in inputs {
        let dir = if p.is_dir() { p.clone() } else { p.parent().map(Path::to_path_buf).unwrap_or_default() };
        for mut r in crate::html::load(std::slice::from_ref(p))? {
            if !r.transcript.is_empty() && dir.join(&r.transcript).is_file() {
                r.counters = from_file(&dir.join(&r.transcript));
            }
            all.push(r);
        }
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn use_(id: &str, name: &str, input: Value) -> Value {
        json!({"type":"tool_use","id":id,"name":name,"input":input})
    }

    fn res(id: &str, text: &str) -> Value {
        json!({"type":"tool_result","tool_use_id":id,"content":[{"type":"text","text":text}]})
    }

    /// One request per step: it carries every earlier result (as the real transcript does) and
    /// the reply with this step's calls.
    fn transcript(steps: &[(Value, Value)]) -> String {
        let mut out = Vec::new();
        let mut seen: Vec<Value> = Vec::new();
        for (calls, results) in steps {
            seen.extend(results.as_array().unwrap().iter().cloned());
            let msgs = json!([{"role":"user","content":seen}]);
            out.push(json!({"messages": msgs, "reply": {"role":"assistant","content":calls}}).to_string());
        }
        out.join("\n")
    }

    #[test]
    fn recall_calls_and_overlapping_rereads_are_counted() {
        let t = transcript(&[
            (
                json!([use_("a", "read", json!({"path":"x.rs"})), use_("b", "recall", json!({"message":"m"}))]),
                json!([]),
            ),
            (
                json!([
                    use_("c", "read", json!({"path":"./x.rs","offset":10,"limit":5})),
                    use_("d", "read", json!({"path":"y.rs"}))
                ]),
                json!([res("a", "one"), res("b", "two")]),
            ),
            (json!([use_("e", "recall", json!({"message":"n"}))]), json!([res("c", "three"), res("d", "four")])),
        ]);
        let c = from_text(&t);
        assert_eq!((c.recall_calls, c.re_reads), (2, 1), "{c:?}");
    }

    #[test]
    fn disjoint_ranges_and_other_files_are_not_rereads() {
        let t = transcript(&[(
            json!([
                use_("a", "read", json!({"path":"x.rs","limit":10})),
                use_("b", "read", json!({"path":"x.rs","offset":11,"limit":10})),
                use_("c", "read", json!({"path":"y.rs","limit":10})),
                use_("d", "read", json!({"path":"x.rs","offset":20,"limit":3}))
            ]),
            json!([]),
        )]);
        assert_eq!(from_text(&t).re_reads, 1, "only the last overlaps (lines 20..22 vs 11..20)");
    }

    #[test]
    fn the_same_shell_command_again_is_a_reread() {
        let t = transcript(&[(
            json!([
                use_("a", "bash", json!({"command":"cargo test"})),
                use_("b", "bash", json!({"command":"cargo test "})),
                use_("c", "bash", json!({"command":"ls"}))
            ]),
            json!([]),
        )]);
        assert_eq!(from_text(&t).re_reads, 1);
    }

    #[test]
    fn lines_already_returned_by_an_earlier_tool_are_duplicates() {
        let t = transcript(&[
            (json!([use_("a", "read", json!({"path":"x"})), use_("b", "bash", json!({"command":"cat x"}))]), json!([])),
            (
                json!([]),
                json!([res("a", "first line\nsecond line\n}\n\nfirst line"), res("b", "second line\nthird line\n}")]),
            ),
        ]);
        let c = from_text(&t);
        // a: "first line", "second line", "first line" (a repeat inside one output is not a
        // duplicate); the brace and the empty line are too short. b: "second line" is one.
        assert_eq!((c.result_lines, c.dup_lines), (5, 1), "{c:?}");
        assert!((c.dup_percent() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn an_empty_or_broken_transcript_counts_nothing() {
        assert_eq!(from_text(""), Counters::default());
        assert_eq!(from_text("not json\n{}"), Counters::default());
        assert_eq!(Counters::default().dup_percent(), 0.0);
    }

    /// One call per step, each answered `ok` (or an error for ids starting with `!`) in the next.
    fn one_by_one(calls: &[(&str, &str, Value)]) -> String {
        let mut steps = Vec::new();
        let mut prev: Option<&str> = None;
        for (id, name, input) in calls {
            let answered = prev.map_or(json!([]), |p| {
                let mut r = res(p, "ok");
                if p.starts_with('!') {
                    r["is_error"] = json!(true);
                }
                json!([r])
            });
            steps.push((json!([use_(id, name, input.clone())]), answered));
            prev = Some(id);
        }
        steps.push((json!([]), json!([res(prev.unwrap_or("none"), "ok")])));
        transcript(&steps)
    }

    fn needless_of(calls: &[(&str, &str, Value)]) -> Vec<(u32, String)> {
        from_text(&one_by_one(calls)).needless.into_iter().map(|n| (n.at, n.what)).collect()
    }

    #[test]
    fn a_read_of_lines_already_read_and_unchanged_is_needless() {
        let n = needless_of(&[
            ("a", "read", json!({"path":"src/a.rs"})),
            ("b", "bash", json!({"command":"ls src | wc -l"})),
            ("c", "read", json!({"path":"./src/a.rs","offset":10,"limit":20})),
        ]);
        assert_eq!(n, vec![(3, "read src/a.rs:10-29".to_string())]);
        // two halves read before cover the whole
        let n = needless_of(&[
            ("a", "read", json!({"path":"a","limit":10})),
            ("b", "read", json!({"path":"a","offset":11,"limit":10})),
            ("c", "read", json!({"path":"a","offset":5,"limit":10})),
        ]);
        assert_eq!(n.len(), 1, "{n:?}");
    }

    #[test]
    fn a_change_in_between_or_a_failed_first_read_makes_it_needed() {
        for between in [
            ("b", "edit", json!({"path":"src/a.rs","old_string":"x","new_string":"y"})),
            ("b", "write", json!({"path":"src/a.rs","content":"y"})),
            ("b", "bash", json!({"command":"cargo test"})),
            ("b", "bash", json!({"command":"echo x > src/a.rs"})),
            ("b", "bash", json!({"command":"sed -i s/x/y/ src/a.rs"})),
        ] {
            let n = needless_of(&[
                ("a", "read", json!({"path":"src/a.rs"})),
                between.clone(),
                ("c", "read", json!({"path":"src/a.rs"})),
            ]);
            assert!(n.is_empty(), "{between:?}: {n:?}");
        }
        let n = needless_of(&[("!a", "read", json!({"path":"x"})), ("b", "read", json!({"path":"x"}))]);
        assert!(n.is_empty(), "{n:?}");
        // an edit elsewhere leaves the read unchanged
        let n = needless_of(&[
            ("a", "read", json!({"path":"x"})),
            ("b", "edit", json!({"path":"y","old_string":"1","new_string":"2"})),
            ("c", "read", json!({"path":"x"})),
        ]);
        assert_eq!(n.len(), 1, "{n:?}");
    }

    #[test]
    fn a_listing_again_with_nothing_written_is_needless_and_after_a_write_is_not() {
        let n = needless_of(&[
            ("a", "bash", json!({"command":"ls -la  src"})),
            ("b", "ls", json!({"path":"tests"})),
            ("c", "bash", json!({"command":"ls -la src"})),
            ("d", "ls", json!({"path":"tests"})),
            ("e", "write", json!({"path":"tests/new.rs","content":""})),
            ("f", "ls", json!({"path":"tests"})),
            ("g", "bash", json!({"command":"ls -la src"})),
        ]);
        assert_eq!(n, vec![(3, "bash: ls -la src".to_string()), (4, "ls tests".to_string())]);
    }

    #[test]
    fn only_commands_that_surely_change_nothing_only_look() {
        for c in ["ls -la", "cat a | grep x 2>/dev/null", "git status --short && git diff", "find . -name '*.rs'"] {
            assert!(only_looks(c), "{c}");
        }
        for c in [
            "cargo test",
            "cat a > b",
            "find . -delete",
            "git commit -m x",
            "sed -i s/a/b/ f",
            "echo $(rm x)",
            "ls; make",
        ] {
            assert!(!only_looks(c), "{c}");
        }
    }
}
