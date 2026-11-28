//! The frontier trim (engine::frontier).

use super::*;

/// The parts of a build log: a head, progress lines that name nothing, a part with one name of its own, the
/// part the agent goes on from, a table that is all names, and the verdict.
fn build_log() -> Vec<String> {
    let plain = |a: usize, n: usize| -> Vec<String> {
        (a..a + n).map(|k| format!("compiling the {} unit of the {} tree, done", word(k), word(k + 50))).collect()
    };
    let mut head = vec!["make: entering the build of the service tree".to_string()];
    head.extend(plain(0, 4));
    let mut own = plain(20, 9);
    own.insert(4, "wrote the archive obj_kalomi to the cache".to_string());
    let mut next = plain(30, 5);
    next.push("the linker waits at link_step9 of the plan".to_string());
    let table: Vec<String> =
        (0..5).map(|k| format!("sym_{} sym_{} sym_{}", word(k + 300), word(k + 400), word(k + 600))).collect();
    let tail = ["build finished", "one notice in the tree", "see the lines above", "the verdict is in the last line"];
    let parts = [head, plain(10, 8), own, next, table, tail.map(str::to_string).to_vec()];
    parts.iter().map(|p| p.join("\n")).collect()
}

/// The engine and its view on the request after the agent replied to the build log, printed by `command`,
/// with a call that names `link_step9`.
fn after_the_reply(spec: &str, command: &str) -> (Engine, Vec<Value>) {
    let log = build_log().join("\n\n");
    let mut e = Engine::new(params(spec).unwrap());
    let mut msgs = vec![json!({"role": "user", "content": "Build the service and fix what stops it."})];
    msgs.extend(step(1, command, &log));
    e.sync(&msgs);
    assert_eq!(output(&e.view(), 2), log, "the output just asked for goes whole");
    msgs.extend(step(2, "grep -n link_step9 plan.mk", "14:link_step9: final.o"));
    e.sync(&msgs);
    let view = e.view();
    (e, view)
}

#[test]
fn what_the_reply_did_not_take_up_of_a_process_output_leaves_it_on_the_next_request_and_no_name_is_lost() {
    let parts = build_log();
    let log = parts.join("\n\n");
    let has = |view: &[Value], k: usize| output(view, 2).contains(parts[k].as_str());
    // off, the output stays as it was sent
    let (e, view) = after_the_reply("trim=0", "make service");
    assert_eq!(output(&view, 2), log);
    assert_eq!(e.stats.get("miss_trim"), None);
    // 1: the progress lines name nothing and leave; the part with a name of its own stays
    let (e, view) = after_the_reply("trim=1", "make service");
    assert!(!has(&view, 1) && output(&view, 2).contains(&format!("{MARK} … lines omitted …")), "{view:?}");
    assert!([0, 2, 3, 4, 5].iter().all(|&k| has(&view, k)), "{view:?}");
    assert_eq!(e.stats.get("trim_parts"), Some(&1), "{:?}", e.stats);
    // the cache misses from that message on, and the count says so
    assert!(e.stats.get("miss_trim").is_some_and(|&t| t > 0) && e.stats.get("miss") >= e.stats.get("miss_trim"));
    // 2: that part leaves too, but for the line that holds its name; what the reply named, the table of
    // names, the head and the verdict stay
    let (e, view) = after_the_reply("trim=2", "make service");
    let text = output(&view, 2);
    assert!(!has(&view, 1) && !has(&view, 2), "{text}");
    let gone = format!("{MARK} … lines omitted …");
    assert!(text.contains(&format!("{gone}\nwrote the archive obj_kalomi to the cache\n{gone}")), "{text}");
    assert_eq!(text.matches(gone.as_str()).count(), 2, "{text}");
    assert!([0, 3, 4, 5].iter().all(|&k| has(&view, k)), "{text}");
    assert_eq!((e.stats.get("trim_parts"), e.stats.get("trim_names")), (Some(&2), Some(&1)), "{:?}", e.stats);
    let kept = crate::distinctive_words(&text);
    let lost: Vec<String> = crate::distinctive_words(&log).into_iter().filter(|w| !kept.contains(w)).collect();
    assert!(lost.is_empty(), "{lost:?}");
    // the request after it finds the message as the trim left it: nothing else changes
    let mut more = e.tr.msgs.clone();
    more.extend(step(3, "wc -l plan.mk", "40 plan.mk"));
    let mut e = e;
    e.sync(&more);
    assert_eq!(output(&e.view(), 2), text);
    // the same lines printed from a file are what the agent reads: only 3 takes from them
    let (_, view) = after_the_reply("trim=2", "cat nightly.out");
    assert_eq!(output(&view, 2), log);
    let (_, view) = after_the_reply("trim=3", "cat nightly.out");
    assert!(!has(&view, 1) && !has(&view, 2) && has(&view, 3), "{view:?}");
    // where the rewrite costs more than the rent it saves, nothing leaves: a cache that reads for next to
    // nothing
    let (e, view) = after_the_reply("trim=2;ahead=0.25;ratio=0.001", "make service");
    assert_eq!(output(&view, 2), log, "{:?}", e.stats);
    let (_, view) = after_the_reply("trim=2;ahead=0.25;ratio=0.1", "make service");
    assert!(!has(&view, 1) && !has(&view, 2), "{view:?}");
}
