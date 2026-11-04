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
                          content. Prefer `edit` to change part of an existing file: it is smaller and \
                          keeps what you did not mean to touch."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "The file to write, absolute or relative to the working directory."},
                    "content": {"type": "string", "description": "The complete new content of the file."}
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
        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return ToolOutput::error(format!("write: cannot create {}: {e}", parent.display()));
            }
        }
        if let Err(e) = write_atomic(&path, content.as_bytes()).await {
            return ToolOutput::error(format!("write: cannot write {shown}: {e}"));
        }
        let lines = content.lines().count();
        let what = if old.is_some() { "replaced" } else { "created" };
        ToolOutput::ok(format!("{what} {shown} ({}, {} bytes)", nexo_core::count(lines, "line"), content.len()))
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
    async fn a_huge_old_file_is_not_loaded_for_the_diff() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let mut text = "line\n".repeat(2000).into_bytes();
        text.resize(crate::util::MAX_FILE_BYTES as usize + 1, b'x');
        std::fs::write(d.path().join("big.log"), &text).unwrap();
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
        let out = Write.run(json!({"path": "run.sh", "content": "new\n"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "new\n");
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o751);
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }
}
