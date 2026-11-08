//! Messages as they enter the engine (engine::ingest).

use super::*;

#[test]
fn what_a_command_prints_of_a_file_comes_whole_and_a_process_folds_without_loss() {
    // lines of one form whose numbers count up: a file holds them, and a process loses none of them
    let rows = (0..60).map(|k| format!("movdqa xmm0, cs:xmmword_{}", 4340 + 10 * k)).collect::<Vec<_>>().join("\n");
    let mut msgs = vec![json!({"role": "user", "content": "What does the listing load?"})];
    msgs.extend(step(1, "cd material; sed -n 1,60p listing.txt", &rows));
    msgs.extend(step(2, "./gen.sh", &rows));
    // with a way back (the host offers `recall`) or without, the output of a process folds only what
    // comes back whole: every value is still there
    for offer in [true, false] {
        let mut e = Engine::new(params("nudge=0").unwrap());
        if offer {
            e.tools(&[RECALL_TOOL.to_string()]);
        }
        e.sync(&msgs);
        let view = e.view();
        assert_eq!(output(&view, 2), rows);
        let out = output(&view, 4);
        assert!(!out.contains("more lines with the same pattern"), "{out}");
        assert!((0..60).all(|k| out.contains(&(4340 + 10 * k).to_string())), "{out}");
    }
}

#[test]
fn a_line_the_host_adds_after_the_results_is_part_of_the_same_request() {
    // the model gets the results and the line as one message, so the answer closes the read
    let mut msgs = sessions("Done.", None);
    msgs.insert(3, json!({"role": "user", "content": "Give your answer now, please."}));
    let mut e = Engine::new(params("nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1").unwrap());
    e.sync(&msgs);
    let view = sent(&e.view());
    assert!(!view.contains(&word(0)[..]), "{view}");
    assert!(view.contains("are left out of this context: bash cat report.txt"), "{view}");
}

#[test]
fn a_rule_given_again_in_the_message_that_comes_with_a_rewrite_moves_past_what_withdrew_it() {
    let user = |t: &str| json!({"role": "user", "content": t});
    let reply = |t: &str| json!({"role": "assistant", "content": t});
    let texts = |e: &Engine| e.rules.list.iter().map(|r| r.text.clone()).collect::<Vec<_>>();
    let mut e = Engine::new(params("nudge=0").unwrap());
    let mut msgs = vec![user("Fix the login form."), reply("On it.")];
    msgs.extend([user("Never push to main."), reply("Understood.")]);
    msgs.extend([user("You may push to main from now on."), reply("Noted.")]);
    e.sync(&msgs);
    assert_eq!(texts(&e), ["Never push to main.", "You may push to main from now on."]);
    // the host compacts the history; the same request carries the person's new message
    let summary = "Summary: the person said never push to main, then allowed it.";
    e.sync(&[user(summary), reply("Continuing."), user("Never push to main.")]);
    assert_eq!(texts(&e), ["You may push to main from now on.", "Never push to main."]);
    // read anew, a rule before the last reply stays where it was
    let mut e = Engine::new(params("nudge=0").unwrap());
    e.sync(&[user("Never push to main."), reply("Ok."), user("You may push to main from now on.")]);
    e.sync(&[user("Never push to main."), reply("Ok."), user("Go on.")]);
    assert_eq!(texts(&e), ["Never push to main.", "You may push to main from now on."]);
}

#[test]
fn a_rewrite_with_no_reply_is_read_anew_but_its_newest_message() {
    let user = |t: &str| json!({"role": "user", "content": t});
    let reply = |t: &str| json!({"role": "assistant", "content": t});
    let rules = |e: &Engine| e.rules.list.iter().map(|r| (r.text.clone(), r.turn)).collect::<Vec<_>>();
    let mut msgs = vec![user("Fix the login form."), reply("On it.")];
    msgs.extend([user("Never push to main."), reply("Understood.")]);
    msgs.extend([user("You may push to main from now on."), reply("Noted.")]);
    let fresh = || {
        let mut e = Engine::new(params("nudge=0").unwrap());
        e.sync(&msgs);
        e
    };
    let before = rules(&fresh());
    // the host compacts the history into one summary that quotes the rules in another order
    let summary = "Summary of the session so far.\nYou may push to main from now on.\nNever push to main.";
    let mut e = fresh();
    e.sync(&[user(summary)]);
    assert_eq!(rules(&e), before);
    // the summary and a new message, no reply between them: only the new message is new
    let mut e = fresh();
    e.sync(&[user(summary), user("Go on.")]);
    assert_eq!(rules(&e), before);
    let mut e = fresh();
    e.sync(&[user(summary), user("Never push to main.")]);
    let texts: Vec<String> = rules(&e).into_iter().map(|r| r.0).collect();
    assert_eq!(texts, ["You may push to main from now on.", "Never push to main."]);
}
