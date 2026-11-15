use nexo_core::{ToolContext, ToolDisplay, ToolOutput};
use serde_json::Value;

use super::one::read_one;
use super::{MIN_ROOM, MULTI_CAP};
use crate::util::{display_path, outside_note, resolve};
/// The name a file goes by in answers: relative to the working directory when inside it.
pub(super) fn shown_of(ctx: &ToolContext, p: &str) -> String {
    let path = resolve(&ctx.cwd, p);
    format!("{}{}", display_path(&ctx.cwd, &path), outside_note(&ctx.cwd, &path))
}

/// Several files in one answer: each under a `=== name ===` header, the same `offset` and `limit` for
/// all, `MULTI_CAP` characters of text in all (what does not fit is cut at a line, a file that finds no
/// room is left unread and says so). A file that cannot be read is told in its place; the call fails
/// only when none could be.
pub(super) async fn read_many(args: &Value, ctx: &ToolContext, files: &[String]) -> ToolOutput {
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
