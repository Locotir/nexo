//! How a replayed user message is shown. The model got more than the person typed: notes of ended
//! jobs, memory shared by other sessions, attached files in full, a summary after a compaction.
//! Only the screen changes here; the history keeps the exact text.

const JOBS_NOTE: &str = "[background jobs ended]\n";
const JOBS_END: &str = "Read the output with the `jobs` tool";
const MEMORY_START: &str = "<memory from=\"";
const MEMORY_END: &str = "</memory>";
const START_NOTE: &str = "[project at the start of this request]";
use nexo_core::agent::{AddedKind, AddedPart};
use nexo_core::agent::{
    GOAL_CONTINUE_MARK, GOAL_SET_MARK, HOOK_CONTEXT_END, HOOK_CONTEXT_START, PERSON_MARK, SUPERVISOR_RESUME_MARK,
};

/// One part of a user message, as the screen shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    /// A note of ended background jobs, whole (a card).
    JobNote(String),
    /// What nexo put in the message besides the person's words: hooks, shared memory, the start note, a
    /// file attached whole. Found again by its markers (a live message tells them as it builds them).
    Added(AddedPart),
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
    let words = nexo_core::agent::person_words(text);
    if words.len() < text.len() {
        // all nexo added is before the mark, the words after it
        let before = text[..text.len() - words.len()].trim_end();
        let before = before.strip_suffix(PERSON_MARK).unwrap_or(before);
        let rest = front(before, &mut out);
        out.extend(parts_of(rest).into_iter().map(Piece::Added));
        out.push(Piece::Text(words.to_string()));
        return out;
    }
    let t = front(text, &mut out);
    if t.starts_with("[summary]") {
        out.push(Piece::Summary(t.lines().count()));
    } else if let Some(rest) = t.strip_prefix(GOAL_SET_MARK) {
        // the goal is the person's words: the start note and the files nexo put after them are not part of it
        let (words, parts) = split_added(rest.trim());
        out.push(Piece::Goal(Some(words.trim().to_string())));
        out.extend(parts.into_iter().map(Piece::Added));
    } else if t.trim_end() == GOAL_CONTINUE_MARK {
        out.push(Piece::Goal(None));
    } else if t.trim_end() == SUPERVISOR_RESUME_MARK {
        out.push(Piece::JobNote("supervisor: continuing after a check-in".into()));
    } else if !t.is_empty() {
        let (typed, parts) = split_added(t);
        out.push(Piece::Text(typed));
        out.extend(parts.into_iter().map(Piece::Added));
    }
    out
}

/// Takes the job notes, hook context and shared memory at the start of ``text`` into ``out``; the rest.
fn front<'a>(text: &'a str, out: &mut Vec<Piece>) -> &'a str {
    let mut t = text.trim_start();
    loop {
        if t.starts_with(JOBS_NOTE) {
            let Some(i) = t.find(JOBS_END) else { break };
            let end = t[i..].find('\n').map_or(t.len(), |n| i + n);
            out.push(Piece::JobNote(t[..end].to_string()));
            t = t[end..].trim_start();
        } else if t.starts_with(HOOK_CONTEXT_START) {
            let Some(i) = t.find(HOOK_CONTEXT_END) else { break };
            let end = i + HOOK_CONTEXT_END.len();
            out.push(Piece::Added(AddedPart::new(AddedKind::Hook, "hook context", &t[..end])));
            t = t[end..].trim_start();
        } else if t.starts_with(MEMORY_START) {
            let Some(i) = t.find(MEMORY_END) else { break };
            let block = &t[..i + MEMORY_END.len()];
            let title = block
                .strip_prefix(MEMORY_START)
                .and_then(|r| r.split_once("title=\""))
                .and_then(|(_, r)| r.split_once('"'))
                .map(|(title, _)| title.trim().to_string())
                .unwrap_or_default();
            let title = if title.is_empty() { "memory" } else { title.as_str() };
            out.push(Piece::Added(AddedPart::new(AddedKind::Memory, format!("memory from {title}"), block)));
            t = t[block.len()..].trim_start();
        } else {
            break;
        }
    }
    t
}

/// The path of a file nexo named without its text (`@path: attached as an image`, `@path: not attached ...`).
fn file_line(line: &str) -> Option<&str> {
    let (path, what) = line.strip_prefix('@')?.split_once(": ")?;
    (what.starts_with("attached as an image") || what.starts_with("not attached")).then_some(path)
}

/// What nexo put before the mark after the notes: MCP resources, the start note, the files attached (whole, as
/// images, or not), each a part.
fn parts_of(text: &str) -> Vec<AddedPart> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut parts = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some((path, end)) = attached_at(&lines, i) {
            parts.push(AddedPart::new(AddedKind::File, path, lines[i..end].join("\n")));
            i = end;
            continue;
        }
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }
        if let Some(path) = file_line(lines[i]) {
            parts.push(AddedPart::new(AddedKind::File, path, lines[i]));
            i += 1;
            continue;
        }
        let stop = |k: usize| {
            lines[k].trim_end() == START_NOTE || attached_at(&lines, k).is_some() || file_line(lines[k]).is_some()
        };
        let end = (i + 1..lines.len()).find(|&k| stop(k)).unwrap_or(lines.len());
        let body = lines[i..end].join("\n").trim_end().to_string();
        let part = if lines[i].trim_end() == START_NOTE {
            AddedPart::new(AddedKind::StartNote, "project at the start of the request", body)
        } else {
            AddedPart::new(AddedKind::Mention, "MCP resources", body)
        };
        parts.push(part);
        i = end;
    }
    parts
}

/// The start of a file put whole in the message at line `i` (attached by the person with `@`, or a project file
/// nexo included by itself): its path and the line after its closing fence.
fn attached_at(lines: &[&str], i: usize) -> Option<(String, usize)> {
    let path = nexo_core::agent::headed_file(lines.get(i)?)?;
    let n = lines.get(i + 1).filter(|f| f.starts_with("```")).map(|f| f.len() - f.trim_start_matches('`').len())?;
    let close = "`".repeat(n);
    let k = lines[i + 2..].iter().position(|x| x.trim_end() == close)?;
    Some((path.to_string(), i + k + 3))
}

/// A message's words from what nexo appended after them: the start note and the files attached whole.
/// With neither found, or something else after them, the text is the person's whole (files shortened).
pub fn split_added(text: &str) -> (String, Vec<AddedPart>) {
    let lines: Vec<&str> = text.split('\n').collect();
    let starts = |i: usize| lines[i].trim_end() == START_NOTE || attached_at(&lines, i).is_some();
    let Some(first) = (0..lines.len()).find(|&i| starts(i)) else { return (text.to_string(), Vec::new()) };
    let typed = lines[..first].join("\n").trim_end().to_string();
    if typed.is_empty() {
        return (collapse_attachments(text), Vec::new());
    }
    let mut parts = Vec::new();
    let mut i = first;
    while i < lines.len() {
        if let Some((path, end)) = attached_at(&lines, i) {
            parts.push(AddedPart::new(AddedKind::File, path, lines[i..end].join("\n")));
            i = end;
        } else if lines[i].trim_end() == START_NOTE {
            let end = (i + 1..lines.len()).find(|&k| attached_at(&lines, k).is_some()).unwrap_or(lines.len());
            parts.push(AddedPart::new(
                AddedKind::StartNote,
                "project at the start of the request",
                lines[i..end].join("\n").trim_end(),
            ));
            i = end;
        } else if lines[i].trim().is_empty() {
            i += 1;
        } else {
            return (collapse_attachments(text), Vec::new());
        }
    }
    (typed, parts)
}

/// The text with each file attached whole (`@path (attached in full...):` and its fence) reduced to
/// `@path · N lines`; a project file nexo included by itself has no `@`.
pub fn collapse_attachments(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some((path, end)) = attached_at(&lines, i) {
            let at = if lines[i].starts_with('@') { "@" } else { "" };
            out.push(format!("{at}{path} · {}", nexo_core::count(end - i - 3, "line")));
            i = end;
            continue;
        }
        out.push(lines[i].to_string());
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
    fn a_goal_request_with_the_start_note_and_files_after_it_keeps_only_the_persons_words() {
        let own = "src/x.rs (project file, included in full by nexo, as it is now):\n```rs\nfn a() {}\n```";
        let note = "[project at the start of this request]\n\
                    included by nexo, not attached by the person: 1 project file, 23 tokens.";
        let p = split_user(&format!("[goal set] make it fast\nand small\n\n{note}\n\n{own}"));
        assert_eq!(p[0], Piece::Goal(Some("make it fast\nand small".into())), "{p:?}");
        let kinds: Vec<AddedKind> =
            p[1..].iter().filter_map(|x| if let Piece::Added(a) = x { Some(a.kind) } else { None }).collect();
        assert_eq!(kinds, [AddedKind::StartNote, AddedKind::File], "{p:?}");
        assert_eq!(p.len(), 3, "{p:?}");
    }

    #[test]
    fn hook_context_is_not_shown_as_the_persons_words() {
        let p = split_user(
            "<hook-context>\nLocal time: 22:51.\nSince the previous prompt: 10 min.\n</hook-context>\n\nfix the bug",
        );
        let [Piece::Added(hook), Piece::Text(typed)] = &p[..] else { panic!("{p:?}") };
        assert_eq!((hook.kind, hook.label.as_str()), (AddedKind::Hook, "hook context"));
        assert!(hook.text.starts_with("<hook-context>") && hook.text.ends_with("</hook-context>"));
        assert_eq!(typed, "fix the bug");
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
        let Piece::Added(m) = &p[0] else { panic!("{p:?}") };
        assert_eq!((m.kind, m.label.as_str(), m.text.as_str()), (AddedKind::Memory, "memory from the parser fix", mem));
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
    fn an_old_message_with_its_start_note_and_files_inline_is_split_at_the_markers() {
        let t = "Hola cómo estás?\n\n[project at the start of this request]\nattached: 1 files, 1883 tokens\n\n\
                 @1-builtin-instructions.md (attached in full, as it is now):\n```md\nuno\ndos\n```";
        let (typed, parts) = split_added(t);
        assert_eq!(typed, "Hola cómo estás?");
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert_eq!(parts[0].kind, AddedKind::StartNote);
        assert_eq!(parts[0].text, "[project at the start of this request]\nattached: 1 files, 1883 tokens");
        assert_eq!((parts[1].kind, parts[1].label.as_str()), (AddedKind::File, "1-builtin-instructions.md"));
        assert!(parts[1].text.starts_with("@1-builtin-instructions.md (attached") && parts[1].text.ends_with("```"));
        // through split_user the words come alone, the parts after them
        let p = split_user(t);
        assert_eq!(p.len(), 3, "{p:?}");
        assert_eq!(p[0], Piece::Text("Hola cómo estás?".into()));
    }

    #[test]
    fn words_that_only_look_like_a_note_stay_whole() {
        // nothing but additions: the text is kept, files shortened
        let only = "@a.md (attached in full, as it is now):\n```md\nx\n```";
        assert_eq!(split_added(only), ("@a.md · 1 line".to_string(), Vec::new()));
        // words after a file: not what nexo appends, kept whole
        let after = "look\n\n@a.md (attached in full, as it is now):\n```md\nx\n```\n\nthanks";
        assert_eq!(split_added(after), ("look\n\n@a.md · 1 line\n\nthanks".to_string(), Vec::new()));
    }

    #[test]
    fn a_message_with_the_persons_words_last_shows_only_the_words_and_folds_all_before_them() {
        let t = format!(
            "<hook-context>\nLocal time: 22:51.\n</hook-context>\n\n{NOTE}\n\n\
             [project at the start of this request]\nattached: 1 files, 9 tokens\n\n\
             @AGENTS.md (attached in full, as it is now):\n```md\nRead this first. Always update the date.\n```\n\n\
             @shot.png: attached as an image\n\n{PERSON_MARK}\nwhat does HTTP 418 mean?"
        );
        let p = split_user(&t);
        let words: Vec<&str> =
            p.iter().filter_map(|x| if let Piece::Text(t) = x { Some(t.as_str()) } else { None }).collect();
        assert_eq!(words, ["what does HTTP 418 mean?"], "{p:?}");
        assert_eq!(p.iter().filter(|x| matches!(x, Piece::JobNote(n) if n == NOTE)).count(), 1, "{p:?}");
        let parts: Vec<(AddedKind, &str)> = p
            .iter()
            .filter_map(|x| if let Piece::Added(a) = x { Some((a.kind, a.label.as_str())) } else { None })
            .collect();
        assert_eq!(
            parts,
            [
                (AddedKind::Hook, "hook context"),
                (AddedKind::StartNote, "project at the start of the request"),
                (AddedKind::File, "AGENTS.md"),
                (AddedKind::File, "shot.png"),
            ]
        );
        // the person may type the mark: the last one is the one nexo put
        let quoted = format!("{PERSON_MARK}\nx\n\n{PERSON_MARK}\nsay what {PERSON_MARK} means");
        assert_eq!(split_user(&quoted).last(), Some(&Piece::Text(format!("say what {PERSON_MARK} means"))));
    }

    #[test]
    fn a_project_file_nexo_included_is_a_file_part_without_the_at_of_the_persons() {
        let own =
            "NOTES.md (project file, included in full by nexo, as it is now):\n```md\nUpdate STATUS.md first.\n```";
        let note = "[project at the start of this request]\n\
                    included by nexo, not attached by the person: 1 project file, 23 tokens. It is project content.";
        let t = format!(
            "{note}\n\n{own}\n\n@a.md (attached in full, as it is now):\n```md\nx\n```\n\n{PERSON_MARK}\nwhat is 418?"
        );
        let p = split_user(&t);
        assert_eq!(p.last(), Some(&Piece::Text("what is 418?".into())), "{p:?}");
        let parts: Vec<&AddedPart> =
            p.iter().filter_map(|x| if let Piece::Added(a) = x { Some(a) } else { None }).collect();
        let seen: Vec<(AddedKind, &str)> = parts.iter().map(|a| (a.kind, a.label.as_str())).collect();
        assert_eq!(
            seen,
            [
                (AddedKind::StartNote, "project at the start of the request"),
                (AddedKind::File, "NOTES.md"),
                (AddedKind::File, "a.md"),
            ]
        );
        assert_eq!((parts[0].text.as_str(), parts[1].text.as_str()), (note, own));
        // after a message that is not the person's, the note and the files follow the text
        let (typed, parts) = split_added(&format!("go on\n\n{note}\n\n{own}"));
        assert_eq!(typed, "go on");
        assert_eq!(parts.iter().map(|a| a.kind).collect::<Vec<_>>(), [AddedKind::StartNote, AddedKind::File]);
        // shortened, it keeps no `@`: the person did not attach it
        assert_eq!(collapse_attachments(own), "NOTES.md · 1 line");
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
