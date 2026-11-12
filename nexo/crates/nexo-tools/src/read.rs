//! `read`: a file, as numbered lines; a picture, as an image the model looks at.

use async_trait::async_trait;
use base64::Engine;
use nexo_core::message::ImageSource;
use nexo_core::tool::{call_paths, paths_label};
use nexo_core::{Compact, Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

use crate::picture;
use crate::util::{cut_line, display_path, int_arg, is_binary, outside_note, resolve, MAX_FILE_BYTES};

/// Lines returned when `limit` is not given.
pub const DEFAULT_LIMIT: u64 = 2000;
/// Characters kept of one line.
pub const LINE_CHARS: usize = 2000;
/// Files one call may name in `paths`.
pub const MAX_PATHS: usize = 20;
/// Characters of file text one call of several files returns in all; what does not fit is cut at a line.
pub const MULTI_CAP: usize = 100_000;
/// Characters of file text one single-file `read` call returns at most. Without it one call could put a
/// whole 150 000-character file (about 50 000 tokens) into the model's context; other agents cap one
/// read at 10 000 to 25 000 tokens, and 64 000 characters is about 16 000 tokens. What does not fit is
/// cut at a line and the trailer says which `offset` reads on.
pub const FILE_CAP: usize = 64_000;
/// Room left below which a file of a multi-file call is not started.
const MIN_ROOM: usize = 1_000;

pub struct Read;

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str {
        "read"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read".into(),
            description: "Read text files: numbered lines (`   12│ text`), at most `limit` \
                          from line `offset` (1-based), default the first 2000; a \
                          line over 2000 characters is cut; a call is cut at 64000 characters. Several files at once: `paths` (each \
                          under a `=== path ===` header; one that fails is reported in place). A PNG, JPEG, GIF, WebP, PPM, PGM \
                          or BMP file is shown as an image (at most 5 MB and 2000 px); a Jupyter notebook as its cells and outputs. Directories and other \
                          binary files are refused (use `ls` for a directory)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "The file; relative paths start at the working directory."},
                    "paths": {"type": "array", "items": {"type": "string"}, "description": "Several files instead of `path` (at most 20, about 100000 characters in all); offset and limit apply to each."},
                    "offset": {"type": "integer", "minimum": 1, "description": "First line to return (1-based). Default 1."},
                    "limit": {"type": "integer", "minimum": 1, "description": "Lines to return. Default 2000."}
                }
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: "Read text files as numbered lines; shows images. Not for directories: use `ls`.",
            params: &[
                ("paths", "Several files, at most 20"),
                ("offset", "First line (1-based)"),
                ("limit", "Lines, default 2000; a call returns at most 64000 characters"),
            ],
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn summary(&self, args: &Value) -> String {
        let files = call_paths(args);
        if files.is_empty() {
            return "read (no path)".into();
        }
        let range = match (int_arg(args, "offset"), int_arg(args, "limit")) {
            (Some(o), Some(l)) => format!(" [{o}..{}]", o.saturating_add(l).saturating_sub(1)),
            (Some(o), None) => format!(" [{o}..]"),
            (None, Some(l)) => format!(" [..{l}]"),
            _ => String::new(),
        };
        format!("read {}{range}", paths_label(&files))
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let files = call_paths(&args);
        match files.as_slice() {
            [] => ToolOutput::error("read: `path` or `paths` is required"),
            [one] => read_one(&args, ctx, one, None).await,
            _ if files.len() > MAX_PATHS => {
                ToolOutput::error(format!("read: {} files named; at most {MAX_PATHS} in one call", files.len()))
            }
            _ => read_many(&args, ctx, &files).await,
        }
    }
}

/// The name a file goes by in answers: relative to the working directory when inside it.
fn shown_of(ctx: &ToolContext, p: &str) -> String {
    let path = resolve(&ctx.cwd, p);
    format!("{}{}", display_path(&ctx.cwd, &path), outside_note(&ctx.cwd, &path))
}

/// Several files in one answer: each under a `=== name ===` header, the same `offset` and `limit` for
/// all, `MULTI_CAP` characters of text in all (what does not fit is cut at a line, a file that finds no
/// room is left unread and says so). A file that cannot be read is told in its place; the call fails
/// only when none could be.
async fn read_many(args: &Value, ctx: &ToolContext, files: &[String]) -> ToolOutput {
    let mut content = String::new();
    let mut items = Vec::new();
    let mut images = Vec::new();
    let (mut left, mut failed) = (MULTI_CAP, 0usize);
    for p in files {
        let shown = shown_of(ctx, p);
        if left < MIN_ROOM {
            content.push_str(&format!(
                "=== {shown} (not read) ===\nthe combined output of a call is capped at {MULTI_CAP} characters; read this file in its own call\n"
            ));
            items.push(format!("{shown}: not read (cap)"));
            failed += 1;
            continue;
        }
        let out = read_one(args, ctx, p, Some(left)).await;
        left = left.saturating_sub(out.content.len());
        let note = if out.is_error {
            failed += 1;
            " (failed)"
        } else {
            ""
        };
        content.push_str(&format!("=== {shown}{note} ===\n{}", out.content));
        if !out.content.ends_with('\n') {
            content.push('\n');
        }
        items.push(match &out.display {
            ToolDisplay::File { text, .. } if !out.is_error => {
                format!("{shown}: {}", nexo_core::count(text.lines().count(), "line"))
            }
            _ if out.is_error => format!("{shown}: failed"),
            _ => shown.clone(),
        });
        images.extend(out.images);
    }
    let mut out = if failed == files.len() { ToolOutput::error(content) } else { ToolOutput::ok(content) };
    out.display = ToolDisplay::List { items };
    out.with_images(images)
}

/// One file as numbered lines, a picture as an image. With `room`, at most that many characters of
/// text are returned (the lines that do not fit are left to a later call with `offset`).
async fn read_one(args: &Value, ctx: &ToolContext, p: &str, room: Option<usize>) -> ToolOutput {
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
            // all of it the model can be shown: enough to replace it on purpose
            ctx.seen.mark(&path);
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
    ctx.seen.mark(&path);
    if let Some(pic) = picture::sniff(&bytes) {
        return show_picture(&shown, pic, &bytes, ctx.vision);
    }
    if let Some(raster) = picture::decode_raster(&bytes) {
        return show_raster(&shown, raster, bytes.len(), ctx.vision);
    }
    if let Some((name, ext)) = picture::foreign(&bytes) {
        return ToolOutput::error(format!(
            "read: {shown} is a {name} image ({} bytes), which `read` cannot show; convert a copy to PNG with \
                 `bash`, e.g. `magick in.{ext} out.png`, and read that",
            bytes.len()
        ));
    }
    if is_binary(&bytes) {
        return ToolOutput::error(format!("read: {shown} is a binary file ({} bytes)", bytes.len()));
    }
    let text = String::from_utf8_lossy(&bytes);
    // a notebook is JSON whose outputs can hold megabytes of base64: show its cells as text instead
    let notebook =
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ipynb")) { notebook(&text) } else { None };
    let text = match &notebook {
        Some((_, cells)) => std::borrow::Cow::Borrowed(cells.as_str()),
        None => text,
    };
    if text.is_empty() {
        return ToolOutput::ok(format!("{shown}: (empty file)")).with_display(ToolDisplay::File {
            path: shown,
            text: String::new(),
            start_line: 1,
        });
    }
    let offset = int_arg(args, "offset").unwrap_or(1).max(1) as usize;
    let limit = int_arg(args, "limit").unwrap_or(DEFAULT_LIMIT).max(1) as usize;
    let total = text.lines().count();
    if offset > total {
        return ToolOutput::error(format!("read: {shown} has {total} lines; offset {offset} is past the end"));
    }
    let mut out = match &notebook {
        Some((n, _)) => {
            format!("({n} notebook cells shown as text; the file is JSON, so change cells with `bash`, e.g. `jq`)\n")
        }
        None => String::new(),
    };
    let mut raw = String::new();
    let mut n = 0usize;
    let mut capped = false;
    let mut chars = out.chars().count();
    for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        let shown_line = cut_line(line, LINE_CHARS);
        let numbered = format!("{:>6}│ {}\n", i + 1, shown_line);
        // always one line, so that a file that gets any room shows something
        let here = numbered.chars().count();
        let over_room = room.is_some_and(|r| out.len() + numbered.len() > r);
        if n >= offset && (over_room || chars + here > FILE_CAP) {
            capped = true;
            break;
        }
        out.push_str(&numbered);
        chars += here;
        raw.push_str(&shown_line);
        raw.push('\n');
        n = i + 1;
    }
    if capped {
        out.push_str(&format!(
            "… lines {offset}-{n} of {total} returned, {} more; the call hit its size cap, so read with offset {} to continue\n",
            total - n,
            n + 1
        ));
    } else if n < total {
        out.push_str(&format!("… {} more lines (of {total}); read with offset {} to continue\n", total - n, n + 1));
    }
    ToolOutput::ok(out).with_display(ToolDisplay::File { path: shown, text: raw, start_line: offset })
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

/// A PPM, PGM or BMP as a PNG the model looks at; same caps and wording as `show_picture`.
fn show_raster(shown: &str, raster: Result<picture::Raster, String>, len: usize, vision: bool) -> ToolOutput {
    let r = match raster {
        Ok(r) => r,
        Err(why) => {
            return ToolOutput::error(format!(
                "read: {shown} is an image `read` cannot decode ({why}); convert a copy to PNG with `bash`, \
                 e.g. `magick in.img out.png`, and read that"
            ));
        }
    };
    let what = format!("{} image of {}x{} px, {len} bytes", r.label, r.width, r.height);
    if !vision {
        return ToolOutput::ok(format!("{shown}: a {what}; this model cannot view images."));
    }
    let too_large = |extra: &str| {
        ToolOutput::error(format!(
            "read: {shown} is a {what}{extra}, too large to show (at most 5 MB and {} px on the long side); \
             shrink a copy first, e.g. with `bash` and `magick in.img -resize 2000x2000 out.png`",
            picture::MAX_SIDE
        ))
    };
    if r.width.max(r.height) > picture::MAX_SIDE {
        return too_large("");
    }
    let png = match r.to_png() {
        Ok(p) => p,
        Err(e) => return ToolOutput::error(format!("read: {shown} is a {what} that could not be converted: {e}")),
    };
    if png.len() > picture::MAX_BYTES {
        return too_large(&format!(" ({} bytes as PNG)", png.len()));
    }
    let data = base64::engine::general_purpose::STANDARD.encode(&png);
    ToolOutput::ok(format!("{shown}: {what}, shown as PNG"))
        .with_images(vec![ImageSource::Base64 { media_type: "image/png".into(), data }])
}

/// A Jupyter notebook's cells as plain text: each cell's source, then its outputs; a picture or
/// other binary output becomes one line naming its type and size. `None` when it is not one.
fn notebook(text: &str) -> Option<(usize, String)> {
    let v: Value = serde_json::from_str(text).ok()?;
    let cells = v.get("cells")?.as_array()?;
    let joined = |v: Option<&Value>| -> String {
        let s = match v {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
            _ => String::new(),
        };
        if s.is_empty() || s.ends_with('\n') {
            s
        } else {
            s + "\n"
        }
    };
    let mut out = String::new();
    for (i, c) in cells.iter().enumerate() {
        let kind = c.get("cell_type").and_then(Value::as_str).unwrap_or("cell");
        out.push_str(&format!("[cell {} · {kind}]\n", i + 1));
        out.push_str(&joined(c.get("source")));
        let outputs = c.get("outputs").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        for o in outputs {
            out.push_str("[output]\n");
            match o.get("output_type").and_then(Value::as_str) {
                Some("stream") => out.push_str(&joined(o.get("text"))),
                Some("error") => {
                    let field = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    out.push_str(&format!("{}: {}\n", field("ename"), field("evalue")));
                }
                _ => {
                    let Some(data) = o.get("data").and_then(Value::as_object) else { continue };
                    if let Some(t) = data.get("text/plain") {
                        out.push_str(&joined(Some(t)));
                    }
                    for (mime, body) in data.iter().filter(|(m, _)| *m != "text/plain") {
                        let size = joined(Some(body)).len();
                        out.push_str(&format!("({mime}, {} KiB, not shown)\n", size.div_ceil(1024)));
                    }
                }
            }
        }
    }
    Some((cells.len(), out))
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
    async fn a_notebook_reads_as_its_cells_without_the_base64_of_its_pictures() {
        let d = tempfile::tempdir().unwrap();
        let png = "A".repeat(40_000);
        let nb = json!({"cells": [
            {"cell_type": "markdown", "source": ["# Fit\n", "A line."]},
            {"cell_type": "code", "source": "print(1)", "outputs": [
                {"output_type": "stream", "text": ["1\n"]},
                {"output_type": "display_data", "data": {"image/png": png, "text/plain": ["<Figure>"]}},
                {"output_type": "error", "ename": "ValueError", "evalue": "bad", "traceback": ["\u{1b}[31mlong"]}
            ]}
        ], "metadata": {}, "nbformat": 4});
        std::fs::write(d.path().join("fit.ipynb"), nb.to_string()).unwrap();
        let out = Read.run(json!({"path": "fit.ipynb"}), &ctx(d.path())).await;
        assert!(!out.is_error, "{}", out.content);
        let c = &out.content;
        assert!(c.starts_with("(2 notebook cells shown as text;"), "{c}");
        for want in [
            "│ [cell 1 · markdown]",
            "│ # Fit",
            "│ A line.",
            "│ [cell 2 · code]",
            "│ print(1)",
            "│ 1",
            "│ <Figure>",
            "│ (image/png, 40 KiB, not shown)",
            "│ ValueError: bad",
        ] {
            assert!(c.contains(want), "no {want:?} in\n{c}");
        }
        assert!(!c.contains("AAAA") && !c.contains("traceback"), "{c}");
        // a file named .ipynb that is not a notebook reads as it is
        std::fs::write(d.path().join("x.ipynb"), "not json\n").unwrap();
        let out = Read.run(json!({"path": "x.ipynb"}), &ctx(d.path())).await;
        assert_eq!(out.content, "     1│ not json\n");
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

    #[tokio::test]
    async fn a_ppm_and_a_bmp_come_back_as_png_images() {
        let d = tempfile::tempdir().unwrap();
        let mut ppm = b"P6\n2 1\n255\n".to_vec();
        ppm.extend([10, 20, 30, 40, 50, 60]);
        std::fs::write(d.path().join("s.ppm"), &ppm).unwrap();
        let out = Read.run(json!({"path": "s.ppm"}), &ctx(d.path())).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.content, format!("s.ppm: PPM image of 2x1 px, {} bytes, shown as PNG", ppm.len()));
        match out.images.as_slice() {
            [ImageSource::Base64 { media_type, data }] => {
                assert_eq!(media_type, "image/png");
                let png = base64::engine::general_purpose::STANDARD.decode(data).unwrap();
                let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap().to_rgb8();
                assert_eq!(img.into_raw(), vec![10, 20, 30, 40, 50, 60]);
            }
            other => panic!("{other:?}"),
        }
        let mut c = ctx(d.path());
        c.vision = false;
        let out = Read.run(json!({"path": "s.ppm"}), &c).await;
        assert!(!out.is_error && out.images.is_empty() && out.content.contains("2x1"), "{}", out.content);
        assert!(out.content.contains("cannot view images"), "{}", out.content);
    }

    #[tokio::test]
    async fn a_raster_over_the_cap_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let mut f = b"P5\n2001 2\n255\n".to_vec();
        f.resize(f.len() + 2001 * 2, 7);
        std::fs::write(d.path().join("wide.pgm"), f).unwrap();
        let out = Read.run(json!({"path": "wide.pgm"}), &ctx(d.path())).await;
        assert!(out.is_error && out.images.is_empty(), "{}", out.content);
        assert!(out.content.contains("2001x2") && out.content.contains("too large"), "{}", out.content);
        std::fs::write(d.path().join("cut.ppm"), b"P6 9 9 255\nabc").unwrap();
        let out = Read.run(json!({"path": "cut.ppm"}), &ctx(d.path())).await;
        assert!(out.is_error && out.content.contains("cut short"), "{}", out.content);
    }

    #[tokio::test]
    async fn a_tiff_gets_a_convert_hint_not_binary_file() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("scan.tiff"), b"II*\0\x08\0\0\0\0\0").unwrap();
        let out = Read.run(json!({"path": "scan.tiff"}), &ctx(d.path())).await;
        assert!(out.is_error && out.images.is_empty());
        assert!(
            out.content.contains("TIFF image") && out.content.contains("magick in.tiff out.png"),
            "{}",
            out.content
        );
        assert!(!out.content.contains("binary file"));
    }

    #[tokio::test]
    async fn several_files_come_back_under_headers_and_one_failure_does_not_fail_the_rest() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(d.path());
        std::fs::write(d.path().join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(d.path().join("b.txt"), "three\nfour\nfive\n").unwrap();
        std::fs::write(d.path().join("z.bin"), b"\x00\x01").unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let out = Read.run(json!({"paths": ["a.txt", "missing.txt", "b.txt", "z.bin", "sub"]}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        let t = &out.content;
        assert!(t.contains("=== a.txt ===\n     1│ one\n     2│ two\n"), "{t}");
        assert!(t.contains("=== missing.txt (failed) ===\nread: missing.txt does not exist"), "{t}");
        assert!(t.contains("=== b.txt ===\n     1│ three\n"), "{t}");
        assert!(t.contains("=== z.bin (failed) ===") && t.contains("binary"), "{t}");
        assert!(t.contains("=== sub (failed) ===") && t.contains("directory"), "{t}");
        // the order asked for is the order given
        assert!(t.find("a.txt").unwrap() < t.find("b.txt").unwrap());
        match &out.display {
            ToolDisplay::List { items } => {
                assert_eq!(items[0], "a.txt: 2 lines");
                assert_eq!(items[1], "missing.txt: failed");
                assert_eq!(items.len(), 5);
            }
            _ => panic!("{:?}", out.display),
        }
        // both files that were read count as seen
        assert_eq!(c.seen.unseen(&resolve(d.path(), "a.txt")), None);
        assert_eq!(c.seen.unseen(&resolve(d.path(), "b.txt")), None);
        // all failing is an error
        let out = Read.run(json!({"paths": ["nope1", "nope2"]}), &c).await;
        assert!(out.is_error && out.content.contains("=== nope2 (failed) ==="), "{}", out.content);
    }

    #[tokio::test]
    async fn path_and_paths_are_one_list_and_a_single_file_reads_as_before() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(d.path());
        std::fs::write(d.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(d.path().join("b.txt"), "x\ny\nz\n").unwrap();
        let plain = Read.run(json!({"path": "a.txt"}), &c).await;
        let one = Read.run(json!({"paths": ["a.txt", "a.txt", " "]}), &c).await;
        assert_eq!(one.content, plain.content);
        let both = Read.run(json!({"path": "a.txt", "paths": ["b.txt", "a.txt"]}), &c).await;
        assert_eq!(both.content.matches("=== a.txt ===").count(), 1, "{}", both.content);
        assert!(both.content.contains("=== b.txt ==="));
        // the window applies to each file
        let w = Read.run(json!({"paths": ["a.txt", "b.txt"], "offset": 2, "limit": 1}), &c).await;
        assert!(w.content.contains("=== a.txt ===\n     2│ two\n… 1 more lines"), "{}", w.content);
        assert!(w.content.contains("=== b.txt ===\n     2│ y\n"), "{}", w.content);
        let none = Read.run(json!({"paths": []}), &c).await;
        assert!(none.is_error && none.content.contains("`path` or `paths`"), "{}", none.content);
        assert_eq!(Read.summary(&json!({"paths": ["a.txt", "b.txt"]})), "read a.txt, b.txt");
        assert_eq!(Read.summary(&json!({"paths": ["a", "b", "c", "d"]})), "read a, b (+2)");
    }

    #[tokio::test]
    async fn a_call_is_capped_as_a_whole_and_says_how_to_go_on() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(d.path());
        let big: String =
            (0..3000).map(|i| format!("line number {i} with some padding to make it long enough\n")).collect();
        for n in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(d.path().join(n), &big).unwrap();
        }
        let out = Read.run(json!({"paths": ["a.txt", "b.txt", "c.txt"]}), &c).await;
        assert!(!out.is_error);
        assert!(out.content.len() < MULTI_CAP + 2_000, "{}", out.content.len());
        assert!(out.content.contains("the call hit its size cap, so read with offset"), "capped note missing");
        assert!(out.content.contains("=== c.txt (not read) ==="), "last file should be left out");
        let many: Vec<String> = (0..=MAX_PATHS).map(|i| format!("f{i}")).collect();
        let out = Read.run(json!({"paths": many}), &c).await;
        assert!(out.is_error && out.content.contains("at most"), "{}", out.content);
    }

    /// Rebuilds a file from the successive reads that the trailers lead, and checks the cap each time.
    async fn read_all(c: &ToolContext, name: &str) -> (String, usize) {
        let (mut got, mut offset, mut calls) = (String::new(), 1usize, 0usize);
        loop {
            let out = Read.run(json!({"path": name, "offset": offset}), c).await;
            assert!(!out.is_error, "{}", out.content);
            assert!(out.content.chars().count() <= FILE_CAP + 200, "over the cap: {}", out.content.len());
            calls += 1;
            match out.content.rsplit_once("… lines ") {
                Some((body, trailer)) => {
                    got.push_str(body);
                    let next: usize =
                        trailer.rsplit("read with offset ").next().unwrap().split(' ').next().unwrap().parse().unwrap();
                    assert!(next > offset, "no progress: {trailer}");
                    offset = next;
                }
                None => {
                    got.push_str(&out.content);
                    return (got, calls);
                }
            }
        }
    }

    #[tokio::test]
    async fn a_file_under_the_cap_comes_back_unchanged_and_over_it_is_cut_at_a_line() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(d.path());
        std::fs::write(d.path().join("s.txt"), "one\ntwo\n").unwrap();
        let out = Read.run(json!({"path": "s.txt"}), &c).await;
        assert_eq!(out.content, "     1│ one\n     2│ two\n");
        let text: String = (1..=401).map(|i| format!("{i} {}\n", "x".repeat(380))).collect();
        std::fs::write(d.path().join("b.txt"), &text).unwrap();
        let out = Read.run(json!({"path": "b.txt"}), &c).await;
        assert!(out.content.chars().count() <= FILE_CAP + 200, "{}", out.content.len());
        let (body, trailer) = out.content.rsplit_once("… lines ").unwrap();
        assert!(body.ends_with('\n') && trailer.starts_with("1-"), "{trailer}");
        assert!(trailer.contains("of 401 returned") && trailer.contains("read with offset"), "{trailer}");
        // reading on with the offsets the trailers give: no gap, no overlap, to the end
        let (all, calls) = read_all(&c, "b.txt").await;
        assert!(calls >= 2);
        let want: String = text.lines().enumerate().map(|(i, l)| format!("{:>6}│ {l}\n", i + 1)).collect();
        assert_eq!(all, want);
        // an explicit window is capped the same way
        let out = Read.run(json!({"path": "b.txt", "offset": 10, "limit": 300}), &c).await;
        assert!(out.content.contains("… lines 10-") && out.content.contains("of 401 returned"), "{}", out.content);
    }

    #[tokio::test]
    async fn a_giant_first_line_and_multibyte_text_at_the_cut_do_not_panic() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(d.path());
        std::fs::write(d.path().join("g.txt"), format!("{}\nnext\n", "é".repeat(200_000))).unwrap();
        let out = Read.run(json!({"path": "g.txt"}), &c).await;
        assert!(!out.is_error && out.content.chars().count() < FILE_CAP, "{}", out.content.len());
        let text: String = (1..=300).map(|i| format!("{i} {}\n", "日".repeat(1990))).collect();
        std::fs::write(d.path().join("m.txt"), &text).unwrap();
        let (all, calls) = read_all(&c, "m.txt").await;
        assert!(calls >= 2 && all.contains("   300│ 300 日"), "{calls}");
        let out = Read.run(json!({"paths": ["m.txt", "g.txt"]}), &c).await;
        assert!(out.content.contains("=== g.txt"), "{}", out.content.len());
    }
}
