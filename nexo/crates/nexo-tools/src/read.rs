//! `read`: a file, as numbered lines.

use async_trait::async_trait;
use nexo_core::{Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

use crate::util::{cut_line, display_path, int_arg, is_binary, outside_note, resolve, str_arg, MAX_FILE_BYTES};

/// Lines returned when `limit` is not given.
pub const DEFAULT_LIMIT: u64 = 2000;
/// Characters kept of one line.
pub const LINE_CHARS: usize = 2000;

pub struct Read;

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str {
        "read"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read".into(),
            description: "Read a text file. Returns its lines numbered (`   12│ text`), at most `limit` \
                          lines from line `offset` (1-based); the default is the first 2000 lines, and a \
                          line longer than 2000 characters is cut. Use `grep` to find where something is \
                          and `read` with `offset`/`limit` to look at that part of a large file. Directories \
                          and binary files are refused (use `ls` for a directory)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "The file to read, absolute or relative to the working directory."},
                    "offset": {"type": "integer", "minimum": 1, "description": "First line to return (1-based). Default 1."},
                    "limit": {"type": "integer", "minimum": 1, "description": "Lines to return. Default 2000."}
                },
                "required": ["path"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn summary(&self, args: &Value) -> String {
        match str_arg(args, "path") {
            Some(p) => {
                let range = match (int_arg(args, "offset"), int_arg(args, "limit")) {
                    (Some(o), Some(l)) => format!(" [{o}..{}]", o + l - 1),
                    (Some(o), None) => format!(" [{o}..]"),
                    (None, Some(l)) => format!(" [..{l}]"),
                    _ => String::new(),
                };
                format!("read {p}{range}")
            }
            None => "read (no path)".into(),
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(p) = str_arg(&args, "path") else {
            return ToolOutput::error("read: `path` is required");
        };
        let path = resolve(&ctx.cwd, p);
        let shown = format!("{}{}", display_path(&ctx.cwd, &path), outside_note(&ctx.cwd, &path));
        if path.is_dir() {
            return ToolOutput::error(format!("read: {shown} is a directory; use `ls` to list it"));
        }
        if let Ok(m) = tokio::fs::metadata(&path).await {
            if m.len() > MAX_FILE_BYTES {
                return ToolOutput::error(format!(
                    "read: {shown} is {} MiB, too large to load; use `bash` with `head`, `tail`, `sed -n` or `grep` on it",
                    m.len() >> 20
                ));
            }
        }
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolOutput::error(format!("read: {shown} does not exist"));
            }
            Err(e) => return ToolOutput::error(format!("read: cannot read {shown}: {e}")),
        };
        if is_binary(&bytes) {
            return ToolOutput::error(format!("read: {shown} is a binary file ({} bytes)", bytes.len()));
        }
        let text = String::from_utf8_lossy(&bytes);
        if text.is_empty() {
            return ToolOutput::ok(format!("{shown}: (empty file)")).with_display(ToolDisplay::File {
                path: shown,
                text: String::new(),
                start_line: 1,
            });
        }
        let offset = int_arg(&args, "offset").unwrap_or(1).max(1) as usize;
        let limit = int_arg(&args, "limit").unwrap_or(DEFAULT_LIMIT).max(1) as usize;
        let total = text.lines().count();
        if offset > total {
            return ToolOutput::error(format!("read: {shown} has {total} lines; offset {offset} is past the end"));
        }
        let mut out = String::new();
        let mut raw = String::new();
        let mut n = 0usize;
        for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
            let shown_line = cut_line(line, LINE_CHARS);
            out.push_str(&format!("{:>6}│ {}\n", i + 1, shown_line));
            raw.push_str(&shown_line);
            raw.push('\n');
            n = i + 1;
        }
        if n < total {
            out.push_str(&format!("… {} more lines (of {total}); read with offset {} to continue\n", total - n, n + 1));
        }
        ToolOutput::ok(out).with_display(ToolDisplay::File { path: shown, text: raw, start_line: offset })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext::new(dir.to_path_buf())
    }

    #[tokio::test]
    async fn reads_numbered_lines_with_a_window() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        let out = Read.run(json!({"path": "a.txt"}), &ctx(d.path())).await;
        assert!(!out.is_error);
        assert!(out.content.starts_with("     1│ one\n     2│ two\n"));
        let out = Read.run(json!({"path": "a.txt", "offset": 2, "limit": 2}), &ctx(d.path())).await;
        assert_eq!(out.content, "     2│ two\n     3│ three\n… 1 more lines (of 4); read with offset 4 to continue\n");
        match out.display {
            ToolDisplay::File { start_line, text, .. } => {
                assert_eq!(start_line, 2);
                assert_eq!(text, "two\nthree\n");
            }
            _ => panic!(),
        }
        assert_eq!(Read.summary(&json!({"path": "a.txt", "offset": 2, "limit": 2})), "read a.txt [2..3]");
    }

    #[tokio::test]
    async fn refuses_what_it_cannot_read() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(d.path());
        assert!(Read.run(json!({}), &c).await.is_error);
        let out = Read.run(json!({"path": "missing.txt"}), &c).await;
        assert!(out.is_error && out.content.contains("does not exist"));
        assert!(Read.run(json!({"path": "."}), &c).await.content.contains("directory"));
        std::fs::write(d.path().join("b.bin"), b"\x00\x01\x02").unwrap();
        assert!(Read.run(json!({"path": "b.bin"}), &c).await.content.contains("binary"));
        std::fs::write(d.path().join("e.txt"), "").unwrap();
        let out = Read.run(json!({"path": "e.txt"}), &c).await;
        assert!(!out.is_error && out.content.contains("empty file"));
        std::fs::write(d.path().join("l.txt"), "x".repeat(3000)).unwrap();
        let out = Read.run(json!({"path": "l.txt"}), &c).await;
        assert!(out.content.ends_with("…\n"));
        assert!(out.content.chars().count() < 2100);
        let out = Read.run(json!({"path": "l.txt", "offset": 9}), &c).await;
        assert!(out.is_error && out.content.contains("past the end"));
        let big = std::fs::File::create(d.path().join("big.log")).unwrap();
        big.set_len(MAX_FILE_BYTES + 1).unwrap(); // sparse: no bytes are written
        let out = Read.run(json!({"path": "big.log"}), &c).await;
        assert!(out.is_error && out.content.contains("too large"), "{}", out.content);
    }
}
