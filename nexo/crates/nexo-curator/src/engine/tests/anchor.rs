//! The anchor: ledger, rules and the notes it holds (engine::anchor).

use super::*;

#[test]
fn only_a_read_of_a_path_weighs_as_on_disk_in_the_ledger() {
    let mut msgs = vec![json!({"role": "user", "content": "Map where each module of the realm is configured."})];
    for k in 0..40 {
        let out = format!("the module_{k} service reads its limits from the shared table and retries them");
        // a file read, and a read of a thread (a tool with no path): only the file is one read away
        let input =
            if k % 2 == 0 { json!({"path": format!("config/{}.toml", word(k))}) } else { json!({"thread": word(k)}) };
        let name = if k % 2 == 0 { "read_file" } else { "read_thread" };
        msgs.extend(tool(k + 1, name, input, &out, false));
        msgs.push(json!({"role": "user", "content": "Go on."}));
    }
    let mut e = Engine::new(params("ledger=6000;target=3000;eoq=0;nudge=0").unwrap());
    e.sync(&msgs);
    e.view();
    assert!(e.start > 0 && !e.pool.is_empty());
    let weight = |k: usize| e.pool.iter().find(|x| x.1.contains(&format!("module_{k} "))).map(|x| x.3);
    let (file, thread) = ((0..40).step_by(2).find_map(weight), (1..40).step_by(2).find_map(weight));
    assert_eq!(file, Some(ON_DISK));
    assert_eq!(thread, Some(1.0));
}

#[test]
fn ledger_text_fits_its_budget_with_header() {
    let mut msgs = vec![json!({"role": "user", "content": "Map where each module of the realm is configured."})];
    for k in 0..40 {
        let out: String = (0..6)
            .map(|j| format!("{} = {} in config/{}.toml\n", word(k * 10 + j), word(k * 10 + j + 900), word(k)))
            .collect();
        msgs.extend(step(k + 1, &format!("cat config/{}.toml", word(k)), &out));
        msgs.push(
            json!({"role": "assistant", "content": format!("{} is set in config/{}.toml.", word(k * 10), word(k))}),
        );
        msgs.push(json!({"role": "user", "content": "Go on."}));
    }
    let mut e = Engine::new(params("ledger=6000;target=3000;eoq=0;nudge=0").unwrap());
    e.sync(&msgs);
    e.view();
    assert!(e.start > 0 && !e.pool.is_empty());
    let mut some = false;
    for budget in [40, 60, 100, 200, 400] {
        let text = e.ledger_text(budget);
        some |= !text.is_empty();
        assert!(est_str(&text) <= budget, "{budget}: {} tokens\n{text}", est_str(&text));
    }
    assert!(some);
}

#[test]
fn the_anchor_takes_a_rule_the_view_lost_at_once_and_lets_go_of_one_it_holds_again_with_a_rewrite() {
    let mut e = Engine::new(params("").unwrap());
    e.sync(&[json!({"role": "user", "content": "Look at the logs."})]);
    // a rule whose message is not in the view
    e.rules.add(40, "Never delete the logs.".into(), false);
    let v = e.map_ver;
    assert!(e.refresh_rules(false));
    assert!(e.map_ver > v && e.rules_shown.contains("Never delete the logs."), "{}", e.rules_shown);
    // its message is in the view again: the anchor, the first message, stays as it was sent
    e.rules.list[0].msg = Some(0);
    let v = e.map_ver;
    assert!(!e.refresh_rules(false));
    assert_eq!(e.map_ver, v);
    assert!(e.rules_shown.contains("Never delete the logs."));
    // the view is rewritten from its start anyway: the line goes with it
    assert!(e.refresh_rules(true));
    assert!(e.rules_shown.is_empty());
}

#[test]
fn rules_come_from_what_the_person_wrote_and_a_withdrawal_follows_the_rule_it_changes() {
    let rules = |e: &Engine| e.rules.list.iter().map(|r| r.text.clone()).collect::<Vec<_>>();
    let mut e = Engine::new(params("").unwrap());
    let first = vec![
        json!({"role": "user", "content": [
                {"type": "text", "text": "<system-reminder>\nNever edit the lockfile.\nYou must reply in French.\n</system-reminder>"},
                {"type": "text", "text": "Never use unwrap.\nERROR: not allowed to write /etc/x"}]}),
        json!({"role": "assistant", "content": [{"type": "text", "text": "Understood."}]}),
        json!({"role": "user", "content": "you can use unwrap now"}),
    ];
    e.sync(&first);
    assert_eq!(rules(&e), ["Never use unwrap.", "you can use unwrap now"]);
    // a compaction: the later word stays after the rule it changes
    e.sync(&[json!({"role": "user", "content": "Summary: the parser is half done."})]);
    assert_eq!(rules(&e), ["Never use unwrap.", "you can use unwrap now"]);
    let block = e.rules.text(|_| true);
    assert!(block.find("Never use unwrap").unwrap() < block.find("you can use unwrap now").unwrap(), "{block}");
}

#[test]
fn a_fact_the_agent_saved_with_note_is_shown_as_its_own_note_and_not_as_a_request_to_answer() {
    // the system text asks the agent to save a lasting fact it finds as a `Rule:` line; that line is the agent's,
    // and the block must not hand it back as something the person asked for in the current request
    let mut e = Engine::new(params("").unwrap());
    e.sync(&[
        json!({"role": "user", "content": "Why does the installer fail on a second run?"}),
        json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": "n1", "name": "note",
             "input": {"text": "Rule: the install folder is always %LOCALAPPDATA%\\App; a second run only rewrites the config", "scope": "project"}}]}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "n1", "content": "noted 1 line"}]}),
        json!({"role": "user", "content": "Never delete the logs."}),
    ]);
    let block = e.rules.text(|_| true);
    let own = block.lines().find(|l| l.contains("the install folder is always")).unwrap_or_else(|| panic!("{block}"));
    assert!(own.starts_with(&format!("turn 1, {}: the install folder", crate::rules::OWN_NOTE)), "{block}");
    assert!(own.contains("not a request") && own.contains("no answer"), "{block}");
    // what the person wrote stays as it was, word for word
    assert!(block.lines().any(|l| l == "turn 2: Never delete the logs."), "{block}");
    // one header line, then one line per rule: the parts that read the block line by line rely on it
    assert_eq!(block.lines().count(), 3, "{block}");
    assert!(
        block.lines().skip(1).all(|l| l.split_once(": ").is_some_and(|(head, _)| head.starts_with("turn "))),
        "{block}"
    );
}
