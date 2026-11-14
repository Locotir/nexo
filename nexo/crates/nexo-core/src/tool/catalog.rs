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

/// What a native tool does for the person watching: the one answer the TUI's phase line and its call counts
/// both read, so a new tool is placed here once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Reads a file.
    ReadFile,
    /// Reads a web page.
    ReadPage,
    /// Changes files.
    Edit,
    /// Keeps the model's own notes or list (todo, note).
    Note,
    /// Runs a shell command.
    Shell,
    /// Looks for text or paths by pattern.
    FindPattern,
    /// Lists a directory.
    FindDir,
    /// Searches the web.
    FindWeb,
    /// Starts a sub-agent.
    Agent,
    /// Any other tool, including MCP tools.
    Other,
}

/// The role of a tool by name; names it does not know are `Other`.
pub fn role(name: &str) -> Role {
    match name {
        "read" => Role::ReadFile,
        "web_fetch" => Role::ReadPage,
        "write" | "edit" => Role::Edit,
        "todo" | "note" => Role::Note,
        "bash" => Role::Shell,
        "grep" | "glob" => Role::FindPattern,
        "ls" => Role::FindDir,
        "web_search" => Role::FindWeb,
        "task" => Role::Agent,
        _ => Role::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_tool_name_keeps_its_role() {
        let table = [
            ("read", Role::ReadFile),
            ("web_fetch", Role::ReadPage),
            ("write", Role::Edit),
            ("edit", Role::Edit),
            ("todo", Role::Note),
            ("note", Role::Note),
            ("bash", Role::Shell),
            ("grep", Role::FindPattern),
            ("glob", Role::FindPattern),
            ("ls", Role::FindDir),
            ("web_search", Role::FindWeb),
            ("task", Role::Agent),
            ("mcp__x__y", Role::Other),
            ("", Role::Other),
        ];
        for (name, want) in table {
            assert_eq!(role(name), want, "{name}");
        }
    }
}
