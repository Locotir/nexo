//! What the ledger keeps of the messages that left the view: the lines the agent will need again, against
//! the noise of a long session.

use nexo_curator::Curator;
use serde_json::{json, Value};

const BASE: &str = "directives=1;nudge=8;close=0;ahead=0;dedup=0;live=0;lead=0;aim=0;follow=0;brief=0;digest=0;hold=0;asked=0;sup=0;named=0;margin=1;mission=0";

fn user(text: &str) -> Value {
    json!({"role": "user", "content": text})
}

fn call(k: usize, name: &str, input: Value, out: &str) -> [Value; 2] {
    let id = format!("call_{k}");
    [
        json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": out}]}),
    ]
}

fn bash(k: usize, command: &str, out: &str) -> [Value; 2] {
    call(k, "bash", json!({ "command": command }), out)
}

/// A made-up word, different for every seed.
fn word(seed: u64) -> String {
    const SYL: [&str; 16] =
        ["ka", "lo", "mi", "ren", "tu", "vas", "zel", "or", "pim", "qu", "dra", "sen", "ul", "fo", "bri", "hax"];
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut w = String::new();
    for _ in 0..3 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        w += SYL[(x >> 60) as usize];
    }
    w
}

/// A step that searches a part of the tree and gets a long output, no two lines alike.
fn search(k: usize) -> [Value; 2] {
    let out: String = (0..40)
        .map(|l| {
            let s = (k * 1000 + l * 7) as u64;
            format!(
                "src/{}/{}.rs:{}: let {} = {}(&{}, {});\n",
                word(s),
                word(s + 1),
                l * 13 + k,
                word(s + 2),
                word(s + 3),
                word(s + 4),
                word(s + 5)
            )
        })
        .collect();
    bash(k, &format!("grep -rn {} src", word(k as u64)), &out)
}

struct Session {
    cur: Curator,
    msgs: Vec<Value>,
    last: Value,
}

impl Session {
    fn new(first: &str) -> Session {
        Session { cur: Curator::new(&format!("{BASE};")).unwrap(), msgs: vec![user(first)], last: Value::Null }
    }

    fn push(&mut self, step: [Value; 2]) {
        self.msgs.extend(step);
        self.view();
    }

    fn say(&mut self, text: &str) {
        self.msgs.push(user(text));
        self.view();
    }

    fn view(&mut self) {
        self.last = self.cur.answer(&json!({"op": "view", "system_tokens": 0, "messages": self.msgs}), 0.0);
        assert_eq!(self.last["ok"], true, "{}", self.last);
    }

    /// `n` noisy searches, from step `from`.
    fn noise(&mut self, from: usize, n: usize) {
        for k in from..from + n {
            self.push(search(k));
        }
    }

    fn stat(&self, k: &str) -> i64 {
        self.last["stats"][k].as_i64().unwrap_or(0)
    }

    /// The ledger: its lines, without the header.
    fn ledger(&self) -> String {
        let first = &self.last["view"][0]["content"];
        let text = match first {
            Value::Array(bs) => bs.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"),
            Value::String(s) => s.clone(),
            _ => String::new(),
        };
        match text.find("Facts from the messages that moved out") {
            Some(at) => text[at..].lines().skip(1).take_while(|l| l.starts_with('m')).collect::<Vec<_>>().join("\n"),
            None => String::new(),
        }
    }
}

const TEST_RUN: &str = "running 12 tests\n\
test money::tests::adds ... ok\n\
test money::tests::rounds_half_up ... FAILED\n\
\n\
failures:\n\
\n\
---- money::tests::rounds_half_up stdout ----\n\
thread 'money::tests::rounds_half_up' panicked at src/money.rs:41:9:\n\
assertion `left == right` failed\n\
  left: 1204\n\
 right: 1200\n\
\n\
test result: FAILED. 11 passed; 1 failed; 0 ignored\n";

#[test]
fn a_failing_test_and_where_it_failed_outlive_a_noisy_session() {
    let mut s = Session::new("Port the billing module.");
    s.push(bash(0, "cargo test", TEST_RUN));
    s.noise(1, 90);
    assert!(s.stat("left_messages") > 60, "{}", s.last["stats"]);
    let l = s.ledger();
    assert!(l.contains("rounds_half_up"), "{l}");
    assert!(l.contains("src/money.rs:41"), "{l}");
}

#[test]
fn a_rename_the_agent_made_outlives_a_noisy_session() {
    let mut s = Session::new("Port the billing module.");
    s.push(call(
        0,
        "Edit",
        json!({"file_path": "src/fees.rs", "old_string": "pub fn calc_fee(n: i64) -> i64 {", "new_string": "pub fn fee_in_cents(n: i64) -> i64 {"}),
        "The file src/fees.rs has been updated.",
    ));
    s.noise(1, 90);
    assert!(s.stat("left_messages") > 60, "{}", s.last["stats"]);
    let l = s.ledger();
    assert!(l.contains("fee_in_cents"), "{l}");
}

#[test]
fn a_cut_marker_is_not_a_fact() {
    let mut s = Session::new("Port the billing module.");
    s.push(bash(0, "cat build.log", "step one finished\n... [12395 characters truncated] ...\nstep nine finished\n"));
    s.noise(1, 90);
    let l = s.ledger();
    assert!(!l.contains("truncated"), "{l}");
}

#[test]
fn the_line_that_holds_a_name_the_person_asked_about_is_kept() {
    let mut s = Session::new("Port the billing module.");
    s.push(bash(0, "grep -rn parse_header src", "src/net/codec.rs:88: fn parse_header(buf: &[u8]) -> Header {\n"));
    s.noise(1, 30);
    s.say("Make parse_header reject frames shorter than 8 bytes.");
    s.noise(31, 70);
    assert!(s.stat("left_messages") > 60, "{}", s.last["stats"]);
    let l = s.ledger();
    assert!(l.contains("src/net/codec.rs:88"), "{l}");
}

/// A table of examples as a spec file lists them: price, units, total.
fn price_table() -> String {
    let rows: String = (0..24)
        .map(|i| format!("| {}.{:02} | {} | {}.{:02} |\n", 1 + i % 5, 17 + i * 3, 100 + i * 37, 11 + i * 29, 5 + i * 7))
        .collect();
    format!("## Examples\n\n| price | units | total |\n|---|---|---|\n{rows}")
}

#[test]
fn the_rows_of_a_table_of_values_outlive_a_noisy_session() {
    let table = price_table();
    let mut s = Session::new("Write tests from the examples in docs/spec/05-bulk.md.");
    s.push(bash(0, "cat docs/spec/05-bulk.md", &table));
    s.noise(1, 90);
    assert!(s.stat("left_messages") > 60, "{}", s.last["stats"]);
    let l = s.ledger();
    // the last row, far past the first few
    assert!(l.contains(table.lines().last().unwrap()), "{l}");
    let kept = table.lines().filter(|r| l.contains(*r)).count();
    assert!(kept >= 20, "{kept} rows kept: {l}");
}

#[test]
fn rows_of_small_counters_are_still_noise() {
    let rows: String = (0..24).map(|i| format!("| {} | {} | {} |\n", i, i * 2, i % 7)).collect();
    let mut s = Session::new("Port the billing module.");
    s.push(bash(0, "cat counts.md", &format!("## Counts\n{rows}")));
    s.noise(1, 90);
    let l = s.ledger();
    assert!(!l.contains("| 5 | 10 | 5 |"), "{l}");
}
