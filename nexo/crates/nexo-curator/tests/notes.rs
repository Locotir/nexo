//! What the curator does so that a long session keeps what the agent learnt and wrote: it asks a
//! silent agent for its findings (a `note` call), keeps the notes once their messages leave, and keeps the
//! files the agent wrote in view.

use nexo_curator::Curator;
use serde_json::{json, Value};

/// The settings these tests start from: the anchor, the nudges and the size cap as they ran before the pure
/// default (cycle 50), so a change of the default spec does not move what they check.
const BASE: &str = "directives=1;nudge=8;close=0;ahead=0;dedup=0;live=0;lead=0;aim=0;follow=0;brief=0;digest=0;hold=0;loops=0;asked=0;sup=0;named=0;margin=1;mission=0";

const NUDGE: &str = "[curator] Before the next step, keep what these results settled with the `note` tool: one line beginning with \"Finding:\"";

fn user(text: &str) -> Value {
    json!({"role": "user", "content": text})
}

/// A step of tool work: ``text`` (none for a silent step), a call and its result.
fn call(k: usize, text: &str, name: &str, input: Value, out: &str) -> [Value; 2] {
    let id = format!("call_{k}");
    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    content.push(json!({"type": "tool_use", "id": id, "name": name, "input": input}));
    [
        json!({"role": "assistant", "content": content}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": out}]}),
    ]
}

/// A step that searches a different part of the tree and gets a long output.
fn noise(k: usize, text: &str) -> [Value; 2] {
    let out: String =
        (0..40).map(|l| format!("src/unit_{k}/part_{l}.rs:{l}: value_{k}_{l} = compute_{l}(input_{k})\n")).collect();
    call(k, text, "bash", json!({"command": format!("grep -rn value src/unit_{k}")}), &out)
}

struct Session {
    cur: Curator,
    msgs: Vec<Value>,
    last: Value,
}

impl Session {
    fn new(spec: &str) -> Session {
        Session {
            cur: Curator::new(&format!("{BASE};{spec}")).unwrap(),
            msgs: vec![user("Port the billing module.")],
            last: Value::Null,
        }
    }

    fn push(&mut self, step: [Value; 2]) -> &Value {
        self.msgs.extend(step);
        self.last = self.cur.answer(&json!({"op": "view", "system_tokens": 0, "messages": self.msgs}), 0.0);
        assert_eq!(self.last["ok"], true, "{}", self.last);
        &self.last
    }

    fn nudges(&self) -> i64 {
        self.last["stats"]["nudges"].as_i64().unwrap_or(0)
    }

    /// The text of the last tool result in the view.
    fn last_result(&self) -> String {
        let view = self.last["view"].as_array().unwrap();
        let bs = view.last().unwrap()["content"].as_array().unwrap();
        let b = bs.iter().rev().find(|b| b["type"] == "tool_result").unwrap();
        b["content"].as_str().unwrap().to_string()
    }

    /// The text the first message carries after the request (the anchor).
    fn anchor(&self) -> String {
        match &self.last["view"][0]["content"] {
            Value::Array(bs) => bs.iter().skip(1).filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"),
            _ => String::new(),
        }
    }
}

#[test]
fn a_silent_agent_is_nudged_after_n_steps_and_not_before() {
    let mut s = Session::new("");
    for k in 0..7 {
        s.push(noise(k, ""));
        assert_eq!(s.nudges(), 0, "step {k}");
        assert!(!s.last_result().contains(NUDGE));
    }
    s.push(noise(7, ""));
    assert_eq!(s.nudges(), 1);
    assert!(s.last_result().ends_with("the notes stay."), "{}", s.last_result());
    assert!(s.last_result().contains(NUDGE));
    // once per n steps; the nudged result stays as it was sent
    for k in 8..15 {
        s.push(noise(k, ""));
        assert_eq!(s.nudges(), 1, "step {k}");
    }
    let view = s.last["view"].to_string();
    assert_eq!(view.matches("keep what these results settled").count(), 1, "{view}");
    s.push(noise(15, ""));
    assert_eq!(s.nudges(), 2);
}

#[test]
fn a_note_line_resets_the_count() {
    let mut s = Session::new("");
    for k in 0..5 {
        s.push(noise(k, ""));
    }
    s.push(noise(5, "Finding: the rates are in cents.\nPlan: port the invoices next."));
    for k in 6..13 {
        s.push(noise(k, "Checking."));
        assert_eq!(s.nudges(), 0, "step {k}");
    }
    s.push(noise(13, ""));
    assert_eq!(s.nudges(), 1);
}

#[test]
fn a_note_tool_call_counts_as_a_note_and_is_kept_once_its_message_leaves() {
    let mut s = Session::new("");
    for k in 0..5 {
        s.push(noise(k, ""));
    }
    let note =
        json!({"text": "Finding: the rates are in cents.\nPlan: port the invoices next.\nRule: never touch main."});
    s.push(call(5, "", "note", note, "noted 3 lines"));
    for k in 6..13 {
        s.push(noise(k, ""));
        assert_eq!(s.nudges(), 0, "step {k}");
    }
    s.push(noise(13, ""));
    assert_eq!(s.nudges(), 1);
    for k in 14..300 {
        s.push(noise(k, ""));
    }
    assert!(s.last["stats"]["left_messages"].as_i64().unwrap() > 200, "{}", s.last["stats"]);
    let a = s.anchor();
    assert!(a.contains("Finding: the rates are in cents."), "{a}");
    assert!(a.contains("Plan: port the invoices next."), "{a}");
    assert!(a.contains("never touch main."), "{a}");
}

#[test]
fn only_the_latest_nudge_stays_in_view() {
    let mut s = Session::new("");
    for k in 0..24 {
        s.push(noise(k, ""));
    }
    assert_eq!(s.nudges(), 3);
    let view = s.last["view"].to_string();
    assert_eq!(view.matches("keep what these results settled").count(), 1, "{view}");
    assert!(s.last_result().contains(NUDGE));
}

#[test]
fn nudge_zero_never_nudges() {
    let mut s = Session::new("nudge=0");
    for k in 0..60 {
        s.push(noise(k, ""));
    }
    assert_eq!(s.nudges(), 0);
    assert!(!s.last["view"].to_string().contains("keep what these results settled"));
}

fn write(k: usize, path: &str, content: &str) -> [Value; 2] {
    call(k, "", "write", json!({"path": path, "content": content}), "wrote the file")
}

fn module(tag: &str, lines: usize) -> String {
    (0..lines).map(|l| format!("pub fn {tag}_{l}(x: u32) -> u32 {{ x * {l} + 7 }}\n")).collect()
}

#[test]
fn a_written_file_stays_in_view_hundreds_of_steps_later() {
    let mut s = Session::new("");
    let text = module("rate", 20);
    s.push(write(0, "src/billing/rates.rs", &text));
    for k in 1..300 {
        s.push(noise(k, "Looking."));
    }
    assert!(s.last["stats"]["left_messages"].as_i64().unwrap() > 200, "{}", s.last["stats"]);
    let a = s.anchor();
    assert!(a.contains("Files you wrote, as last written (whole):"), "{a}");
    assert!(a.contains("--- src/billing/rates.rs (written in m00002) ---"), "{a}");
    assert!(a.contains(text.trim_end()), "{a}");
}

#[test]
fn a_later_edit_marks_the_written_file() {
    let mut s = Session::new("");
    s.push(write(0, "src/rates.rs", &module("rate", 20)));
    s.push(call(1, "", "edit", json!({"path": "src/rates.rs", "old_string": "x * 3", "new_string": "x * 4"}), "ok"));
    for k in 2..300 {
        s.push(noise(k, "Looking."));
    }
    let a = s.anchor();
    assert!(
        a.contains("--- src/rates.rs (written in m00002; edited later in m00004: re-read before trusting) ---"),
        "{a}"
    );
}

#[test]
fn a_large_edit_drops_the_written_file() {
    let mut s = Session::new("");
    let old = module("rate", 10);
    s.push(write(0, "src/rates.rs", &old));
    s.push(call(1, "", "edit", json!({"path": "src/rates.rs", "old_string": old, "new_string": "x"}), "ok"));
    for k in 2..200 {
        s.push(noise(k, "Looking."));
    }
    assert!(!s.anchor().contains("--- src/rates.rs"), "{}", s.anchor());
}

#[test]
fn a_later_write_replaces_the_written_file() {
    let mut s = Session::new("");
    s.push(write(0, "src/rates.rs", &module("old", 10)));
    s.push(write(1, "src/rates.rs", &module("new", 10)));
    for k in 2..300 {
        s.push(noise(k, "Looking."));
    }
    let a = s.anchor();
    assert!(a.contains("pub fn new_3(x: u32)"), "{a}");
    assert!(!a.contains("pub fn old_3(x: u32)"), "{a}");
    assert_eq!(a.matches("--- src/rates.rs").count(), 1, "{a}");
}

#[test]
fn the_written_files_keep_the_pin_budget_newest_first() {
    let mut s = Session::new("pin=1200");
    for k in 0..6 {
        // about 380 tokens each
        s.push(write(k, &format!("src/m{k}.rs"), &module(&format!("m{k}"), 32)));
    }
    for k in 6..200 {
        s.push(noise(k, "Looking."));
    }
    let a = s.anchor();
    let block = &a[a.find("Files you wrote").expect(&a)..];
    let block = &block[..block.find("\n[curator]").unwrap_or(block.len())];
    assert!(block.contains("--- src/m5.rs") && block.contains("--- src/m4.rs"), "{block}");
    assert!(!block.contains("--- src/m0.rs") && !block.contains("--- src/m1.rs"), "{block}");
    assert!(block.len() / 4 <= 1200, "{}", block.len());
    assert!(block.find("src/m5.rs").unwrap() < block.find("src/m4.rs").unwrap(), "{block}");
}

#[test]
fn a_long_written_file_keeps_its_head_and_tail() {
    let mut s = Session::new("");
    s.push(write(0, "src/big.rs", &module("big", 300)));
    for k in 1..200 {
        s.push(noise(k, "Looking."));
    }
    let a = s.anchor();
    assert!(a.contains("pub fn big_0(x: u32)") && a.contains("pub fn big_299(x: u32)"), "{a}");
    assert!(a.contains(" lines cut …"), "{a}");
    assert!(!a.contains("pub fn big_150(x: u32)"), "{a}");
}

/// A partial read: ``lines`` lines of a file from ``offset``.
fn part(k: usize, offset: usize, lines: usize) -> [Value; 2] {
    let out: String = (offset..offset + lines).map(|l| format!("    mov eax, [rbx+{l}]\n")).collect();
    call(k, "", "read", json!({"path": "src/big.asm", "offset": offset, "limit": lines}), &out)
}

#[test]
fn a_note_withdraws_the_request() {
    let mut s = Session::new("");
    for k in 0..8 {
        s.push(noise(k, ""));
    }
    assert_eq!(s.nudges(), 1);
    assert!(s.last["view"].to_string().contains("keep what these results settled"));
    // the agent notes: the result that carried the request goes back to how it was sent
    s.push(call(8, "", "note", json!({"text": "Finding: the rates are in cents.\nPlan: port the invoices."}), "noted"));
    let view = s.last["view"].to_string();
    assert!(!view.contains("keep what these results settled"), "{view}");
    assert_eq!(s.nudges(), 1);
}

#[test]
fn a_read_in_parts_is_not_interrupted_until_twice_the_count() {
    let mut s = Session::new("");
    for k in 0..6 {
        s.push(noise(k, ""));
    }
    // the agent reads a file in parts: no request between two parts until since_note reaches 2n
    for k in 6..16 {
        s.push(part(k, 1 + 200 * (k - 6), 200));
        assert_eq!(s.nudges(), i64::from(k >= 15), "step {k}");
    }
    // a step that is not a partial read ends the wait at once
    let mut s = Session::new("");
    for k in 0..7 {
        s.push(noise(k, ""));
    }
    s.push(part(7, 1, 200));
    assert_eq!(s.nudges(), 0);
    s.push(noise(8, ""));
    assert_eq!(s.nudges(), 1);
}

#[test]
fn without_a_note_tool_the_request_asks_for_lines_in_the_reply() {
    let mut s = Session::new("");
    for k in 0..7 {
        s.push(noise(k, ""));
    }
    s.msgs.extend(noise(7, ""));
    s.last =
        s.cur.answer(&json!({"op": "view", "system_tokens": 0, "messages": s.msgs, "tools": ["read", "bash"]}), 0.0);
    assert_eq!(s.nudges(), 1);
    let r = s.last_result();
    assert!(r.contains("start your next reply with one line beginning with \"Finding:\""), "{r}");
    assert!(!r.contains("`note`"), "{r}");
    // the lines in the reply answer it
    let before = s.msgs.clone();
    s.push(noise(8, "Finding: the rates are in cents.\nPlan: port the invoices."));
    assert!(!s.last["view"].to_string().contains("keep what these results settled"));
    // the host is remembered across a reset
    s.cur.reset();
    s.last = s.cur.answer(&json!({"op": "view", "system_tokens": 0, "messages": before}), 0.0);
    assert!(s.last["view"].to_string().contains("start your next reply"));
}

#[test]
fn repeats_fold_in_a_command_output_and_stay_in_a_file() {
    let block = "    mov eax, [rbx+8]\n    add eax, ecx\n    mov [rdx+8], eax\n    inc rdx\n";
    let text: String = format!("section .text\n{}ret\n", block.repeat(8));
    let mut s = Session::new("");
    s.push(call(0, "", "read", json!({"path": "src/loop.asm"}), &text));
    assert_eq!(s.last_result(), text, "what a tool returns of a file stays as it came");
    let mut s = Session::new("");
    s.push(call(0, "", "bash", json!({"command": "objdump -d loop.o"}), &text));
    let r = s.last_result();
    assert!(r.contains("[curator]") && r.contains("repeats"), "{r}");
    assert!(r.len() < text.len());
}
