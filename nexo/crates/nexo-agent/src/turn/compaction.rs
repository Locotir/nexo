//! Part of the agent loop; see the parent module.

use super::*;

/// Compacts the history now. `in_turn` keeps the request in progress whole.
/// Returns whether it happened.
pub(super) async fn compact_now(
    state: &Shared,
    events: &EventTx,
    in_turn: Option<()>,
    cancel: &CancellationToken,
) -> bool {
    let mut guard = state.lock().await;
    let st = &mut *guard;
    if st.history.is_empty() {
        notice(events, Level::Info, "nothing to compact");
        return false;
    }
    let keep = match in_turn {
        Some(()) => match compact::keep_recent(&st.history) {
            Some(k) => k,
            None => {
                notice(events, Level::Info, "nothing to compact");
                return false;
            }
        },
        None => st.history.len(),
    };
    compact_locked(st, keep, cancel).await
}

pub(super) async fn compact_locked(st: &mut State, keep_from: usize, cancel: &CancellationToken) -> bool {
    let s = Summariser {
        client: st.client.clone(),
        target: st.target.clone(),
        retries: st.config.agent.retries,
        max_tokens: SUMMARY_TOKENS,
        window: st.limits.context_window,
    };
    let before = st.history.len();
    let (tokens_before, events) = (st.history.tokens(), st.events.clone());
    notice(&events, Level::Info, "compacting the history…");
    if crate::hooks::listens(&st.config.hooks, "pre_compact") {
        let fields = serde_json::json!({"trigger": "compact", "messages": before, "tokens": tokens_before});
        let sid = st.session.id().to_string();
        fire_with(&st.config.hooks, &events, &st.cwd, &sid, "pre_compact", Some("compact"), &fields).await;
    }
    let mut work = st.history.to_vec();
    let done = tokio::select! {
        r = compact::compact(&s, &mut work, keep_from) => r,
        _ = cancel.cancelled() => Err(anyhow::anyhow!("interrupted")),
    };
    match done {
        Ok(done) => {
            // the checkpoints count positions of the old history: those in the kept tail move with
            // it, and those of the summarised part go to 0, where no rewind puts them back (the
            // summary reports their writes as done)
            let from = keep_from.min(before);
            let shift = work.len().saturating_sub(before - from);
            for c in &mut st.checkpoints {
                c.0 = if c.0 >= from { c.0 - from + shift } else { 0 };
            }
            // the turn in progress starts where its first kept message now sits
            st.turn_start = if st.turn_start >= from { st.turn_start - from + shift } else { 0 };
            st.history.replace(work);
            // the failures it held are out of the history: a line pointing back at them would point at nothing
            st.repeats = Default::default();
            add_usage(st, &events, &done.model, done.usage, Usage::default(), UsageKind::Compact);
            st.last_prompt = None;
            if let Err(e) = st.session.reset_with(&st.history.to_vec()) {
                notice(&events, Level::Warn, format!("cannot save the session: {e}"));
            }
            st.policy.compacted();
            emit(
                &events,
                AgentEvent::Compacted {
                    before_tokens: tokens_before.min(u32::MAX as u64) as u32,
                    after_tokens: st.history.tokens().min(u32::MAX as u64) as u32,
                },
            );
            notice(
                &events,
                Level::Info,
                format!(
                    "history compacted: {} messages, ~{} tokens → {} messages, ~{} tokens",
                    before,
                    tokens_before,
                    st.history.len(),
                    st.history.tokens()
                ),
            );
            true
        }
        Err(e) => {
            notice_failure(
                &events,
                "Could not compact the conversation",
                e,
                &["Try `/compact` again in a moment.", "Or start fresh with `/clear`."],
            );
            false
        }
    }
}
