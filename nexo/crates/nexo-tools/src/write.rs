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

/// How to see what `read` cut from the lines `cut` of `path`: the part past its first `read::LINE_CHARS` chars, from a
/// shell. At most five lines are named in the command; the others need the same.
fn cut_how(cut: &[usize], path: &std::path::Path, cwd: &std::path::Path) -> String {
    const NAMED: usize = 5;
    let named: Vec<String> = cut.iter().take(NAMED).map(|n| format!("{n}p")).collect();
    let script = format!("'{}'", named.join(";"));
    let more = if cut.len() > NAMED {
        format!(", and the same for the other {} lines", cut.len() - NAMED)
    } else {
        String::new()
    };
    let of = if cut.len() == 1 { "that line" } else { "those lines" };
    format!(
        "`read` shows no more of {of}; `bash` shows the rest: `sed -n {} {} | cut -c{}-`{more}. \
         `edit` changes part of the file without replacing it",
        script,
        crate::program::shell_quoted(&display_path(cwd, path)),
        crate::read::LINE_CHARS + 1
    )
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
                // the file is valid UTF-8 here, so `same` is a plain byte compare
                format_note = format_change(&b, content).map(|n| format!("; {n}")).unwrap_or_default();
                Some(String::from_utf8_lossy(&b).into_owned())
            }
            Err(_) => None,
        };
        // A huge old file was not read in full, so do not guess which characters are new.
        let (cleaned, stripped) = if huge {
            (content.to_string(), String::new())
        } else {
            crate::marks::strip_added(old.as_deref().unwrap_or(""), content)
        };
        let content = cleaned.as_str();
        let same = old.as_deref().is_some_and(|o| !huge && o == content);
        // the size of what is replaced (a huge file was read only in part: its size is the file's own)
        let old_bytes = match &old {
            Some(_) if huge => tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0),
            Some(o) => o.len() as u64,
            None => 0,
        };
        if same {
            return ToolOutput::error(format!("write: {shown} already has this content; nothing to write{stripped}"));
        }
        if let Some(why) = ctx.seen.unseen(&path) {
            // lines `read` cut stay cut however often it is called: reading again would only repeat the refusal
            let how = match ctx.seen.cut_only(&path) {
                Some(cut) => cut_how(&cut, &path, &ctx.cwd),
                None => "`read` it first, or use `edit` to change part of it".to_string(),
            };
            return ToolOutput::error(format!(
                "write: {shown} already exists and {why}; replacing it would lose what is there. {how}"
            ));
        }
        // the folders this write creates: the highest one that is missing, and how many there are, so a stray
        // folder (a path the model spelled wrongly) is told with where it was made
        let mut new_dirs = 0usize;
        let mut top_new = None;
        if let Some(parent) = path.parent() {
            for a in parent.ancestors().take_while(|a| !a.as_os_str().is_empty()) {
                if tokio::fs::metadata(a).await.is_ok() {
                    break;
                }
                new_dirs += 1;
                top_new = Some(a);
            }
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return ToolOutput::error(format!("write: cannot create {}: {e}", parent.display()));
            }
        }
        let made = match (top_new, new_dirs) {
            (Some(top), 1) => format!("new folder: {}", nexo_core::shell::slashed(top)),
            (Some(top), n) => format!("new folders: {} and {} below it", nexo_core::shell::slashed(top), n - 1),
            _ => String::new(),
        };
        let new_folders = if made.is_empty() { String::new() } else { format!("; {made}") };
        if let Err(e) = write_atomic(&path, content.as_bytes()).await {
            // the folders made for this file stay, empty: the model is told, not left to find them
            let left =
                if made.is_empty() { String::new() } else { format!("; made before it failed and left empty: {made}") };
            return ToolOutput::error(format!("write: cannot write {shown}: {e}{left}"));
        }
        if tokio::fs::read(&path).await.ok().as_deref() != Some(content.as_bytes()) {
            return ToolOutput::error(format!(
                "write: {shown} is not as intended after the write; read it before going on"
            ));
        }
        // all of it is the model's own text now
        ctx.seen.whole(&path);
        let lines = content.lines().count();
        let what = if old.is_some() { "replaced" } else { "created" };
        // what was there before: its size and what the new text changes
        let was = match &old {
            Some(_) if huge => format!("was {old_bytes} bytes (too large to count its lines), "),
            Some(o) => {
                let (added, removed) = crate::edit::line_changes(o, content);
                format!(
                    "was {} ({old_bytes} bytes), +{added} −{removed} lines, ",
                    nexo_core::count(o.lines().count(), "line")
                )
            }
            None => String::new(),
        };
        let now = match &old {
            Some(o) if !huge => crate::edit::now_text(o, content),
            _ => crate::edit::whole(content).unwrap_or_default(),
        };
        let now = if now.is_empty() { String::new() } else { format!("\nfile now:\n{now}") };
        let marks =
            if huge { None } else { crate::marks::added_for_path(old.as_deref().unwrap_or(""), content, &path) };
        let marks = marks.map(|m| format!("\n{m}")).unwrap_or_default();
        // a step-level check after the whole step covers a source file inside the project folder only
        let diag = if crate::edit_check::step_covers(ctx, &path) {
            String::new()
        } else {
            crate::edit_check::report(&path).await
        };
        let proof = match str_arg(&args, "check").filter(|c| !c.trim().is_empty()) {
            Some(c) => crate::postcheck::run_own(c, ctx).await,
            None => String::new(),
        };
        ToolOutput::ok(format!(
            "{what} {shown}: file now has {} ({} bytes), {was}{}{format_note}{new_folders}{now}{stripped}{marks}{diag}{proof}",
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
    async fn a_file_with_invalid_bytes_is_refused_even_for_its_replacement_form() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let f = d.path().join("b.txt");
        std::fs::write(&f, [b'a', 0xFF, b'b']).unwrap();
        assert!(!crate::read::Read.run(json!({"path": "b.txt"}), &c).await.is_error);
        let out = Write.run(json!({"path": "b.txt", "content": "a\u{FFFD}b"}), &c).await;
        assert!(out.is_error && out.content.contains("not UTF-8"), "{}", out.content);
        assert!(out.content.contains("invalid byte at offset 1"), "{}", out.content);
        assert_eq!(std::fs::read(&f).unwrap(), [b'a', 0xFF, b'b']);
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
                "replaced n.txt: file now has 4 lines (8 bytes), was 3 lines (6 bytes), +2 −1 lines, \
                 applied and verified on disk\nfile now:\n"
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

    /// A file of `n` lines, `line 1` to `line {n}`: each numbered row is under 20 characters, so a default
    /// `read` of 2000 of them is far under the size cap.
    fn numbered_file(d: &std::path::Path, name: &str, n: usize) {
        let text: String = (1..=n).map(|i| format!("line {i}\n")).collect();
        std::fs::write(d.join(name), text).unwrap();
    }

    #[tokio::test]
    async fn a_file_read_in_part_is_not_replaced() {
        use crate::read::Read;
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        numbered_file(d.path(), "STATUS.md", 3000);
        let r = Read.run(json!({"path": "STATUS.md"}), &c).await;
        assert!(!r.is_error && r.content.contains("2000│ line 2000"), "{}", r.content);
        let out = Write.run(json!({"path": "STATUS.md", "content": "new\n"}), &c).await;
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("you were shown only lines 1-2000 of its 3000 lines"), "{}", out.content);
        assert!(out.content.contains("offset 2001"), "{}", out.content);
        // the rest read: nothing is left unseen, and the result says what was there
        assert!(!Read.run(json!({"path": "STATUS.md", "offset": 2001}), &c).await.is_error);
        let out = Write.run(json!({"path": "STATUS.md", "content": "new\n"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("was 3000 lines"), "{}", out.content);
    }

    #[tokio::test]
    async fn a_part_read_then_edited_file_is_not_replaced_after_a_short_read() {
        use crate::{edit::Edit, read::Read};
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        numbered_file(d.path(), "big.md", 3000);
        assert!(!Read.run(json!({"path": "big.md"}), &c).await.is_error);
        // lines 10-1009 go: the file has 2000 lines now, and the old line numbers mean something else
        let old: String = (10..=1009).map(|i| format!("line {i}\n")).collect();
        let e = Edit.run(json!({"path": "big.md", "old_string": old, "new_string": ""}), &c).await;
        assert!(!e.is_error, "{}", e.content);
        let out = Write.run(json!({"path": "big.md", "content": "new\n"}), &c).await;
        assert!(out.is_error && out.content.contains("before your last change"), "{}", out.content);
        assert!(!Read.run(json!({"path": "big.md", "offset": 1, "limit": 10}), &c).await.is_error);
        let out = Write.run(json!({"path": "big.md", "content": "new\n"}), &c).await;
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("lines 1-10 of its 2000 lines"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("big.md")).unwrap().lines().count(), 2000);
    }

    #[tokio::test]
    async fn a_file_with_a_cut_line_is_not_replaced() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::write(d.path().join("w.txt"), format!("a\n{}\nc\n", "x".repeat(3000))).unwrap();
        assert!(!crate::read::Read.run(json!({"path": "w.txt"}), &c).await.is_error);
        let out = Write.run(json!({"path": "w.txt", "content": "new\n"}), &c).await;
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("line 2") && out.content.contains("longer than 2000 chars"), "{}", out.content);
        // reading again cannot show more of the line: the refusal says so and names how to see the rest
        assert!(!out.content.contains("`read` it first"), "{}", out.content);
        assert!(out.content.contains("`read` shows no more of that line"), "{}", out.content);
        assert!(out.content.contains("`sed -n '2p' w.txt | cut -c2001-`"), "{}", out.content);
        // lines unseen as well as cut: reading is still the way
        std::fs::write(d.path().join("v.txt"), format!("{}\n{}\nc\n", "y".repeat(3000), "z".repeat(3000))).unwrap();
        let r = crate::read::Read.run(json!({"path": "v.txt", "limit": 1}), &c).await;
        assert!(!r.is_error, "{}", r.content);
        let out = Write.run(json!({"path": "v.txt", "content": "new\n"}), &c).await;
        assert!(out.content.contains("`read` it first"), "{}", out.content);
        // several cut lines: all of them are in the command, up to five
        std::fs::write(d.path().join("m.txt"), format!("{0}\n{0}\n", "q".repeat(2500))).unwrap();
        assert!(!crate::read::Read.run(json!({"path": "m.txt"}), &c).await.is_error);
        let out = Write.run(json!({"path": "m.txt", "content": "new\n"}), &c).await;
        assert!(out.content.contains("those lines") && out.content.contains("sed -n '1p;2p' m.txt"), "{}", out.content);
    }

    #[tokio::test]
    async fn a_symbol_read_is_not_a_whole_read() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::write(d.path().join("a.rs"), "fn one() {\n}\nfn two() {\n}\nfn three() {\n}\n").unwrap();
        let r = crate::read::Read.run(json!({"path": "a.rs", "symbol": "one"}), &c).await;
        assert!(!r.is_error, "{}", r.content);
        let out = Write.run(json!({"path": "a.rs", "content": "fn other() {}\n"}), &c).await;
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("you were shown only lines 1-2 of its 6 lines"), "{}", out.content);
        assert!(out.content.contains("offset 3"), "{}", out.content);
    }

    #[tokio::test]
    async fn a_home_variable_path_names_the_home_folder() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let home = dirs::home_dir().unwrap();
        assert_eq!(crate::util::resolve(d.path(), "$HOME/n.md"), home.join("n.md"));
        assert_eq!(crate::util::resolve(d.path(), "${HOME}/n.md"), home.join("n.md"));
        // the real home is never written: the folders below are in the temp dir
        let out = Write.run(json!({"path": "sub/deeper/x.md", "content": "x\n"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("; new folders: "), "{}", out.content);
        assert!(out.content.contains("and 1 below it"), "{}", out.content);
        let out = Write.run(json!({"path": "sub/deeper/y.md", "content": "y\n"}), &c).await;
        assert!(!out.content.contains("new folder"), "{}", out.content);
        let out = Write.run(json!({"path": "one/z.md", "content": "z\n"}), &c).await;
        assert!(out.content.contains("; new folder: ") && !out.content.contains("below it"), "{}", out.content);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_this_user_cannot_write_is_not_replaced() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let f = d.path().join("f.txt");
        std::fs::write(&f, "old\n").unwrap();
        // the group may write, the owner may not: for this owner the file is read-only, and the mode says the rest
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o464)).unwrap();
        assert!(!crate::read::Read.run(json!({"path": "f.txt"}), &c).await.is_error);
        let out = Write.run(json!({"path": "f.txt", "content": "new\n"}), &c).await;
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(out.is_error && out.content.contains("the file is read-only"), "{}", out.content);
        assert!(out.content.contains("mode 464"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "old\n");
    }

    #[test]
    fn changed_lines_are_counted_without_a_diff() {
        assert_eq!(crate::edit::line_changes("a\nb\nc\n", "a\nB\nc\nd\n"), (2, 1));
        assert_eq!(crate::edit::line_changes("a\n", "a\n"), (0, 0));
        assert_eq!(crate::edit::line_changes("", "x\ny\n"), (2, 0));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_write_names_the_folders_it_had_made() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        // the temporary file's name is longer than the file's: a 250 char name is valid, its sibling is not
        let name = "n".repeat(250);
        let out = Write.run(json!({"path": format!("fresh/deeper/{name}"), "content": "x\n"}), &c).await;
        assert!(out.is_error && out.content.contains("cannot write"), "{}", out.content);
        assert!(out.content.contains("made before it failed and left empty: new folders: "), "{}", out.content);
        assert!(out.content.contains("and 1 below it"), "{}", out.content);
        assert!(d.path().join("fresh/deeper").is_dir());
        // a folder that existed is not claimed
        let out = Write.run(json!({"path": format!("fresh/deeper/{name}"), "content": "x\n"}), &c).await;
        assert!(out.is_error && !out.content.contains("left empty"), "{}", out.content);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unwritable_folder_is_named_as_the_cause() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::create_dir(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("sub/f.txt"), "old\n").unwrap();
        assert!(!crate::read::Read.run(json!({"path": "sub/f.txt"}), &c).await.is_error);
        std::fs::set_permissions(d.path().join("sub"), std::fs::Permissions::from_mode(0o555)).unwrap();
        let out = Write.run(json!({"path": "sub/f.txt", "content": "new\n"}), &c).await;
        std::fs::set_permissions(d.path().join("sub"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("the folder") && out.content.contains("is not writable"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("sub/f.txt")).unwrap(), "old\n");
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

#[cfg(test)]
mod marks_tests {
    use super::*;
    #[tokio::test]
    async fn strips_new_invisible_characters_on_write() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let out = Write.run(json!({"path":"a.txt", "content":"a\u{200b}b"}), &c).await;
        assert!(out.content.contains("removed newly added invisible characters"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "ab");
    }
    #[tokio::test]
    async fn write_reports_new_marks() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let out = Write.run(json!({"path":"a.txt", "content":"a—b"}), &c).await;
        assert!(out.content.contains("new marks of machine-written text:"), "{}", out.content);
    }
}
