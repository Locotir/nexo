//! Bounded, read-only snapshots of persisted session records. Call off the UI thread.
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const LIMIT: u64 = 256 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct HistorySnapshot {
    pub records: Vec<String>,
    pub older_omitted: bool,
    /// A record crossing the beginning of the window was omitted in its entirety.
    pub partial_record_omitted: bool,
    pub incomplete_tail: bool,
    /// Complete lines in the window that could not be read back (garbled bytes or a payload of the wrong shape).
    /// They are skipped so one bad line, for example after a crash mid-write, does not hide the rest of the history.
    pub unreadable_records: usize,
}

/// Reads only the most recent 256 KiB. A partial first or last record is never presented as complete.
/// The result is persisted records, not a live transcript or a reconstruction after reset.
pub fn history(id: &str) -> anyhow::Result<HistorySnapshot> {
    history_in(&super::dir(), id)
}

fn history_in(dir: &Path, id: &str) -> anyhow::Result<HistorySnapshot> {
    let id = super::checked_id(id)?;
    let path = dir.join(format!("{id}.jsonl"));
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = opts.open(path)?;
    let meta = file.metadata()?;
    anyhow::ensure!(meta.is_file(), "session is not a regular file");
    let start = meta.len().saturating_sub(LIMIT);
    let partial_first = if start > 0 {
        file.seek(SeekFrom::Start(start - 1))?;
        let mut previous = [0];
        file.read_exact(&mut previous)?;
        previous[0] != b'\n'
    } else {
        false
    };
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(LIMIT).read_to_end(&mut bytes)?;
    let incomplete_tail = !bytes.is_empty() && !bytes.ends_with(b"\n");
    let mut lines = bytes.split(|b| *b == b'\n').peekable();
    if partial_first {
        lines.next();
    }
    let mut records = Vec::new();
    let mut unreadable_records = 0;
    while let Some(line) = lines.next() {
        if lines.peek().is_none() || line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            unreadable_records += 1;
            continue;
        };
        match value.get("type").and_then(|v| v.as_str()) {
            Some("message") => {
                let Ok(message) = serde_json::from_value::<super::Message>(value["message"].clone()) else {
                    unreadable_records += 1;
                    continue;
                };
                let role = match message.role {
                    super::Role::User => "User",
                    super::Role::Assistant => "Assistant",
                };
                for part in &message.content {
                    render_part(part, role, &mut records);
                }
            }
            Some("tool_output") => {
                let Ok(output) = serde_json::from_value::<super::ToolOutput>(value["output"].clone()) else {
                    unreadable_records += 1;
                    continue;
                };
                records.push(clean(&format!(
                    "Tool output{}: {}",
                    if output.is_error { " (error)" } else { "" },
                    output.content
                )));
            }
            Some("reset") => {
                records.push("History reset: following records replace earlier conversation context.".into())
            }
            _ => {}
        }
    }
    Ok(HistorySnapshot {
        records,
        older_omitted: start > 0,
        partial_record_omitted: partial_first,
        incomplete_tail,
        unreadable_records,
    })
}

// Never render terminal controls, bidirectional formatting, or opaque image/reasoning data.
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| {
            if (c.is_control() && c != '\n' && c != '\t')
                || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                '�'
            } else {
                c
            }
        })
        .collect()
}

fn render_part(part: &super::Part, role: &str, out: &mut Vec<String>) {
    match part {
        super::Part::Text { text } => out.push(clean(&format!("{role}: {text}"))),
        super::Part::Reasoning { text, .. } => out.push(clean(&format!("Reasoning: {text}"))),
        super::Part::RedactedReasoning { .. } => out.push("Reasoning: [redacted]".into()),
        super::Part::ToolCall { name, .. } => out.push(clean(&format!("Tool call: {name} (arguments omitted)"))),
        super::Part::ToolResult { content, is_error, .. } => {
            let label = if *is_error { "Tool result (error)" } else { "Tool result" };
            for p in content {
                render_part(p, label, out);
            }
        }
        super::Part::Image { .. } => out.push(format!("{role}: [image omitted]")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_partial_and_invalid_ids() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("abc.jsonl"), b"{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n{\"type\":\"reset\"}\n{\"type\":").unwrap();
        let s = history_in(dir.path(), "abc").unwrap();
        assert_eq!(s.records.len(), 2);
        assert!(s.incomplete_tail);
        assert!(history_in(dir.path(), "../abc").is_err());
        std::fs::write(dir.path().join("abc.jsonl"), vec![b'x'; LIMIT as usize + 1]).unwrap();
        let s = history_in(dir.path(), "abc").unwrap();
        assert!(s.older_omitted && s.incomplete_tail);
    }
    #[test]
    fn oversized_record_is_reported_even_without_complete_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc.jsonl");
        let mut data = b"{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"".to_vec();
        data.extend(vec![b'x'; LIMIT as usize + 1]);
        data.extend_from_slice(b"\"}}\n");
        std::fs::write(&path, &data).unwrap();
        let snapshot = history_in(dir.path(), "abc").unwrap();
        assert!(snapshot.records.is_empty());
        assert!(snapshot.partial_record_omitted && snapshot.older_omitted);
        assert!(!snapshot.incomplete_tail);
        data.extend_from_slice(b"{\"type\":\"reset\"}\n");
        std::fs::write(&path, data).unwrap();
        let snapshot = history_in(dir.path(), "abc").unwrap();
        assert!(snapshot.partial_record_omitted);
        assert_eq!(snapshot.records.len(), 1);
    }
    #[test]
    fn growth_reset_and_malformed_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc.jsonl");
        std::fs::write(&path, b"{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"first\"}}\n")
            .unwrap();
        assert_eq!(history_in(dir.path(), "abc").unwrap().records.len(), 1);
        std::fs::write(
            &path,
            b"{\"type\":\"reset\"}\n{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"content\":\"next\"}}\n",
        )
        .unwrap();
        let snapshot = history_in(dir.path(), "abc").unwrap();
        assert_eq!(snapshot.records.len(), 2);
        assert!(snapshot.records[0].contains("reset"));
        std::fs::write(&path, b"broken\n").unwrap();
        let snapshot = history_in(dir.path(), "abc").unwrap();
        assert!(snapshot.records.is_empty());
        assert_eq!(snapshot.unreadable_records, 1);
    }
    #[test]
    fn a_garbled_middle_line_is_counted_and_the_records_around_it_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let data = concat!(
            r#"{"type":"message","message":{"role":"user","content":"before"}}"#,
            "\n",
            "{\"type\":\"mess\u{0}garbled\n",
            r#"{"type":"message","message":{"role":"user","content":"after"}}"#,
            "\n"
        );
        std::fs::write(dir.path().join("abc.jsonl"), data).unwrap();
        let snapshot = history_in(dir.path(), "abc").unwrap();
        assert_eq!(snapshot.records, vec!["User: before".to_string(), "User: after".to_string()]);
        assert_eq!(snapshot.unreadable_records, 1);
    }
    #[test]
    fn a_valid_line_with_a_bad_payload_is_counted_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let data = concat!(
            r#"{"type":"message","message":{"role":"nobody","content":5}}"#,
            "\n",
            r#"{"type":"tool_output","output":"not an object"}"#,
            "\n",
            r#"{"type":"reset"}"#,
            "\n"
        );
        std::fs::write(dir.path().join("abc.jsonl"), data).unwrap();
        let snapshot = history_in(dir.path(), "abc").unwrap();
        assert_eq!(snapshot.records.len(), 1);
        assert_eq!(snapshot.unreadable_records, 2);
    }
    #[test]
    fn readable_parts_and_controls_without_opaque_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let data = concat!(
            r#"{"type":"message","index":0,"ts":"now","message":{"role":"assistant","content":[{"type":"text","text":"hello\u001b[2J"},{"type":"tool_use","id":"x","name":"read","input":{"secret":"hidden"}},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"opaque"}}]}}"#,
            "\n",
            r#"{"type":"tool_output","call_id":"x","output":{"content":"found","is_error":false,"display":{"kind":"plain"}}}"#,
            "\n"
        );
        std::fs::write(dir.path().join("abc.jsonl"), data).unwrap();
        let joined = history_in(dir.path(), "abc").unwrap().records.join("\n");
        assert!(joined.contains("Assistant: hello�[2J"));
        assert!(joined.contains("Tool call: read"));
        assert!(joined.contains("Tool output: found"));
        assert!(!joined.contains("hidden") && !joined.contains("opaque"));
    }

    #[test]
    fn boundary_at_newline_keeps_first_complete_record() {
        let dir = tempfile::tempdir().unwrap();
        let record = b"{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"boundary\"}}\n";
        let mut data = vec![b' '; LIMIT as usize + 1 - record.len()];
        data[0] = b'\n';
        data[1..1 + record.len()].copy_from_slice(record);
        // Fill remaining bytes with harmless complete empty lines.
        data[1 + record.len()..].fill(b'\n');
        std::fs::write(dir.path().join("abc.jsonl"), data).unwrap();
        assert!(history_in(dir.path(), "abc").unwrap().records.iter().any(|r| r.contains("boundary")));
    }
    #[cfg(unix)]
    #[test]
    fn rejects_symlink() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("abc.jsonl")).unwrap();
        assert!(history_in(dir.path(), "abc").is_err());
    }
}
