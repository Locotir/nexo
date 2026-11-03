//! What the agent is doing right now, as one word for the separator: derived from the live
//! block (the tool being run, the text being written) or from a pending question.

/// The agent's phase. `label` is the word the activity line shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Thinking,
    Reading,
    Searching,
    Editing,
    Running,
    Writing,
    Waiting,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Thinking => "Thinking",
            Phase::Reading => "Reading",
            Phase::Searching => "Searching",
            Phase::Editing => "Editing",
            Phase::Running => "Running",
            Phase::Writing => "Writing",
            Phase::Waiting => "Waiting",
        }
    }
}

/// The phase a tool call puts the agent in, by the tool's name.
pub fn phase_of_tool(name: &str) -> Phase {
    match name {
        "read" | "web_fetch" => Phase::Reading,
        "write" | "edit" | "todo" | "note" => Phase::Editing,
        "grep" | "glob" | "ls" | "web_search" => Phase::Searching,
        "task" => Phase::Waiting,
        _ => Phase::Running,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_map_to_their_phase() {
        assert_eq!(phase_of_tool("read"), Phase::Reading);
        assert_eq!(phase_of_tool("web_fetch"), Phase::Reading);
        assert_eq!(phase_of_tool("edit"), Phase::Editing);
        assert_eq!(phase_of_tool("write"), Phase::Editing);
        assert_eq!(phase_of_tool("grep"), Phase::Searching);
        assert_eq!(phase_of_tool("glob"), Phase::Searching);
        assert_eq!(phase_of_tool("web_search"), Phase::Searching);
        assert_eq!(phase_of_tool("bash"), Phase::Running);
        assert_eq!(phase_of_tool("mcp__x__y"), Phase::Running);
        assert_eq!(phase_of_tool("task"), Phase::Waiting);
    }

    #[test]
    fn labels_are_single_words() {
        for p in [
            Phase::Thinking,
            Phase::Reading,
            Phase::Searching,
            Phase::Editing,
            Phase::Running,
            Phase::Writing,
            Phase::Waiting,
        ] {
            assert!(!p.label().contains(' '));
        }
    }
}
