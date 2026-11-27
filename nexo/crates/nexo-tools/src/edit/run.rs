use nexo_core::{ToolContext, ToolDisplay, ToolOutput};
use serde_json::Value;

use super::apply::{apply, changes};
use super::miss::elsewhere;
use super::show::{now_text, unified};
use super::VERIFIED;
use crate::util::{display_path, is_binary, outside_note, resolve, str_arg, utf16_text, write_atomic, MAX_FILE_BYTES};

/// One file's change, written: what to tell the model and what to show.
pub(super) struct Done {
    shown: String,
    path: std::path::PathBuf,
    note: String,
    added: usize,
    removed: usize,
    diff: String,
    now: String,
    old: String,
    new: String,
}

/// Applies the changes of one file and writes it; the error text when it cannot be done (nothing written).
pub(super) async fn edit_file(p: &str, args: &Value, ctx: &ToolContext) -> Result<Done, String> {
    let changes = changes(args)?;
    let path = resolve(&ctx.cwd, p);
    let shown = format!("{}{}", display_path(&ctx.cwd, &path), outside_note(&ctx.cwd, &path));
    if let Ok(m) = tokio::fs::metadata(&path).await {
        if !m.file_type().is_file() {
            return Err(format!("edit: {shown} is not a regular file (device, pipe or socket)"));
        }
        if m.len() > MAX_FILE_BYTES {
            return Err(format!("edit: {shown} is too large to edit here ({} MiB); use `bash`", m.len() >> 20));
        }
    }
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("edit: {shown} does not exist; use `write` to create it"));
        }
        Err(e) => return Err(format!("edit: cannot read {shown}: {e}")),
    };
    if is_binary(&bytes) {
        // UTF-16 text looks binary (a NUL in every other byte); saying what it is tells the model to use `bash`
        if utf16_text(&bytes).is_some() {
            return Err(format!("edit: {shown} is UTF-16 text, not UTF-8; change it with `bash` in its own encoding"));
        }
        return Err(format!("edit: {shown} is a binary file"));
    }
    if ctx.seen.changed(&path) {
        return Err(format!("edit: {shown} changed since you last read it; `read` it again, then edit"));
    }
    // rewriting a file in another encoding as UTF-8 would mangle every byte it does not share
    let text = String::from_utf8(bytes).map_err(|e| {
        format!(
            "edit: {shown} is not UTF-8 text (invalid byte at offset {}); edit it with `bash` in its own encoding",
            e.utf8_error().valid_up_to()
        )
    })?;
    let (edited, note) = match apply(&text, &changes, &shown) {
        Ok(r) => r,
        Err(e) if e.contains("`old_string` not found") => {
            let hint = elsewhere(&ctx.cwd, &path, &text, &changes).await.unwrap_or_default();
            return Err(format!("{e}{hint}"));
        }
        Err(e) => return Err(e),
    };
    let (edited, stripped) = crate::marks::strip_added(&text, &edited);
    if edited == text {
        return Err(format!("edit: {shown} would not change; the new text is already there{stripped}"));
    }
    write_atomic(&path, edited.as_bytes()).await.map_err(|e| format!("edit: cannot write {shown}: {e}"))?;
    if tokio::fs::read(&path).await.ok().as_deref() != Some(edited.as_bytes()) {
        return Err(format!("edit: {shown} is not as intended after the write; read it before going on"));
    }
    ctx.seen.mark(&path);
    let (added, removed, diff) = unified(&text, &edited, &shown);
    let now = now_text(&text, &edited);
    let now = if now.is_empty() { String::new() } else { format!("\nfile now:\n{now}") };
    let now = format!("{now}{stripped}");
    Ok(Done { shown, path, note, added, removed, diff, now, old: text, new: edited })
}

/// The per-edit check of a written file: empty when the step-level check covers it (a source file inside
/// the project folder), so a data file or a file outside the folder still gets its own.
async fn diag_of(d: &Done, ctx: &ToolContext) -> String {
    if crate::edit_check::step_covers(ctx, &d.path) {
        String::new()
    } else {
        crate::edit_check::report(&d.path).await
    }
}

/// The result text of the files written so far, one `edited …` block each. Their checks run here, one after
/// another and in file order, after every write: a file that uses an item a later file adds is not checked
/// before that file is written.
async fn blocks(written: &[Done], ctx: &ToolContext) -> String {
    let mut text = String::new();
    for d in written {
        let diag = diag_of(d, ctx).await;
        let marks = crate::marks::added_for_path(&d.old, &d.new, &d.path).map(|m| format!("\n{m}")).unwrap_or_default();
        text.push_str(&format!(
            "edited {}{}: +{} −{} lines, {VERIFIED}\n{}{}{marks}{diag}\n",
            d.shown, d.note, d.added, d.removed, d.diff, d.now
        ));
    }
    text
}

pub(super) async fn run_one(args: &Value, ctx: &ToolContext) -> ToolOutput {
    let Some(p) = str_arg(args, "path") else {
        return ToolOutput::error("edit: `path` is required");
    };
    let d = match edit_file(p, args, ctx).await {
        Ok(d) => d,
        Err(e) => return ToolOutput::error(e),
    };
    let diag = diag_of(&d, ctx).await;
    let marks = crate::marks::added_for_path(&d.old, &d.new, &d.path).map(|m| format!("\n{m}")).unwrap_or_default();
    ToolOutput::ok(format!(
        "edited {}{}: +{} −{} lines, {VERIFIED}\n{}{}{marks}{diag}",
        d.shown, d.note, d.added, d.removed, d.diff, d.now
    ))
    // the whole file, before and after: the diff numbers from its first line
    .with_display(ToolDisplay::Diff { path: d.shown, old: d.old, new: d.new, start_line: 1 })
}

/// `files: [{path, edits}]`: files changed in order, the first that fails stops the rest.
pub(super) async fn run_files(args: &Value, ctx: &ToolContext) -> ToolOutput {
    if args.get("path").is_some() || args.get("edits").is_some() || args.get("old_string").is_some() {
        return ToolOutput::error("edit: give either `files` or `path` with its changes, not both");
    }
    let Some(list) = args["files"].as_array().filter(|l| !l.is_empty()) else {
        return ToolOutput::error("edit: `files` must be a list of {path, edits} objects");
    };
    // every entry is read before any is written: a malformed one is found without a half-done step
    let mut entries = Vec::new();
    for (i, f) in list.iter().enumerate() {
        let Some(p) = str_arg(f, "path") else {
            return ToolOutput::error(format!("edit: files[{}]: `path` is required", i + 1));
        };
        if let Err(e) = changes(f) {
            return ToolOutput::error(format!("edit: files[{}] ({p}): {}", i + 1, e.trim_start_matches("edit: ")));
        }
        entries.push((p, f));
    }
    let mut written: Vec<Done> = Vec::new();
    for (i, (p, f)) in entries.iter().enumerate() {
        match edit_file(p, f, ctx).await {
            Ok(d) => written.push(d),
            Err(e) => {
                let rest: Vec<&str> = entries[i + 1..].iter().map(|(p, _)| *p).collect();
                let done: Vec<&str> = written.iter().map(|d| d.shown.as_str()).collect();
                let applied = if done.is_empty() { "none".to_string() } else { done.join(", ") };
                let skipped = if rest.is_empty() { String::new() } else { format!("; not tried: {}", rest.join(", ")) };
                let text = blocks(&written, ctx).await;
                return ToolOutput::error(format!("{text}{e}\napplied before the failure: {applied}{skipped}"));
            }
        }
    }
    let names: Vec<&str> = written.iter().map(|d| d.shown.as_str()).collect();
    let text = blocks(&written, ctx).await;
    ToolOutput::ok(format!("edited {} files: {}\n{text}", written.len(), names.join(", ")))
}
