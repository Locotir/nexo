//! `write`: a whole file.

use async_trait::async_trait;
use nexo_core::{Compact, Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

use crate::util::{display_path, is_binary, outside_note, resolve, str_arg, utf16_text, write_atomic, MAX_FILE_BYTES};

pub struct Write;

/// The first 8 KiB of a file, what `is_binary` looks at.
async fn read_head(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::with_capacity(8192);
    tokio::fs::File::open(path).await?.take(8192).read_to_end(&mut buf).await?;
    Ok(buf)
}

/// What the new content changes about the line ends and the byte order mark of the file it
/// replaces, as a fact, or `None` when both stay as they were. Nothing is kept or rewritten: a
/// write is the model's text as given.
fn format_change(old: &[u8], content: &str) -> Option<String> {
    let (old_crlf, old_bom) = (old.windows(2).any(|w| w == b"\r\n"), old.starts_with(&[0xEF, 0xBB, 0xBF]));
    let (new_crlf, new_bom) = (content.contains("\r\n"), content.starts_with('\u{FEFF}'));
    let ends = |crlf: bool| if crlf { "CRLF line ends" } else { "LF line ends" };
    let mark = |bom: bool| if bom { "a UTF-8 BOM" } else { "no BOM" };
    let (mut was, mut now) = (Vec::new(), Vec::new());
    if old_crlf != new_crlf {
        was.push(ends(old_crlf));
        now.push(ends(new_crlf));
    }
    if old_bom != new_bom {
        was.push(mark(old_bom));
        now.push(mark(new_bom));
    }
    if was.is_empty() {
        return None;
    }
    Some(format!("the file it replaced used {}; the new content has {}", was.join(" and "), now.join(" and ")))
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
                    "path": {"type": "string"},
                    "content": {"type": "string"},
                    "check": {"type": "string", "description": "Short command to run after; exit and tail come back."}
                },
                "required": ["path", "content"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: "Write a whole file, creating its directories; use `edit` for part of a file.",
            params: &[("check", "Command to run after")],
        })
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
        let mut same = false;
        let mut format_note = String::new();
        let old = match read {
            // UTF-16 text looks binary (a NUL in every other byte); the fact is what the file is
            Ok(b) if is_binary(&b) && utf16_text(&b).is_some() => {
                return ToolOutput::error(format!(
                    "write: {shown} is UTF-16 text, not UTF-8; not overwriting it, change it with `bash` in its own encoding"
                ));
            }
            Ok(b) if is_binary(&b) => {
                return ToolOutput::error(format!("write: {shown} is a binary file; not overwriting it"));
            }
            // another encoding (Latin-1, CP1252, Shift-JIS): replacing it with UTF-8 text would change it silently,
            // as `edit` refuses; a head cut in the middle of a character (no `error_len`) is not a bad byte
            Ok(b) if std::str::from_utf8(&b).err().is_some_and(|e| !huge || e.error_len().is_some()) => {
                let at = std::str::from_utf8(&b).err().map_or(0, |e| e.valid_up_to());
                return ToolOutput::error(format!(
                    "write: {shown} is not UTF-8 text (invalid byte at offset {at}); not overwriting it, change it with `bash` in its own encoding"
                ));
            }
            Ok(_) if huge => Some(String::new()),
            Ok(b) => {
                // compared as bytes: a lossy text would call a file with invalid bytes the same as its U+FFFD form
                same = b == content.as_bytes();
                format_note = format_change(&b, content).map(|n| format!("; {n}")).unwrap_or_default();
                Some(String::from_utf8_lossy(&b).into_owned())
            }
            Err(_) => None,
        };
        if same {
            return ToolOutput::error(format!("write: {shown} already has this content; nothing to write"));
        }
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
        if tokio::fs::read(&path).await.ok().as_deref() != Some(content.as_bytes()) {
            return ToolOutput::error(format!(
                "write: {shown} is not as intended after the write; read it before going on"
            ));
        }
        ctx.seen.mark(&path);
        let lines = content.lines().count();
        let what = if old.is_some() { "replaced" } else { "created" };
        let now = match &old {
            Some(o) if !huge => crate::edit::now_text(o, content),
            _ => crate::edit::whole(content).unwrap_or_default(),
        };
        let now = if now.is_empty() { String::new() } else { format!("\nfile now:\n{now}") };
        let diag = if ctx.post_check { String::new() } else { crate::edit_check::report(&path).await };
        let proof = match str_arg(&args, "check").filter(|c| !c.trim().is_empty()) {
            Some(c) => crate::postcheck::run_own(c, ctx).await,
            None => String::new(),
        };
        ToolOutput::ok(format!(
            "{what} {shown}: file now has {} ({} bytes), {}{format_note}{now}{diag}{proof}",
            nexo_core::count(lines, "line"),
            content.len(),
            crate::edit::VERIFIED
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
    async fn a_binary_file_is_not_overwritten() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::write(d.path().join("x.bin"), [1u8, 0, 2]).unwrap();
        let out = Write.run(json!({"path": "x.bin", "content": "b\n"}), &c).await;
        assert_eq!(out.content, "write: x.bin is a binary file; not overwriting it");
        assert_eq!(std::fs::read(d.path().join("x.bin")).unwrap(), [1u8, 0, 2]);
    }

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
        // the same content again: nothing to write, the file is left as it is
        let same = Write.run(json!({"path": "new/a.txt", "content": "a\n"}), &c).await;
        assert!(same.is_error && same.content.contains("already has this content"), "{}", same.content);
        // read again after the outside change: the write goes through
        assert!(!crate::read::Read.run(json!({"path": "notes.md"}), &c).await.is_error);
        assert!(!Write.run(json!({"path": "notes.md", "content": "z\n"}), &c).await.is_error);
        std::fs::write(d.path().join("e.txt"), "one\n").unwrap();
        let e = Edit.run(json!({"path": "e.txt", "old_string": "one", "new_string": "two"}), &c).await;
        assert!(!e.is_error, "{}", e.content);
        assert!(!Write.run(json!({"path": "e.txt", "content": "three\n"}), &c).await.is_error);
    }

    #[tokio::test]
    async fn a_file_with_invalid_bytes_is_not_taken_for_the_same_content() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let f = d.path().join("b.txt");
        std::fs::write(&f, [b'a', 0xFF, b'b']).unwrap();
        assert!(!crate::read::Read.run(json!({"path": "b.txt"}), &c).await.is_error);
        let out = Write.run(json!({"path": "b.txt", "content": "a\u{FFFD}b"}), &c).await;
        assert!(!out.content.contains("already has this content"), "{}", out.content);
    }

    #[tokio::test]
    async fn a_latin1_file_is_not_overwritten_with_utf8() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let original = [0x63u8, 0x61, 0x66, 0xE9, b'\n'];
        std::fs::write(d.path().join("a.txt"), original).unwrap();
        assert!(!crate::read::Read.run(json!({"path": "a.txt"}), &c).await.is_error);
        let out = Write.run(json!({"path": "a.txt", "content": "cafe\n"}), &c).await;
        assert!(out.is_error && out.content.contains("not UTF-8"), "{}", out.content);
        assert!(out.content.contains("invalid byte at offset 3"), "{}", out.content);
        assert_eq!(std::fs::read(d.path().join("a.txt")).unwrap(), original);
        // control: a UTF-8 file that has been read is still replaced
        std::fs::write(d.path().join("u.txt"), "caf\u{e9}\n").unwrap();
        assert!(!crate::read::Read.run(json!({"path": "u.txt"}), &c).await.is_error);
        let out = Write.run(json!({"path": "u.txt", "content": "cafe\n"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("u.txt")).unwrap(), "cafe\n");
    }

    #[tokio::test]
    async fn a_replaced_file_reports_its_line_count_and_the_changed_lines() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let out = Write.run(json!({"path": "n.txt", "content": "a\nb\nc\n"}), &c).await;
        assert!(
            out.content.starts_with("created n.txt: file now has 3 lines (6 bytes), applied and verified on disk"),
            "{}",
            out.content
        );
        let out = Write.run(json!({"path": "n.txt", "content": "a\nB\nc\nd\n"}), &c).await;
        assert!(
            out.content.starts_with(
                "replaced n.txt: file now has 4 lines (8 bytes), applied and verified on disk\nfile now:\n"
            ),
            "{}",
            out.content
        );
        assert!(out.content.contains("     1│ a\n     2│ B\n     3│ c\n     4│ d\n"), "{}", out.content);
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

#[cfg(test)]
mod format_tests {
    use super::*;

    async fn replace(old: &[u8], content: &str) -> String {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::write(d.path().join("f.txt"), old).unwrap();
        assert!(!crate::read::Read.run(json!({"path": "f.txt"}), &c).await.is_error);
        let out = Write.run(json!({"path": "f.txt", "content": content}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        out.content
    }

    #[tokio::test]
    async fn a_crlf_file_rewritten_with_lf_says_so() {
        let out = replace(b"a\r\nb\r\n", "a\nc\n").await;
        assert!(out.contains("; the file it replaced used CRLF line ends; the new content has LF line ends"), "{out}");
    }

    #[tokio::test]
    async fn a_bom_file_rewritten_without_one_says_so() {
        let out = replace(b"\xEF\xBB\xBFa\r\n", "b\r\n").await;
        assert!(out.contains("; the file it replaced used a UTF-8 BOM; the new content has no BOM"), "{out}");
        let out = replace(b"\xEF\xBB\xBFa\r\n", "b\n").await;
        assert!(
            out.contains("used CRLF line ends and a UTF-8 BOM; the new content has LF line ends and no BOM"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn no_note_is_added_when_line_ends_and_bom_stay_the_same() {
        assert!(!replace(b"a\r\n", "b\r\n").await.contains("the file it replaced"));
        assert!(!replace(b"a\n", "b\n").await.contains("the file it replaced"));
        assert!(!replace(b"\xEF\xBB\xBFa\n", "\u{FEFF}b\n").await.contains("the file it replaced"));
    }
}
