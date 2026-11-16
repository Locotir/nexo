use nexo_core::{ToolContext, ToolDisplay, ToolOutput};
use serde_json::Value;

use super::ipynb::notebook;
use super::show::{show_picture, show_raster};
use super::symbol::read_symbol;
use super::{DEFAULT_LIMIT, FILE_CAP, LINE_CHARS};
use crate::picture;
use crate::program::{about, bin_offered};
use crate::util::{cut_line, display_path, int_arg, is_binary, outside_note, resolve, MAX_FILE_BYTES};

/// A compiled program shows nothing as text: the answer is what it is and the call that reads it.
fn not_text(
    ctx: &ToolContext,
    shown: &str,
    path: &std::path::Path,
    kind: &nexo_bin::Compiled,
    size: u64,
) -> ToolOutput {
    let file = display_path(&ctx.cwd, path);
    ToolOutput::error(format!("read: {shown} is not text: {}", about(kind, Some(size), &file, bin_offered(ctx))))
}

/// One file as numbered lines, a picture as an image. With `room`, at most that many characters of
/// text are returned (the lines that do not fit are left to a later call with `offset`).
pub(super) async fn read_one(args: &Value, ctx: &ToolContext, p: &str, room: Option<usize>) -> ToolOutput {
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
            if let Some(kind) = nexo_bin::kind_at(&path) {
                return not_text(ctx, &shown, &path, &kind, m.len());
            }
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
    if args.get("symbol").is_some() && !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("rs")) {
        return ToolOutput::error(
            "read: `symbol` currently supports Rust (.rs) files only; use line ranges for other files",
        );
    }
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
        if let Some(kind) = nexo_bin::kind_of(&bytes) {
            return not_text(ctx, &shown, &path, &kind, bytes.len() as u64);
        }
        return ToolOutput::error(format!("read: {shown} is a binary file ({} bytes)", bytes.len()));
    }
    if let Some(symbol) = args.get("symbol").and_then(Value::as_str) {
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) => text,
            Err(_) => return ToolOutput::error("read: Rust symbol lookup requires UTF-8"),
        };
        return read_symbol(ctx, &shown, text, symbol);
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
    // the call's cap, held to what the window leaves of a request
    let cap = ctx.cap(FILE_CAP);
    for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        let shown_line = cut_line(line, LINE_CHARS);
        let numbered = format!("{:>6}│ {}\n", i + 1, shown_line);
        // always one line, so that a file that gets any room shows something
        let here = numbered.chars().count();
        let over_room = room.is_some_and(|r| out.len() + numbered.len() > r);
        if n >= offset && (over_room || chars + here > cap) {
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
        // where the window, not the tool's own cap, made the cut, the fact says how much of the whole was kept
        let window = if cap < FILE_CAP {
            let rest: usize = text
                .lines()
                .enumerate()
                .skip(n)
                .take(offset - 1 + limit - n)
                .map(|(i, l)| format!("{:>6}│ {}\n", i + 1, cut_line(l, LINE_CHARS)).chars().count())
                .sum();
            ctx.cut_fact(FILE_CAP, chars, chars + rest, &format!("read with offset {} to continue", n + 1))
        } else {
            None
        };
        out.push_str(&match window {
            Some(fact) => format!("… lines {offset}-{n} of {total} returned, {} more; {fact}\n", total - n),
            None => format!(
                "… lines {offset}-{n} of {total} returned, {} more; the call hit its size cap, so read with offset {} to continue\n",
                total - n,
                n + 1
            ),
        });
    } else if n < total {
        out.push_str(&format!("… {} more lines (of {total}); read with offset {} to continue\n", total - n, n + 1));
    }
    ToolOutput::ok(out).with_display(ToolDisplay::File { path: shown, text: raw, start_line: offset })
}
