//! `write`: a whole file.

use async_trait::async_trait;
use nexo_core::{Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

use crate::util::{display_path, is_binary, outside_note, resolve, str_arg, write_atomic, MAX_FILE_BYTES};

pub struct Write;

/// The first 8 KiB of a file, what `is_binary` looks at.
async fn read_head(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::with_capacity(8192);
    tokio::fs::File::open(path).await?.take(8192).read_to_end(&mut buf).await?;
    Ok(buf)
}

#[async_trait]
impl Tool for Write {
    fn name(&self) -> &str {
        "write"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write".into(),
            description: "Write a whole file, creating it and its parent directories, or replacing its \
                          content. Prefer `edit` for part of an existing file."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "The file."},
                    "content": {"type": "string", "description": "The complete new content of the file."},
                    "check": {"type": "string", "description": "Command to run after; exit and tail come back."}
                },
                "required": ["path", "content"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Write
    }

    fn summary(&self, args: &Value) -> String {
        match str_arg(args, "path") {
            Some(p) => format!("write {p}"),
            None => "write (no path)".into(),
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(p) = str_arg(&args, "path") else {
            return ToolOutput::error("write: `path` is required");
        };
        let Some(content) = args.get("content").and_then(Value::as_str) else {
            return ToolOutput::error("write: `content` (a string) is required");
        };
        let path = resolve(&ctx.cwd, p);
        let shown = format!("{}{}", display_path(&ctx.cwd, &path), outside_note(&ctx.cwd, &path));
        if path.is_dir() {
            return ToolOutput::error(format!("write: {shown} is a directory"));
        }
        if tokio::fs::metadata(&path).await.is_ok_and(|m| !m.file_type().is_file()) {
            return ToolOutput::error(format!("write: {shown} is not a regular file (device, pipe or socket)"));
        }
        // a file too large to load: only its head is read, for the binary check, and the diff starts empty
        let huge = tokio::fs::metadata(&path).await.map(|m| m.len() > MAX_FILE_BYTES).unwrap_or(false);
        let read = if huge { read_head(&path).await } else { tokio::fs::read(&path).await };
        let old = match read {
            Ok(b) if is_binary(&b) => {
                return ToolOutput::error(format!("write: {shown} is a binary file; not overwriting it"))
            }
            Ok(_) if huge => Some(String::new()),
            Ok(b) => Some(String::from_utf8_lossy(&b).into_owned()),
            Err(_) => None,
        };
        if let Some(why) = ctx.seen.unseen(&path) {
            return ToolOutput::error(format!(
                "write: {shown} already exists and {why}; replacing it would lose what is there. \
                 `read` it first, or use `edit` to change part of it"
            ));
        }
        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return ToolOutput::error(format!("write: cannot create {}: {e}", parent.display()));
            }
        }
        if let Err(e) = write_atomic(&path, content.as_bytes()).await {
            return ToolOutput::error(format!("write: cannot write {shown}: {e}"));
        }
        ctx.seen.mark(&path);
        let lines = content.lines().count();
        let what = if old.is_some() { "replaced" } else { "created" };
        let now = match &old {
            Some(o) if !huge => crate::edit::around(o, content),
            _ => String::new(),
        };
        let now = if now.is_empty() { String::new() } else { format!("\nchanged lines:\n{now}") };
        let diag = if ctx.post_check { String::new() } else { crate::edit_check::report(&path).await };
        let proof = match str_arg(&args, "check").filter(|c| !c.trim().is_empty()) {
            Some(c) => crate::postcheck::run_own(c, ctx).await,
            None => String::new(),
        };
        ToolOutput::ok(format!(
            "{what} {shown}: file now has {} ({} bytes){now}{diag}{proof}",
            nexo_core::count(lines, "line"),
            content.len()
        ))
        .with_display(ToolDisplay::Diff {
            path: shown,
            old: old.unwrap_or_default(),
            new: content.to_string(),
            start_line: 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_file_the_model_has_not_seen_as_it_is_is_not_replaced() {
        use crate::{edit::Edit, read::Read};
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let f = d.path().join("notes.md");
        std::fs::write(&f, "the person notes\n").unwrap();
        let out = Write.run(json!({"path": "notes.md", "content": "x\n"}), &c).await;
        assert!(out.is_error && out.content.contains("have not read it"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "the person notes\n");
        // read, then written: fine; written again by the model: fine
        assert!(!Read.run(json!({"path": "notes.md"}), &c).await.is_error);
        assert!(!Write.run(json!({"path": "notes.md", "content": "x\n"}), &c).await.is_error);
        assert!(!Write.run(json!({"path": "notes.md", "content": "y\n"}), &c).await.is_error);
        // changed behind its back (an editor, a formatter): read it again first
        std::fs::write(&f, "y\nand a line the person added\n").unwrap();
        let out = Write.run(json!({"path": "notes.md", "content": "z\n"}), &c).await;
        assert!(out.is_error && out.content.contains("changed since"), "{}", out.content);
        // a new file, and one the model edited, need no read
        assert!(!Write.run(json!({"path": "new/a.txt", "content": "a\n"}), &c).await.is_error);
        std::fs::write(d.path().join("e.txt"), "one\n").unwrap();
        let e = Edit.run(json!({"path": "e.txt", "old_string": "one", "new_string": "two"}), &c).await;
        assert!(!e.is_error, "{}", e.content);
        assert!(!Write.run(json!({"path": "e.txt", "content": "three\n"}), &c).await.is_error);
    }

    #[tokio::test]
    async fn a_replaced_file_reports_its_line_count_and_the_changed_lines() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let out = Write.run(json!({"path": "n.txt", "content": "a\nb\nc\n"}), &c).await;
        assert_eq!(out.content, "created n.txt: file now has 3 lines (6 bytes)");
        let out = Write.run(json!({"path": "n.txt", "content": "a\nB\nc\nd\n"}), &c).await;
        assert!(
            out.content.starts_with("replaced n.txt: file now has 4 lines (8 bytes)\nchanged lines:\n"),
            "{}",
            out.content
        );
        assert!(out.content.contains("     2│ B\n     3│ c\n     4│ d\n"), "{}", out.content);
    }

    #[tokio::test]
    async fn a_huge_old_file_is_not_loaded_for_the_diff() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let mut text = "line\n".repeat(2000).into_bytes();
        text.resize(crate::util::MAX_FILE_BYTES as usize + 1, b'x');
        std::fs::write(d.path().join("big.log"), &text).unwrap();
        assert!(crate::read::Read.run(json!({"path": "big.log"}), &c).await.content.contains("too large"));
        let out = Write.run(json!({"path": "big.log", "content": "new\n"}), &c).await;
        assert!(out.content.starts_with("replaced big.log"), "{}", out.content);
        match out.display {
            ToolDisplay::Diff { old, .. } => assert!(old.len() < 1024, "old text loaded: {} bytes", old.len()),
            _ => panic!(),
        }
        assert_eq!(std::fs::read_to_string(d.path().join("big.log")).unwrap(), "new\n");
    }

    #[tokio::test]
    async fn creates_parents_and_reports_a_diff_against_the_old_content() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let out = Write.run(json!({"path": "a/b/c.txt", "content": "hi\n"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.starts_with("created a/b/c.txt"));
        assert_eq!(std::fs::read_to_string(d.path().join("a/b/c.txt")).unwrap(), "hi\n");
        let out = Write.run(json!({"path": "a/b/c.txt", "content": "bye\n"}), &c).await;
        assert!(out.content.starts_with("replaced"));
        match out.display {
            ToolDisplay::Diff { old, new, start_line, .. } => {
                assert_eq!((old.as_str(), new.as_str()), ("hi\n", "bye\n"));
                assert_eq!(start_line, 1);
            }
            _ => panic!(),
        }
        assert!(Write.run(json!({"path": "a"}), &c).await.is_error);
        assert!(Write.run(json!({"path": "a", "content": "x"}), &c).await.content.contains("directory"));
        assert_eq!(Write.summary(&json!({"path": "x.rs"})), "write x.rs");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_replaced_file_keeps_its_mode_and_no_temporary_file_stays() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let f = d.path().join("run.sh");
        std::fs::write(&f, "old\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o751)).unwrap();
        c.seen.mark(&f);
        let out = Write.run(json!({"path": "run.sh", "content": "new\n"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "new\n");
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o751);
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }
}
