//! The built-in tools of nexo (files, shell, todo, web) and the client for MCP servers.
//!
//! Every tool implements [`nexo_core::Tool`]: a spec the model reads, a
//! [`nexo_core::ToolKind`] the permission policy decides by, and `run`,
//! which never panics on bad arguments but answers with an error output the
//! model can read and act on. Paths are resolved against the context's
//! `cwd`; a path outside it is allowed, and the call's summary says so for
//! the permission dialog.

pub mod bash;
pub mod browser;
pub mod checkers;
pub mod data_check;
pub mod debug;
pub mod edit;
pub mod edit_check;
pub mod glob;
pub mod grep;
pub mod guarded;
pub mod jobs;
pub mod ls;
pub mod lsp;
pub mod mail;
pub mod mcp;
pub mod mcp_http;
pub mod mcp_hub;
pub mod mcp_oauth;
pub mod note;
pub mod picture;
pub mod postcheck;
pub mod project;
pub mod read;
pub mod recall;
pub mod sandbox;
pub mod send;
pub mod skill;
pub mod todo;
pub mod util;
pub mod web;
pub mod write;

use std::sync::Arc;

use nexo_core::config::WebConfig;
use nexo_core::ToolSet;

/// The built-in tools with the web ones as configured: `web_search` and
/// `web_fetch` join the local eight unless `[web] enabled = false`. `browser` is always there; it starts
/// no browser until the model first uses it.
pub fn builtin_with(web: &WebConfig) -> ToolSet {
    let mut set = local();
    for t in web::tools(web) {
        set.add(t);
    }
    set.add(Arc::new(browser::BrowserTool::new()));
    set
}

/// The built-in tools with the default web settings.
pub fn builtin() -> ToolSet {
    builtin_with(&WebConfig::default())
}

/// The local tools: `read`, `write`, `edit`, `bash`, `jobs`, `glob`, `grep`, `ls`, `note`, `recall`, `todo`, `diagnostics`.
/// One set per session: the `todo` tool keeps the session's list.
pub fn local() -> ToolSet {
    ToolSet::new()
        .with(Arc::new(read::Read))
        .with(Arc::new(write::Write))
        .with(Arc::new(edit::Edit))
        .with(Arc::new(bash::Bash))
        .with(Arc::new(glob::Glob))
        .with(Arc::new(grep::Grep))
        .with(Arc::new(jobs::Jobs))
        .with(Arc::new(ls::Ls))
        .with(Arc::new(note::Note))
        .with(Arc::new(recall::Recall))
        .with(Arc::new(todo::Todo::new()))
        .with(Arc::new(lsp::Diagnostics))
}

/// Variables as `(name, value)`.
pub type Env = Arc<Vec<(String, String)>>;

tokio::task_local! {
    /// Set around a tool call: the programs the tool starts on its own (language servers,
    /// checkers) get exactly these variables instead of this process's environment.
    pub static CHILD_ENV: Env;
}

/// The environment set by [`CHILD_ENV`] for the call running now, if any.
pub(crate) fn child_env() -> Option<Env> {
    CHILD_ENV.try_with(Arc::clone).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bash tests that run without a session leave `$NEXO_TMP` of the process in the temp folder: it goes
    /// when the test binary exits. (Registered by a test, so a run that filters this one out does not sweep.)
    #[cfg(unix)]
    #[test]
    fn the_scratch_folder_of_the_tests_goes_when_the_process_exits() {
        extern "C" fn sweep() {
            bash::wipe_tmp("");
        }
        // SAFETY: `sweep` is a plain `extern "C"` function that outlives the process.
        assert_eq!(unsafe { libc::atexit(sweep) }, 0);
    }

    #[test]
    fn the_builtin_set_has_its_tools_with_schemas() {
        let set = builtin();
        let names: Vec<&str> = set.names().collect();
        assert_eq!(
            names,
            vec![
                "bash",
                "browser",
                "diagnostics",
                "edit",
                "glob",
                "grep",
                "jobs",
                "ls",
                "note",
                "read",
                "recall",
                "todo",
                "web_fetch",
                "web_search",
                "write"
            ]
        );
        let off = builtin_with(&WebConfig { enabled: false, ..Default::default() });
        assert_eq!(off.names().count(), 13, "no web tools when disabled");
        for spec in set.specs() {
            assert_eq!(spec.parameters["type"], "object", "{}", spec.name);
            assert!(!spec.description.is_empty(), "{}", spec.name);
        }
    }
}
