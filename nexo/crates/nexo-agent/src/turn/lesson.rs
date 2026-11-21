//! Part of the agent loop; see the parent module.

use super::*;

/// What a provider's error taught about the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Lesson {
    None,
    /// The request is longer than the window and a policy that keeps its view under the window could not make
    /// the next view smaller (compaction is the answer of the policies that do not).
    Context,
    /// The request is longer than the window and the policy keeps its view under the window: the ratio of
    /// provider tokens to estimated tokens, the window itself, or the smallest request refused (`State::refused`)
    /// was corrected, so a new view is smaller and may fit with the history as it is.
    Refit,
    /// `max_tokens` was above the largest reply; it is capped now (retry).
    Output,
    UnknownModel,
}

/// Reads the error for what it says about the model and remembers it:
/// the context window, the largest reply, or that the model does not exist. `wanted` is the reply room the failed
/// request asked for.
pub(super) async fn learn(state: &Shared, e: &ProviderError, model: &str, wanted: u32) -> Lesson {
    let mut guard = state.lock().await;
    let st = &mut *guard;
    let (provider, mut cfg, _) = st.target.get();
    // a 429 is a throttle (tokens per minute), never the window
    let throttled = e.kind == ErrorKind::RateLimit || hints::is_rate_message(&e.message);
    if throttled {
        return Lesson::None;
    }
    let named = hints::context_window(&e.message);
    // under the curator a request that was too long for the window is answered with a smaller view, never a summary
    if st.policy.fits_window() && (named.is_some() || e.kind == ErrorKind::ContextLength) {
        return refit(st, &provider, &mut cfg, model, e, named);
    }
    if let Some(n) = named {
        let before = st.limits.context_window;
        // a smaller window the person set stays: only a lower number is learned
        if before.is_some_and(|b| n >= b) {
            return Lesson::Context;
        }
        remember(st, &provider, &mut cfg, model, "context_window", n);
        // A policy that does not keep its view under the window (the curator never gets here: `refit` answered it
        // above) can still retry once: when the request itself is well under the window, it was the reply's room
        // (prompt plus max_tokens) that went over, which a summary does not fix and the next pass shortens.
        if before.is_none_or(|b| n < b) && window_fit::room_left(st, u64::from(n), wanted) {
            return Lesson::Refit;
        }
        return Lesson::Context;
    }
    if e.kind == ErrorKind::ContextLength {
        return Lesson::Context;
    }
    if let Some(n) = hints::max_output(&e.message) {
        remember(st, &provider, &mut cfg, model, "max_output", n);
        return Lesson::Output;
    }
    if hints::unknown_model(e) {
        // no notice of its own: the problem that follows says it once, with the closest names
        return Lesson::UnknownModel;
    }
    Lesson::None
}

/// The new views one call makes after context errors before it ends the turn.
pub(super) const MAX_REFITS: u32 = 3;

/// Whether the session's policy keeps its view under the window (the curator), so a context error is answered
/// with a smaller view and never with a summary.
pub(super) async fn curated(state: &Shared) -> bool {
    state.lock().await.policy.fits_window()
}

/// Tells the person a request would not fit the window of the model after `refits` smaller views, and what
/// they can do. The first error of a call that could not be answered with a smaller view is shown as the
/// provider's own card. With no error (`e` None) the request was not sent: it is not smaller than one the provider
/// refused as too long without a number (`known_too_long`). Past the provider's card, the sizes are given.
pub(super) async fn tell_not_fitting(state: &Shared, events: &EventTx, e: Option<&ProviderError>, refits: u32) {
    let card = match e {
        Some(err) if refits == 0 => problem_of(state, err, false).await,
        _ => {
            let views = if refits == 1 { "1 smaller view".to_string() } else { format!("{refits} smaller views") };
            let (sizes, bare) = {
                let st = state.lock().await;
                // an error that names a window was not a refusal with no number, whatever an earlier one set
                let numbered = e.is_some_and(|e| hints::context_window(&e.message).is_some());
                (refused_sizes(&st), st.refused.is_some() && !numbered)
            };
            // a request refused with no number did not fail a window the provider named: it was refused as too long
            let why = match e {
                None => {
                    "It was not sent: it is not smaller than a request the provider refused as too long.".to_string()
                }
                Some(_) if refits >= MAX_REFITS && bare => format!("It was still refused after {views}."),
                Some(_) if refits >= MAX_REFITS => format!("It still did not fit after {views}."),
                Some(_) => format!("It could not be made smaller after {views}."),
            };
            let title = if bare {
                "The provider refused the request as too long"
            } else {
                "The request did not fit the model's window"
            };
            let card = Problem::new(title, format!("{why}{sizes}"));
            if e.is_none() {
                // `/compact` and `/clear` keep `State::refused`: only a model switch lifts it
                card.step("`/model` switches the model; switch back to try this one again.")
            } else {
                card.step("`/compact` summarises the history.").step("`/clear` starts over.")
            }
        }
    };
    notice_problem(events, card);
}

/// How much more provider tokens a request takes per estimated token after an error that gives no count of the
/// request it can use (an error with no number at all lowers `State::refused` instead).
const RATIO_STEP: f64 = 1.15;

/// What a context error teaches a policy that keeps its view under the window: the ratio of provider tokens to
/// estimated tokens rises (from the request's own count when the error gives it, else by a step, to what the
/// window leaves), and a window that is not known yet is guessed for this session. An error with no number at all
/// (neither a window nor the request's size) leaves the ratio alone and lowers the smallest request refused
/// (`lower_refused`) instead. `Refit` when the next view will be smaller, `Context` when it would be the same request.
fn refit(
    st: &mut State,
    provider: &str,
    cfg: &mut ProviderConfig,
    model: &str,
    e: &ProviderError,
    named: Option<u32>,
) -> Lesson {
    let r0 = st.tok_ratio.unwrap_or(1.0);
    let mut moved = false;
    // a window named smaller than the known one is remembered, as for every policy
    if let Some(n) = named.filter(|n| st.limits.context_window.is_none_or(|b| *n < b)) {
        remember(st, provider, cfg, model, "context_window", n);
        moved = true;
    }
    let est = st.est_sent;
    let reply = reply_tokens(st);
    let said = hints::request_tokens(&e.message).filter(|_| est >= 500);
    // the provider said only that the request was too long: neither its window nor the request's size
    let bare = named.is_none() && hints::request_tokens(&e.message).is_none();
    let lowered = bare && lower_refused(st);
    let ratio = match (said, st.limits.context_window) {
        (Some(said), _) => said as f64 / est as f64,
        // an error with no number says nothing of the ratio: the smallest request refused answers it (`lower_refused`)
        (None, Some(w)) if est > 0 && !bare => {
            let room = f64::from(w.saturating_sub(reply));
            (r0 * RATIO_STEP).max(room / est as f64 * RATIO_STEP)
        }
        _ => r0,
    };
    let ratio = ratio.max(r0).clamp(0.5, 3.0);
    let up = ratio > r0 + 1e-9;
    if up || st.tok_ratio.is_some() {
        st.tok_ratio = Some(ratio);
    }
    // no window known and none named: a guess for this session only, never written to the person's file
    if st.limits.context_window.is_none() && est > 0 {
        st.limits.context_window = Some(guessed_window(st, est));
        st.limits.source = GUESSED.into();
        emit(&st.events, st.limits.event(provider, model));
        moved = true;
    }
    if up || moved || lowered {
        Lesson::Refit
    } else {
        Lesson::Context
    }
}

/// The `source` of a window nexo guessed from a refused request, not one the provider or the person stated.
const GUESSED: &str = "estimate";

/// The window to go by when none is known: the reply and room for three quarters of the request that failed, so
/// the next view is that much smaller. The reply's size depends on the window, so it is worked out from it.
fn guessed_window(st: &State, est: u64) -> u32 {
    let spare = if st.tok_ratio.is_some() { 0.95 } else { 0.85 };
    let room = (est as f64 * 0.75 * st.tok_ratio.unwrap_or(1.0) / spare).ceil() as u64;
    let mut limits = st.limits.clone();
    let mut window = u64::from(reply_tokens(st)) + room;
    for _ in 0..3 {
        limits.context_window = Some(u32::try_from(window).unwrap_or(u32::MAX));
        window = u64::from(limits.reply_room(st.config.agent.max_tokens, st.reply_boost)) + room;
    }
    u32::try_from(window).unwrap_or(u32::MAX)
}

/// Lowers `State::refused` to the request just refused (`st.est_sent`) when it is smaller than the smallest one
/// refused before, and tells the person once a turn. False when it is not smaller: the next view would not be either.
fn lower_refused(st: &mut State) -> bool {
    let est = st.est_sent;
    if est == 0 || st.refused.is_some_and(|r| est >= r) {
        return false;
    }
    st.refused = Some(est);
    // once a turn: `noticed` is emptied when a turn starts
    if st.noticed.insert("refused with no number".to_string()) {
        let ratio = st.tok_ratio;
        let t = |n: u64| window_fit::provider_tokens(n, ratio);
        notice(
            &st.events,
            Level::Info,
            format!(
                "the provider refused a request of about {} tokens as too long and did not say its window: requests \
                 to this model are kept under about {} tokens from now on (until a model switch or a restart)",
                t(est),
                t(refused_cap(est))
            ),
        );
    }
    true
}

/// Whether the request the session last built (`st.est_sent`) is not smaller than one the provider refused as too
/// long without a number, under the curator: sent, it would be refused again.
pub(super) fn known_too_long(st: &State) -> bool {
    st.policy.fits_window() && st.refused.is_some_and(|r| st.est_sent >= r)
}

/// [`known_too_long`] of the session.
pub(super) async fn refused_again(state: &Shared) -> bool {
    known_too_long(&*state.lock().await)
}

/// The sizes the not-fitting card gives, in the provider's tokens as far as they are known: this request, the
/// smallest request refused without a number and the ceiling it set (when there is one), and the window (when known).
fn refused_sizes(st: &State) -> String {
    let t = |n: u64| window_fit::provider_tokens(n, st.tok_ratio);
    let mut parts = vec![format!("this request {}", t(st.est_sent))];
    if let Some(r) = st.refused {
        parts.push(format!("the smallest refused {}", t(r)));
        parts.push(format!("the ceiling {}", t(refused_cap(r))));
    }
    if let Some(w) = st.limits.context_window {
        // a window nexo guessed (`refit`) is not one the provider stated
        parts.push(if st.limits.source == GUESSED {
            format!("the window nexo assumes, about {w}")
        } else {
            format!("the window {w}")
        });
    }
    format!(" Sizes in tokens, about: {}.", parts.join(", "))
}

/// Stores a limit learned from an error in the running configuration and
/// the user's file, and tells the interface.
pub(super) fn remember(st: &mut State, provider: &str, cfg: &mut ProviderConfig, model: &str, which: &str, n: u32) {
    let file = nexo_core::paths::user_config_file();
    let saved = config_edit::set_limit(&file, provider, model, which, n);
    let window = which == "context_window";
    let map = if window { &mut cfg.context_window } else { &mut cfg.max_output };
    map.insert(model.to_string(), n);
    if let Some(c) = st.config.providers.get_mut(provider) {
        let map = if window { &mut c.context_window } else { &mut c.max_output };
        map.insert(model.to_string(), n);
    }
    st.target.set(provider, cfg.clone(), model);
    if window {
        st.limits.context_window = Some(n);
    } else {
        st.limits.max_output = Some(n);
    }
    st.limits.source = "error".into();
    emit(&st.events, st.limits.event(provider, model));
    let what = if window { "context window" } else { "largest reply" };
    match saved {
        Ok(()) => notice(
            &st.events,
            Level::Info,
            format!(
                "{what} of {model} set to {n} tokens (learned from the provider's error; saved to {})",
                file.display()
            ),
        ),
        Err(e) => notice(
            &st.events,
            Level::Warn,
            format!("{what} of {model} set to {n} tokens for this session (could not save it: {e})"),
        ),
    }
}

/// The error as a card: what failed, the likely cause and what to do, made concrete with the provider,
/// model, address and key source of the call, and the window the session knows. `shown` tells whether part of
/// the reply had already arrived, so a dropped connection is not described as a cut answer when nothing came.
pub(super) async fn problem_of(state: &Shared, e: &ProviderError, shown: bool) -> Problem {
    let st = state.lock().await;
    let (provider, cfg, model) = st.target.get();
    let mut ctx = ProblemCtx::from_config(&provider, &model, &cfg);
    ctx.shown = shown;
    ctx.window = st.limits.context_window.or_else(|| hints::context_window(&e.message)).or(ctx.window);
    let mut p = Problem::of(e, &ctx);
    if hints::unknown_model(e) {
        let mut names: Vec<String> = cfg.models.clone();
        if let Some(c) = catalogue::cached() {
            names.extend(c.model_names(&provider));
        }
        let close = hints::closest(&model, names.iter().map(String::as_str), 5);
        if !close.is_empty() {
            p = p.step(format!("Closest names: {} (`/model <name>`).", close.join(", ")));
        }
    }
    p
}
