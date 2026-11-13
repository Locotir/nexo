use super::*;

/// Runs the hooks of `event` with what the session knows, tells the person which failed, and returns
/// what they said. Nothing runs (and the state is not read twice) when no hook listens.
pub(super) async fn fire(state: &Shared, event: &str, subject: Option<&str>, fields: Value) -> crate::hooks::Outcome {
    let (hooks, cwd, sid, events) = {
        let st = state.lock().await;
        if !crate::hooks::listens(&st.config.hooks, event) {
            return Default::default();
        }
        (st.config.hooks.clone(), st.cwd.clone(), st.session.id().to_string(), st.events.clone())
    };
    fire_with(&hooks, &events, &cwd, &sid, event, subject, &fields).await
}

pub(super) async fn fire_with(
    hooks: &[nexo_core::config::HookConfig],
    events: &EventTx,
    cwd: &Path,
    sid: &str,
    event: &str,
    subject: Option<&str>,
    fields: &Value,
) -> crate::hooks::Outcome {
    let o = crate::hooks::run(hooks, event, subject, fields, cwd, sid).await;
    for f in &o.failures {
        notice(events, Level::Warn, f.clone());
    }
    o
}

/// Tells the hooks of `Notification` (a permission question, a request that ended in trouble) without
/// holding anything up.
pub(super) fn notify_hooks(state: &Shared, kind: &'static str, message: String) {
    let state = state.clone();
    tokio::spawn(async move {
        fire(&state, "notification", Some(kind), serde_json::json!({"notification_type": kind, "message": message}))
            .await;
    });
}
