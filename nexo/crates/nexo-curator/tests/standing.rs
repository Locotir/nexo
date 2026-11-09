//! Standing facts: what the session learnt that overrides a default comes back when the action about to be
//! made supposes the default, although the request never names the fact.

use nexo_curator::Curator;
use serde_json::{json, Value};

/// The settings these tests start from (as in `rules.rs`), so a change of the default spec does not move them.
const BASE: &str = "directives=1;nudge=8;close=0;ahead=0;dedup=0;live=0;lead=0;aim=0;follow=0;brief=0;digest=0;hold=0;asked=0;sup=0;named=0;margin=1;mission=0";

fn user(text: &str) -> Value {
    json!({"role": "user", "content": text})
}

fn said(text: &str) -> Value {
    json!({"role": "assistant", "content": [{"type": "text", "text": text}]})
}

/// One step of tool work: a command and its long output (about a different unit every time).
fn step(k: usize, extra: &str) -> [Value; 2] {
    let id = format!("call_{k}");
    let mut out: String =
        (0..40).map(|l| format!("src/unit_{k}/part_{l}.rs:{l}: value_{k}_{l} = compute_{l}(input_{k})\n")).collect();
    out += extra;
    [
        json!({"role": "assistant", "content": [
            {"type": "text", "text": format!("Looking at unit {k}.")},
            {"type": "tool_use", "id": id, "name": "bash", "input": {"command": format!("grep -rn value src/unit_{k}")}}]}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": out}]}),
    ]
}

/// A conversation of ``opening`` and ``steps`` steps of tool work (the first one carries ``extra`` in its
/// output), viewed after every step.
fn work(cur: &mut Curator, opening: &[Value], steps: usize, extra: &str) -> Vec<Value> {
    let mut msgs: Vec<Value> = opening.to_vec();
    for k in 0..steps {
        msgs.extend(step(k, if k == 0 { extra } else { "" }));
        let r = cur.answer(&json!({"op": "view", "system_tokens": 0, "messages": msgs}), 0.0);
        assert_eq!(r["ok"], true, "{r}");
    }
    msgs
}

fn strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        Value::Object(m) => m.iter().filter(|(k, _)| *k != "type" && *k != "id").for_each(|(_, x)| strings(x, out)),
        _ => {}
    }
}

/// The view for ``msgs`` followed by ``task``: all its text, and the `Standing:` lines it carries.
fn ask(cur: &mut Curator, mut msgs: Vec<Value>, task: &str) -> (String, Vec<String>) {
    msgs.push(user(task));
    let r = cur.answer(&json!({"op": "view", "system_tokens": 0, "messages": msgs}), 0.0);
    assert_eq!(r["ok"], true, "{r}");
    let mut all = Vec::new();
    strings(&r["view"], &mut all);
    let text = all.join("\n");
    let lines = text.lines().filter(|l| l.starts_with("Standing: ")).map(str::to_string).collect();
    (text, lines)
}

fn curator() -> Curator {
    Curator::new(BASE).unwrap()
}

const RC: &str = "My shell rc lives at /etc/skel-77/rc, not ~/.bashrc.";

#[test]
fn the_five_forms_come_back_for_a_task_that_supposes_the_default() {
    // (what the session learnt, how, the task that supposes the default, the line that must come back)
    let cases: [(&str, &str, &str); 5] = [
        // the person says it, with no signal word
        (RC, "Add an alias to my shell.", RC),
        // a command printed it
        ("", "Add an alias to my shell.", "shell_rc = /etc/skel-77/rc"),
        // the agent noted it
        (
            "",
            "Connect to the database and count the rows in users.",
            "the database (postgres) listens on port 5433, not 5432",
        ),
        // a prohibition
        (
            "Don't touch /srv/legacy9, it is the legacy folder.",
            "Clean up the legacy folder.",
            "Don't touch /srv/legacy9, it is the legacy folder.",
        ),
        // a preferred tool
        ("", "Search the repo for TODO.", "searching the repo goes with rg, not grep"),
    ];
    for (k, (said_by_person, task, line)) in cases.into_iter().enumerate() {
        let mut opening = vec![user("Fix the overflow in the parser."), said("On it.")];
        let mut extra = String::new();
        match k {
            0 | 3 => {
                opening.push(user(said_by_person));
                opening.push(said("Understood."));
            }
            1 => extra = format!("{line}\n"),
            _ => opening.push(said(&format!("Checked it.\nFinding: {line}"))),
        }
        let mut cur = curator();
        let msgs = work(&mut cur, &opening, 250, &extra);
        let (text, standing) = ask(&mut cur, msgs, task);
        assert!(text.contains(line), "case {k}: {task:?} lost {line:?}");
        if k != 3 {
            // (a prohibition is a rule as well: the rules block may carry it)
            assert!(standing.iter().any(|l| l.contains(line)), "case {k}: not in the tail: {standing:?}");
        }
    }
}

#[test]
fn a_path_is_the_same_target_however_it_is_written() {
    let opening = [user("Fix the overflow in the parser."), said("On it."), user(RC), said("Understood.")];
    let mut cur = curator();
    let msgs = work(&mut cur, &opening, 250, "");
    for task in [
        "Add an alias to my shell.",
        "Append export PATH=/opt/bin to ~/.bashrc.",
        "Run echo hi >> /home/dev/.bashrc",
        "Edit bashrc and reload it.",
        "Show me the rc.",
    ] {
        let (_, standing) = ask(&mut cur, msgs.clone(), task);
        assert!(standing.iter().any(|l| l.contains(RC)), "{task:?} did not bring the fact: {standing:?}");
    }
    let (_, standing) = ask(&mut cur, msgs, "Rename the parser module to lexer.");
    assert!(standing.is_empty(), "{standing:?}");
}

#[test]
fn a_fact_still_whole_in_the_view_is_not_sent_twice() {
    let opening = [user("Fix the overflow in the parser."), said("On it."), user(RC), said("Understood.")];
    let mut cur = curator();
    let msgs = work(&mut cur, &opening, 2, "");
    let (text, standing) = ask(&mut cur, msgs, "Add an alias to my shell.");
    assert!(standing.is_empty(), "{standing:?}");
    assert_eq!(text.matches(RC).count(), 1);
}

#[test]
fn a_later_fact_about_the_same_subject_replaces_the_old_one() {
    let old = "The staging server is at stage1.internal:8797, not localhost:8080.";
    let new = "The staging server is at stage2.internal:9000, not localhost:8080.";
    let opening = [user("Fix the overflow in the parser."), said("On it."), user(old), said("Understood.")];
    let mut cur = curator();
    let mut msgs = work(&mut cur, &opening, 100, "");
    msgs.extend([user(new), said("Understood.")]);
    let msgs = work(&mut cur, &msgs, 150, "");
    let (_, standing) = ask(&mut cur, msgs, "Deploy to staging.");
    assert_eq!(standing.len(), 1, "{standing:?}");
    assert!(standing[0].starts_with(&format!("Standing: {new} (antes: {old}")), "{standing:?}");
}

#[test]
fn facts_survive_a_rewritten_history() {
    let opening = [user("Fix the overflow in the parser."), said("On it."), user(RC), said("Understood.")];
    let mut cur = curator();
    let _ = work(&mut cur, &opening, 20, "");
    // the host compacts the history into a summary that does not hold the fact
    let summary = vec![user("Summary: the parser overflow is fixed; the tests pass."), said("Understood.")];
    let (_, standing) = ask(&mut cur, summary, "Add an alias to my shell.");
    assert!(standing.iter().any(|l| l.contains(RC)), "{standing:?}");
}

#[test]
fn a_word_most_requests_hold_is_not_a_target() {
    let mut opening = vec![user("Fix the overflow in the parser."), said("On it.")];
    for k in 0..60 {
        opening.push(user(&format!("Check the shell output of unit {k}.")));
        opening.push(said("Done."));
    }
    opening.push(user("The rc of the build lives at /etc/skel-77/rc, not ~/.bashrc."));
    opening.push(said("Understood."));
    let mut cur = curator();
    let msgs = work(&mut cur, &opening, 250, "");
    let (_, standing) = ask(&mut cur, msgs.clone(), "Check the shell output of unit 61.");
    assert!(standing.is_empty(), "'shell' is in nearly every request: {standing:?}");
    let (_, standing) = ask(&mut cur, msgs, "Add a line to ~/.bashrc.");
    assert_eq!(standing.len(), 1, "{standing:?}");
}
