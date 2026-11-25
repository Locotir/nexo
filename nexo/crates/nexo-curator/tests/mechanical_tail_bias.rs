//! Cold, deterministic measurement of Curator's tail (recency) bias. No model is called. A synthetic
//! ~600-message history (about 78k estimated tokens against a 20k window) carries ONE needed datum at a
//! chosen distance back from the final request, in a chosen role, with a final request that either
//! shares its topic with the datum or does not. For each cell we record whether the datum is in the
//! admitted view (as the full original message, as a pointer: a "Facts" line of the curator note or a
//! quoted "Historical evidence" row, or not at all), the view's estimated tokens and how many times the
//! datum text occurs in the view. Measurement only: the assertions are invariants that hold today.
//! Run `cargo test -p nexo-curator --test mechanical_tail_bias -- --nocapture` for the table;
//! set NEXO_TAILBIAS_CSV=<path> to also write one CSV row per trial.
use nexo_curator::Curator;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const HEAD: u64 = 640;
const WINDOW: u64 = 20_000;
const TURNS: usize = 150;
const SEEDS: [usize; 3] = [3, 17, 41];
/// distance buckets (share of the history back from the final request); the datum sits at the middle
const BUCKETS: usize = 10;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Role {
    UserRule,
    UserFact,
    Assistant,
    Tool,
    ToolBuried,
}
const ROLES: [Role; 5] = [Role::UserRule, Role::UserFact, Role::Assistant, Role::Tool, Role::ToolBuried];
impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::UserRule => "user-rule",
            Role::UserFact => "user-fact",
            Role::Assistant => "assistant",
            Role::Tool => "tool-first",
            Role::ToolBuried => "tool-buried",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Form {
    Full,
    Pointer,
    Absent,
}

struct Trial {
    form: Form,
    tokens: i64,
    repeats: usize,
}

fn msg(role: &str, content: impl Into<Value>) -> Value {
    json!({"role": role, "content": content.into()})
}

/// Deterministic xorshift, so seeds vary row counts and shelf names without any dependency.
fn rng(seed: usize, k: usize) -> usize {
    let mut x = (seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (k as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9)
        ^ 0x1234_5678;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    (x >> 11) as usize
}

/// Turn index (0 = first request) for a bucket: bucket 0 is the 0-10% back (most recent).
fn turn_for(bucket: usize, seed: usize) -> usize {
    let centre = (bucket as f64 + 0.5) / BUCKETS as f64; // share of history back from the end
    let jitter = (rng(seed, 99 + bucket) % 5) as isize - 2; // +-2 turns (1.3%) so seeds differ
    let n = ((1.0 - centre) * TURNS as f64) as isize + jitter;
    n.clamp(1, TURNS as isize - 2) as usize
}

fn datum(role: Role, seed: usize) -> (String, String) {
    // (unique needle, sentence carrying it)
    match role {
        Role::UserRule => {
            let n = format!("PLUMB{seed}Q");
            (n.clone(), format!("Rule from now on: never put the word {n} in a file name."))
        }
        Role::UserFact => {
            let n = format!("ZQ{seed}77");
            (n.clone(), format!("Remember that the vault-key is {n}."))
        }
        Role::Assistant => {
            let n = format!("ZQ{seed}77");
            (n.clone(), format!("I found that the vault-key is {n}."))
        }
        Role::Tool => {
            let n = format!("ZQ{seed}77");
            (n.clone(), format!("vault-key = {n}"))
        }
        // no keyword, in the middle of the body: only its place in the output marks it
        Role::ToolBuried => {
            let n = format!("ZQ{seed}77");
            (n.clone(), format!("table/own,{n},bin_3,quantity_9"))
        }
    }
}

fn history(role: Role, bucket: usize, seed: usize, same_topic: bool, dense: bool) -> (Vec<Value>, String) {
    let at = turn_for(bucket, seed);
    let (needle, sentence) = datum(role, seed);
    let mut h = vec![];
    for n in 0..TURNS {
        let rows_n = 32 + rng(seed, n) % 16;
        let rows = (0..rows_n)
            .map(|i| format!("table/s{n}/{i:03}.csv,article_{i},bin_{},quantity_{}\n", i % 17, i * 53))
            .collect::<String>();
        let mut ask = format!("Please read shelf {n}.");
        let mut body = rows;
        let mut done = format!("Shelf {n} done.");
        if dense && n != at {
            // a decoy of the same role and shape on every other turn: the datum competes with 149 look-alikes
            let d = format!("{}", 100 + n);
            let decoy = match role {
                Role::UserRule => format!("Rule from now on: never put the word DECOY{d}Z in a file name."),
                Role::UserFact => format!("Remember that the vault-key is DK{d}0."),
                Role::Assistant => format!("I found that the vault-key is DK{d}0."),
                Role::Tool => format!("vault-key = DK{d}0"),
                Role::ToolBuried => format!("table/own,DK{d}0,bin_3,quantity_9"),
            };
            match role {
                Role::UserRule | Role::UserFact => ask = format!("{ask} {decoy}"),
                Role::Assistant => done = format!("{done} {decoy}"),
                Role::Tool => body = format!("{decoy}\n{body}"),
                Role::ToolBuried => {
                    let mut ls: Vec<&str> = body.lines().collect();
                    ls.insert(ls.len() / 2, &decoy);
                    body = ls.join("\n");
                }
            }
        }
        if n == at {
            match role {
                Role::UserRule | Role::UserFact => ask = format!("{ask} {sentence}"),
                Role::Assistant => done = format!("{done} {sentence}"),
                Role::Tool => body = format!("{sentence}\n{body}"),
                Role::ToolBuried => {
                    let mut ls: Vec<&str> = body.lines().collect();
                    ls.insert(ls.len() / 2, &sentence);
                    body = ls.join("\n");
                }
            }
        }
        h.push(msg("user", ask));
        h.push(msg(
            "assistant",
            json!([{"type":"tool_use","id":format!("t{n}"),"name":"read_file","input":{"path":format!("s{n}.csv")}}]),
        ));
        h.push(msg("user", json!([{"type":"tool_result","tool_use_id":format!("t{n}"),"content":body}])));
        h.push(msg("assistant", done));
    }
    let q = match (role, same_topic) {
        (Role::UserRule, true) => "Name the next output file, following my rule about file names.",
        (Role::UserRule, false) => "Please read shelf 150 and summarize the quantity column.",
        (Role::ToolBuried, true) => "Which bin and quantity does the own table row have? Give its code.",
        (_, true) => "What was the vault-key?",
        (_, false) => "Please read shelf 150 and summarize the quantity column.",
    };
    h.push(msg("user", q));
    (h, needle)
}

fn text_of(v: &Value) -> String {
    serde_json::to_string(v).unwrap()
}

fn run(role: Role, bucket: usize, seed: usize, same_topic: bool, dense: bool) -> Trial {
    let (h, needle) = history(role, bucket, seed, same_topic, dense);
    let mut c = Curator::new("").unwrap();
    c.window(WINDOW);
    c.tools(&["read_file".into(), "recall".into()]);
    let r = c.view(HEAD, &h, 1.0).unwrap();
    assert!(r.tokens + HEAD as i64 <= WINDOW as i64, "over budget: {}", r.tokens);
    let last = r.view.len() - 1;
    let mut full = false;
    let mut pointer = false;
    let mut repeats = 0;
    for (i, m) in r.view.iter().enumerate() {
        let blocks = m["content"].as_array().cloned().unwrap_or_else(|| vec![m["content"].clone()]);
        for b in blocks {
            let s = text_of(&b);
            let k = s.matches(&needle).count();
            if k == 0 {
                continue;
            }
            repeats += k;
            let t = b["text"].as_str().unwrap_or("");
            let curator_note = t.starts_with("[curator]") || (i == last && t.starts_with("Historical evidence"));
            if curator_note {
                pointer = true
            } else {
                full = true
            }
        }
    }
    let form = if full {
        Form::Full
    } else if pointer {
        Form::Pointer
    } else {
        Form::Absent
    };
    Trial { form, tokens: r.tokens, repeats }
}

fn label(b: usize) -> String {
    format!("{:>3}-{:<3}%", b * 100 / BUCKETS, (b + 1) * 100 / BUCKETS)
}

#[test]
fn tail_bias_table_and_invariants() {
    // (dense, role, same-topic, bucket) -> trials; trials run on two threads
    let mut jobs = vec![];
    for dense in [false, true] {
        for &role in &ROLES {
            for same in [true, false] {
                for b in 0..BUCKETS {
                    for &seed in &SEEDS {
                        jobs.push((dense, role, same, b, seed));
                    }
                }
            }
        }
    }
    let done: Vec<_> = std::thread::scope(|sc| {
        let hs: Vec<_> = jobs
            .chunks(jobs.len().div_ceil(2))
            .map(|ch| {
                sc.spawn(move || {
                    ch.iter().map(|&(d, r, s, b, seed)| ((d, r, s, b, seed), run(r, b, seed, s, d))).collect::<Vec<_>>()
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    let mut cells: BTreeMap<(bool, Role, bool, usize), Vec<Trial>> = BTreeMap::new();
    let mut csv = String::from("decoys,role,topic,bucket,seed,form,view_tokens,repeats\n");
    for ((d, role, same, b, seed), t) in done {
        csv.push_str(&format!(
            "{},{},{},{},{},{:?},{},{}\n",
            d,
            role.name(),
            if same { "same" } else { "other" },
            b,
            seed,
            t.form,
            t.tokens,
            t.repeats
        ));
        cells.entry((d, role, same, b)).or_default().push(t);
    }
    if let Ok(p) = std::env::var("NEXO_TAILBIAS_CSV") {
        std::fs::write(p, &csv).unwrap();
    }
    let n = SEEDS.len();
    for dense in [false, true] {
        println!(
            "\n== {} == admit rate = (full+pointer)/{n} seeds; F=full original message, P=pointer only",
            if dense { "WITH 149 same-role decoys" } else { "SPARSE (one datum)" }
        );
        println!("distance back |{}", ROLES.iter().map(|r| format!(" {:^19}|", r.name())).collect::<String>());
        println!("              |{}", ROLES.iter().map(|_| "  same     other  |".to_string()).collect::<String>());
        for b in 0..BUCKETS {
            let mut line = format!("{} |", label(b));
            for &role in &ROLES {
                for same in [true, false] {
                    let ts = &cells[&(dense, role, same, b)];
                    let f = ts.iter().filter(|t| t.form == Form::Full).count();
                    let p = ts.iter().filter(|t| t.form == Form::Pointer).count();
                    line.push_str(&format!(" {f}F{p}P{} ", if f + p == 0 { " ·" } else { "  " }));
                }
                line.push('|');
            }
            println!("{line}");
        }
        for &role in &ROLES {
            for same in [true, false] {
                let ts: Vec<&Trial> = (0..BUCKETS).flat_map(|b| cells[&(dense, role, same, b)].iter()).collect();
                let adm = ts.iter().filter(|t| t.form != Form::Absent).count();
                let rep = ts.iter().map(|t| t.repeats).sum::<usize>() as f64 / ts.len() as f64;
                let tok = ts.iter().map(|t| t.tokens).sum::<i64>() / ts.len() as i64;
                println!(
                    "{:<11} {:<5} admitted {:>3}/{} repeats {:.2} view-tokens {}",
                    role.name(),
                    if same { "same" } else { "other" },
                    adm,
                    ts.len(),
                    rep,
                    tok
                );
            }
        }
    }

    // Invariants that hold today (regression guard, not a quality claim):
    for dense in [false, true] {
        for &role in &ROLES {
            for same in [true, false] {
                // 1. the 0-10% bucket (the most recent turns) always keeps the datum in full;
                assert!(
                    cells[&(dense, role, same, 0)].iter().all(|t| t.form == Form::Full),
                    "{role:?} same={same} dense={dense}: recent datum lost"
                );
            }
        }
        // 2. user messages (rules and facts) stay admitted at every distance, with or without decoys;
        for role in [Role::UserRule, Role::UserFact] {
            for same in [true, false] {
                for b in 0..BUCKETS {
                    assert!(
                        cells[&(dense, role, same, b)].iter().all(|t| t.form != Form::Absent),
                        "{role:?} same={same} dense={dense} bucket {b}: user datum dropped"
                    );
                }
            }
        }
    }
    // 3. a datum buried mid-body of an old tool result among 149 look-alikes is admitted in at least 39 of 60 trials
    //    (34 before the ledger spent what the tail leaves, `least=0.0004`; a ledger that squeezes the recent
    //    tail out of the view fails invariant 1 instead);
    let buried = |t: &&Trial| t.form != Form::Absent;
    let kept: usize = [true, false]
        .iter()
        .map(|&same| (0..BUCKETS).flat_map(|b| cells[&(true, Role::ToolBuried, same, b)].iter()).filter(buried).count())
        .sum();
    assert!(kept >= 39, "buried tool datum with decoys admitted in only {kept}/60 trials");
    // 4. the budget (asserted in `run`) holds and no datum is repeated more than four times in a view
    //    (observed maximum today: 4, a user message that is both in the curator note and quoted as evidence).
    assert!(cells.values().flatten().all(|t| t.repeats <= 4), "datum repeated more than four times in a view");
}
