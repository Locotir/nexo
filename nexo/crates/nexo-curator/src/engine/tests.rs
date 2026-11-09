use super::files::{basename, file_key, files_in, head_and_tail, name_parts, same_file, scratch};
use super::fixes::{named_in, TEST_COMMAND, TEST_FAILED};
use super::steps::topics;
use super::view::add_blocks;
use super::*;
use crate::hindsight::UKind;
use crate::json::est_str;
use crate::ledger::{ERROR_MARKS, ON_DISK, POOL};
use crate::mechanical::MARK;
use crate::msg::{blocks, get_str, text_block, text_of};
use serde_json::json;

/// The settings of the mechanism tests: each test turns on what it checks over this base, so a change of
/// the default spec does not move them.
const BASE: &str = "directives=1;nudge=8;close=0;ahead=0;dedup=0;live=0;lead=0;aim=0;follow=0;brief=0;digest=0;hold=0;asked=0;sup=0;named=0;margin=1;mission=0";

fn params(spec: &str) -> Result<Params, String> {
    crate::params(&format!("{BASE};{spec}"))
}

/// A step of the agent: a shell command and its output.
fn step(k: usize, command: &str, out: &str) -> [Value; 2] {
    let id = format!("call_{k}");
    [
        json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "bash", "input": {"command": command}}]}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": out}]}),
    ]
}

/// A made-up word, different for every seed.
fn word(seed: usize) -> String {
    const SYL: [&str; 16] =
        ["ka", "lo", "mi", "ren", "tu", "vas", "zel", "or", "pim", "qu", "dra", "sen", "ul", "fo", "bri", "hax"];
    let mut x = (seed as u64).wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut w = String::new();
    for _ in 0..3 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        w += SYL[(x >> 60) as usize];
    }
    w
}

/// Lines `a..=b` of a listing, no two of the same shape (a squeeze folds lines that differ only in
/// their numbers).
fn listing(a: usize, b: usize) -> String {
    (a..=b)
        .map(|l| format!("{:>5}:  {} {}, {}   ; slot {l}", 0x400 + 4 * l, word(l), word(l + 100), word(l + 200)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// An engine with `spec` that took in a request and reads of `ranges` of the listing.
fn reads(spec: &str, ranges: &[(usize, usize)]) -> Engine {
    let mut e = Engine::new(params(&format!("fresh=0;{spec}")).unwrap());
    let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
    for (k, &(a, b)) in ranges.iter().enumerate() {
        msgs.extend(step(k + 1, &format!("sed -n '{a},{b}p' listing.txt"), &listing(a, b)));
    }
    e.sync(&msgs);
    e
}

/// The text of the tool result in message `mi` of the view.
fn output(view: &[Value], mi: usize) -> String {
    text_of(blocks(view[mi].get("content"))[0].get("content")).into_owned()
}

/// A report with a line per invoice, every line its own words.
fn report() -> String {
    (0..30)
        .map(|k| format!("invoice {} total {} client {}", word(k), 1000 + 37 * k, word(k + 500)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A first request answered from a file read, a second answered from a short command, and `third` if
/// some: what the person asks next.
fn sessions(reply: &str, third: Option<&str>) -> Vec<Value> {
    let mut msgs =
        vec![json!({"role": "user", "content": "Which client has the largest invoice total in report.txt?"})];
    msgs.extend(step(1, "cat report.txt", &report()));
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": reply}]}));
    msgs.push(json!({"role": "user", "content": "How many lines has notes.md?"}));
    if let Some(q) = third {
        msgs.extend(step(2, "wc -l notes.md", "12 notes.md"));
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "12"}]}));
        msgs.push(json!({"role": "user", "content": q}));
    }
    msgs
}

fn sent(view: &[Value]) -> String {
    serde_json::to_string(view).unwrap()
}

/// A step of the agent with a tool of its own: the call and its output (`error`: the tool failed).
fn tool(k: usize, name: &str, input: Value, out: &str, error: bool) -> [Value; 2] {
    let id = format!("call_{k}");
    [
        json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": out, "is_error": error}]}),
    ]
}

/// The text of a file of `n` lines, different for every `seed`.
fn source(seed: usize, n: usize) -> String {
    (0..n).map(|k| format!("let v{k} = {} + {};", word(seed + k), word(seed + k + 500))).collect::<Vec<_>>().join("\n")
}

/// The read result in message `mi`.
fn result_in(e: &Engine, mi: usize) -> usize {
    *e.fz.by_msg(mi).iter().find(|&&i| e.tr.units[i].kind == UKind::Result).unwrap()
}

mod anchor;
mod cap;
mod decide;
mod dups;
mod files;
mod fixes;
mod idle;
mod ingest;
mod named;
mod recall;
mod stale;
mod steps;
mod view;
mod window;
