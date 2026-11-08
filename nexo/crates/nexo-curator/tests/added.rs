//! What a view carries beyond the conversation is named and sized, and none of it changes the view.

use nexo_curator::{AddedKind, AddedState, Curator};
use serde_json::{json, Value};

fn step(k: usize, lines: usize) -> [Value; 2] {
    let id = format!("call_{k}");
    let out: String =
        (0..lines).map(|l| format!("data/part_{k}/row_{l}.csv:{l}: amount_{k}_{l} = {}\n", l * 7)).collect();
    [
        json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "bash", "input": {"command": format!("cat data/part_{k}")}}]}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": out}]}),
    ]
}

#[test]
fn a_short_conversation_adds_nothing() {
    let mut cur = Curator::new("").unwrap();
    let msgs = vec![json!({"role": "user", "content": "Sum the amounts of every part."})];
    let r = cur.view(0, &msgs, 0.0).unwrap();
    assert!(r.curated.is_empty(), "{:?}", r.curated);
}

#[test]
fn a_long_one_names_the_map_the_rule_and_what_it_held_back() {
    let mut cur = Curator::new("").unwrap();
    cur.window(12_000);
    let mut msgs = vec![json!({"role": "user", "content": "Sum the amounts of every part."})];
    let mut last = Vec::new();
    for k in 0..40 {
        msgs.extend(step(k, 60));
        if k == 1 {
            msgs.push(json!({"role": "user", "content": "Always run cargo fmt before you commit."}));
        }
        last = cur.view(1_000, &msgs, 0.0).unwrap().curated;
    }
    let of = |kind, state| last.iter().filter(|a| a.kind == kind && a.state == state).count();
    assert!(of(AddedKind::Start, AddedState::In) >= 1, "the map: {last:?}");
    assert_eq!(of(AddedKind::Rule, AddedState::In), 1, "{last:?}");
    assert!(last.iter().any(|a| a.kind == AddedKind::Rule && a.label.contains("cargo")), "{last:?}");
    let held: Vec<_> = last.iter().filter(|a| a.state == AddedState::Out).collect();
    assert!(!held.is_empty(), "outputs left out are named: {last:?}");
    assert!(held.iter().all(|a| a.tokens > 0 && !a.label.is_empty()), "{held:?}");
    assert!(held.iter().any(|a| a.label.contains("bash")), "named by tool and command: {held:?}");
    // reading it twice changes nothing: it is telemetry
    let again = cur.view(1_000, &msgs, 0.0).unwrap().curated;
    assert_eq!(again, last);
}

#[test]
fn naming_changes_no_decision() {
    let mut a = Curator::new("").unwrap();
    let mut b = Curator::new("").unwrap();
    a.window(12_000);
    b.window(12_000);
    let mut msgs = vec![json!({"role": "user", "content": "Sum the amounts of every part."})];
    for k in 0..20 {
        msgs.extend(step(k, 60));
        let (ra, rb) = (a.view(500, &msgs, 0.0).unwrap(), b.view(500, &msgs, 0.0).unwrap());
        assert_eq!(ra.view, rb.view);
        assert_eq!(ra.curated, rb.curated);
    }
}
