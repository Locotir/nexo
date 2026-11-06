//! What a long session keeps across the curator's rewrites: the newest text of each file the agent wrote
//! (by a write tool or a here-document), the short outputs it measured, the values it stated, and an anchor
//! that says plainly what does not come back by itself.

use nexo_curator::Curator;
use serde_json::{json, Value};

/// The settings these tests start from: the anchor, the nudges and the size cap as they ran before the pure
/// default (cycle 50), so a change of the default spec does not move what they check.
const BASE: &str = "directives=1;nudge=8;close=0;ahead=0;dedup=0;live=0;lead=0;aim=0;follow=0;brief=0;digest=0;hold=0;asked=0;sup=0;named=0;margin=1;mission=0";

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

fn bash(k: usize, text: &str, command: &str, out: &str) -> [Value; 2] {
    call(k, text, "bash", json!({ "command": command }), out)
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

/// A step that searches a different part of the tree and gets a long output, no two lines alike.
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
    bash(k, "", &format!("grep -rn {} src", word(k as u64)), &out)
}

/// A step that reads a disassembly through a read tool, which numbers its lines `  N│ `.
fn disassembly(k: usize) -> [Value; 2] {
    let out: String = (0..60)
        .map(|l| {
            let a = 0x1000 + 16 * (k * 60 + l);
            format!("{:>6}│     {a:x}:\t8b 45 fc 89 c7 \tmov    -0x{:x}(%rbp),%eax\n", l + 1, 4 * l + 4)
        })
        .collect();
    call(k, "", "read", json!({"path": "dis.txt", "offset": 60 * k}), &out)
}

struct Session {
    cur: Curator,
    msgs: Vec<Value>,
    last: Value,
}

impl Session {
    fn new() -> Session {
        Session::with("")
    }

    /// A session whose curator runs with `spec` merged over the defaults.
    fn with(spec: &str) -> Session {
        Session {
            cur: Curator::new(&format!("{BASE};{spec}")).unwrap(),
            msgs: vec![user("Port the billing module.")],
            last: Value::Null,
        }
    }

    fn push(&mut self, step: [Value; 2]) {
        self.msgs.extend(step);
        self.last = self.cur.answer(&json!({"op": "view", "system_tokens": 0, "messages": self.msgs}), 0.0);
        assert_eq!(self.last["ok"], true, "{}", self.last);
    }

    fn stat(&self, k: &str) -> i64 {
        self.last["stats"][k].as_i64().unwrap_or(0)
    }

    /// The text the first message carries after the request (the anchor).
    fn anchor(&self) -> String {
        match &self.last["view"][0]["content"] {
            Value::Array(bs) => bs.iter().skip(1).filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"),
            _ => String::new(),
        }
    }

    /// The result the view holds for call ``id``, if the view holds it.
    fn result(&self, id: &str) -> Option<String> {
        for m in self.last["view"].as_array().unwrap() {
            if let Value::Array(bs) = &m["content"] {
                for b in bs {
                    if b["type"] == "tool_result" && b["tool_use_id"] == id {
                        return Some(b["content"].as_str().unwrap_or("").to_string());
                    }
                }
            }
        }
        None
    }
}

const RATES_V1: &str = "pub const RATE_CENTS: i64 = 4521;\npub fn fee(n: i64) -> i64 {\n    n * RATE_CENTS / 100\n}\n";
const RATES_V2: &str =
    "pub const RATE_CENTS: i64 = 4377;\npub fn fee(n: i64) -> i64 {\n    (n * RATE_CENTS + 50) / 100\n}\n";

fn heredoc(path: &str, body: &str) -> String {
    format!("cat > {path} <<'EOF'\n{body}EOF\ncargo build -q")
}

#[test]
fn the_anchor_does_not_promise_that_passages_come_back() {
    let mut s = Session::new();
    for k in 0..80 {
        s.push(search(k));
    }
    assert!(s.stat("left_messages") > 0, "{}", s.last["stats"]);
    let a = s.anchor();
    assert!(!a.contains("recalled") && !a.contains("Nothing is lost"), "{a}");
    assert!(a.contains("read the file or run the command again"), "{a}");
}

#[test]
fn a_file_written_by_a_here_document_stays_in_the_anchor_as_last_written() {
    let mut s = Session::new();
    s.push(bash(0, "", &heredoc("src/rates.rs", RATES_V1), ""));
    for k in 1..30 {
        s.push(search(k));
    }
    s.push(bash(30, "", &heredoc("src/rates.rs", RATES_V2), ""));
    for k in 31..90 {
        s.push(search(k));
    }
    assert!(s.stat("left_messages") > 62, "{}", s.last["stats"]);
    let a = s.anchor();
    assert!(a.contains("--- src/rates.rs (written in m00062) ---"), "{a}");
    assert!(a.contains("pub const RATE_CENTS: i64 = 4377;") && a.contains("(n * RATE_CENTS + 50) / 100"), "{a}");
    assert!(!a.contains("4521"), "the first version is not the file any more:\n{a}");
}

/// A verdict of about 70 tokens: worth more than its stub to a curator that counts only size.
const VERDICT: &str = "checked 300 saves against the reference build\n\
mismatches 2:\n  s17.sav   gold 1180 want 1184 (floor where round was expected)\n\
  s245.sav  gold 77 want 76 (bonus applied twice on level up)\n\
all other fields match; elapsed 4.1 s\n";

#[test]
fn a_short_output_is_never_left_out_in_place() {
    // a threshold this high leaves out every old chunk it may: only the size rule can keep the short one
    // `still=0`: what the agent has not written about may go on a guess too, so only size keeps the short one
    let mut s = Session::with("threshold=0.99;still=0");
    s.push(search(0));
    s.push(bash(1, "", "./check.sh 300", VERDICT));
    let mut seen = 0;
    for k in 2..80 {
        s.push(search(k));
        if let Some(r) = s.result("call_1") {
            assert!(r.starts_with(VERDICT), "step {k}: {r}");
            seen += 1;
        }
    }
    assert!(s.stat("dropped_chunks") > 0 && seen > 5, "{} {seen}", s.last["stats"]);
}

#[test]
fn a_disassembly_read_through_a_numbering_viewer_stays_out_of_the_ledger() {
    let mut s = Session::new();
    for k in 0..60 {
        s.push(if k % 3 == 0 { disassembly(k) } else { search(k) });
    }
    assert!(s.stat("ledger_lines") > 0, "{}", s.last["stats"]);
    let a = s.anchor();
    assert!(!a.contains('│') && !a.contains("%rbp"), "{a}");
}

#[test]
fn what_the_agent_stated_in_its_text_reaches_the_ledger_whole() {
    let mut s = Session::new();
    s.push(search(0));
    let said = "Measured: retry_backoff 3 -> 41 ms, 5 -> 97 ms, 8 -> 260 ms, 13 -> 1310 ms";
    s.push(bash(1, said, "./bench.sh retries", "done"));
    for k in 2..80 {
        s.push(search(k));
    }
    assert!(s.stat("left_messages") > 4, "{}", s.last["stats"]);
    let a = s.anchor();
    assert!(a.contains(said), "{a}");
}

/// A long session of every kind of step: the view stays small, and what the agent needs is still there at
/// the end.
#[test]
fn a_long_mixed_session_keeps_what_the_agent_needs() {
    let mut s = Session::new();
    let said = "Measured: retry_backoff 3 -> 41 ms, 5 -> 97 ms, 8 -> 260 ms, 13 -> 1310 ms";
    let mut biggest = 0;
    for k in 0..160 {
        let step = match k {
            3 => bash(k, "", &heredoc("src/rates.rs", RATES_V1), ""),
            5 => bash(k, said, "./bench.sh retries", "done"),
            70 => bash(k, "", &heredoc("src/rates.rs", RATES_V2), ""),
            _ if k % 4 == 0 => disassembly(k),
            _ if k % 7 == 0 => bash(k, "", "./check.sh 300", &format!("checked 300, mismatches {}", k % 5)),
            _ => search(k),
        };
        s.push(step);
        let tokens = s.last["view_tokens"].as_i64().or_else(|| s.last["tokens"].as_i64()).unwrap_or(0);
        biggest = biggest.max(tokens);
        // a short check result in view is whole
        for j in (7..=k).step_by(7).filter(|j| j % 4 != 0 && *j != 70) {
            if let Some(r) = s.result(&format!("call_{j}")) {
                assert!(r.starts_with(&format!("checked 300, mismatches {}", j % 5)), "step {k}: {r}");
            }
        }
    }
    // the view is rewritten as it grows, by the cap or by a guess
    assert!(s.stat("rewrites") + s.stat("cap_rewrites") > 2 && s.stat("left_messages") > 150, "{}", s.last["stats"]);
    let a = s.anchor();
    assert!(!a.contains("recalled"), "{a}");
    assert!(a.contains("pub const RATE_CENTS: i64 = 4377;") && !a.contains("4521"), "{a}");
    assert!(a.contains(said), "{a}");
    assert!(!a.contains('│') && !a.contains("%rbp"), "{a}");
}
