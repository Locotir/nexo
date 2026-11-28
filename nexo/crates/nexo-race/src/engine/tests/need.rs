//! The names-kept rule (engine::need).

use super::*;

/// A build log in parts: a head, a part whose names the person said already, a part with a name of its own,
/// and the verdict. The made-up words are not names; the code-shaped words are.
fn log_parts() -> Vec<String> {
    let plain = |a: usize, n: usize| -> Vec<String> {
        (a..a + n).map(|k| format!("compiling the {} unit of the {} tree, done", word(k), word(k + 50))).collect()
    };
    let mut head = vec!["make: entering the build of the service tree".to_string()];
    head.extend(plain(0, 4));
    let mut shared = plain(10, 6);
    shared.insert(3, "linked stage_alpha1 against lib_beta2 with no change".to_string());
    let mut own = plain(20, 6);
    own.insert(3, "wrote the archive obj_kalomi to the cache".to_string());
    let tail = ["build finished", "one notice in the tree", "see the lines above", "the verdict is in the last line"];
    [head, shared, own, tail.map(str::to_string).to_vec()].iter().map(|p| p.join("\n")).collect()
}

const ASK: &str = "Build the service and fix what stops it (stage_alpha1 and lib_beta2 are known good).";

/// The history up to `n` messages: the request, the build (`error`: it failed), the agent's text about it, then
/// two steps of its own, the first running `second`.
fn history(n: usize, error: bool, second: &str) -> Vec<Value> {
    let log = log_parts().join("\n\n");
    let mut msgs = vec![json!({"role": "user", "content": ASK})];
    msgs.extend(tool(1, "bash", json!({"command": "make service"}), &log, error));
    msgs.push(
        json!({"role": "assistant", "content": [{"type": "text", "text": "The build ran to its end, no stop shows."}]}),
    );
    msgs.extend(step(2, second, "40 plan.mk"));
    msgs.extend(step(3, "ls build", "out.o"));
    msgs.truncate(n);
    msgs
}

fn run(spec: &str, n: usize, error: bool, second: &str) -> (Engine, Vec<Value>) {
    let mut e = Engine::new(params(spec).unwrap());
    e.sync(&history(n, error, second));
    let view = e.view();
    (e, view)
}

#[test]
fn with_need_off_the_view_is_the_default_one() {
    let (e, off) = run("need=0", 8, false, "wc -l plan.mk");
    let (_, default) = run("", 8, false, "wc -l plan.mk");
    assert_eq!(sent(&off), sent(&default));
    assert_eq!(output(&off, 2), log_parts().join("\n\n"));
    assert!(e.stats.keys().all(|k| !k.contains("need")), "{:?}", e.stats);
}

#[test]
fn a_part_whose_names_live_elsewhere_leaves_after_the_reply_and_two_idle_replies() {
    let parts = log_parts();
    let has = |view: &[Value], k: usize| output(view, 2).contains(parts[k].as_str());
    // the agent has not written about the build yet: nothing leaves
    let (_, view) = run("need=2", 3, false, "wc -l plan.mk");
    assert!(has(&view, 1), "{}", output(&view, 2));
    // the text and a call after it: the part leaves, and no name of it with it (both are in the request)
    let (e, view) = run("need=2", 8, false, "wc -l plan.mk");
    let text = output(&view, 2);
    assert!(!has(&view, 1) && !text.contains("lib_beta2"), "{text}");
    assert!([0, 3].iter().all(|&k| has(&view, k)), "{text}");
    assert!(e.stats.get("need_parts").is_some_and(|&p| p >= 1), "{:?}", e.stats);
    assert!(e.stats.get("miss_need").is_some_and(|&t| t > 0), "{:?}", e.stats);
}

#[test]
fn a_part_with_a_name_of_its_own_keeps_the_first_line_that_holds_it() {
    let parts = log_parts();
    let (e, view) = run("need=2", 8, false, "wc -l plan.mk");
    let text = output(&view, 2);
    let gone = format!("{MARK} … lines omitted …");
    assert!(!text.contains(parts[2].as_str()), "{text}");
    assert!(text.contains(&format!("{gone}\nwrote the archive obj_kalomi to the cache\n{gone}")), "{text}");
    assert!(e.stats.get("need_names").is_some_and(|&n| n >= 1), "{:?}", e.stats);
}

#[test]
fn a_name_said_in_a_recent_reply_keeps_its_part() {
    let parts = log_parts();
    let (_, view) = run("need=2", 8, false, "grep -n obj_kalomi build.out");
    let text = output(&view, 2);
    assert!(text.contains(parts[2].as_str()), "{text}");
    assert!(!text.contains(parts[1].as_str()), "{text}");
}

#[test]
fn an_error_output_never_leaves() {
    let (e, view) = run("need=2", 8, true, "wc -l plan.mk");
    assert_eq!(output(&view, 2), log_parts().join("\n\n"));
    assert_eq!(e.stats.get("need_parts"), None);
}

#[test]
fn the_count_of_places_of_a_name_goes_back_down_when_a_result_leaves() {
    let places = |e: &Engine, name: &str| e.need.places.get(&e.int.find(name).expect(name)).copied().unwrap_or(0);
    let mut e = Engine::new(params("need=2").unwrap());
    e.sync(&history(4, false, "wc -l plan.mk"));
    let _ = e.view();
    // the request and the build hold each of its names: two places
    assert_eq!(places(&e, "lib_beta2"), 2);
    assert_eq!(places(&e, "obj_kalomi"), 1);
    e.sync(&history(8, false, "wc -l plan.mk"));
    let _ = e.view();
    // the part left: the request alone holds the name now; the part with a name of its own left it on a line
    assert_eq!(places(&e, "lib_beta2"), 1);
    assert_eq!(places(&e, "obj_kalomi"), 1);
}
