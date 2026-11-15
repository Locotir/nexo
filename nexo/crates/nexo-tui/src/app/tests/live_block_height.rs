//! While a reply streams and the view follows the end, nothing above the live block may move down.

use super::*;

/// The row of the marker line in a frame, if it is on screen.
fn marker_row(rows: &[String]) -> Option<usize> {
    rows.iter().position(|r| r.contains("MARKER-ABOVE"))
}

/// Deltas of a reasoning stream: short lines, long lines that wrap, a line that grows and then gets its
/// newline, a blank line, a list item, and at the end the answer.
fn script() -> Vec<AgentEvent> {
    let mut v = Vec::new();
    let r = |t: &str| AgentEvent::ReasoningDelta { text: t.to_string() };
    for t in ["Okay", ",", " the", " user", " asks\n", "short\n", "x\n", "\n"] {
        v.push(r(t));
    }
    let long = "then I should weigh the first option against the second one carefully because the \
                tradeoffs are not obvious and the wrapped line keeps growing across several rows of the view";
    for w in long.split_inclusive(' ') {
        v.push(r(w));
    }
    v.push(r("\n"));
    v.push(r("- a list item that is also fairly long so that it wraps around the edge of the narrow view\n"));
    v.push(r("ok\n"));
    for w in long.split_inclusive(' ') {
        v.push(r(w));
    }
    v.push(r("\n\n"));
    v.push(r("tiny\n"));
    v.push(r("tiny two\n"));
    v.push(r("tiny three\n"));
    v.push(AgentEvent::TextDelta { text: "The answer".into() });
    v.push(AgentEvent::TextDelta { text: " starts here and goes on for a while,".into() });
    v.push(AgentEvent::TextDelta { text: " wrapping over more than one row of the view.\n\n".into() });
    v.push(AgentEvent::TextDelta { text: "Second paragraph.".into() });
    v
}

fn run(w: u16, h: u16, animations: bool) {
    let (mut a, _rx) = bare();
    a.opts.animations = animations;
    for i in 0..60 {
        a.push(Block::Notice { level: Level::Info, text: format!("filler {i}") });
    }
    a.push(Block::Notice { level: Level::Info, text: "MARKER-ABOVE".into() });
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    let mut prev: Option<usize> = None;
    let mut seen = 0;
    for ev in script() {
        a.handle_agent(ev);
        a.tick_frame(false);
        term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
        assert!(a.follow, "the view must keep following the end");
        let rows = crate::ui::rows_of(term.backend().buffer());
        let now = marker_row(&rows);
        if let (Some(p), Some(n)) = (prev, now) {
            assert!(
                n <= p,
                "{w}x{h} animations={animations}: marker moved down from row {p} to {n}:\n{}",
                rows.join("\n")
            );
            seen += 1;
        }
        prev = now;
    }
    assert!(seen > 10, "the marker was in sight for only {seen} frames");
}

#[test]
fn live_thinking_never_pushes_the_text_above_it_down() {
    for (w, h) in [(100, 30), (80, 24), (40, 20)] {
        run(w, h, false);
    }
}

#[test]
fn live_thinking_never_pushes_the_text_above_it_down_with_animations() {
    for (w, h) in [(100, 30), (80, 24)] {
        run(w, h, true);
    }
}

#[test]
fn tool_cards_groups_and_thinking_between_calls_never_push_the_text_above_down() {
    let mut evs = vec![AgentEvent::ReasoningDelta { text: "first I look at the file\nthen at the other one\n".into() }];
    for n in 0..3 {
        let id = format!("c{n}");
        evs.push(AgentEvent::ToolCall {
            id: id.clone(),
            name: "bash".into(),
            args: serde_json::json!({"command": "ls -la"}),
            summary: format!("ls {n}"),
        });
        evs.push(AgentEvent::ToolStart { id: id.clone() });
        for k in 0..6 {
            evs.push(AgentEvent::ToolProgress { id: id.clone(), line: format!("output line {k} of call {n}") });
        }
        evs.push(AgentEvent::ToolDone { id, output: ToolOutput::ok("done"), elapsed_ms: 5 });
        evs.push(AgentEvent::ReasoningDelta { text: "now what comes next, a short thought\n".into() });
    }
    evs.push(AgentEvent::TextDelta { text: "All done.".into() });
    for (w, h) in [(100, 30), (80, 24)] {
        let (mut a, _rx) = bare();
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        for i in 0..60 {
            a.push(Block::Notice { level: Level::Info, text: format!("filler {i}") });
        }
        a.push(Block::Notice { level: Level::Info, text: "MARKER-ABOVE".into() });
        a.handle_agent(AgentEvent::TurnStart { turn: 1 });
        let mut prev: Option<usize> = None;
        for ev in evs.clone() {
            a.handle_agent(ev);
            a.tick_frame(false);
            term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
            let rows = crate::ui::rows_of(term.backend().buffer());
            let now = marker_row(&rows);
            if let (Some(p), Some(n)) = (prev, now) {
                assert!(n <= p, "{w}x{h}: marker moved down from row {p} to {n}:\n{}", rows.join("\n"));
            }
            prev = now;
        }
    }
}
