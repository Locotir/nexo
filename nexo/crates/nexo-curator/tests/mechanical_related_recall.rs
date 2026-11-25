//! Cold, deterministic check: does Curator surface an OLD related fragment (constraint, correction,
//! decision, resolved error) in full, without any recall call, in domains other than remote connection?
//! No model is called. Each history has ~600 messages (~78k tokens against a 20k window); the fragment sits
//! at 25-35% of the history (so >50% back from the end), among tool noise; the final request words the topic
//! differently but shares one meaningful term. Oracle: the fragment's whole sentence is in the view (a bare pointer such as `m00146 s36.txt` does not
//! count; a Facts line or a quoted evidence row that carries the complete sentence does; `Form::Full` = the
//! original message block itself, `Form::Pointer` = a curator-note line carrying the sentence).
use nexo_curator::Curator;
use serde_json::{json, Value};

const HEAD: u64 = 640;
const WINDOW: u64 = 20_000;
const TURNS: usize = 150;
const SEEDS: [usize; 3] = [5, 23, 61];

struct Case {
    domain: &'static str,
    /// role of the message that carries the fragment: "user" or "assistant"
    role: &'static str,
    fragment: &'static str,
    request: &'static str,
}

const CASES: [Case; 5] = [
    Case {
        domain: "build-error",
        role: "assistant",
        fragment: "The E0277 trait bound failure is resolved: enabling the derive feature of the codec crate in Cargo.toml fixed it.",
        request: "Rebuild the workspace and tell me whether the trait bound complaint comes back.",
    },
    Case {
        domain: "constraint",
        role: "user",
        fragment: "Hard rule for this repo: never use the moment library for dates, use the standard Intl APIs only.",
        request: "Add a helper that formats a date for the invoice header.",
    },
    Case {
        domain: "correction",
        role: "user",
        fragment: "No, the metrics port is 8443 not 8080, fix that everywhere.",
        request: "Write the health probe for the gateway and point it at the right port.",
    },
    Case {
        domain: "decision",
        role: "assistant",
        fragment: "Decision: we use PostgreSQL advisory locks instead of Redis for coordinating the job queue workers.",
        request: "How should two workers avoid taking the same queue item?",
    },
    Case {
        domain: "resolved-error-db",
        role: "assistant",
        fragment: "The migration deadlock was resolved by running the index build CONCURRENTLY outside the transaction.",
        request: "Plan the next migration that adds an index to the orders table.",
    },
];

fn msg(role: &str, content: impl Into<Value>) -> Value {
    json!({"role": role, "content": content.into()})
}

fn rng(seed: usize, k: usize) -> usize {
    let mut x = (seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (k as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9)
        ^ 0x1234_5678;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    (x >> 11) as usize
}

const TOPICS: [&str; 8] = [
    "css grid layout",
    "unit test fixtures",
    "log rotation",
    "image resizing",
    "csv export",
    "cache warmup",
    "locale files",
    "retry backoff",
];

/// `fragment`: (turn, role, text) to plant; None = negative control.
fn history(seed: usize, plant: Option<(&Case, usize)>, request: &str) -> Vec<Value> {
    let mut h = vec![];
    for n in 0..TURNS {
        let topic = TOPICS[rng(seed, n) % TOPICS.len()];
        let rows = (0..30 + rng(seed, n + 7) % 15)
            .map(|i| {
                format!(
                    "src/{topic}/part{n}_{i:03}.txt: line {} touched, checksum {:08x}\n",
                    i * 31,
                    rng(seed, n * 100 + i) & 0xffff_ffff
                )
            })
            .collect::<String>();
        let mut ask = format!("Please look at the {topic} work, step {n}.");
        let mut done = format!("Step {n} on {topic} is done.");
        if let Some((c, at)) = plant {
            if n == at {
                if c.role == "user" {
                    ask = format!("{ask} {}", c.fragment);
                } else {
                    done = format!("{done} {}", c.fragment);
                }
            }
        }
        h.push(msg("user", ask));
        h.push(msg(
            "assistant",
            json!([{"type":"tool_use","id":format!("t{n}"),"name":"read_file","input":{"path":format!("s{n}.txt")}}]),
        ));
        h.push(msg("user", json!([{"type":"tool_result","tool_use_id":format!("t{n}"),"content":rows}])));
        h.push(msg("assistant", done));
    }
    h.push(msg("user", request));
    h
}

#[derive(Debug, PartialEq)]
enum Form {
    Full,
    Pointer,
    Absent,
}

/// (form of `needle` in the view, view tokens, count of old (<50% back) turn markers kept in full)
fn run(h: &[Value], needle: &str) -> (Form, i64, usize) {
    let mut c = Curator::new("").unwrap();
    c.window(WINDOW);
    c.tools(&["read_file".into(), "recall".into()]);
    let r = c.view(HEAD, h, 1.0).unwrap();
    assert!(r.tokens + HEAD as i64 <= WINDOW as i64, "over budget: {}", r.tokens);
    let last = r.view.len() - 1;
    let (mut full, mut pointer, mut old_full) = (false, false, 0);
    for (i, m) in r.view.iter().enumerate() {
        let blocks = m["content"].as_array().cloned().unwrap_or_else(|| vec![m["content"].clone()]);
        for b in blocks {
            let s = serde_json::to_string(&b).unwrap();
            let t = b["text"].as_str().unwrap_or("");
            let note = t.starts_with("[curator]") || (i == last && t.starts_with("Historical evidence"));
            if s.contains(needle) {
                if note {
                    pointer = true
                } else {
                    full = true
                }
            }
            if !note {
                // an old turn kept in full: user ask of a step below TURNS/2
                if let Some(rest) = t.split("work, step ").nth(1) {
                    if rest.trim_end_matches('.').parse::<usize>().is_ok_and(|n| n < TURNS / 2) {
                        old_full += 1;
                    }
                }
            }
        }
    }
    (
        if full {
            Form::Full
        } else if pointer {
            Form::Pointer
        } else {
            Form::Absent
        },
        r.tokens,
        old_full,
    )
}

fn domain_pass(c: &Case) -> usize {
    SEEDS
        .iter()
        .filter(|&&seed| {
            let at = 35 + rng(seed, 1) % 15; // 23-33% from the start => 67-77% back
            let h = history(seed, Some((c, at)), c.request);
            let (form, tokens, _) = run(&h, c.fragment);
            println!("{:<18} seed {seed:>2} turn {at}: {form:?} ({tokens} tokens)", c.domain);
            form != Form::Absent
        })
        .count()
}

macro_rules! domain_test {
    ($name:ident, $idx:expr $(, $attr:meta)?) => {
        #[test]
        $(#[$attr])?
        fn $name() {
            let c = &CASES[$idx];
            let ok = domain_pass(c);
            assert_eq!(ok, SEEDS.len(), "{}: old related fragment admitted with its full sentence in {ok}/{} seeds", c.domain, SEEDS.len());
        }
    };
}

domain_test!(related_build_error_resolved, 0);
domain_test!(related_user_constraint, 1);
domain_test!(related_user_correction, 2);
domain_test!(related_design_decision, 3);
domain_test!(related_db_error_resolved, 4);

#[test]
fn negative_control_nothing_related_keeps_recent_data() {
    for seed in SEEDS {
        // an unrelated old fragment exists, the request is about something else entirely
        let c = &CASES[3];
        let at = 35 + rng(seed, 1) % 15;
        let h = history(seed, Some((c, at)), "Summarize the last step on retry backoff.");
        let (form, _, _) = run(&h, c.fragment);
        println!("negative seed {seed}: unrelated fragment {form:?}");
        assert_ne!(form, Form::Full, "unrelated old fragment admitted in full (seed {seed})");
        // the recent steps must still be present in full
        let mut cu = Curator::new("").unwrap();
        cu.window(WINDOW);
        cu.tools(&["read_file".into(), "recall".into()]);
        let r = cu.view(HEAD, &h, 1.0).unwrap();
        let text = serde_json::to_string(&r.view).unwrap();
        for n in (TURNS - 3)..TURNS {
            assert!(text.contains(&format!("work, step {n}.")), "recent step {n} lost (seed {seed})");
        }
    }
}
