//! What a model call costs the agent in copies of the conversation, timed on a 300-message history of
//! about 120k tokens (0 $). `nxcargo test -p nexo-agent --release per_call -- --ignored --nocapture`.

use std::time::{Duration, Instant};

use nexo_core::message::{Message, Part, Role};
use serde_json::json;

use crate::context::{BuiltIn, ContextPolicy};
use crate::history::{History, Sent};

/// `n` messages in turns of a request, a call, its large output and a reply (about 6 KB a turn).
fn synthetic(n: usize) -> Vec<Message> {
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while out.len() < n {
        out.push(Message::user(format!("please look at module {i} and fix the failing check, then say what changed")));
        out.push(Message::new(
            Role::Assistant,
            vec![Part::tool_call(format!("c{i}"), "read", json!({"path": format!("src/m{i}.rs"), "limit": 400}))],
        ));
        out.push(Message::tool_results(vec![Part::tool_result(
            format!("c{i}"),
            format!("fn f{i}(a: u32) -> u32 {{ a + {i} }}\n").repeat(150),
            false,
        )]));
        out.push(Message::assistant(format!("module {i} is fixed: the check passes now. ").repeat(6)));
        i += 1;
    }
    out.truncate(n);
    out
}

fn p50(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

/// What the agent did before the copies were saved: serialize the whole view to count it, and clone it
/// whole to compare with the next one.
fn old_bytes(ms: &[Message]) -> u64 {
    ms.iter().map(|m| serde_json::to_string(m).map(|s| s.len() as u64).unwrap_or(0)).sum()
}

fn old_same(last: &[Message], view: &[Message]) -> usize {
    last.iter().zip(view).take_while(|(a, b)| a == b).count()
}

/// The history as it is and four ways the next view can differ from it.
fn variants() -> Vec<(&'static str, Vec<Message>)> {
    let base = synthetic(300);
    let mut middle = base.clone();
    middle[150] = Message::user("a different request in the middle of the conversation");
    // the same length of JSON, other text: a size cache that only looked at lengths would miss it
    let mut same_len = base.clone();
    same_len[40] =
        Message::user("please look at module 9 and fix the failing check, then say what changed".to_string());
    let mut shorter = base.clone();
    shorter.pop();
    let mut longer = base.clone();
    longer.push(Message::user("and one more thing"));
    vec![
        ("middle edited", middle),
        ("same length edited", same_len),
        ("one removed", shorter),
        ("one appended", longer),
    ]
}

#[test]
fn a_request_is_counted_and_compared_as_before_whatever_changed() {
    let base = synthetic(300);
    for (name, next) in variants() {
        let mut sent = Sent::default();
        assert_eq!(sent.follow(&base), (0, old_bytes(&base)), "{name}: a first request has nothing before it");
        assert_eq!(sent.messages(), &base[..]);
        let got = sent.follow(&next);
        assert_eq!(got, (old_same(&base, &next), old_bytes(&next)), "{name}");
        assert_eq!(sent.messages(), &next[..], "{name}: what is kept is the request just made");
        // and back to the first one: the sizes followed the source both ways
        assert_eq!(sent.follow(&base), (old_same(&next, &base), old_bytes(&base)), "{name}: back");
        assert_eq!(sent.messages(), &base[..]);
    }
    let mut sent = Sent::default();
    sent.follow(&base);
    assert_eq!(sent.follow(&base), (300, old_bytes(&base)), "the same request twice repeats whole");
    sent.clear();
    assert!(sent.is_empty());
    assert_eq!(sent.follow(&base[..3]), (0, old_bytes(&base[..3])), "after a clear nothing repeats");
}

#[test]
fn the_map_counts_each_message_as_the_serialization_did() {
    let base = synthetic(300);
    for (name, next) in variants() {
        let history = History::new(base.clone());
        let cells = crate::context_map::map(&history, &next);
        assert_eq!(cells.len(), base.len(), "{name}");
        for (c, m) in cells.iter().zip(&base) {
            assert_eq!(c.tokens, nexo_core::message::message_tokens(m).max(1), "{name}");
        }
        // the states follow the view, not the sizes: the edited message is no longer whole
        let whole = cells.iter().filter(|c| c.state == nexo_core::CellState::Whole).count();
        assert!((298..=300).contains(&whole), "{name}: {whole}");
    }
}

#[tokio::test]
#[ignore = "timing"]
async fn per_call_cost_of_each_copy() {
    let mut history = History::new(synthetic(300));
    eprintln!("history: {} messages, {} tokens, {} bytes", history.len(), history.tokens(), history.bytes());
    let mut policy = BuiltIn::new("").unwrap();
    let mut last_sent: Vec<Message> = Vec::new();
    let mut sent = Sent::default();
    let (mut t_view, mut t_convert, mut t_old_bytes, mut t_old_last) = (vec![], vec![], vec![], vec![]);
    let (mut t_map, mut t_follow) = (vec![], vec![]);
    for call in 0..100 {
        history.push(Message::user(format!("and then call {call}")));
        policy.history_version(history.version());
        let t = Instant::now();
        let view = policy.view(1000, &history).await.unwrap();
        t_view.push(t.elapsed());
        // the conversion of the whole curated list, alone
        let values: Vec<serde_json::Value> = view.messages.iter().map(|m| serde_json::to_value(m).unwrap()).collect();
        let t = Instant::now();
        let back: Vec<Message> = values.into_iter().map(|v| serde_json::from_value(v).unwrap()).collect();
        t_convert.push(t.elapsed());
        assert_eq!(back.len(), view.messages.len());
        let t = Instant::now();
        let bytes = old_bytes(&view.messages);
        t_old_bytes.push(t.elapsed());
        let t = Instant::now();
        let same = old_same(&last_sent, &view.messages);
        last_sent = view.messages.clone();
        t_old_last.push(t.elapsed());
        let t = Instant::now();
        let cells = crate::context_map::map(&history, &view.messages);
        t_map.push(t.elapsed());
        let t = Instant::now();
        let got = sent.follow(&view.messages);
        t_follow.push(t.elapsed());
        assert_eq!(got, (same, bytes));
        assert_eq!(cells.len(), history.len());
    }
    let old = p50(t_old_bytes.clone()) + p50(t_old_last.clone());
    eprintln!(
        "p50 over 100 calls: view {:?} (of it, from_value alone {:?}); counting+copying before {:?} (bytes {:?}, last_sent {:?}), after {:?}; map after {:?}",
        p50(t_view),
        p50(t_convert),
        old,
        p50(t_old_bytes),
        p50(t_old_last),
        p50(t_follow),
        p50(t_map)
    );
}
