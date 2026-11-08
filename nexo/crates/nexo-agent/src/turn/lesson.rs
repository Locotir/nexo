//! Part of the agent loop; see the parent module.

use super::*;

/// What a provider's error taught about the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Lesson {
    None,
    /// The request is longer than the window (compaction is the answer).
    Context,
    /// The request is longer than the window, which was just learned smaller than thought, and the
    /// policy keeps its view under the window: a new view may fit with the history as it is.
    Refit,
    /// `max_tokens` was above the largest reply; it is capped now (retry).
    Output,
    UnknownModel,
}

/// Reads the error for what it says about the model and remembers it:
/// the context window, the largest reply, or that the model does not exist.
pub(super) async fn learn(state: &Shared, e: &ProviderError, model: &str) -> Lesson {
    let mut guard = state.lock().await;
    let st = &mut *guard;
    let (provider, mut cfg, _) = st.target.get();
    // a 429 is a throttle (tokens per minute), never the window
    let throttled = e.kind == ErrorKind::RateLimit || hints::is_rate_message(&e.message);
    if let Some(n) = hints::context_window(&e.message).filter(|_| !throttled) {
        let before = st.limits.context_window;
        // a smaller window the person set stays: only a lower number is learned
        if before.is_some_and(|b| n >= b) {
            return Lesson::Context;
        }
        remember(st, &provider, &mut cfg, model, "context_window", n);
        if before.is_none_or(|b| n < b) && st.policy.fits_window() {
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
        let mut names: Vec<String> = cfg.models.clone();
        if let Some(c) = catalogue::cached() {
            names.extend(c.model_names(&provider));
        }
        let close = hints::closest(model, names.iter().map(String::as_str), 5);
        let hint =
            if close.is_empty() { String::new() } else { format!("; closest: {} (`/model <name>`)", close.join(", ")) };
        notice(&st.events, Level::Warn, format!("{provider} does not know the model `{model}`{hint}"));
        return Lesson::UnknownModel;
    }
    Lesson::None
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

/// The error as shown, with the wait a rate limit asked for.
pub(super) fn explain(e: &ProviderError) -> String {
    match hints::wait_hint(e) {
        Some(h) => format!("{} ({h})", e.human()),
        None => e.human(),
    }
}
