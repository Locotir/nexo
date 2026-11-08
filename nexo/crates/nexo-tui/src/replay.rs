//! How a replayed user message is shown. The model got more than the person typed: notes of ended
//! jobs, memory shared by other sessions, attached files in full, a summary after a compaction.
//! Only the screen changes here; the history keeps the exact text.

const JOBS_NOTE: &str = "[background jobs ended]\n";
const JOBS_END: &str = "Read the output with the `jobs` tool";
const MEMORY_START: &str = "<memory from=\"";
const MEMORY_END: &str = "</memory>";
const ATTACHED: &str = "(attached in full, as it is now):";
use nexo_core::agent::{
    GOAL_CONTINUE_MARK, GOAL_SET_MARK, HOOK_CONTEXT_END, HOOK_CONTEXT_START, SUPERVISOR_RESUME_MARK,
};

/// One part of a user message, as the screen shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    /// A note of ended background jobs, whole (a card).
    JobNote(String),
    /// What hooks added for the model: how many lines.
    Hook(usize),
    /// A block of memory shared from another session: its title and about how many tokens.
    Memory { title: String, tokens: u64 },
    /// What replaced the conversation before a compaction: the number of lines it takes.
    Summary(usize),
    /// The loop's own request for a goal: the person's words when it was set, None when it keeps it going.
    Goal(Option<String>),
    /// What the person wrote, attachments shortened to a line each.
    Text(String),
}

/// Splits a user message into what the harness put in front of the person's own words and the words.
pub fn split_user(text: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut t = text.trim_start();
    loop {
        if t.starts_with(JOBS_NOTE) {
            let Some(i) = t.find(JOBS_END) else { break };
            let end = t[i..].find('\n').map_or(t.len(), |n| i + n);
            out.push(Piece::JobNote(t[..end].to_string()));
            t = t[end..].trim_start();
        } else if t.starts_with(HOOK_CONTEXT_START) {
            let Some(i) = t.find(HOOK_CONTEXT_END) else { break };
            let inner = t[HOOK_CONTEXT_START.len()..i].trim();
            out.push(Piece::Hook(inner.lines().count()));
            t = t[i + HOOK_CONTEXT_END.len()..].trim_start();
        } else if t.starts_with(MEMORY_START) {
            let Some(i) = t.find(MEMORY_END) else { break };
            let block = &t[..i + MEMORY_END.len()];
            let title = block
                .strip_prefix(MEMORY_START)
                .and_then(|r| r.split_once("title=\""))
                .and_then(|(_, r)| r.split_once('"'))
                .map(|(title, _)| title.trim().to_string())
                .unwrap_or_default();
            out.push(Piece::Memory { title, tokens: (block.len() as u64).div_ceil(4) });
            t = t[block.len()..].trim_start();
        } else {
            break;
        }
    }
    if t.starts_with("[summary]") {
        out.push(Piece::Summary(t.lines().count()));
    } else if let Some(words) = t.strip_prefix(GOAL_SET_MARK) {
        out.push(Piece::Goal(Some(words.trim().to_string())));
    } else if t.trim_end() == GOAL_CONTINUE_MARK {
        out.push(Piece::Goal(None));
    } else if t.trim_end() == SUPERVISOR_RESUME_MARK {
        out.push(Piece::JobNote("supervisor: continuing after a check-in".into()));
    } else if !t.is_empty() {
        out.push(Piece::Text(collapse_attachments(t)));
    }
    out
}

/// The text with each file attached whole (`@path (attached in full...):` and its fence) reduced to
/// `@path · N lines`.
pub fn collapse_attachments(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        let path = l.strip_prefix('@').and_then(|r| r.strip_suffix(ATTACHED)).map(str::trim_end);
        let fence =
            lines.get(i + 1).filter(|f| f.starts_with("```")).map(|f| f.len() - f.trim_start_matches('`').len());
        if let (Some(path), Some(n)) = (path, fence) {
            let close = "`".repeat(n);
            if let Some(k) = lines[i + 2..].iter().position(|x| x.trim_end() == close) {
                out.push(format!("@{path} · {}", nexo_core::count(k, "line")));
                i += k + 3;
                continue;
            }
        }
        out.push(l.to_string());
        i += 1;
    }
    out.join("\n")
}

/// The name of a tool as the card head shows it: `mcp__server__tool` reads `server · tool`.
pub fn tool_label(name: &str) -> String {
    match name.strip_prefix("mcp__").and_then(|r| r.split_once("__")) {
        Some((server, tool)) if !server.is_empty() && !tool.is_empty() => format!("{server} · {tool}"),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "[background jobs ended]\n- job 3 `make` done (exit 0) after 4s, 2 lines of output\nRead the output with the `jobs` tool (action \"output\", the job's id).";

    #[test]
    fn goal_requests_of_the_loop_are_not_the_persons_words() {
        assert_eq!(split_user("[goal set] make it fast"), vec![Piece::Goal(Some("make it fast".into()))]);
        assert_eq!(split_user("[goal: continue]"), vec![Piece::Goal(None)]);
        assert_eq!(split_user("my [goal set] idea"), vec![Piece::Text("my [goal set] idea".into())]);
    }

    #[test]
    fn hook_context_is_not_shown_as_the_persons_words() {
        let p = split_user(
            "<hook-context>\nLocal time: 22:51.\nSince the previous prompt: 10 min.\n</hook-context>\n\nfix the bug",
        );
        assert_eq!(p, vec![Piece::Hook(2), Piece::Text("fix the bug".into())]);
    }

    #[test]
    fn a_job_note_glued_to_the_typed_text_is_split_from_it() {
        let p = split_user(&format!("{NOTE}\n\nnow fix the test"));
        assert_eq!(p, vec![Piece::JobNote(NOTE.into()), Piece::Text("now fix the test".into())]);
    }

    #[test]
    fn a_job_note_after_a_memory_block_is_found() {
        let mem = "<memory from=\"s-1\" title=\"the parser fix\">\nShared from another session.\n</memory>";
        let p = split_user(&format!("{mem}\n\n{NOTE}\n\nhello"));
        assert!(matches!(&p[0], Piece::Memory { title, tokens } if title == "the parser fix" && *tokens > 0), "{p:?}");
        assert_eq!(p[1], Piece::JobNote(NOTE.into()));
        assert_eq!(p[2], Piece::Text("hello".into()));
    }

    #[test]
    fn a_summary_is_one_piece() {
        let p = split_user("[summary] The conversation before this point was replaced\n[request] fix it\n[files]");
        assert_eq!(p, vec![Piece::Summary(3)]);
    }

    #[test]
    fn an_attached_file_is_a_chip_not_its_body() {
        let t =
            "look at this\n\n@src/x.rs (attached in full, as it is now):\n```rs\nfn a() {}\nfn b() {}\n```\n\nthanks";
        assert_eq!(collapse_attachments(t), "look at this\n\n@src/x.rs · 2 lines\n\nthanks");
        let long = "@a.md (attached in full, as it is now):\n````md\n```\ncode\n```\n````";
        assert_eq!(collapse_attachments(long), "@a.md · 3 lines");
        // not closed: left as it is
        let open = "@a.md (attached in full, as it is now):\n```md\nx";
        assert_eq!(collapse_attachments(open), open);
    }

    #[test]
    fn plain_text_is_untouched() {
        assert_eq!(split_user("just a question"), vec![Piece::Text("just a question".into())]);
    }

    #[test]
    fn mcp_tools_read_server_dot_tool() {
        assert_eq!(tool_label("mcp__github__create_issue"), "github · create_issue");
        assert_eq!(tool_label("read"), "read");
        assert_eq!(tool_label("mcp__broken"), "mcp__broken");
    }
}
