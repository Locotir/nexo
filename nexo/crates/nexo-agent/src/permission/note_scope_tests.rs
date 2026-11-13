use super::*;
use serde_json::json;

fn policy(mode: PermissionMode) -> Policy {
    Policy { mode, ..Policy::default() }
}

#[test]
fn a_note_that_saves_a_rule_asks_like_a_file_write() {
    let d = tempfile::tempdir().unwrap();
    let (c, call) = (d.path(), |s: &str| json!({"text": "Rule: x", "scope": s}));
    assert_eq!(policy(PermissionMode::Ask).decide("note", ToolKind::Other, &call("session"), c), Decision::Allow);
    assert_eq!(policy(PermissionMode::Ask).decide("note", ToolKind::Other, &call("project"), c), Decision::Ask);
    assert_eq!(
        policy(PermissionMode::AcceptEdits).decide("note", ToolKind::Other, &call("project"), c),
        Decision::Allow
    );
    assert_eq!(policy(PermissionMode::Auto).decide("note", ToolKind::Other, &call("project"), c), Decision::Allow);
    assert!(matches!(
        policy(PermissionMode::ReadOnly).decide("note", ToolKind::Other, &call("project"), c),
        Decision::Deny(_)
    ));
    let denied = Policy { deny: vec!["note".into()], ..policy(PermissionMode::Auto) };
    assert!(matches!(denied.decide("note", ToolKind::Other, &call("project"), c), Decision::Deny(_)));
}

#[test]
fn a_workflow_rule_covers_saved_names_never_text_the_model_wrote() {
    let d = tempfile::tempdir().unwrap();
    let c = d.path();
    // the turn puts the subject of the file as it is now in a call that names a saved workflow
    let (saved, other, inline) = (
        json!({"name": "bugs", "pinned": "bugs@1a2b3c4d1a2b3c4d1a2b3c4d1a2b3c4d"}),
        json!({"name": "other", "pinned": "other@00000000000000000000000000000000"}),
        json!({"inline": "[[phase]]", "name": "", "pinned": "bugs@1a2b3c4d1a2b3c4d1a2b3c4d1a2b3c4d"}),
    );
    let unpinned = json!({"name": "bugs"});
    let decide = |p: &Policy, a: &Value| p.decide("workflow", ToolKind::Other, a, c);
    let with =
        |allow: &[&str]| Policy { allow: allow.iter().map(|r| r.to_string()).collect(), ..policy(PermissionMode::Ask) };
    for a in [&saved, &inline] {
        assert_eq!(decide(&policy(PermissionMode::Ask), a), Decision::Ask);
        assert_eq!(decide(&policy(PermissionMode::AcceptEdits), a), Decision::Ask);
        // like `task`, it runs where only reads run: it asks, and its sub-agents are judged call by call
        assert_eq!(decide(&policy(PermissionMode::ReadOnly), a), Decision::Ask);
        assert_eq!(decide(&policy(PermissionMode::Auto), a), Decision::Allow);
    }
    assert_eq!(decide(&with(&["workflow(bugs@1a2b3c4d1a2b3c4d1a2b3c4d1a2b3c4d)"]), &saved), Decision::Allow);
    assert_eq!(decide(&with(&["workflow(bugs@1a2b3c4d1a2b3c4d1a2b3c4d1a2b3c4d)"]), &other), Decision::Ask);
    // the file changed since the rule was made (another hash), or the rule holds the name alone: asks again
    assert_eq!(decide(&with(&["workflow(bugs@ffffffffffffffffffffffffffffffff)"]), &saved), Decision::Ask);
    assert_eq!(decide(&with(&["workflow(bugs)"]), &saved), Decision::Ask);
    // a name that does not load has no subject: only the whole-tool rules cover it
    assert_eq!(decide(&with(&["workflow(bugs@1a2b3c4d1a2b3c4d1a2b3c4d1a2b3c4d)"]), &unpinned), Decision::Ask);
    assert_eq!(decide(&with(&["workflow(b*)"]), &saved), Decision::Allow);
    assert_eq!(decide(&with(&["workflow"]), &saved), Decision::Allow);
    assert_eq!(decide(&with(&["workflow(*)"]), &saved), Decision::Allow);
    for rule in ["workflow", "workflow(*)", "workflow(bugs@1a2b3c4d1a2b3c4d1a2b3c4d1a2b3c4d)", "work*"] {
        assert_eq!(decide(&with(&[rule]), &inline), Decision::Ask, "{rule}");
    }
    // a deny rule covers both
    let denied = Policy { deny: vec!["workflow".into()], ..policy(PermissionMode::Auto) };
    assert!(matches!(decide(&denied, &saved), Decision::Deny(_)));
    assert!(matches!(decide(&denied, &inline), Decision::Deny(_)));
    // a deny rule by name holds whatever the file says, and also when it does not load
    let by_name = Policy { deny: vec!["workflow(bugs)".into()], ..policy(PermissionMode::Auto) };
    assert!(matches!(decide(&by_name, &saved), Decision::Deny(_)));
    assert!(matches!(decide(&by_name, &unpinned), Decision::Deny(_)));
    assert_eq!(decide(&by_name, &other), Decision::Allow);
    // what is offered to remember: the name and the hash, nothing for inline text or a name that does not load
    assert_eq!(
        suggested_rules("workflow", ToolKind::Other, &saved, c),
        vec!["workflow(bugs@1a2b3c4d1a2b3c4d1a2b3c4d1a2b3c4d)".to_string()]
    );
    assert!(suggested_rules("workflow", ToolKind::Other, &inline, c).is_empty());
    assert!(suggested_rules("workflow", ToolKind::Other, &unpinned, c).is_empty());
}

#[test]
fn a_workflow_rule_made_with_the_old_short_hash_no_longer_matches_and_the_question_is_asked_again() {
    let d = tempfile::tempdir().unwrap();
    let c = d.path();
    let subject = crate::workflow::file::subject("bugs", "abc");
    let hex = subject.split_once('@').unwrap().1;
    assert_eq!(hex.len(), 32);
    let call = json!({"name": "bugs", "pinned": subject});
    let with = |rule: &str| Policy { allow: vec![rule.to_string()], ..policy(PermissionMode::Ask) };
    let decide = |p: &Policy| p.decide("workflow", ToolKind::Other, &call, c);
    assert_eq!(decide(&with(&format!("workflow({subject})"))), Decision::Allow);
    // the first 8 hex, as rules were saved before the pin grew: it asks again
    assert_eq!(decide(&with(&format!("workflow(bugs@{})", &hex[..8]))), Decision::Ask);
    // and the rule it then offers is the long one
    assert_eq!(suggested_rules("workflow", ToolKind::Other, &call, c), vec![format!("workflow({subject})")]);
}
