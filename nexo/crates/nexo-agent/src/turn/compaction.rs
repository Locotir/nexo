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
        room: st
            .limits
            .context_window
            .map(|w| u64::from(w).saturating_sub(u64::from(reply_tokens(st)) + fixed_tokens(&st.system, &offered(st)))),
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
            // The summary was billed even when persisting its replacement fails.
            add_usage(st, &events, &done.model, done.usage, Usage::default(), UsageKind::Compact);
            // A separate recovery slot is committed first: a crash before the session rename leaves
            // the old snapshot usable; a crash after it finds this prepared state.
            // The summariser may prepend its summary to the first retained user message; for curator
            // indexing that message belongs to the replaced prefix, not to an exact tail.
            let old = st.history.to_vec();
            let mut curator_from = keep_from.min(before);
            if !work.ends_with(&old[curator_from..])
                && curator_from < before
                && work.ends_with(&old[curator_from + 1..])
            {
                curator_from += 1;
            }
            let curator_shift = work.len().saturating_sub(before - curator_from);
            let path = st.session.path().to_path_buf();
            if !path.as_os_str().is_empty() && !st.session.is_lost() {
                let prepared =
                    st.policy.prepare_compaction_snapshot(&old, curator_from, &work).and_then(|bytes| match bytes {
                        Some(bytes) => crate::context::saved::write_compaction(&path, bytes, &work).map_err(Into::into),
                        None => Ok(()),
                    });
                if let Err(e) = prepared {
                    notice_failure(
                        &events,
                        "Could not compact the conversation",
                        &e,
                        &["Could not prepare context recovery; check session storage and try again."],
                    );
                    return false;
                }
            }
            let persistence = st.session.reset_with(&work);
            saving(st);
            if let Err(e) = persistence {
                if !e.is::<crate::session::NotReopened>() {
                    notice_failure(
                        &events,
                        "Could not save the compacted conversation",
                        &e,
                        &["The original history was kept."],
                    );
                    return false;
                }
                // Rename committed: Session and disk already hold `work`, even though saving is paused.
                notice_failure(
                    &events,
                    "Compacted conversation saved but could not be reopened",
                    &e,
                    &["Check session saving before continuing."],
                );
            }
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
            // the person's last message moves the same way; one that went into the summary leaves 0, where the turn
            // falls back to its own start
            st.person_at = if st.person_at >= from { st.person_at - from + shift } else { 0 };
            // a summary of the message that held the line about compiled programs takes the line out of what the
            // model reads: the next message of the person's carries it again
            if st.ask_told && !crate::ask_hint::told(&work) {
                st.ask_again = crate::ask_hint::said(&st.history);
            }
            st.policy.compacted_prefix(&old, curator_from, curator_shift);
            st.history.replace(work);
            // what the map showed may be in the part the summary replaced: the next match shows it again
            st.machine.forget();
            // the failures it held are out of the history: a line pointing back at them would point at nothing
            st.repeats = Default::default();
            st.last_prompt = None;
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
            // Rejecting a response must not erase what the provider already billed. Keep the
            // original history, but account the side call exactly as on the success path.
            if let Some(failed) = e.downcast_ref::<compact::CompactionFailure>() {
                add_usage(st, &events, &failed.model, failed.usage, Usage::default(), UsageKind::Compact);
            }
            notice_failure(&events, "Could not compact the conversation", &e, compaction_failure_hints(&e));
            false
        }
    }
}

fn compaction_failure_hints(error: &anyhow::Error) -> &'static [&'static str] {
    if error.downcast_ref::<compact::ImagesInCompactionPrefix>().is_some() {
        &["Text-only compaction cannot preserve images in the part being compacted."]
    } else {
        // Not every other failure is transient (for example, a request may not fit the window).
        // Avoid promising that a retry will help.
        &["Review the error before retrying."]
    }
}

#[cfg(test)]
mod failure_hint_tests {
    use super::*;

    #[test]
    fn hints_distinguish_image_rejection_from_other_failures() {
        let image = anyhow::Error::new(compact::ImagesInCompactionPrefix).context("compaction failed");
        let hints = compaction_failure_hints(&image);
        assert!(hints.iter().any(|hint| hint.contains("images")));
        assert!(!hints.iter().any(|hint| hint.contains("retry") || hint.contains("/clear")));
        let other = anyhow::anyhow!("provider unavailable");
        assert_eq!(compaction_failure_hints(&other), &["Review the error before retrying."]);
    }
}
