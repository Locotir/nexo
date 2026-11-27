//! Part of the agent loop; see the parent module.
//!
//! The last look at a built request before it goes: what it takes with the room its reply asks, against the model's
//! window. A request that fits, or one to a model whose window is not known, goes exactly as it was built.

use super::*;

/// A policy can squeeze a user block to a small recall pointer before `fit` sees it. Check the minimum
/// input using the unaltered words first, even before deciding whether to spend a model call on compaction.
/// History (including files, tool results, and earlier failed turns) isn't part of this lower bound.
pub(super) fn check_words(st: &State, specs: &[nexo_core::provider::ToolSpec], extra: u64) -> anyhow::Result<()> {
    let Some(window) = st.limits.context_window.map(u64::from) else { return Ok(()) };
    if st.turn_words_tokens == 0 {
        return Ok(());
    }
    let ratio = fit_ratio(st.tok_ratio, window);
    let fixed = fixed_tokens(&st.system, specs);
    let conversation = st.turn_words_tokens.saturating_add(extra);
    let input = provider_tokens(fixed.saturating_add(conversation), ratio);
    let reply = reply_tokens(st).min(limits::REPLY_FLOOR);
    if input.saturating_add(u64::from(reply)) > window {
        let system = st.system.len() as u64 / 4;
        anyhow::bail!(
            "request not sent: preserving your words needs about {input} input tokens for a {window}-token window \
             (system {}, tools {}, conversation {}, plus {reply} for the reply). Try a shorter request; your session history is unchanged.",
            provider_tokens(system, ratio), provider_tokens(fixed.saturating_sub(system), ratio), provider_tokens(conversation, ratio),
        );
    }
    Ok(())
}

/// What a request takes, in the provider's tokens as far as they are known: `est` estimated tokens (4 characters a
/// token) through the ratio learned from the provider's counts.
pub(super) fn provider_tokens(est: u64, ratio: Option<f64>) -> u64 {
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

/// Admit the assembled request, including request-only notes, before it can reach a provider.
/// Only the outbound copy is shortened. The newest request and tool call/result pairs stay intact.
pub(super) async fn fit(st: &mut State, req: &mut ModelRequest, notes: Option<&str>, extra: u64) -> anyhow::Result<()> {
    req.output_ceiling = st.limits.max_output;
    let Some(window) = st.limits.context_window.map(u64::from) else { return Ok(()) };
    let (_, provider, _) = st.target.get();
    let opaque = |ms: &[Message]| opaque_reserve(ms, &provider.protocol, &req.model);
    let admit = |ms: &[Message]| admission_tokens(ms, &provider.protocol, &req.model);
    let wanted = req.max_tokens;
    let floor = wanted.min(limits::REPLY_FLOOR);
    // Opaque blocks and images are not ordinary prose: learned text ratios must
    // never discount their reserves, including media reintroduced from raw history.
    let ratio = fit_ratio(st.tok_ratio, window).map(|r| {
        if opaque(&st.history) > 0
            || opaque(&req.messages) > 0
            || st.history.iter().chain(&req.messages).any(|m| contains_images(&m.content))
        {
            r.max(1.0)
        } else {
            r
        }
    });
    let fixed = fixed_tokens(&req.system, &req.tools);
    let estimate = |ms: &[Message]| fixed.saturating_add(admit(ms));
    st.est_sent = estimate(&req.messages);
    let mut used = provider_tokens(st.est_sent, ratio);
    let useful = wanted.min(limits::REPLY_FLOOR.max((window / 8) as u32));
    // a policy that keeps to the window only does when its view ran: after it failed the request carries the whole
    // history, and the fitting curator is what still brings it under the window
    let fitted = st.policy.fits_window() && !st.view_failed;
    if used + u64::from(useful) > window && !fitted && !known_too_long(st) {
        if let Some((messages, tokens)) = curator_view(st, req, notes, extra, ratio).await {
            st.fit_tokens = Some(tokens);
            req.messages = messages;
            st.est_sent = estimate(&req.messages);
            used = provider_tokens(st.est_sent, ratio);
        }
    }
    if used + u64::from(floor) > window {
        // Keep this turn's tools usable even when many results land in one round. Never change
        // call IDs, input arguments, or the stored transcript. Prefer newer results at equal size.
        // Verify the turn before replacing any of its result bodies.
        let cut = turn_boundary(st, &req.messages, notes);
        let emergency = historical_image_view(st, notes).is_some_and(|messages| {
            req.messages = messages;
            st.est_sent = estimate(&req.messages);
            true
        });
        // If raw history contains old media but provenance/shape cannot be proved,
        // do not fall through to historical-prefix deletion.
        if !emergency
            && st
                .history
                .get(..st.turn_start)
                .is_some_and(|history| history.iter().any(|message| contains_images(&message.content)))
        {
            req.max_tokens = floor;
            anyhow::bail!("{}", over_window(st, req, window, ratio));
        }
        // Preserve the emergency view's historical prefix. Only proven current-turn
        // result bodies may be shortened; calls, sibling instructions and IDs stay intact.
        {
            let result_start = if emergency { st.turn_start } else { 0 };
            let mut results = Vec::new();
            let needs_trim = provider_tokens(st.est_sent, ratio) + u64::from(floor) > window;
            for (i, m) in req.messages.iter().enumerate().skip(result_start).filter(|_| needs_trim) {
                for (j, part) in m.content.iter().enumerate() {
                    if let Part::ToolResult { content, .. } = part {
                        let Some(n) = trimmable_text_bytes(content) else { continue };
                        if n > 256 {
                            results.push((std::cmp::Reverse(n), i, j));
                        }
                    }
                }
            }
            results.sort_unstable();
            let mut sizes: Vec<_> = req.messages.iter().map(|m| admit(std::slice::from_ref(m))).collect();
            for (_, i, j) in results {
                let Part::ToolResult { content, .. } = &mut req.messages[i].content[j] else { continue };
                // Only text was measured for this reduction. Keep every non-text part
                // (and its order) rather than silently dropping media or opaque data.
                let mut marked = false;
                content.retain_mut(|part| {
                    if matches!(part, Part::Text { .. }) {
                        if marked {
                            return false;
                        }
                        *part = Part::text(
                            "[curator] Tool output omitted to fit the context window. Request a smaller range if needed.",
                        );
                        marked = true;
                    }
                    true
                });
                let new = admit(std::slice::from_ref(&req.messages[i]));
                st.est_sent = st.est_sent - sizes[i] + new;
                sizes[i] = new;
                if provider_tokens(st.est_sent, ratio) + u64::from(wanted) <= window {
                    break;
                }
            }
            used = provider_tokens(st.est_sent, ratio);
            // Drop only the prefix before the verified turn, never up to a later queued request or job note.
            if !emergency && used + u64::from(floor) > window {
                if let Some(i) = cut.filter(|i| *i > 0) {
                    req.messages.drain(..i);
                    st.est_sent = estimate(&req.messages);
                }
            }
        }
        used = provider_tokens(st.est_sent, ratio);
        if used + u64::from(floor) > window {
            req.max_tokens = floor;
            anyhow::bail!("{}", over_window(st, req, window, ratio));
        }
        st.fit_tokens = Some(nexo_core::message::estimate_tokens(&req.messages));
    }
    if st.fit_tokens.is_some() {
        st.reads.note_view(&req.messages);
        st.last_sent.follow(&req.messages);
    }
    let room = (window - used) as u32;
    req.output_ceiling = Some(room.min(st.limits.max_output.unwrap_or(u32::MAX)));
    req.max_tokens = wanted.min(req.output_ceiling.unwrap());
    Ok(())
}

/// Reserve one token per serialized UTF-8 byte of every opaque reasoning block,
/// recursively. Responses omits top-level reasoning before the latest person message;
/// other protocols may send it. This deliberately double-counts visible reasoning
/// and any opaque data already included in the generic estimate. It also counts
/// JSON escaping (not just Unicode scalar count), signatures and encrypted data,
/// except top-level foreign blocks that Anthropic and Responses unconditionally omit.
/// Nested blocks stay conservative: result_text can transmit their readable text.
/// Other protocols and untagged legacy blocks retain the full reserve. No field is changed.
/// This is a conservative admission heuristic, not a provider token-count API.
fn opaque_reserve(messages: &[Message], protocol: &str, model: &str) -> u64 {
    fn parts(ps: &[Part]) -> u64 {
        ps.iter()
            .map(|p| match p {
                Part::Reasoning { signature, encrypted, .. } if signature.is_some() || encrypted.is_some() => {
                    serde_json::to_vec(p).map_or(u64::MAX, |s| s.len() as u64)
                }
                Part::RedactedReasoning { .. } => serde_json::to_vec(p).map_or(u64::MAX, |s| s.len() as u64),
                Part::ToolResult { content, .. } => parts(content),
                _ => 0,
            })
            .fold(0, u64::saturating_add)
    }
    let turn = nexo_core::message::latest_person(messages).unwrap_or(0);
    messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            m.content
                .iter()
                .filter(|p| {
                    !(protocol == "openai-responses"
                        && i < turn
                        && matches!(p, Part::Reasoning { .. } | Part::RedactedReasoning { .. }))
                })
                .filter(|p| !matches!(protocol, "anthropic" | "openai-responses") || !p.is_foreign_reasoning(model))
                .map(|p| parts(std::slice::from_ref(p)))
                .fold(0, u64::saturating_add)
        })
        .fold(0, u64::saturating_add)
}

fn admission_tokens(messages: &[Message], protocol: &str, model: &str) -> u64 {
    // The generic estimate includes current-turn signatures as well as visible
    // reasoning. Exempt exactly the top-level blocks these serializers omit,
    // keeping the original person frontier (and all nested data) conservative.
    let generic = if matches!(protocol, "anthropic" | "openai-responses") {
        let from = nexo_core::message::latest_person(messages).map_or(0, |i| i + 1);
        messages
            .iter()
            .enumerate()
            .map(|(i, message)| {
                let filtered;
                let message = if message.content.iter().any(|p| p.is_foreign_reasoning(model)) {
                    filtered = Message {
                        role: message.role,
                        content: message.content.iter().filter(|p| !p.is_foreign_reasoning(model)).cloned().collect(),
                    };
                    &filtered
                } else {
                    message
                };
                nexo_core::message::message_tokens(message).saturating_add(if i >= from {
                    nexo_core::message::reasoning_tokens(message)
                } else {
                    0
                })
            })
            .fold(0, u64::saturating_add)
    } else {
        nexo_core::message::estimate_tokens(messages)
    };
    generic.saturating_add(opaque_reserve(messages, protocol, model))
}

/// The text of a result that can be omitted to make room: its own text parts. Images and opaque blocks stay in
/// place, in order, and are counted by the caller (a request never goes over the window: the text is what gives);
/// `None` when the result has no text to give.
fn trimmable_text_bytes(content: &[Part]) -> Option<usize> {
    let n = content.iter().map(|p| if let Part::Text { text } = p { text.len() } else { 0 }).sum();
    (n > 0).then_some(n)
}

fn contains_images(parts: &[Part]) -> bool {
    parts.iter().any(|part| match part {
        Part::Image { .. } => true,
        Part::ToolResult { content, .. } => contains_images(content),
        _ => false,
    })
}

/// Emergency image-only recovery from raw history, using the recorded source index.
/// No policy selection, paging, role merging, or synthetic tail can discard historical
/// instructions. Image-only bodies get an explicit omission marker, never a silent
/// deletion. Ambiguous frontiers and histories needing protocol repair fail closed.
fn historical_image_view(st: &State, notes: Option<&str>) -> Option<Vec<Message>> {
    if st.person_turn_version != Some(st.history.version()) {
        return None;
    }
    let turn = st.history.get(st.turn_start..)?;
    if !turn.first()?.is_person() {
        return None;
    }
    // Preserve the ENTIRE suffix by recorded source index, including queued asks,
    // loop-generated text and mixed result/instruction messages. Its contents need
    // not prove their origin: none of them is removed, merged or transformed.
    let mut messages = st.history.to_vec();
    if !nexo_core::message::is_well_formed(&messages) {
        return None;
    }
    fn remove_images(parts: &mut Vec<Part>) -> bool {
        let mut changed = false;
        let mut previous_image = false;
        let mut replacement = Vec::with_capacity(parts.len());
        for mut part in std::mem::take(parts) {
            if matches!(part, Part::Image { .. }) {
                changed = true;
                // Mark every contiguous run in place, including mixed text/image bodies.
                // Otherwise the model can mistake incomplete evidence for a complete record.
                if !previous_image {
                    replacement.push(Part::text("[Historical image content omitted to fit the context window.]"));
                }
                previous_image = true;
                continue;
            }
            previous_image = false;
            if let Part::ToolResult { content, .. } = &mut part {
                changed |= remove_images(content);
            }
            replacement.push(part);
        }
        *parts = replacement;
        changed
    }
    let mut changed = false;
    for message in &mut messages[..st.turn_start] {
        changed |= remove_images(&mut message.content);
        // Pre-existing empty messages are not repaired by this image-only recovery.
        if message.content.is_empty() {
            return None;
        }
    }
    if !changed || !nexo_core::message::is_well_formed(&messages) {
        return None;
    }
    if let Some(note) = notes {
        attach_hidden(&mut messages, note.to_string());
    }
    Some(messages)
}

/// `turn_start` indexes history, not the outbound view. Match the complete tail from the end (including
/// queued requests), allowing only `attach_hidden`'s known final addition. A missing/rewritten tail,
/// replaced history or synthetic continuation gives no proven boundary: do not drain it.
fn turn_boundary(st: &State, view: &[Message], notes: Option<&str>) -> Option<usize> {
    if st.person_turn_version != Some(st.history.version()) {
        return None;
    }
    let turn = st.history.get(st.turn_start..)?;
    let (last, prefix) = turn.split_last()?;
    if !turn[0].is_person() {
        return None;
    }
    let before = match notes {
        None => view.strip_suffix(turn)?,
        Some(note) => {
            let (end, prior) = view.split_last()?;
            let (added, original) = end.content.split_last()?;
            if end.role != Role::User || !matches!(added, Part::Text { text } if text == note) {
                return None;
            }
            if last.role == Role::User && original == last.content {
                prior.strip_suffix(prefix)?
            } else if last.role == Role::Assistant && original.is_empty() {
                prior.strip_suffix(turn)?
            } else {
                return None;
            }
        }
    };
    Some(before.len())
}

/// Silent optional calls use a clone of the last request with new messages. The old ceiling
/// cannot be reused: skip the optional call if its own reply no longer fits.
pub(super) fn side_fits(st: &State, req: &mut ModelRequest) -> bool {
    let Some(w) = st.limits.context_window.map(u64::from) else {
        req.output_ceiling = st.limits.max_output;
        return req.max_tokens <= req.output_ceiling.unwrap_or(u32::MAX);
    };
    let (_, provider, _) = st.target.get();
    let est = fixed_tokens(&req.system, &req.tools).saturating_add(admission_tokens(
        &req.messages,
        &provider.protocol,
        &req.model,
    ));
    let ratio = fit_ratio(st.tok_ratio, w).map(|r| {
        if opaque_reserve(&req.messages, &provider.protocol, &req.model) > 0
            || req.messages.iter().any(|m| contains_images(&m.content))
        {
            r.max(1.0)
        } else {
            r
        }
    });
    let room = w.saturating_sub(provider_tokens(est, ratio)) as u32;
    req.output_ceiling = Some(room.min(st.limits.max_output.unwrap_or(u32::MAX)));
    req.max_tokens <= req.output_ceiling.unwrap()
}

/// `req`'s messages as the curator would send them under the window, with the room `req`'s reply asked for, and
/// the tokens the curator counts them at, or `None` when the curator fails or its view does not hold the person's
/// newest message whole.
async fn curator_view(
    st: &mut State,
    req: &ModelRequest,
    notes: Option<&str>,
    extra: u64,
    ratio: Option<f64>,
) -> Option<(Vec<Message>, u64)> {
    let window = st.limits.context_window?;
    let reply = req.max_tokens;
    let names: Vec<String> = req.tools.iter().map(|s| s.name.clone()).collect();
    let (_, cfg, model) = st.target.get();
    let prices = st.limits.pricing.or_else(|| cfg.pricing_for(&model));
    // `recall` is offered only with the curator's own policy: this view leaves out what it cannot hold without
    // pointing at a tool the model does not have
    let cur = st.fit_view.get_or_insert_with(|| context::BuiltIn::lenient("recall=0;relations=0"));
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
    let tokens = view.tokens;
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
    Some((messages, tokens))
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
    let (_, provider, _) = st.target.get();
    for (i, m) in req.messages.iter().enumerate() {
        let n = t(admission_tokens(std::slice::from_ref(m), &provider.protocol, &req.model));
        if n > largest.1 {
            largest = (part_name(m, i + 1), n);
        }
    }
    format!(
        "request over the window: about {} tokens with the reply, for a {window}-token window (system {system}, \
         tools {tools}, conversation {conversation}, reply {reply}); the largest part is {} at {} tokens. Not sent. Try a shorter request; your session history is unchanged.",
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
    fn emergency_trimming_counts_only_text_and_leaves_images_in_place() {
        let image = Part::Image {
            source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/a.png".into() },
        };
        let parts = vec![Part::text("a".repeat(300)), image.clone(), Part::text("caption b")];
        assert_eq!(trimmable_text_bytes(&parts), Some(309));
        assert_eq!(trimmable_text_bytes(&[image]), None);
        assert_eq!(trimmable_text_bytes(&[Part::text("x".repeat(300))]), Some(300));
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

#[cfg(test)]
mod opaque_tests {
    use super::*;
    use nexo_core::Role;

    #[test]
    fn responses_reserves_current_but_not_old_encrypted_reasoning() {
        let opaque =
            Part::Reasoning { text: String::new(), signature: None, encrypted: Some("E".repeat(188_423)), model: None };
        let history = vec![
            Message { role: Role::User, content: vec![Part::text("x".repeat(348_000))] },
            Message { role: Role::Assistant, content: vec![opaque.clone(), Part::text("answer")] },
            Message { role: Role::User, content: vec![Part::text("continue")] },
        ];
        let after = admission_tokens(&history, "openai-responses", "m");
        let before = after + serde_json::to_vec(&opaque).unwrap().len() as u64;
        println!("session-shaped admission before={before} after={after}");
        assert!(before > 272_000 && after < 272_000);
        assert_eq!(opaque_reserve(&history, "openai-responses", "m"), 0);
        assert!(opaque_reserve(&history, "anthropic", "m") > 188_423);
        assert!(opaque_reserve(&history, "gemini", "m") > 188_423);
        let mut current = history;
        current.push(Message { role: Role::Assistant, content: vec![opaque] });
        assert!(opaque_reserve(&current, "openai-responses", "m") > 188_423);
        assert!(admission_tokens(&current, "openai-responses", "m") > 272_000);
    }
}
