//! The decision for each request (engine::decide).

use super::*;

/// A view with what a model cannot tell apart left out: call ids and the numbers in curator lines.
fn shape(view: &[Value]) -> String {
    let mut s = sent(view);
    for k in 0..40 {
        s = s.replace(&format!("call_{k}\""), "call\"");
    }
    s.lines()
        .map(|l| {
            if l.contains(MARK) {
                l.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<String>>()
        .join("\n")
}

#[test]
fn the_default_never_sends_a_view_it_sent_before_when_the_agent_repeats_a_step() {
    // at a fixed temperature a model sent the same view takes the same step again: the history must grow
    let mut msgs = vec![json!({"role": "user", "content": "Which blocks jump to the handler?"})];
    msgs.extend(step(0, "sed -n 1,40p listing.txt", &listing(1, 40)));
    let mut e = Engine::new(crate::params("").unwrap());
    let mut seen = FxHashSet::default();
    for k in 1..=12 {
        msgs.extend(step(k, "sed -n 1,40p listing.txt", &listing(1, 40)));
        e.sync(&msgs);
        assert!(seen.insert(shape(&e.view())), "step {k} sent a view sent before");
    }
}
