//! The padded revert tasks at 0 $: they score a known-good state 1 and a wrong one less, and a
//! scripted replay through the curator shows the first topic leaving the view.

use nexo_bench::tasks::{self, padded};
use nexo_curator::Curator;
use serde_json::{json, Value};

fn task(id: &str) -> tasks::Task {
    tasks::all().into_iter().find(|t| t.id == id).expect("a task")
}

#[test]
fn the_data_task_scores_its_solution_1_and_the_start_and_a_wrong_state_less() {
    let t = task("padded-revert-data");
    let wd = tempfile::tempdir().unwrap();
    t.materialize(wd.path()).unwrap();
    let start = (t.check)(wd.path());
    assert!(!start.ok && start.score < 1.0, "{start:?}");
    (t.solution)(wd.path());
    let good = (t.check)(wd.path());
    assert!(good.ok && good.score == 1.0, "{}", good.detail);
    // the revert also dropped the channel column: worse
    let p = wd.path().join("data/orders.csv");
    let csv = std::fs::read_to_string(&p).unwrap().replace(",web", "").replace(",channel", "");
    std::fs::write(&p, csv).unwrap();
    let bad = (t.check)(wd.path());
    assert!(bad.score < 1.0 && bad.score > 0.0, "{bad:?}");
    // the discount change was never undone
    (t.solution)(wd.path());
    let q = wd.path().join("sql/transform.sql");
    let sql = std::fs::read_to_string(&q).unwrap().replace("SUM(qty * price)", "SUM(qty * price * (1 - discount))");
    std::fs::write(&q, sql).unwrap();
    let bad = (t.check)(wd.path());
    assert!(!bad.ok && bad.score < 1.0, "{bad:?}");
}

#[test]
fn the_code_task_scores_its_solution_1_and_a_wrong_state_less() {
    let t = task("padded-revert-code");
    let wd = tempfile::tempdir().unwrap();
    t.materialize(wd.path()).unwrap();
    // untouched: shipping is original but tax and invoice are not there
    let start = (t.check)(wd.path());
    assert!(!start.ok && start.score < 1.0, "{start:?}");
    (t.solution)(wd.path());
    let good = (t.check)(wd.path());
    assert!(good.ok && good.score == 1.0, "{}", good.detail);
    // the shipping change left in: worse
    let p = wd.path().join("src/shipping.rs");
    let src =
        std::fs::read_to_string(&p).unwrap().replace("round2(4.5 + 1.2 * weight_kg)", "round2(6.0 + 1.2 * weight_kg)");
    std::fs::write(&p, src).unwrap();
    let bad = (t.check)(wd.path());
    assert!(!bad.ok && bad.score < 1.0, "{bad:?}");
}

#[test]
fn the_chat_scores_the_reference_1_and_wrong_answers_less() {
    let c = padded::chat_task();
    let good = (c.check)(&(c.reference)());
    assert!(good.ok && good.score == 1.0, "{}", good.detail);
    let mut other = (c.reference)();
    let second = other[2].clone();
    *other.last_mut().unwrap() = second;
    let bad = (c.check)(&other);
    assert!(!bad.ok && bad.score < 1.0, "{bad:?}");
    let mut wordy = (c.reference)();
    *wordy.last_mut().unwrap() =
        format!("Sure, going back, it was {} euro, as computed from the Norvik card.", wordy[0]);
    assert!((c.check)(&wordy).score < 1.0);
    assert_eq!((c.check)(&[]).score, 0.0);
}

// ---------------------------------------------------------------- replay through the curator

type Step = (&'static str, Value, String);

fn replay(
    turns: &[(u32, String)],
    steps: &dyn Fn(usize) -> Vec<Step>,
    finals: &dyn Fn(usize) -> String,
    upto: usize,
) -> Vec<Value> {
    let mut m = Vec::new();
    for (i, (_, text)) in turns.iter().enumerate().take(upto) {
        m.push(json!({"role": "user", "content": text}));
        if i + 1 == upto {
            break;
        }
        for (k, (tool, input, out)) in steps(i).into_iter().enumerate() {
            let id = format!("call_{i}_{k}");
            m.push(
                json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": tool, "input": input}]}),
            );
            m.push(json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": out}]}));
        }
        m.push(json!({"role": "assistant", "content": [{"type": "text", "text": finals(i)}]}));
    }
    m
}

fn weight(msgs: &[Value]) -> i64 {
    Curator::new("").unwrap().view(2_000, msgs, 0.0).unwrap().tokens
}

/// The view at the last request, as text; the curator sees one request per user message.
fn last_view(msgs: &[Value], tools: &[&str]) -> String {
    let mut c = Curator::new("").unwrap();
    c.tools(&tools.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let mut text = String::new();
    let mut outline = String::new();
    for (i, m) in msgs.iter().enumerate() {
        let user_text = m["role"] == "user" && m["content"].is_string();
        if user_text || i + 1 == msgs.len() {
            let v = c.view(2_000, &msgs[..=i], 0.0).unwrap().view;
            outline = v
                .iter()
                .map(|m| format!("{} {}", m["role"], m["content"].to_string().chars().take(110).collect::<String>()))
                .collect::<Vec<_>>()
                .join("\n");
            text = serde_json::to_string(&v).unwrap();
        }
    }
    if std::env::var("PADDED_OUTLINE").is_ok() {
        eprintln!("{outline}");
    }
    text
}

fn check_hidden(
    name: &str,
    turns: &[(u32, String)],
    steps: &dyn Fn(usize) -> Vec<Step>,
    finals: &dyn Fn(usize) -> String,
    tools: &[&str],
    original: &str,
) {
    let before = replay(turns, steps, finals, turns.len() - 1);
    let w = weight(&before) - weight(&before[..1]);
    eprintln!(
        "{name}: earlier topics (without the first request, which always stays) weigh {w} tokens (curator count)"
    );
    assert!(w > 8_000, "{name}: only {w} tokens");
    let all = replay(turns, steps, finals, turns.len());
    let view = last_view(&all, tools);
    assert!(
        view.contains("other topics"),
        "{name}: no card in the last view: {}",
        view.chars().take(600).collect::<String>()
    );
    assert!(!view.contains(original), "{name}: the original content is still in the view");
}

#[test]
fn code_style_replay_hides_the_first_topic() {
    let t = task("padded-revert-code");
    let lib = t.files.iter().find(|f| f.0 == "src/shipping.rs").unwrap().1.clone();
    let steps = move |i: usize| -> Vec<Step> {
        match i {
            0 => vec![
                ("read", json!({"path": "src/shipping.rs"}), lib.clone()),
                (
                    "edit",
                    json!({"path": "src/shipping.rs", "old": "round2(4.5 + 1.2 * weight_kg)", "new": "round2((4.5 + 1.2 * weight_kg + if weight_kg > 20.0 { 15.0 } else { 0.0 }).max(6.0))"}),
                    "edited src/shipping.rs".into(),
                ),
            ],
            1 => {
                vec![("edit", json!({"path": "src/tax.rs", "old": "0.18", "new": "0.21"}), "edited src/tax.rs".into())]
            }
            _ => vec![("bash", json!({"command": "cargo test"}), "running 3 tests\ntest result: ok. 3 passed".into())],
        }
    };
    check_hidden(
        "padded-revert-code",
        &t.turns,
        &steps,
        &|i| format!("Done with request {}.", i + 1),
        &["read", "edit", "bash", "recall"],
        "4.5 + 1.2 * weight_kg)",
    );
}

#[test]
fn data_style_replay_hides_the_first_topic() {
    let t = task("padded-revert-data");
    let sql = t.files.iter().find(|f| f.0 == "sql/transform.sql").unwrap().1.clone();
    let steps = move |i: usize| -> Vec<Step> {
        match i {
            0 => vec![
                ("read", json!({"path": "sql/transform.sql"}), sql.clone()),
                (
                    "edit",
                    json!({"path": "sql/transform.sql", "old": "SUM(qty * price)", "new": "SUM(qty * price * (1 - discount))"}),
                    "edited sql/transform.sql".into(),
                ),
            ],
            1 => vec![(
                "edit",
                json!({"path": "sql/transform.sql", "old": "region,", "new": "region,\n  channel,"}),
                "edited sql/transform.sql".into(),
            )],
            _ => vec![("write", json!({"path": "sql/top_customers.sql"}), "wrote sql/top_customers.sql".into())],
        }
    };
    check_hidden(
        "padded-revert-data",
        &t.turns,
        &steps,
        &|i| format!("Done with request {}.", i + 1),
        &["read", "edit", "write", "recall"],
        "Monthly revenue per region",
    );
}

#[test]
fn chat_style_replay_hides_the_first_topic() {
    let c = padded::chat_task();
    let reference = (c.reference)();
    check_hidden(
        "padded-revert-chat",
        &c.turns,
        &|_| Vec::new(),
        &|i| reference[i].clone(),
        &["recall"],
        "Z042 | base",
    );
}
