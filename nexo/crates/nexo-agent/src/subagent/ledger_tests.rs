//! The ledger of what a sub-agent did is read from its tool calls and their results.

use super::*;
use ledger::Effects;

/// One round: the calls the sub-agent made and what each answered (`is_error` last).
fn round(n: usize, calls: &[(&str, Value, &str, bool)]) -> [Message; 2] {
    let mut asked = Vec::new();
    let mut back = Vec::new();
    for (i, (name, args, out, is_error)) in calls.iter().enumerate() {
        let id = format!("c{n}-{i}");
        asked.push(Part::tool_call(&id, *name, args.clone()));
        back.push(Part::ToolResult {
            call_id: id,
            name: Some(name.to_string()),
            content: vec![Part::text(*out)],
            is_error: *is_error,
        });
    }
    [Message::new(Role::Assistant, asked), Message::tool_results(back)]
}

fn log(rounds: &[&[(&str, Value, &str, bool)]]) -> Vec<Message> {
    let mut m = vec![Message::user("do it")];
    for r in rounds {
        m.extend(round(m.len(), r));
    }
    m
}

fn edited(path: &str, added: usize, removed: usize) -> String {
    format!("edited {path}: +{added} \u{2212}{removed} lines, verified\n@@ -1 +1 @@\n-a\n+b\n")
}

#[test]
fn two_edits_and_two_commands_show_with_their_counts_and_exit_codes() {
    let a = edited("src/a.rs", 3, 1);
    let b = "created src/b.rs: file now has 12 lines (300 bytes), verified";
    let m = log(&[&[
        ("edit", json!({"path": "src/a.rs"}), &a, false),
        ("write", json!({"path": "src/b.rs"}), b, false),
        ("read", json!({"path": "src/c.rs"}), "text", false),
        ("bash", json!({"command": "cargo test -p x"}), "ok", false),
        ("bash", json!({"command": "cargo clippy"}), "warning\n\n[exit code 1]", false),
    ]]);
    let fx = Effects::of(&m);
    assert_eq!(fx.calls, 5);
    assert_eq!(
        fx.lines(),
        vec![
            "wrote src/a.rs: +3 -1",
            "wrote src/b.rs: new, 12 lines",
            "ran `cargo test -p x`: exit 0",
            "ran `cargo clippy`: exit 1",
        ]
    );
    assert!(fx.block().starts_with("[tool log] 5 tool calls:\n- wrote src/a.rs: +3 -1"));
}

#[test]
fn edits_of_one_file_add_up_and_a_failed_edit_of_several_files_keeps_the_ones_it_wrote() {
    let twice = format!("{}{}", edited("a.rs", 1, 0), edited("b.rs", 2, 2));
    let failed =
        format!("{}edit: files[2]: `old_string` not found\napplied before the failure: a.rs", edited("a.rs", 4, 1));
    let m = log(&[
        &[("edit", json!({"path": "a.rs"}), &edited("a.rs (2 changes)", 5, 5), false)],
        &[("edit", json!({"files": [{"path": "a.rs"}, {"path": "b.rs"}]}), &twice, false)],
        &[("edit", json!({"files": [{"path": "a.rs"}, {"path": "c.rs"}]}), &failed, true)],
    ]);
    assert_eq!(Effects::of(&m).lines(), vec!["wrote a.rs: +10 -6", "wrote b.rs: +2 -2"]);
}

#[test]
fn a_write_whose_result_has_no_counts_is_still_listed_and_a_refused_one_is_not() {
    let m = log(&[&[
        ("write", json!({"path": "x.txt"}), "written", false),
        ("write", json!({"path": "y.txt"}), "write: y.txt is a directory", true),
    ]]);
    assert_eq!(Effects::of(&m).lines(), vec!["wrote x.txt"]);
}

#[test]
fn a_command_that_timed_out_was_stopped_or_has_no_result_says_so() {
    let m = log(&[&[
        ("bash", json!({"command": "sleep 99"}), "\n\n[killed: timed out after 120 s]", true),
        ("bash", json!({"command": "sleep 98"}), "x\n\n[cancelled]", true),
        ("bash", json!({"command": "make\nmake install"}), "Started background job 4: `make`.", false),
    ]]);
    let mut m = m;
    // a call the sub-agent made whose result never came
    m.push(Message::new(Role::Assistant, vec![Part::tool_call("z", "bash", json!({"command": "ls"}))]));
    assert_eq!(
        Effects::of(&m).lines(),
        vec![
            "ran `sleep 99`: timed out",
            "ran `sleep 98`: stopped",
            "ran `make…`: started in the background",
            "ran `ls`: no result",
        ]
    );
}

#[test]
fn a_write_another_sub_agent_held_back_is_listed_with_its_path_and_holder() {
    let refusal = ledger::claim_refusal("src/lib.rs", "refactor the parser");
    let m = log(&[&[("edit", json!({"path": "src/lib.rs"}), &refusal, true)]]);
    assert_eq!(Effects::of(&m).lines(), vec!["refused src/lib.rs: held by refactor the parser"]);
}

#[test]
fn a_long_run_lists_the_first_ones_and_counts_the_rest() {
    let mut calls: Vec<(String, Value, String, bool)> = Vec::new();
    for i in 0..12 {
        calls.push(("edit".into(), json!({"path": format!("f{i}.rs")}), edited(&format!("f{i}.rs"), 1, 0), false));
        calls.push(("bash".into(), json!({"command": format!("cmd {i}")}), "ok".into(), false));
    }
    let borrowed: Vec<(&str, Value, &str, bool)> =
        calls.iter().map(|(n, a, o, e)| (n.as_str(), a.clone(), o.as_str(), *e)).collect();
    let fx = Effects::of(&log(&[&borrowed]));
    let lines = fx.lines();
    assert!(lines.len() <= 25, "{lines:?}");
    assert!(lines.contains(&"and 4 more files".to_string()) && lines.contains(&"and 4 more commands".to_string()));
    assert_eq!(fx.calls, 24);
}

#[test]
fn a_stopped_run_shows_what_it_did_before_it_was_stopped() {
    let a = edited("src/a.rs", 2, 0);
    let m = log(&[&[
        ("edit", json!({"path": "src/a.rs"}), &a, false),
        ("bash", json!({"command": "cargo test"}), "boom\n\n[exit code 101]", false),
    ]]);
    let out = stopped(&m);
    assert!(out.starts_with("stopped. Before that it made 2 calls;"), "{out}");
    assert!(out.contains("- wrote src/a.rs: +2 -0\n- ran `cargo test`: exit 101\n"), "{out}");
}

#[test]
fn a_run_with_no_calls_has_no_ledger() {
    let m = vec![Message::user("hello")];
    assert!(Effects::of(&m).block().is_empty());
    assert!(progress(&m, 0, 4).is_empty());
}

#[test]
fn a_named_signal_is_still_a_kill() {
    let m = log(&[&[
        ("bash", json!({"command": "run a"}), "x\n\n[killed by a signal: 9 SIGKILL]", true),
        ("bash", json!({"command": "run b"}), "x\n\n[killed by a signal]", true),
        ("bash", json!({"command": "run c"}), "x\n\n[killed by a signal: 64]", true),
    ]]);
    let lines = Effects::of(&m).lines();
    assert!(lines.contains(&"ran `run a`: killed by a signal: 9 SIGKILL".to_string()), "{lines:?}");
    assert!(lines.contains(&"ran `run b`: killed by a signal".to_string()), "{lines:?}");
    assert!(lines.contains(&"ran `run c`: killed by a signal: 64".to_string()), "{lines:?}");
}
