//! `edit`: replace one exact occurrence of a text in a file.

use async_trait::async_trait;
use nexo_core::{Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};
use similar::{ChangeTag, TextDiff};

use crate::util::{bool_arg, display_path, is_binary, outside_note, resolve, str_arg, MAX_FILE_BYTES};

/// Lines of the unified diff shown to the model.
const DIFF_LINES: usize = 80;

pub struct Edit;

#[async_trait]
impl Tool for Edit {
    fn name(&self) -> &str {
        "edit"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description: "Replace one exact occurrence of `old_string` in a file with `new_string`. \
                          `old_string` must match the file's text exactly (indentation included) and \
                          occur exactly once; include a few surrounding lines to make it unique, or set \
                          `replace_all` to change every occurrence (renames). Read the file first so the \
                          text is current. Answers with the lines changed and a unified diff."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "The file to edit, absolute or relative to the working directory."},
                    "old_string": {"type": "string", "description": "The exact text to replace."},
                    "new_string": {"type": "string", "description": "The text that replaces it (empty to delete)."},
                    "replace_all": {"type": "boolean", "description": "Replace every occurrence. Default false."}
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Write
    }

    fn summary(&self, args: &Value) -> String {
        match str_arg(args, "path") {
            Some(p) if bool_arg(args, "replace_all") => format!("edit {p} (all occurrences)"),
            Some(p) => format!("edit {p}"),
            None => "edit (no path)".into(),
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(p) = str_arg(&args, "path") else {
            return ToolOutput::error("edit: `path` is required");
        };
        let (Some(old), Some(new)) =
            (args.get("old_string").and_then(Value::as_str), args.get("new_string").and_then(Value::as_str))
        else {
            return ToolOutput::error("edit: `old_string` and `new_string` (strings) are required");
        };
        if old.is_empty() {
            return ToolOutput::error("edit: `old_string` is empty; use `write` to create a file");
        }
        if old == new {
            return ToolOutput::error("edit: `old_string` and `new_string` are the same");
        }
        let path = resolve(&ctx.cwd, p);
        let shown = format!("{}{}", display_path(&ctx.cwd, &path), outside_note(&ctx.cwd, &path));
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolOutput::error(format!("edit: {shown} does not exist; use `write` to create it"));
            }
            Err(e) => return ToolOutput::error(format!("edit: cannot read {shown}: {e}")),
        };
        if is_binary(&bytes) {
            return ToolOutput::error(format!("edit: {shown} is a binary file"));
        }
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return ToolOutput::error(format!(
                "edit: {shown} is too large to edit here ({} MiB); use `bash`",
                bytes.len() >> 20
            ));
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        // a CRLF file is matched and edited with plain line ends, then written back as it was
        let crlf = text.contains("\r\n");
        let (work, old_n, new_n) = if crlf {
            (text.replace("\r\n", "\n"), old.replace("\r\n", "\n"), new.replace("\r\n", "\n"))
        } else {
            (text.clone(), old.to_string(), new.to_string())
        };
        let (old, new) = (old_n.as_str(), new_n.as_str());
        let count = work.matches(old).count();
        let replace_all = bool_arg(&args, "replace_all");
        let edited = match count {
            0 => return ToolOutput::error(format!("edit: `old_string` not found in {shown}; read the file and copy the text exactly")),
            1 => work.replacen(old, new, 1),
            _ if replace_all => work.replace(old, new),
            n => {
                return ToolOutput::error(format!(
                    "edit: `old_string` occurs {n} times in {shown}; include more surrounding lines to make it unique, or set replace_all"
                ))
            }
        };
        let edited = if crlf { edited.replace('\n', "\r\n") } else { edited };
        if let Err(e) = tokio::fs::write(&path, &edited).await {
            return ToolOutput::error(format!("edit: cannot write {shown}: {e}"));
        }
        let (added, removed, diff) = unified(&text, &edited, &shown);
        let n = if replace_all { format!(" ({count} occurrences)") } else { String::new() };
        ToolOutput::ok(format!("edited {shown}{n}: +{added} −{removed} lines\n{diff}"))
            // the whole file, before and after: the diff numbers from its first line
            .with_display(ToolDisplay::Diff { path: shown, old: text, new: edited, start_line: 1 })
    }
}

/// Lines added and removed, and a unified diff cut to `DIFF_LINES` lines.
pub fn unified(old: &str, new: &str, name: &str) -> (usize, usize, String) {
    let diff = TextDiff::from_lines(old, new);
    let (mut added, mut removed) = (0, 0);
    for c in diff.iter_all_changes() {
        match c.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }
    let text = diff.unified_diff().context_radius(3).header(&format!("a/{name}"), &format!("b/{name}")).to_string();
    let lines: Vec<&str> = text.lines().collect();
    let shown = if lines.len() > DIFF_LINES {
        format!("{}\n… {} more diff lines", lines[..DIFF_LINES].join("\n"), lines.len() - DIFF_LINES)
    } else {
        text
    };
    (added, removed, shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn replaces_exactly_one_occurrence_and_reports_a_diff() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::write(d.path().join("a.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
        let out = Edit.run(json!({"path": "a.rs", "old_string": "fn b() {}", "new_string": "fn b() { 1 }"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.starts_with("edited a.rs: +1 −1 lines\n"), "{}", out.content);
        assert!(out.content.contains("-fn b() {}\n+fn b() { 1 }"));
        assert_eq!(std::fs::read_to_string(d.path().join("a.rs")).unwrap(), "fn a() {}\nfn b() { 1 }\nfn c() {}\n");
        assert!(matches!(out.display, ToolDisplay::Diff { start_line: 1, .. }));
    }

    #[tokio::test]
    async fn refuses_ambiguous_or_missing_text_unless_replace_all() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::write(d.path().join("a.txt"), "x\nx\n").unwrap();
        let out = Edit.run(json!({"path": "a.txt", "old_string": "x", "new_string": "y"}), &c).await;
        assert!(out.is_error && out.content.contains("occurs 2 times"), "{}", out.content);
        let out = Edit.run(json!({"path": "a.txt", "old_string": "z", "new_string": "y"}), &c).await;
        assert!(out.is_error && out.content.contains("not found"));
        let out =
            Edit.run(json!({"path": "a.txt", "old_string": "x", "new_string": "y", "replace_all": true}), &c).await;
        assert!(!out.is_error && out.content.contains("(2 occurrences)"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "y\ny\n");
        assert!(Edit
            .run(json!({"path": "nope", "old_string": "a", "new_string": "b"}), &c)
            .await
            .content
            .contains("does not exist"));
        assert!(Edit.run(json!({"path": "a.txt", "old_string": "a", "new_string": "a"}), &c).await.is_error);
        assert!(Edit.run(json!({"path": "a.txt", "old_string": "", "new_string": "a"}), &c).await.is_error);
        assert_eq!(Edit.summary(&json!({"path": "a.txt", "replace_all": true})), "edit a.txt (all occurrences)");
    }

    #[tokio::test]
    async fn edits_a_crlf_file_and_keeps_its_line_ends() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::write(d.path().join("w.txt"), "one\r\ntwo\r\nthree\r\n").unwrap();
        let out = Edit.run(json!({"path": "w.txt", "old_string": "two\nthree", "new_string": "2\n3"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("w.txt")).unwrap(), "one\r\n2\r\n3\r\n");
    }
}
