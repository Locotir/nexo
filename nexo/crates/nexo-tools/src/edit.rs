//! `edit`: replace one exact occurrence of a text in a file.

use async_trait::async_trait;
use nexo_core::{Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};
use similar::{ChangeTag, TextDiff};

use crate::util::{bool_arg, display_path, is_binary, outside_note, resolve, str_arg, write_atomic, MAX_FILE_BYTES};

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
        if let Ok(m) = tokio::fs::metadata(&path).await {
            if !m.file_type().is_file() {
                return ToolOutput::error(format!("edit: {shown} is not a regular file (device, pipe or socket)"));
            }
            if m.len() > MAX_FILE_BYTES {
                return ToolOutput::error(format!(
                    "edit: {shown} is too large to edit here ({} MiB); use `bash`",
                    m.len() >> 20
                ));
            }
        }
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
        // rewriting a file in another encoding as UTF-8 would mangle every byte it does not share
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(e) => {
                return ToolOutput::error(format!(
                "edit: {shown} is not UTF-8 text (invalid byte at offset {}); edit it with `bash` in its own encoding",
                e.utf8_error().valid_up_to()
            ))
            }
        };
        // the file's text is matched as it is; only the strings given adapt to its line ends
        let (old, new) = match_line_ends(&text, old, new);
        let (old, new) = (old.as_str(), new.as_str());
        let count = text.matches(old).count();
        let replace_all = bool_arg(&args, "replace_all");
        let edited = match count {
            0 => return ToolOutput::error(format!("edit: `old_string` not found in {shown}; read the file and copy the text exactly")),
            1 => text.replacen(old, new, 1),
            _ if replace_all => text.replace(old, new),
            n => {
                return ToolOutput::error(format!(
                    "edit: `old_string` occurs {n} times in {shown}; include more surrounding lines to make it unique, or set replace_all"
                ))
            }
        };
        if let Err(e) = write_atomic(&path, edited.as_bytes()).await {
            return ToolOutput::error(format!("edit: cannot write {shown}: {e}"));
        }
        let (added, removed, diff) = unified(&text, &edited, &shown);
        let n = if replace_all { format!(" ({count} occurrences)") } else { String::new() };
        ToolOutput::ok(format!("edited {shown}{n}: +{added} −{removed} lines\n{diff}"))
            // the whole file, before and after: the diff numbers from its first line
            .with_display(ToolDisplay::Diff { path: shown, old: text, new: edited, start_line: 1 })
    }
}

/// `old` and `new` as they fit `text`'s line ends: as given when `old` is found so, else with
/// CRLF ends (a CRLF file edited with `\n` strings), else with plain ends. `new` follows the
/// ends of the `old` that matched, and a file that ends every line with CRLF gives them to it.
pub fn match_line_ends(text: &str, old: &str, new: &str) -> (String, String) {
    let lf = |s: &str| s.replace("\r\n", "\n");
    let crlf = |s: &str| lf(s).replace('\n', "\r\n");
    let file_crlf = text.contains("\r\n");
    if !text.contains(old) {
        if file_crlf && old.contains('\n') && text.contains(&crlf(old)) {
            return (crlf(old), crlf(new));
        }
        if old.contains("\r\n") && text.contains(&lf(old)) {
            return (lf(old), lf(new));
        }
    }
    let all_crlf = file_crlf && text.matches('\n').count() == text.matches("\r\n").count();
    if all_crlf && !old.contains('\n') {
        (old.to_string(), crlf(new))
    } else {
        (old.to_string(), new.to_string())
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

    #[tokio::test]
    async fn refuses_a_file_that_is_not_utf8_and_leaves_it_alone() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        // Latin-1: "café = 1\n"
        let latin1 = b"caf\xe9 = 1\nx = 2\n".to_vec();
        std::fs::write(d.path().join("l.txt"), &latin1).unwrap();
        let out = Edit.run(json!({"path": "l.txt", "old_string": "x = 2", "new_string": "x = 3"}), &c).await;
        assert!(out.is_error && out.content.contains("not UTF-8"), "{}", out.content);
        assert_eq!(std::fs::read(d.path().join("l.txt")).unwrap(), latin1);
    }

    #[tokio::test]
    async fn a_mixed_line_end_file_keeps_every_line_end_it_had() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::write(d.path().join("m.txt"), "a\nb\r\nc\n").unwrap();
        let out = Edit.run(json!({"path": "m.txt", "old_string": "a", "new_string": "x"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("m.txt")).unwrap(), "x\nb\r\nc\n");
        // a CRLF string against the plain part of the file
        let out = Edit.run(json!({"path": "m.txt", "old_string": "x\r\nb", "new_string": "y\r\nb"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("m.txt")).unwrap(), "y\nb\r\nc\n");
        // a file all CRLF: new lines in `new_string` take its ends
        std::fs::write(d.path().join("w.txt"), "one\r\ntwo\r\n").unwrap();
        let out = Edit.run(json!({"path": "w.txt", "old_string": "two", "new_string": "2\n2b"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("w.txt")).unwrap(), "one\r\n2\r\n2b\r\n");
    }

    #[tokio::test]
    async fn a_file_over_the_limit_is_refused_before_it_is_read() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        // sparse and all NUL: read whole it would be called binary, not too large
        let f = std::fs::File::create(d.path().join("big.txt")).unwrap();
        f.set_len(MAX_FILE_BYTES + 1).unwrap();
        let out = Edit.run(json!({"path": "big.txt", "old_string": "a", "new_string": "b"}), &c).await;
        assert!(out.is_error && out.content.contains("too large"), "{}", out.content);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_edit_replaces_the_file_whole_keeping_its_mode_and_links() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let f = d.path().join("run.sh");
        std::fs::write(&f, "echo a\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o750)).unwrap();
        std::os::unix::fs::symlink(&f, d.path().join("link.sh")).unwrap();
        let out = Edit.run(json!({"path": "link.sh", "old_string": "a", "new_string": "b"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "echo b\n");
        assert!(std::fs::symlink_metadata(d.path().join("link.sh")).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o750);
        let names: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "no temporary file left: {names:?}");
        // a read-only file stays read-only and unchanged
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o444)).unwrap();
        let out = Edit.run(json!({"path": "run.sh", "old_string": "b", "new_string": "c"}), &c).await;
        assert!(out.is_error && out.content.contains("read-only"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "echo b\n");
    }
}
