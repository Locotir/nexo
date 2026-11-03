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
}

/// The counters of a transcript's text.
pub fn from_text(text: &str) -> Counters {
    let mut calls: Vec<Call> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    // The first appearance of a result is the raw one: later views may shorten it.
    let mut results: HashMap<String, String> = HashMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        for m in v["messages"].as_array().into_iter().flatten() {
            for b in m["content"].as_array().into_iter().flatten() {
                if b["type"] == "tool_result" {
                    if let Some(id) = b["tool_use_id"].as_str() {
                        results.entry(id.to_string()).or_insert_with(|| result_text(&b["content"]));
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
                calls.push(Call { id, name: b["name"].as_str().unwrap_or_default().into(), input: b["input"].clone() });
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
                if let Some(p) = call.input["path"].as_str() {
                    let start = call.input["offset"].as_u64().unwrap_or(1).max(1);
                    let end = start + call.input["limit"].as_u64().unwrap_or(READ_DEFAULT).max(1) - 1;
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
    c
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
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
}
