//! Offline paired benchmark: **policy actions to an independently checked answer**, not LLM steps.
//! From each workspace run `nice cargo run --offline --locked -j2 -p nexo-curator --example verified_steps`.
//! Save stdout to base.json. In the other workspace use the identical source and append
//! `-- --spec 'relations=1' --compare /absolute/path/to/base.json`. No network, filesystem search or model.
//!
//! Two fixed orders of the same synthetic paperwork, branched at the final question. The policy sees the
//! public task, text in the actual view (including a curator's quotations), and its own paid receipts;
//! never history, memory diagnostics, the underlying source table, the gold key, or the other arm.
//! Unit of work: one indexed external read for ONE edition/subject/field, or one final submission.
//! Setup is identical unscored history, not free reads during the task. No verifier feedback/retries.
//! An absence needs a paid negative receipt, not an empty view. Cap four actions; charge failures five
//! in the aggregate, and show completion and paired steps on common successes separately. The small
//! corpus and literal reader test access to machine-readable evidence, not comprehension of paraphrases.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use nexo_curator::{Curator, Reply, State};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const WINDOW: u64 = 20_000;
const HEAD: u64 = 1_000;
const CAP: usize = 4;
const PAGES: usize = 24;
const ABSENT: &str = "ABSENT";

// Independent fixture transcription. Fields: source edition subject field value. The page index
// controls placement ONLY. None means the source can be read but the transcript never contained it.
const RECORDS: &[(Option<usize>, &str)] = &[
    (Some(1), "museum E2 Lark carrier Zephyr"),
    (Some(6), "museum E2 Zephyr depot Brindle"),
    (Some(11), "museum E2 Brindle bay bay7"),
    (Some(7), "museum E1 Lark carrier Sable"),
    (Some(9), "museum E1 Sable depot Brindle"),
    (Some(12), "museum E1 Brindle bay bay3"),
    (Some(5), "rail E2 Lark line Sable"),
    (Some(12), "rail E2 Sable terminus Fen"),
    (Some(19), "rail E2 Fen platform violet"),
    (Some(2), "rail E1 Lark line Umber"),
    (Some(3), "rail E1 Umber terminus Cove"),
    (Some(4), "rail E1 Cove platform jade"),
    (Some(2), "stage E2 Lark ensemble Umber"),
    (Some(10), "stage E2 Umber venue Cove"),
    (Some(21), "stage E2 Cove part alto"),
    (Some(6), "stage E1 Lark ensemble Zephyr"),
    (Some(7), "stage E1 Zephyr venue Fen"),
    (Some(8), "stage E1 Fen part tenor"),
    (Some(8), "museum E2 Ibis carrier Kelso"),
    (Some(11), "museum E2 Kelso depot Rowan"),
    (Some(14), "museum E1 Rowan bay bay4"), // no bay exists for Rowan in E2
    (Some(16), "stage E2 Ibis ensemble Amber"),
    (Some(14), "stage E2 Amber venue Vale"),
    (None, "stage E2 Vale part bass"),        // real but never previously read
    (Some(PAGES), "rail E2 Wren line Petal"), // recent control in BOTH orders
    (Some(PAGES), "rail E2 Petal terminus Dale"),
    (Some(PAGES), "rail E2 Dale platform brass"),
];

#[derive(Clone, Copy)]
struct Task {
    id: &'static str,
    source: &'static str,
    start: &'static str,
    fields: [&'static str; 3],
}
const LOAN: [&str; 3] = ["carrier", "depot", "bay"];
const TRAIN: [&str; 3] = ["line", "terminus", "platform"];
const SHOW: [&str; 3] = ["ensemble", "venue", "part"];
const TASKS: &[Task] = &[
    Task { id: "loan", source: "museum", start: "Lark", fields: LOAN },
    Task { id: "schedule", source: "rail", start: "Lark", fields: TRAIN },
    Task { id: "programme", source: "stage", start: "Lark", fields: SHOW },
    Task { id: "not-in-edition", source: "museum", start: "Ibis", fields: LOAN },
    Task { id: "not-in-history", source: "stage", start: "Ibis", fields: SHOW },
    Task { id: "recent", source: "rail", start: "Wren", fields: TRAIN },
];
// Not computed by walking RECORDS or by running the policy. Never passed to the solver or curator.
fn verified(id: &str, submitted: &[String]) -> bool {
    let key = match id {
        "loan" => ["Zephyr", "Brindle", "bay7"],
        "schedule" => ["Sable", "Fen", "violet"],
        "programme" => ["Umber", "Cove", "alto"],
        "not-in-edition" => ["Kelso", "Rowan", ABSENT],
        "not-in-history" => ["Amber", "Vale", "bass"],
        "recent" => ["Petal", "Dale", "brass"],
        _ => panic!("unregistered task {id}"),
    };
    submitted == key
}

// Standard key in these synthetic public registries, not a Curator feature. Quotes embedded in a
// larger text also count if the entire self-contained receipt (including its closing delimiter) survived.
fn record(text: &str) -> Option<(String, String)> {
    let [src, ed, who, attr, val]: [&str; 5] = text.split_ascii_whitespace().collect::<Vec<_>>().try_into().ok()?;
    Some((format!("{src} {ed} {who} {attr}"), val.to_owned()))
}
type Observed = BTreeMap<String, BTreeSet<String>>;
fn observe(content: &Value, seen: &mut Observed) {
    if let Some(s) = content.as_str() {
        for after in s.split("[record ").skip(1) {
            if let Some((key, val)) = after.split_once(']').and_then(|(s, _)| record(s)) {
                seen.entry(key).or_default().insert(val);
            }
        }
    } else if let Some(blocks) = content.as_array() {
        for b in blocks {
            match b["type"].as_str() {
                Some("text") => observe(&b["text"], seen),
                Some("tool_result") if b["is_error"] != true => observe(&b["content"], seen),
                _ => (), // NOT call arguments or hidden identifiers
            }
        }
    }
}

enum Action {
    Lookup(String),
    Submit(Vec<String>),
}
fn decide(task: Task, view: &[Value], receipts: &BTreeMap<String, Option<String>>) -> Action {
    let mut seen = Observed::new();
    for m in view {
        observe(&m["content"], &mut seen);
    }
    let mut path = Vec::new();
    let mut subject = task.start;
    for field in task.fields {
        let key = format!("{} E2 {subject} {field}", task.source);
        let val = match receipts.get(&key) {
            Some(Some(v)) => v,
            Some(None) => {
                path.push(ABSENT.into());
                break;
            }
            None => match seen.get(&key).filter(|vs| vs.len() == 1).and_then(|vs| vs.first()) {
                Some(v) if v != ABSENT => v,
                _ => return Action::Lookup(key),
            },
        };
        path.push(val.clone());
        subject = val;
    }
    Action::Submit(path)
}

fn msg(role: &str, content: impl Into<Value>) -> Value {
    json!({"role":role,"content":content.into()})
}
fn read(messages: &mut Vec<Value>, path: &str, contents: &str) {
    let id = format!("read_{}", messages.len());
    messages.push(msg("assistant", json!([{"type":"tool_use","id":id,"name":"read","input":{"path":path}}])));
    messages.push(msg("user", json!([{"type":"tool_result","tool_use_id":id,"content":contents}])));
}
fn page(p: usize) -> String {
    let mut s = String::new();
    // Interleaved mundane archive entries; a mix of shared names, wrong fields/editions and unique
    // unrelated sites. All inputs and the insertion positions depend only on the fixed page number.
    for row in 0..96 {
        let n = p * 96 + row;
        let (src, who, field, val) = match n % 3 {
            0 => ("museum", "Lark", "carrier", "Crane"),
            1 => ("rail", "Lark", "line", "Quill"),
            _ => ("stage", "Lark", "ensemble", "Finch"),
        };
        if row == 28 + p % 36 {
            for &(pos, fact) in RECORDS.iter().filter(|(pos, _)| *pos == Some(p)) {
                debug_assert_eq!(pos, Some(p));
                writeln!(s, "Filed on sheet {p}, certified transfer: [record {fact}].").unwrap();
            }
        }
        writeln!(
            s,
            "Shelf {p}, entry {row}: [record {src} A{n} {who} {field} {val}{row}] filed after inspection at annex {n}."
        )
        .unwrap();
    }
    s
}
fn view(cur: &mut Curator, history: &[Value]) -> Reply {
    let r = cur.view(HEAD, history, history.len() as f64).expect("real Curator::view");
    assert!((0..=WINDOW as i64 - HEAD as i64).contains(&r.tokens), "view exceeded budget: {} + {HEAD}", r.tokens);
    r
}
fn prepare(reversed: bool, spec: &str) -> (Curator, Vec<Value>) {
    let mut cur = Curator::new(spec).expect("valid curator spec");
    cur.window(WINDOW);
    cur.tools(&["read".to_owned()]);
    let mut h = vec![];
    for i in 0..PAGES {
        let p = if reversed { PAGES - 1 - i } else { i };
        h.push(msg("user", format!("File sheet {p} for the municipal collection. Keep its transfer records on hand.")));
        view(&mut cur, &h);
        read(&mut h, &format!("archive/sheet-{p}.txt"), &page(p));
        view(&mut cur, &h);
        h.push(msg("assistant", "The sheet has been filed."));
    }
    h.push(msg("user", "File the late rail timetable supplement as well."));
    view(&mut cur, &h);
    let late: String =
        RECORDS.iter().filter(|(pos, _)| *pos == Some(PAGES)).map(|(_, s)| format!("[record {s}]\n")).collect();
    read(&mut h, "archive/supplement.txt", &late);
    view(&mut cur, &h);
    h.push(msg("assistant", "Supplement filed."));
    (cur, h)
}

#[derive(Serialize, Deserialize)]
struct Trial {
    case: String,
    verified: bool,
    steps_to_verified: Option<usize>,
    lookups: Vec<String>,
    submitted: Option<Vec<String>>,
    initial_raw_bytes: usize,
    initial_nonwhole_messages: usize,
    // Estimates from real calls, INCLUDING the 1000-token head, not provider-accounted tokens.
    decision_view_tokens: Vec<i64>,
}
fn run(task: Task, order: &str, mut cur: Curator, mut h: Vec<Value>, source: &BTreeMap<String, String>) -> Trial {
    h.push(msg("user", format!("Use the {} registry, edition E2. Starting at {}, follow {} then {} then {}. Give every hop in order, stopping at a confirmed missing record.", task.source, task.start, task.fields[0], task.fields[1], task.fields[2])));
    let mut trial = Trial {
        case: format!("{order}/{}", task.id),
        verified: false,
        steps_to_verified: None,
        lookups: vec![],
        submitted: None,
        initial_raw_bytes: serde_json::to_vec(&h).unwrap().len(),
        initial_nonwhole_messages: 0,
        decision_view_tokens: vec![],
    };
    assert!(trial.initial_raw_bytes / 4 > WINDOW as usize); // ASCII fixture, conservative size sanity check
    let mut receipts = BTreeMap::new();
    for step in 1..=CAP {
        let r = view(&mut cur, &h);
        if step == 1 {
            trial.initial_nonwhole_messages = r.memory.cells.iter().filter(|c| c.state != State::Whole).count();
            // Memory diagnostics never go to `decide`. Require a genuinely curated input.
            assert!(trial.initial_nonwhole_messages > 0, "no cut in {}", trial.case);
        }
        trial.decision_view_tokens.push(r.tokens + HEAD as i64);
        match decide(task, &r.view, &receipts) {
            Action::Lookup(k) => {
                let v = source.get(&k).cloned();
                let result = v.as_ref().map_or_else(|| format!("No entry for {k}."), |v| format!("[record {k} {v}]"));
                read(&mut h, &format!("registry/{k}"), &result);
                trial.lookups.push(k.clone());
                assert!(receipts.insert(k, v).is_none(), "duplicate paid read");
            }
            Action::Submit(path) => {
                trial.verified = verified(task.id, &path);
                trial.steps_to_verified = trial.verified.then_some(step);
                trial.submitted = Some(path);
                break;
            }
        }
    }
    trial
}

// Sanity-check sensitivity of the policy and grader without running either curator: near matches
// and a truncated quote aren't evidence, ambiguous receipts cost a read, stale paths don't verify.
fn check_contract() {
    let t = TASKS[0];
    let receipts = BTreeMap::new();
    let probe = |s: &str| decide(t, &[msg("user", s)], &receipts);
    assert!(
        matches!(probe("[record museum E1 Lark carrier Sable] [record rail E2 Lark carrier Finch] [record museum E2 Lark carrier Zephyr"),Action::Lookup(k) if k=="museum E2 Lark carrier")
    );
    assert!(
        matches!(probe("[record museum E2 Lark carrier Sable] [record museum E2 Lark carrier Zephyr]"),Action::Lookup(k) if k=="museum E2 Lark carrier")
    );
    assert!(!verified("loan", &["Sable".into(), "Brindle".into(), "bay7".into()]));
    assert!(!verified("not-in-edition", &["Kelso".into(), "Rowan".into(), "bay4".into()]));
    assert!(!verified("not-in-history", &["Amber".into(), "Vale".into(), ABSENT.into()]));
}
fn cost(t: &Trial) -> usize {
    t.steps_to_verified.unwrap_or(CAP + 1)
}
#[derive(Serialize, Deserialize)]
struct Report {
    // Identifies the entire fixture, decoder, verifier and scoring rule. A different spec is expected;
    // running a different benchmark with the same case names is NOT a paired comparison.
    harness: String,
    engine_format: String,
    spec: String,
    window: u64,
    head: u64,
    cap: usize,
    verified: usize,
    failure_penalized_sum: usize,
    trials: Vec<Trial>,
    paired: Option<Value>,
}
fn compare(base: Report, arm: &Report) -> Value {
    assert_eq!(
        (&base.harness, base.window, base.head, base.cap),
        (&arm.harness, arm.window, arm.head, arm.cap),
        "incompatible harness/configuration"
    );
    assert_eq!(base.trials.len(), arm.trials.len());
    let (mut n, mut deltas, mut common_sum) = (0, vec![], 0);
    for (a, b) in base.trials.iter().zip(&arm.trials) {
        assert_eq!((&a.case, a.initial_raw_bytes), (&b.case, b.initial_raw_bytes), "unpaired cases");
        let d = a.steps_to_verified.zip(b.steps_to_verified).map(|(a, b)| b as i64 - a as i64);
        if let Some(d) = d {
            n += 1;
            common_sum += d;
        }
        deltas.push(json!({"case":a.case,"baseline_steps":a.steps_to_verified,"candidate_steps":b.steps_to_verified,"candidate_minus_baseline":d}));
    }
    json!({"baseline_engine_format":base.engine_format,"baseline_spec":base.spec,"baseline_verified":base.verified,"candidate_verified":arm.verified,"common_verified":n,"common_step_delta_sum":common_sum,"penalized_delta_sum":arm.failure_penalized_sum as i64-base.failure_penalized_sum as i64,"pairs":deltas})
}
fn main() {
    let (mut spec, mut baseline) = (String::new(), None);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--spec" => spec = args.next().expect("--spec SPEC"),
            "--compare" => baseline = Some(args.next().expect("--compare BASELINE.json")),
            _ => panic!("usage: verified_steps [--spec SPEC] [--compare BASELINE.json]"),
        }
    }
    check_contract();
    let mut source = BTreeMap::new();
    for (_, s) in RECORDS {
        let (k, v) = record(s).expect("valid fixture row");
        assert!(source.insert(k, v).is_none());
    }
    let mut trials = vec![];
    for (order, reversed) in [("forward", false), ("reverse", true)] {
        let (cur, h) = prepare(reversed, &spec);
        for &t in TASKS {
            trials.push(run(t, order, cur.clone(), h.clone(), &source));
        }
    }
    let hash = include_bytes!("verified_steps.rs")
        .iter()
        .fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100000001b3));
    let mut arm = Report {
        harness: format!("verified_steps/fnv1a64:{hash:016x}"),
        engine_format: nexo_curator::snapshot::FORMAT.into(),
        spec,
        window: WINDOW,
        head: HEAD,
        cap: CAP,
        verified: trials.iter().filter(|t| t.verified).count(),
        failure_penalized_sum: trials.iter().map(cost).sum(),
        trials,
        paired: None,
    };
    if let Some(path) = baseline {
        let base =
            serde_json::from_slice(&std::fs::read(path).expect("read baseline report")).expect("parse baseline report");
        arm.paired = Some(compare(base, &arm));
    }
    println!("{}", serde_json::to_string_pretty(&arm).unwrap());
}
