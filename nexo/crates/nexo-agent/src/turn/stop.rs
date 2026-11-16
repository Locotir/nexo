//! The person stopped a request (Esc, or Enter on a queued one): the session records when, and what was running
//! then, and the next request carries it as one line before the person's words. Without it the model saw its own
//! last call, its result and then a new message, nothing saying the request above had been cut short, and a line the
//! person had typed before the stop read as part of what they wanted now.

use super::runtime::Runtime;
use super::*;

/// The most calls one stop line names.
const NAMED: usize = 3;

/// Whether a tool result says its call was cut by the person.
fn cut(content: &[Part]) -> bool {
    content.iter().any(|p| p.as_text().is_some_and(|t| t.contains("interrupted by the person")))
}

/// The calls in `names`, at most [`NAMED`] of them.
fn named(names: &[String]) -> String {
    let shown = names[..names.len().min(NAMED)].join(", ");
    match names.len().saturating_sub(NAMED) {
        0 => shown,
        n => format!("{shown} and {n} more"),
    }
}

/// What was running when the person stopped the request whose messages are `turn` (its request first), and how the
/// request was left: `while you waited for your reply after read README.md; it got no answer`.
pub(super) fn what_ran(turn: &[Message], tools: &ToolSet) -> String {
    let answered = turn.iter().any(|m| m.role == Role::Assistant && !m.text().trim().is_empty());
    let ended = if answered { "the request ended there" } else { "it got no answer" };
    let Some(last) = turn.last().filter(|_| turn.len() > 1) else {
        return format!("while you waited for your reply; {ended}");
    };
    if last.role == Role::Assistant {
        return "while your reply above was being written; it was cut there".to_string();
    }
    // the calls the results answer, by the id each result names
    let calls: Vec<(&str, &str, &Value)> =
        turn.iter().rev().find(|m| m.has_tool_calls()).map_or(Vec::new(), |m| m.tool_calls().collect());
    let summary =
        |id: &str| calls.iter().find(|(c, _, _)| *c == id).map(|(_, name, args)| summary_of(tools, name, args));
    let mut ran = Vec::new();
    let mut before = Vec::new();
    let mut done = Vec::new();
    for p in &last.content {
        let Part::ToolResult { call_id, content, .. } = p else { continue };
        let Some(s) = summary(call_id) else { continue };
        if !cut(content) {
            done.push(s);
        } else if content.iter().any(|c| c.as_text().is_some_and(|t| t.starts_with("The call did not run"))) {
            before.push(s);
        } else {
            ran.push(s);
        }
    }
    if !ran.is_empty() {
        format!("while {} ran; the request ended there", named(&ran))
    } else if !before.is_empty() {
        format!("before {} ran; the request ended there", named(&before))
    } else if !done.is_empty() {
        format!("while you waited for your reply after {}; {ended}", named(&done))
    } else {
        format!("while you waited for your reply; {ended}")
    }
}

/// The turn just ended because the person stopped it: when its request is in the history, the session records the
/// line the next request carries, and the requests queued while it ran are marked as typed before the stop.
pub(super) async fn record(rt: &mut Runtime, reason: TurnEnd, run_queue: bool) {
    let at = rt.stopped.take();
    let mut st = rt.state.lock().await;
    let asked = std::mem::take(&mut st.request_in);
    let (Some(at), TurnEnd::Interrupted, true) = (at, reason, asked) else { return };
    let how = if run_queue { "to run the queued request at once" } else { "(Esc)" };
    let what = what_ran(&st.history[st.turn_start.min(st.history.len())..], &st.tools);
    let line = nexo_core::agent::stop_line(&nexo_core::clock::local_hms(at), how, &what);
    if let Err(e) = st.session.record_stop(&line) {
        tracing::debug!("the stop was not saved: {e}");
    }
    for item in st.inbox.iter_mut().filter(|i| i.during) {
        item.stopped = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str, path: &str) -> Message {
        Message::new(Role::Assistant, vec![Part::tool_call(id, "read", json!({"path": path}))])
    }

    #[test]
    fn a_stop_before_any_reply_says_the_request_got_no_answer() {
        let turn = [Message::user("y documenta cada función")];
        assert_eq!(what_ran(&turn, &ToolSet::default()), "while you waited for your reply; it got no answer");
    }

    #[test]
    fn a_stop_after_a_call_returned_names_the_call_the_reply_was_awaited_after() {
        let turn = [
            Message::user("look at the readme"),
            call("c1", "README.md"),
            Message::tool_results(vec![Part::tool_result("c1", "# nexo", false)]),
        ];
        let what = what_ran(&turn, &ToolSet::default());
        assert!(what.starts_with("while you waited for your reply after read "), "{what}");
        assert!(what.contains("README.md") && what.ends_with("; it got no answer"), "{what}");
    }

    #[test]
    fn a_stop_while_calls_ran_names_them_and_says_the_request_ended_there() {
        let turn = [
            Message::user("run the tests"),
            Message::assistant("Running them."),
            call("c1", "a.rs"),
            Message::tool_results(vec![Part::tool_result("c1", "interrupted by the person", true)]),
        ];
        let what = what_ran(&turn, &ToolSet::default());
        assert!(what.starts_with("while read ") && what.contains("a.rs"), "{what}");
        assert!(what.ends_with(" ran; the request ended there"), "{what}");
        let turn = [
            Message::user("x"),
            call("c1", "b.rs"),
            Message::tool_results(vec![Part::tool_result(
                "c1",
                "The call did not run: interrupted by the person.",
                true,
            )]),
        ];
        assert!(what_ran(&turn, &ToolSet::default()).starts_with("before read "));
    }

    #[test]
    fn a_stop_while_the_reply_came_says_it_was_cut() {
        let turn = [Message::user("explain"), Message::assistant("The loop starts")];
        assert_eq!(what_ran(&turn, &ToolSet::default()), "while your reply above was being written; it was cut there");
    }
}
