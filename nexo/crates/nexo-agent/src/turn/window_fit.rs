//! Part of the agent loop; see the parent module.
//!
//! The last look at a built request before it goes: what it takes with the room its reply asks, against the model's
//! window. A request that fits, or one to a model whose window is not known, goes exactly as it was built.

use super::*;

/// What a request takes, in the provider's tokens as far as they are known: `est` estimated tokens (4 characters a
/// token) through the ratio learned from the provider's counts.
fn provider_tokens(est: u64, ratio: Option<f64>) -> u64 {
    match ratio {
        Some(r) => (est as f64 * r).ceil() as u64,
        None => est,
    }
}

/// The provider tokens per estimated token before the provider has counted any: code and text in scripts such as
/// Chinese run about 3 characters a token where the estimate counts 4.
const FIRST_RATIO: f64 = 4.0 / 3.0;

/// The ratio [`fit`] goes by: the one learned from the provider's counts, or before the first count, in a small
/// window (32k and under, where a third more passes the window), [`FIRST_RATIO`].
fn fit_ratio(learned: Option<f64>, window: u64) -> Option<f64> {
    learned.or_else(|| prompt::is_compact(Some(window)).then_some(FIRST_RATIO))
}

/// Whether a shorter reply is what the next pass can change under `window`: the request the session last built leaves a
/// useful reply (at least `limits::REPLY_FLOOR`) but not the `wanted` one, so [`fit`] shortens it. A window learned
/// from the provider's error says it was the reply's room that went over, not the history, and no compaction is
/// needed then. When the estimate has the whole `wanted` reply fitting, [`fit`] would send the same request again, so
/// this says no and the history is compacted.
pub(super) fn room_left(st: &State, window: u64, wanted: u32) -> bool {
    let used = provider_tokens(st.est_sent, fit_ratio(st.tok_ratio, window));
    used + u64::from(limits::REPLY_FLOOR) <= window && used + u64::from(wanted) > window
}

/// Fits `req`, built from the session's view, to the model's window when it and its reply would pass it. In order:
/// when the session's policy does not keep its view under the window (plain, compact) and the request leaves no
/// useful reply (an eighth of the window, at least `limits::REPLY_FLOOR`), the same request from the curator's view,
/// as long as that view holds the person's newest message as they wrote it; then a shorter reply, down to
/// `limits::REPLY_FLOOR`; and when it still does not fit, the request goes as it is and the person is told once a
/// turn what it holds, so the provider's answer (and the lesson the loop takes from an error) decides what follows.
/// `notes` is the text this request alone carries on its last message, and `extra` its estimated tokens.
pub(super) async fn fit(st: &mut State, req: &mut ModelRequest, notes: Option<&str>, extra: u64) {
    let Some(window) = st.limits.context_window.map(u64::from) else { return };
    let wanted = req.max_tokens;
    let ratio = fit_ratio(st.tok_ratio, window);
    let mut used = provider_tokens(st.est_sent, ratio);
    if used + u64::from(wanted) <= window {
        return;
    }
    let floor = wanted.min(limits::REPLY_FLOOR);
    // a reply with room for an edit call: an eighth of the window, never under the floor nor over what was asked
    let useful = wanted.min(limits::REPLY_FLOOR.max(u32::try_from(window / 8).unwrap_or(u32::MAX)));
    if used + u64::from(useful) > window && !st.policy.fits_window() {
        if let Some(messages) = curator_view(st, req, notes, extra, ratio).await {
            st.reads.note_view(&messages);
            st.last_sent.follow(&messages);
            st.est_sent = fixed_tokens(&st.system, &req.tools) + nexo_core::message::estimate_tokens(&messages);
            req.messages = messages;
            used = provider_tokens(st.est_sent, ratio);
        }
    }
    if used + u64::from(floor) <= window {
        // what is left of the window, never more than was asked
        req.max_tokens = u32::try_from(window - used).unwrap_or(u32::MAX).min(wanted);
        return;
    }
    req.max_tokens = floor;
    if st.noticed.insert(format!("over the window {window}")) {
        notice(&st.events, Level::Warn, over_window(st, req, window, ratio));
    }
}

/// `req`'s messages as the curator would send them under the window, with the room `req`'s reply asked for, or
/// `None` when the curator fails or its view does not hold the person's newest message whole.
async fn curator_view(
    st: &mut State,
    req: &ModelRequest,
    notes: Option<&str>,
    extra: u64,
    ratio: Option<f64>,
) -> Option<Vec<Message>> {
    let window = st.limits.context_window?;
    let reply = req.max_tokens;
    let names: Vec<String> = req.tools.iter().map(|s| s.name.clone()).collect();
    let (_, cfg, model) = st.target.get();
    let prices = st.limits.pricing.or_else(|| cfg.pricing_for(&model));
    // `recall` is offered only with the curator's own policy: this view leaves out what it cannot hold without
    // pointing at a tool the model does not have
    let cur = st.fit_view.get_or_insert_with(|| context::BuiltIn::lenient("recall=0"));
    cur.tools(&names);
    if window > reply {
        cur.window(view_window(window - reply, ratio));
    }
    if let Some(p) = prices {
        cur.prices(p.input, p.cache_read);
    }
    cur.history_version(st.history.version());
    let view = match cur.view(fixed_tokens(&st.system, &req.tools) + extra, &st.history).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("the curator could not fit a request to the window: {e}");
            return None;
        }
    };
    let (mut messages, _) = nexo_core::message::well_formed(view.messages);
    // the person's newest words always go as they wrote them
    if let Some(i) = nexo_core::message::latest_person(&st.history) {
        if !messages.contains(&st.history[i]) {
            return None;
        }
    }
    if let Some(n) = notes {
        attach_hidden(&mut messages, n.to_string());
    }
    Some(messages)
}

/// The notice of a request that goes over the window: its parts in tokens against the window, and its largest part.
fn over_window(st: &State, req: &ModelRequest, window: u64, ratio: Option<f64>) -> String {
    let t = |est: u64| provider_tokens(est, ratio);
    let system = t(st.system.len() as u64 / 4);
    let tools = t(serde_json::to_string(&req.tools).map_or(0, |s| s.len() as u64) / 4);
    let total = t(st.est_sent);
    let conversation = total.saturating_sub(system + tools);
    let reply = u64::from(req.max_tokens);
    let mut largest = ("the system prompt".to_string(), system);
    if tools > largest.1 {
        largest = ("the tool list".to_string(), tools);
    }
    for (i, m) in req.messages.iter().enumerate() {
        let n = t(nexo_core::message::message_tokens(m));
        if n > largest.1 {
            largest = (part_name(m, i + 1), n);
        }
    }
    format!(
        "request over the window: about {} tokens with the reply, for a {window}-token window (system {system}, \
         tools {tools}, conversation {conversation}, reply {reply}); the largest part is {} at {} tokens. Sent as it \
         is; the provider's answer decides what happens next.",
        total + reply,
        largest.0,
        largest.1
    )
}

/// What message `n` (from 1) of a request is, for the person.
fn part_name(m: &Message, n: usize) -> String {
    let tool = m.content.iter().find_map(|p| match p {
        Part::ToolResult { name, .. } => Some(name.clone().unwrap_or_else(|| "tool".into())),
        _ => None,
    });
    match tool {
        Some(name) => format!("the {name} result in message {n}"),
        None if m.is_person() => format!("your message (message {n})"),
        None if m.role == Role::Assistant => format!("a reply of the model (message {n})"),
        None => format!("message {n}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_estimate_goes_through_the_learned_ratio_only_when_one_is_known() {
        assert_eq!(provider_tokens(1_000, None), 1_000);
        assert_eq!(provider_tokens(1_000, Some(1.3)), 1_300);
    }

    #[test]
    fn before_the_first_count_a_small_window_takes_the_estimate_as_a_third_more_and_a_large_one_as_it_is() {
        assert_eq!(fit_ratio(None, 8_000), Some(FIRST_RATIO));
        assert_eq!(provider_tokens(3_000, fit_ratio(None, 8_000)), 4_000);
        assert_eq!(fit_ratio(None, 200_000), None);
        assert_eq!(fit_ratio(Some(1.1), 8_000), Some(1.1), "a learned ratio wins");
    }

    #[test]
    fn a_part_is_named_by_what_it_is() {
        let result = Message::tool_results(vec![Part::ToolResult {
            call_id: "c1".into(),
            name: Some("read".into()),
            content: vec![Part::text("x")],
            is_error: false,
        }]);
        assert_eq!(part_name(&result, 3), "the read result in message 3");
        assert_eq!(part_name(&Message::user("hi"), 1), "your message (message 1)");
        assert_eq!(part_name(&Message::assistant("ok"), 2), "a reply of the model (message 2)");
    }
}
