//! Part of the agent loop; see the parent module.
//!
//! What a session sends before its messages (its tools and the system text) never changes while the
//! session lives, in this process or after a resume: a provider caches a prefix only when it is
//! byte-identical to the one of an earlier request. The parts that could change by themselves (the
//! folder tree, the date, the `diagnostics` tool that depends on the files present) are fixed when
//! the session is created and saved with it (`Session::prefix`).

use super::*;
use crate::session::Prefix;

/// Tools offered or not by what the machine or the folder holds when the process starts, not by the
/// person's choice: a resumed session offers them as it did when it began.
const MACHINE_TOOLS: [&str; 1] = ["diagnostics"];

/// The prefix of `session`: the saved one (the tools of `tools` made to match it), or, for a session
/// that has none (new, or saved before this existed), the one built now and saved.
pub(super) fn settle(
    session: &mut Session,
    cwd: &Path,
    cfg: &nexo_core::config::AgentConfig,
    tools: &mut ToolSet,
) -> Prefix {
    match session.prefix().cloned() {
        Some(mut p) => {
            for name in MACHINE_TOOLS {
                let (stored, has) = (p.tools.iter().any(|t| t == name), tools.get(name).is_some());
                if stored && !has {
                    // a stored name this build does not know is skipped
                    if let Some(t) = machine_tool(name) {
                        tools.add(t);
                    }
                } else if !stored && has {
                    tools.remove(name);
                }
            }
            // instruction files edited since, or read otherwise by a newer nexo (one that no longer reads a
            // home folder's): the session goes on with them as they are now, at the cost of one uncached request
            let now = prompt::Volatile::capture(cwd, cfg).instructions;
            if now != p.volatile.instructions {
                p.volatile.instructions = now;
                let _ = session.record_prefix(&p);
            }
            p
        }
        None => fresh(session, cwd, cfg, tools),
    }
}

/// A new prefix from the tools and the folder as they are now, saved with `session`.
pub(super) fn fresh(
    session: &mut Session,
    cwd: &Path,
    cfg: &nexo_core::config::AgentConfig,
    tools: &ToolSet,
) -> Prefix {
    let p = Prefix {
        tools: tools.names().map(String::from).collect(),
        volatile: prompt::Volatile::capture(cwd, cfg),
        shown: prompt::project_tree(cwd),
    };
    // an unsaved prefix still holds for this process
    let _ = session.record_prefix(&p);
    p
}

fn machine_tool(name: &str) -> Option<Arc<dyn Tool>> {
    match name {
        "diagnostics" => Some(Arc::new(nexo_tools::lsp::Diagnostics)),
        _ => None,
    }
}

/// The lines for the note at the head of a request when the project map changed since the model last
/// saw it (in the system text or in an earlier note); the map is then remembered as shown. None when it
/// did not change.
pub(super) fn tree_note(st: &mut State) -> Option<String> {
    let now = prompt::project_tree(&st.cwd);
    if now == st.prefix.shown {
        return None;
    }
    st.prefix.shown = now.clone();
    if let Err(e) = st.session.record_prefix(&st.prefix) {
        notice(&st.events, Level::Warn, format!("cannot save the project map: {e}"));
    }
    Some(if now.is_empty() { "Project: no files are listed any more.".to_string() } else { now })
}

/// The line for the note at the head of a request when today is not the date the system text gives (the day the
/// session began, kept for the cached prefix): a model would take that day for today. None on the day the
/// session began, and once the conversation has the line for today.
pub(super) fn date_line(st: &State) -> Option<String> {
    let today = time::OffsetDateTime::now_utc().date().to_string();
    if st.prefix.volatile.date.is_empty() || today == st.prefix.volatile.date {
        return None;
    }
    let line = format!("Today is {today}; the Date in the system text is the day this session began.");
    let said = st.history.iter().any(|m| m.role == Role::User && m.text().contains(&line));
    (!said).then_some(line)
}

/// After another session took the place of this one: its prefix, built now when it has none.
pub(super) fn adopt(st: &mut State) {
    st.prefix = settle(&mut st.session, &st.cwd, &st.config.agent, &mut st.tools);
    st.system = full_system(st);
}
