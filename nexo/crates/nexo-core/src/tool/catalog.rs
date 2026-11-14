//! What the harness knows about its native tools by name: the lists that were once kept next to the code
//! that reads them. A new tool is added here, once, instead of in each caller.

/// Tools that touch nothing outside the conversation: never asked about (a `deny` rule still holds).
pub const ALWAYS_ALLOWED: &[&str] = &["todo", "note", "recall", "ask_user", "exit_plan"];

/// The tools an ultracode planning round may call. Conservative allowlist: unknown, network-mutating and
/// execution tools are not planning tools.
pub const ULTRACODE_PLANNING: &[&str] =
    &["ultracode_execute", "read", "glob", "grep", "ls", "recall", "web_search", "web_fetch"];

/// The tools whose calls only read, for the watcher that tells reading from changing.
pub const READING: &[&str] = &["read", "grep", "glob", "ls", "recall"];

/// The tools a window of the compact size or less is not offered: each costs a description in every
/// request and starts work (a second context, a list to keep, notes) that there is no room for.
pub const SMALL_LEAVES_OUT: [&str; 8] = ["task", "workflow", "todo", "note", "debug", "bin", "diagnostics", "browser"];

/// The tools a tiny window is not offered besides those: skills and the web.
pub const TINY_LEAVES_OUT: [&str; 3] = ["skill", "web_fetch", "web_search"];

/// Tools whose presence makes the prompt teach independent calls in one message.
pub const LOOKING: [&str; 7] = ["read", "grep", "glob", "ls", "bash", "web_search", "web_fetch"];

/// What a sub-agent profile gets when none of the tools it lists is known.
pub const SUBAGENT_FALLBACK: [&str; 4] = ["read", "grep", "glob", "ls"];

/// The tools a sub-agent kind that lists none does not get: note, todo and recall are the parent's, and so
/// are the mail tools.
pub const PARENT_ONLY: &[&str] = &["note", "todo", "recall", "send_message", "send_to_session", "list_sessions"];
