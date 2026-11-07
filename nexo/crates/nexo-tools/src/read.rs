//! `read`: a file, as numbered lines; a picture, as an image the model looks at.

use async_trait::async_trait;
use base64::Engine;
use nexo_core::message::ImageSource;
use nexo_core::{Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

use crate::picture;
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
                          and `read` with `offset`/`limit` to look at that part of a large file. A PNG, JPEG, GIF or WebP \
                          file is shown as an image (at most 5 MB and 2000 px). Directories and other \
                          binary files are refused (use `ls` for a directory)."
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
                    (Some(o), Some(l)) => format!(" [{o}..{}]", o.saturating_add(l).saturating_sub(1)),
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
            if !m.file_type().is_file() {
                return ToolOutput::error(format!("read: {shown} is not a regular file (device, pipe or socket)"));
            }
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
        if let Some(pic) = picture::sniff(&bytes) {
            return show_picture(&shown, pic, &bytes, ctx.vision);
        }
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

/// A picture as the image the model looks at, with one line of text; refused over the size cap,
/// and only described where the model cannot see pictures.
fn show_picture(shown: &str, pic: picture::Picture, bytes: &[u8], vision: bool) -> ToolOutput {
    let what = format!("{} image of {}x{} px, {} bytes", pic.label(), pic.width, pic.height, bytes.len());
    if !vision {
        return ToolOutput::ok(format!("{shown}: a {what}; this model cannot view images."));
    }
    if bytes.len() > picture::MAX_BYTES || pic.width.max(pic.height) > picture::MAX_SIDE {
        return ToolOutput::error(format!(
            "read: {shown} is a {what}, too large to show (at most 5 MB and {} px on the long side); \
             shrink a copy first, e.g. with `bash` and `magick in.png -resize 2000x2000 out.png`",
            picture::MAX_SIDE
        ));
    }
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    ToolOutput::ok(format!("{shown}: {what}"))
        .with_images(vec![ImageSource::Base64 { media_type: pic.media_type.into(), data }])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::write::Write;

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

    #[cfg(unix)]
    #[tokio::test]
    async fn a_device_is_refused_not_read() {
        let d = tempfile::tempdir().unwrap();
        let out = Read.run(json!({"path": "/dev/zero"}), &ctx(d.path())).await;
        assert!(out.is_error && out.content.contains("not a regular file"), "{}", out.content);
        let out =
            Write.run(json!({"path": "/dev/zero", "content": "x"}), &ToolContext::new(d.path().to_path_buf())).await;
        assert!(out.is_error && out.content.contains("not a regular file"), "{}", out.content);
    }

    #[test]
    fn a_zero_offset_and_limit_do_not_underflow_the_summary() {
        let s = Read.summary(&json!({"path": "a", "offset": 0, "limit": 0}));
        assert_eq!(s, "read a [0..0]");
    }

    fn png_file(d: &std::path::Path, name: &str, w: u32, h: u32) {
        std::fs::write(d.join(name), crate::picture::tests::png(w, h)).unwrap();
    }

    #[tokio::test]
    async fn a_png_comes_back_as_an_image_with_a_size_line() {
        let d = tempfile::tempdir().unwrap();
        png_file(d.path(), "n.png", 30, 12);
        let out = Read.run(json!({"path": "n.png"}), &ctx(d.path())).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.starts_with("n.png: PNG image of 30x12 px, "), "{}", out.content);
        assert!(out.content.ends_with(" bytes"));
        match out.images.as_slice() {
            [ImageSource::Base64 { media_type, data }] => {
                assert_eq!(media_type, "image/png");
                let back = base64::engine::general_purpose::STANDARD.decode(data).unwrap();
                assert_eq!(back, std::fs::read(d.path().join("n.png")).unwrap());
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn the_magic_bytes_decide_not_the_extension() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("shot.dat"), crate::picture::tests::png(4, 4)).unwrap();
        assert_eq!(Read.run(json!({"path": "shot.dat"}), &ctx(d.path())).await.images.len(), 1);
        std::fs::write(d.path().join("fake.png"), "just text\n").unwrap();
        let out = Read.run(json!({"path": "fake.png"}), &ctx(d.path())).await;
        assert!(out.images.is_empty() && out.content.contains("just text"), "{}", out.content);
    }

    #[tokio::test]
    async fn a_model_without_vision_gets_text_only() {
        let d = tempfile::tempdir().unwrap();
        png_file(d.path(), "n.png", 30, 12);
        let mut c = ctx(d.path());
        c.vision = false;
        let out = Read.run(json!({"path": "n.png"}), &c).await;
        assert!(!out.is_error && out.images.is_empty());
        assert!(out.content.contains("30x12") && out.content.contains("cannot view images"), "{}", out.content);
    }

    #[tokio::test]
    async fn a_picture_over_the_cap_is_refused_with_the_size() {
        let d = tempfile::tempdir().unwrap();
        png_file(d.path(), "wide.png", 2001, 2);
        let out = Read.run(json!({"path": "wide.png"}), &ctx(d.path())).await;
        assert!(out.is_error && out.images.is_empty(), "{}", out.content);
        assert!(out.content.contains("2001x2") && out.content.contains("too large"), "{}", out.content);
        // within 2000 px but over 5 MB: the header is real, the body is padding
        let mut big = crate::picture::tests::png(8, 8);
        big.resize(crate::picture::MAX_BYTES + 1, 0);
        std::fs::write(d.path().join("heavy.png"), big).unwrap();
        let out = Read.run(json!({"path": "heavy.png"}), &ctx(d.path())).await;
        assert!(out.is_error && out.content.contains("5 MB"), "{}", out.content);
    }
}
