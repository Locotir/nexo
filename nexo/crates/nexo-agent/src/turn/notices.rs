use super::*;

pub(super) fn emit(events: &EventTx, ev: AgentEvent) {
    let _ = events.send(ev);
}

/// What a view that lost some sub-agent events is sent so it can catch up: the state of every run the
/// registry lists.
pub(super) fn sub_resync(subs: &nexo_core::subs::Subs) -> Vec<AgentEvent> {
    let runs = subs.list().into_iter().map(|info| AgentEvent::SubState { info });
    runs.chain(subs.flows().into_iter().map(|info| AgentEvent::FlowRun { info })).collect()
}

pub(super) fn notice(events: &EventTx, level: Level, text: impl Into<String>) {
    emit(events, AgentEvent::Notice { level, text: text.into(), problem: None });
}

/// Tells the person about a failure as a card; the plain text of it rides along for the old readers.
pub(super) fn notice_problem(events: &EventTx, problem: Problem) {
    emit(events, AgentEvent::Notice { level: Level::Error, text: problem.plain(), problem: Some(Box::new(problem)) });
}

/// Tells the person an operation failed: the title says what, `cause` why; each step is something to do next.
pub(super) fn notice_failure(
    events: &EventTx,
    title: impl Into<String>,
    cause: impl std::fmt::Display,
    steps: &[&str],
) {
    let cause = cause.to_string();
    let mut p = Problem::new(title, cause.lines().next().unwrap_or("").trim().to_string());
    for s in steps {
        p = p.step(*s);
    }
    // a cause that is long or has more lines than the row shows opens in full
    if cause.lines().count() > 1 || cause.chars().count() > 69 {
        p = p.detail(cause);
    }
    notice_problem(events, p);
}

/// Tells the person how an MCP control ended.
pub(super) fn mcp_result(events: &EventTx, r: Result<String, String>) {
    match r {
        Ok(t) => notice(events, Level::Info, t),
        Err(e) => notice(events, Level::Warn, format!("mcp: {e}")),
    }
}

/// The words of the step the agent takes before a reply starts (see `AgentEvent::Preparing`).
pub(super) const PREP_REQUEST: &str = "preparing the request";
pub(super) const PREP_GOAL: &str = "starting the goal";
pub(super) const PREP_GOAL_NEXT: &str = "continuing the goal";
pub(super) const PREP_MODEL: &str = "waiting for the model";

/// Tells the interface what the agent is doing before the reply starts.
pub(super) fn preparing(events: &EventTx, what: impl Into<String>) {
    emit(events, AgentEvent::Preparing { what: what.into() });
}
