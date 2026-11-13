use super::*;
use serde_json::json;

fn policy(mode: PermissionMode, allow: &[&str], deny: &[&str]) -> Policy {
    Policy {
        mode,
        allow: allow.iter().map(|s| s.to_string()).collect(),
        deny: deny.iter().map(|s| s.to_string()).collect(),
    }
}

fn at(p: &Policy, args: Value, sandbox_on: bool) -> Decision {
    p.decide_boxed("bash", ToolKind::Exec, &args, Path::new("/work/proj"), sandbox_on)
}

#[test]
fn running_outside_the_sandbox_is_never_covered_by_a_rule_or_a_mode() {
    let flagged = json!({"command": "cargo x", "unsandboxed": true});
    let plain = json!({"command": "cargo x"});
    let ruled = policy(PermissionMode::Ask, &["bash(cargo *)"], &[]);
    assert_eq!(at(&ruled, plain.clone(), true), Decision::Allow);
    assert_eq!(at(&ruled, flagged.clone(), true), Decision::Ask);
    // the text forms the tool reads as true count too
    assert_eq!(at(&ruled, json!({"command": "cargo x", "unsandboxed": "yes"}), true), Decision::Ask);
    // an answer remembered this session is a rule like any other
    let mut asked = policy(PermissionMode::Ask, &[], &[]);
    asked.add_allow("bash(cargo *)");
    assert_eq!(at(&asked, flagged.clone(), true), Decision::Ask);
    // sandbox off: the flag changes nothing, so it adds no question
    assert_eq!(at(&ruled, flagged.clone(), false), Decision::Allow);
    // a deny rule and the never-run check hold as before
    let denied = policy(PermissionMode::Ask, &["bash(cargo *)"], &["bash(cargo x*)"]);
    assert!(matches!(at(&denied, flagged.clone(), true), Decision::Deny(_)));
    let wipe = json!({"command": "rm -rf /", "unsandboxed": true});
    assert!(matches!(at(&ruled, wipe, true), Decision::Deny(_)));
    // read-only still denies; yolo keeps asking nothing
    assert!(matches!(at(&policy(PermissionMode::ReadOnly, &[], &[]), flagged.clone(), true), Decision::Deny(_)));
    assert_eq!(at(&policy(PermissionMode::Auto, &[], &[]), flagged, true), Decision::Allow);
}

#[test]
fn writing_into_a_debugged_program_or_attaching_asks_even_with_a_rule() {
    let ruled = policy(PermissionMode::Ask, &["bash"], &[]);
    for c in
        ["nexo dbg attach 12", "nexo dbg write 0x10 90", "nexo dbg setreg rax 1", "nexo dbg do \"cont; write 1 2\""]
    {
        assert_eq!(at(&ruled, json!({"command": c}), true), Decision::Ask, "{c}");
    }
    assert_eq!(at(&ruled, json!({"command": "nexo dbg regs"}), true), Decision::Allow);
    let dbg = |c: &str, k| ruled.decide_boxed("debug", k, &json!({"command": c}), Path::new("/work/proj"), true);
    for c in ["attach 12", "write 0x10 90", "setreg rax 1", "do \"cont; write 1 2\""] {
        assert_eq!(dbg(c, ToolKind::Exec), Decision::Ask, "{c}");
    }
    assert_eq!(dbg("regs", ToolKind::Read), Decision::Allow);
    assert_eq!(
        at(&policy(PermissionMode::Auto, &[], &[]), json!({"command": "nexo dbg attach 12"}), true),
        Decision::Allow
    );
}

#[test]
fn a_detached_command_is_never_covered_by_a_rule_or_a_remembered_answer() {
    let detached = json!({"command": "cargo x", "background": true, "detach": true});
    let ruled = policy(PermissionMode::Ask, &["bash(cargo *)", "bash"], &[]);
    for sandbox_on in [true, false] {
        assert_eq!(at(&ruled, detached.clone(), sandbox_on), Decision::Ask);
        assert_eq!(at(&ruled, json!({"command": "cargo x", "background": true}), sandbox_on), Decision::Allow);
    }
    let mut asked = policy(PermissionMode::Ask, &[], &[]);
    asked.add_allow("bash(cargo *)");
    assert_eq!(at(&asked, detached.clone(), true), Decision::Ask);
    let denied = policy(PermissionMode::Ask, &["bash(cargo *)"], &["bash(cargo *)"]);
    assert!(matches!(at(&denied, detached.clone(), true), Decision::Deny(_)));
    assert_eq!(at(&policy(PermissionMode::Auto, &[], &[]), detached, true), Decision::Allow);
}

#[test]
fn the_request_says_what_the_flags_do() {
    let both = json!({"command": "cargo x", "unsandboxed": true, "background": true, "detach": true});
    let words = flag_words(ToolKind::Exec, &both, true);
    let text = with_flags("bash: cargo x".into(), "bash", &words);
    assert_eq!(text, "bash (outside the sandbox, detached: keeps running after nexo exits): cargo x");
    assert!(text.chars().count() < 80);
    // sandbox off: nothing to say about it; a plain call is unchanged
    assert_eq!(flag_words(ToolKind::Exec, &json!({"command": "a", "unsandboxed": true}), false), Vec::<&str>::new());
    assert_eq!(with_flags("bash: ls".into(), "bash", &[]), "bash: ls");
    assert_eq!(
        with_flags(
            "bash: ls".into(),
            "bash",
            &flag_words(ToolKind::Exec, &json!({"command": "ls", "unsandboxed": true}), true)
        ),
        "bash (outside the sandbox): ls"
    );
}
