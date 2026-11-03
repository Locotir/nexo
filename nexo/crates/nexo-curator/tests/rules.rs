//! The person's standing instructions in long sessions: kept in the view for as long as the session
//! lasts, whatever else leaves it.

use nexo_curator::Curator;
use serde_json::{json, Value};

/// The settings these tests start from: the anchor, the nudges and the size cap as they ran before the pure
/// default (cycle 50), so a change of the default spec does not move what they check.
const BASE: &str = "directives=1;nudge=8;close=0;ahead=0;dedup=0;live=0;lead=0;aim=0;follow=0;brief=0;digest=0;hold=0;loops=0;asked=0;sup=0;named=0;margin=1;mission=0";

fn user(text: &str) -> Value {
    json!({"role": "user", "content": text})
}

fn said(text: &str) -> Value {
    json!({"role": "assistant", "content": [{"type": "text", "text": text}]})
}

/// One step of tool work: a command and its long output, every one about a different file.
fn step(k: usize) -> [Value; 2] {
    let id = format!("call_{k}");
    let out: String =
        (0..40).map(|l| format!("src/unit_{k}/part_{l}.rs:{l}: value_{k}_{l} = compute_{l}(input_{k})\n")).collect();
    [
        json!({"role": "assistant", "content": [
            {"type": "text", "text": format!("Looking at unit {k}.")},
            {"type": "tool_use", "id": id, "name": "bash", "input": {"command": format!("grep -rn value src/unit_{k}")}}]}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": out}]}),
    ]
}

/// The conversation of ``opening`` followed by ``steps`` steps of tool work, viewed after every step;
/// returns the anchor (the text the first message carries) of the last view and the curator's stats.
fn session(spec: &str, opening: &[Value], steps: usize) -> (String, Value) {
    let mut cur = Curator::new(&format!("{BASE};{spec}")).unwrap();
    let mut msgs: Vec<Value> = opening.to_vec();
    let mut last = Value::Null;
    for k in 0..steps {
        msgs.extend(step(k));
        last = cur.answer(&json!({"op": "view", "system_tokens": 0, "messages": msgs}), 0.0);
        assert_eq!(last["ok"], true, "{last}");
    }
    let first = &last["view"][0]["content"];
    let anchor = match first {
        Value::Array(bs) => bs.iter().skip(1).filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    };
    (anchor, last["stats"].clone())
}

#[test]
fn a_rule_given_early_is_in_the_view_hundreds_of_turns_later() {
    let opening = [
        user("Fix the overflow in the parser."),
        said("On it."),
        user("Never use unwrap in library code."),
        said("Understood."),
    ];
    let (anchor, stats) = session("", &opening, 250);
    assert!(stats["left_messages"].as_i64().unwrap() > 200, "{stats}");
    assert!(anchor.contains("Standing instructions the user gave during this session"), "{anchor}");
    assert!(anchor.contains("turn 2: Never use unwrap in library code."), "{anchor}");
}

#[test]
fn the_agents_rule_lines_land_in_the_rules_block() {
    let opening = [
        user("Refactor the lexer; keep it quick."),
        said("Rule: run cargo fmt before every commit.\nFinding: the lexer has two entry points.\nPlan: merge them."),
    ];
    let (anchor, _) = session("", &opening, 250);
    let rules = &anchor[anchor.find("Standing instructions").expect(&anchor)..];
    assert!(rules.contains("turn 1, as you restated it: run cargo fmt before every commit."), "{anchor}");
    assert!(!anchor.contains("Finding: run cargo fmt"), "{anchor}");
    assert!(anchor.contains("Finding: the lexer has two entry points."), "{anchor}");
}

#[test]
fn questions_and_requests_do_not_make_rules() {
    let opening = [user("Why does the parser overflow?"), said("Let me look."), user("ok"), said("Looking.")];
    let (anchor, _) = session("", &opening, 250);
    assert!(!anchor.contains("Standing instructions"), "{anchor}");
}

#[test]
fn a_later_rule_is_listed_after_an_earlier_one() {
    let opening = [
        user("Port the tests."),
        said("Sure."),
        user("Siempre escribe los comentarios en español."),
        said("Vale."),
        user("A partir de ahora, use English for the comments instead."),
        said("Done."),
        user("Never use unwrap in library code."),
        said("Ok."),
        user("never use unwrap in library code"),
        said("Ok."),
    ];
    let (anchor, _) = session("", &opening, 250);
    let rules = &anchor[anchor.find("Standing instructions").expect(&anchor)..];
    let at = |s: &str| rules.find(s).unwrap_or_else(|| panic!("{s} not in {rules}"));
    assert!(at("turn 2: Siempre escribe") < at("turn 3: A partir de ahora"), "{rules}");
    assert!(at("turn 3: A partir de ahora") < at("turn 4: Never use unwrap"), "{rules}");
    assert_eq!(rules.matches("unwrap in library code").count(), 1, "{rules}");
}

#[test]
fn a_big_set_of_rules_shrinks_the_ledger_instead_of_leaving() {
    let mut opening = vec![user("Clean up the codebase."), said("Starting.")];
    let rules: Vec<String> = (0..60)
        .map(|k| format!("Never rename the function handler_{k} or change its signature, the plugins call it by name."))
        .collect();
    for r in &rules {
        opening.push(user(r));
        opening.push(said("Noted."));
    }
    let facts = |anchor: &str| anchor.lines().filter(|l| l.starts_with('m') && l[1..].starts_with("0")).count();
    let (with, _) = session("eoq=6000;rules=100", &opening, 200);
    let (without, _) = session("eoq=6000;rules=0", &opening, 200);
    for (k, r) in rules.iter().enumerate() {
        assert!(with.contains(&format!("turn {}: {r}", k + 2)), "rule {k} missing");
    }
    assert!(facts(&with) < facts(&without), "{} >= {}", facts(&with), facts(&without));
}

#[test]
fn a_rewritten_history_keeps_the_rules() {
    let mut cur = Curator::new("").unwrap();
    let mut msgs = vec![user("Fix the parser."), said("Sure."), user("Never push to main."), said("Understood.")];
    msgs.extend(step(0));
    cur.answer(&json!({"op": "view", "system_tokens": 0, "messages": msgs}), 0.0);
    // the harness compacts the history: the message with the rule is gone from it
    let mut msgs = vec![user("Summary of the work so far: the parser fix is half done."), said("Continuing.")];
    msgs.extend(step(1));
    let r = cur.answer(&json!({"op": "view", "system_tokens": 0, "messages": msgs}), 0.0);
    let first = r["view"][0]["content"].to_string();
    assert!(first.contains("turn 2: Never push to main."), "{first}");
}

#[test]
fn with_the_default_a_long_session_stays_bounded_and_keeps_the_rule() {
    let opening = [
        user("Fix the overflow in the parser."),
        said("On it."),
        user("Never use unwrap in library code."),
        said("Understood."),
    ];
    let mut cur = Curator::new("").unwrap();
    let mut msgs: Vec<Value> = opening.to_vec();
    let mut last = Value::Null;
    for k in 0..600 {
        msgs.extend(step(k));
        last = cur.answer(&json!({"op": "view", "system_tokens": 0, "messages": msgs}), 0.0);
    }
    // 600 outputs of 40 lines each are some 600 000 tokens; the view stays under the cap (`eoq` plus the batch
    // that pays a rewrite, some 31 000 at its peak here), and the rule is in it
    assert!(last["tokens"].as_i64().unwrap() < 40_000, "{}", last["tokens"]);
    assert!(last["view"].to_string().contains("Never use unwrap in library code."));
}
