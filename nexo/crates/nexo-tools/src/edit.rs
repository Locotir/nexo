//! `edit`: replace one exact occurrence of a text in a file.

use async_trait::async_trait;
use nexo_core::{Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};
use similar::{ChangeTag, TextDiff};

use crate::util::{
    bool_arg, cut_line, display_path, is_binary, outside_note, resolve, str_arg, write_atomic, MAX_FILE_BYTES,
};

/// Lines of the unified diff shown to the model.
const DIFF_LINES: usize = 80;
/// A file of at most this many lines comes back whole after a change.
pub const WHOLE_LINES: usize = 80;
/// What the first line of a change's result says once the file was read back and is as intended.
pub const VERIFIED: &str = "applied and verified on disk";

pub struct Edit;

#[async_trait]
impl Tool for Edit {
    fn name(&self) -> &str {
        "edit"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description: "Replace one exact occurrence of `old_string` in a file with `new_string`. \
                          `old_string` must match the file's text exactly (indentation included) and \
                          occur exactly once; include a few surrounding lines to make it unique, or set \
                          `replace_all` to change every occurrence (renames). Several changes to one file go in one call as \
                          `edits`, a list of {old_string, new_string, replace_all} applied in order; if one fails \
                          nothing is written. Several files: `files`. `check` runs a test or build after."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "The file."},
                    "old_string": {"type": "string", "description": "The exact text to replace."},
                    "new_string": {"type": "string", "description": "The text that replaces it (empty to delete)."},
                    "replace_all": {"type": "boolean", "description": "Replace every occurrence."},
                    "edits": {"type": "array", "description": "Several changes to this file: [{old_string, new_string, replace_all}].", "items": {"type": "object"}},
                    "files": {"type": "array", "description": "Instead of `path`: [{path, edits}]; stops at the first failure.", "items": {"type": "object", "properties": {"path": {"type": "string"}, "edits": {"type": "array"}}}},
                    "check": {"type": "string", "description": "Command to run after; exit and tail come back."}
                },
                "required": []
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Write
    }

    fn summary(&self, args: &Value) -> String {
        if let Some(f) = args.get("files").and_then(Value::as_array) {
            return format!("edit {} files", f.len());
        }
        match str_arg(args, "path") {
            Some(p) if args.get("edits").and_then(Value::as_array).is_some_and(|e| e.len() > 1) => {
                format!("edit {p} ({} changes)", args["edits"].as_array().map_or(0, Vec::len))
            }
            Some(p) if bool_arg(args, "replace_all") => format!("edit {p} (all occurrences)"),
            Some(p) => format!("edit {p}"),
            None => "edit (no path)".into(),
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let check = str_arg(&args, "check").filter(|c| !c.trim().is_empty());
        let mut out = if args.get("files").is_some() { run_files(&args, ctx).await } else { run_one(&args, ctx).await };
        if let Some(c) = check {
            // the proof the model asked for, after the change; a failed change has nothing to prove
            if !out.is_error {
                out.content.push_str(&crate::postcheck::run_own(c, ctx).await);
            }
        }
        out
    }
}

/// One file's change, written: what to tell the model and what to show.
struct Done {
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
async fn edit_file(p: &str, args: &Value, ctx: &ToolContext) -> Result<Done, String> {
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
        return Err(format!("edit: {shown} is a binary file"));
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
    write_atomic(&path, edited.as_bytes()).await.map_err(|e| format!("edit: cannot write {shown}: {e}"))?;
    if tokio::fs::read(&path).await.ok().as_deref() != Some(edited.as_bytes()) {
        return Err(format!("edit: {shown} is not as intended after the write; read it before going on"));
    }
    ctx.seen.mark(&path);
    let (added, removed, diff) = unified(&text, &edited, &shown);
    let now = now_text(&text, &edited);
    let now = if now.is_empty() { String::new() } else { format!("\nfile now:\n{now}") };
    Ok(Done { shown, path, note, added, removed, diff, now, old: text, new: edited })
}

async fn run_one(args: &Value, ctx: &ToolContext) -> ToolOutput {
    let Some(p) = str_arg(args, "path") else {
        return ToolOutput::error("edit: `path` is required");
    };
    let d = match edit_file(p, args, ctx).await {
        Ok(d) => d,
        Err(e) => return ToolOutput::error(e),
    };
    // a step-level check after the whole step replaces the per-edit one
    let diag = if ctx.post_check { String::new() } else { crate::edit_check::report(&d.path).await };
    ToolOutput::ok(format!(
        "edited {}{}: +{} −{} lines, {VERIFIED}\n{}{}{diag}",
        d.shown, d.note, d.added, d.removed, d.diff, d.now
    ))
    // the whole file, before and after: the diff numbers from its first line
    .with_display(ToolDisplay::Diff { path: d.shown, old: d.old, new: d.new, start_line: 1 })
}

/// `files: [{path, edits}]`: files changed in order, the first that fails stops the rest.
async fn run_files(args: &Value, ctx: &ToolContext) -> ToolOutput {
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
    let mut done: Vec<String> = Vec::new();
    let mut text = String::new();
    for (i, (p, f)) in entries.iter().enumerate() {
        match edit_file(p, f, ctx).await {
            Ok(d) => {
                let diag = if ctx.post_check { String::new() } else { crate::edit_check::report(&d.path).await };
                text.push_str(&format!(
                    "edited {}{}: +{} −{} lines, {VERIFIED}\n{}{}{diag}\n",
                    d.shown, d.note, d.added, d.removed, d.diff, d.now
                ));
                done.push(d.shown);
            }
            Err(e) => {
                let rest: Vec<&str> = entries[i + 1..].iter().map(|(p, _)| *p).collect();
                let applied = if done.is_empty() { "none".to_string() } else { done.join(", ") };
                let skipped = if rest.is_empty() { String::new() } else { format!("; not tried: {}", rest.join(", ")) };
                return ToolOutput::error(format!("{text}{e}\napplied before the failure: {applied}{skipped}"));
            }
        }
    }
    ToolOutput::ok(format!("edited {} files: {}\n{text}", done.len(), done.join(", ")))
}

/// `text` with `changes` made in order, each on the text the ones before it left, and the note
/// for the answer (" (3 changes)", " (4 occurrences)"); the error of the first that fails.
pub fn apply(text: &str, changes: &[Change], shown: &str) -> Result<(String, String), String> {
    let mut edited = text.to_string();
    let mut last = 0;
    for (i, (old, new, all)) in changes.iter().enumerate() {
        match replace(&edited, old, new, *all, shown) {
            Ok((t, n)) => (edited, last) = (t, n),
            Err(e) if changes.len() > 1 => {
                return Err(format!(
                    "edit: change {} of {}: {}; nothing was written",
                    i + 1,
                    changes.len(),
                    e.trim_start_matches("edit: ")
                ))
            }
            Err(e) => return Err(e),
        }
    }
    let note = match changes {
        [(_, _, true)] => format!(" ({last} occurrences)"),
        [_] => String::new(),
        _ => format!(" ({} changes)", changes.len()),
    };
    Ok((edited, note))
}

/// One change: the text to replace, what replaces it, and whether every occurrence goes.
pub type Change = (String, String, bool);

/// The changes asked for: the one `old_string`/`new_string` pair, or
/// each entry of `edits`.
pub fn changes(args: &Value) -> Result<Vec<Change>, String> {
    let one = |v: &Value, at: &str| -> Result<Change, String> {
        let (Some(old), Some(new)) =
            (v.get("old_string").and_then(Value::as_str), v.get("new_string").and_then(Value::as_str))
        else {
            return Err(format!("edit: {at}`old_string` and `new_string` (strings) are required"));
        };
        if old.is_empty() {
            return Err(format!("edit: {at}`old_string` is empty; use `write` to create a file"));
        }
        if old == new {
            return Err(format!("edit: {at}`old_string` and `new_string` are the same"));
        }
        Ok((old.to_string(), new.to_string(), bool_arg(v, "replace_all")))
    };
    match args.get("edits") {
        Some(Value::Array(list)) if !list.is_empty() => {
            if args.get("old_string").is_some() {
                return Err("edit: give either `old_string`/`new_string` or `edits`, not both".into());
            }
            list.iter().enumerate().map(|(i, v)| one(v, &format!("change {}: ", i + 1))).collect()
        }
        Some(_) => Err("edit: `edits` must be a list of {old_string, new_string} objects".into()),
        None => one(args, "").map(|c| vec![c]),
    }
}

/// `text` with `old` replaced by `new` (every occurrence with `all`), and how many there were.
fn replace(text: &str, old: &str, new: &str, all: bool, shown: &str) -> Result<(String, usize), String> {
    // the file's text is matched as it is; only the strings given adapt to its line ends
    let (old, new) = match_line_ends(text, old, new);
    let (old, new) = (old.as_str(), new.as_str());
    match text.matches(old).count() {
        0 => Err(not_found(text, old, shown)),
        1 => Ok((text.replacen(old, new, 1), 1)),
        n if all => Ok((text.replace(old, new), n)),
        n => Err(format!(
            "edit: `old_string` occurs {n} times in {shown}; include more surrounding lines to make it unique, or set replace_all"
        )),
    }
}

/// Files looked at beside the edited one, and the largest of them.
const SIBLINGS: usize = 200;
const SIBLING_BYTES: u64 = 1 << 20;

/// When the first change whose `old_string` is not in `text` is exactly in one other file of the
/// same folder, a sentence saying where (`edit` changes one file per call). `None` when it is in
/// none or in several, so the hint never guesses.
async fn elsewhere(cwd: &std::path::Path, path: &std::path::Path, text: &str, changes: &[Change]) -> Option<String> {
    let old = &changes.iter().find(|(old, _, _)| !text.contains(old.as_str()))?.0;
    let mut dir = tokio::fs::read_dir(path.parent()?).await.ok()?;
    let mut found: Vec<(std::path::PathBuf, usize)> = Vec::new();
    for _ in 0..SIBLINGS {
        let Some(e) = dir.next_entry().await.ok()? else { break };
        let other = e.path();
        if other == path || !e.metadata().await.is_ok_and(|m| m.is_file() && m.len() <= SIBLING_BYTES) {
            continue;
        }
        let Ok(bytes) = tokio::fs::read(&other).await else { continue };
        let Ok(body) = String::from_utf8(bytes) else { continue };
        if let Some(at) = body.find(old.as_str()) {
            found.push((other, body[..at].matches('\n').count() + 1));
        }
    }
    match found.as_slice() {
        [(other, line)] => Some(format!(
            "; it is in {} line {line}: `edit` changes one file per call, so send that path for that change",
            display_path(cwd, other)
        )),
        _ => None,
    }
}

/// A way to cut whitespace from the ends of a line.
type Trim = fn(&str) -> &str;

/// Lines shown from the file when `old_string` matches it only up to whitespace.
const NEAR_LINES: usize = 30;

/// Where `old` sits in `text` when it differs only in the spaces and tabs around its lines: its
/// first line (1-based), the file's own text there, and which ends differ. `None` when no single
/// place matches, so a hint never points at the wrong copy.
fn near_miss<'a>(text: &'a str, old: &str) -> Option<(usize, Vec<&'a str>, &'static str)> {
    let want: Vec<&str> = old.lines().collect();
    if want.iter().all(|l| l.trim().is_empty()) {
        return None;
    }
    let have: Vec<&str> = text.lines().collect();
    let rules: [(Trim, &str); 2] =
        [(str::trim_end, "at the ends of lines"), (str::trim, "in the indentation or at the ends of lines")];
    for (norm, what) in rules {
        let mut hits = have
            .windows(want.len())
            .enumerate()
            .filter(|(_, w)| w.iter().zip(&want).all(|(a, b)| norm(a) == norm(b)))
            .map(|(i, _)| i);
        if let (Some(i), None) = (hits.next(), hits.next()) {
            return Some((i + 1, have[i..i + want.len()].to_vec(), what));
        }
    }
    None
}

/// The not-found error, with the file's own text when one place matches up to whitespace.
fn not_found(text: &str, old: &str, shown: &str) -> String {
    let Some((first, lines, what)) = near_miss(text, old) else {
        return format!("edit: `old_string` not found in {shown}; read the file and copy the text exactly");
    };
    let last = first + lines.len() - 1;
    let at = if last == first { format!("line {first} matches") } else { format!("lines {first}-{last} match") };
    let mut body: String = lines.iter().take(NEAR_LINES).map(|l| format!("{l}\n")).collect();
    if lines.len() > NEAR_LINES {
        body.push_str(&format!("… {} more lines\n", lines.len() - NEAR_LINES));
    }
    format!(
        "edit: `old_string` not found in {shown} as given; {at} it except for spaces or tabs \
         {what}. The file has there:\n{body}Copy this text exactly (whitespace included) into `old_string`."
    )
}

/// `old` and `new` as they fit `text`'s line ends: as given when `old` is found so, else with
/// CRLF ends (a CRLF file edited with `\n` strings), else with plain ends. `new` follows the
/// ends of the `old` that matched, and a file that ends every line with CRLF gives them to it.
pub fn match_line_ends(text: &str, old: &str, new: &str) -> (String, String) {
    let lf = |s: &str| s.replace("\r\n", "\n");
    let crlf = |s: &str| lf(s).replace('\n', "\r\n");
    let file_crlf = text.contains("\r\n");
    if !text.contains(old) {
        if file_crlf && old.contains('\n') && text.contains(&crlf(old)) {
            return (crlf(old), crlf(new));
        }
        if old.contains("\r\n") && text.contains(&lf(old)) {
            return (lf(old), lf(new));
        }
    }
    let all_crlf = file_crlf && text.matches('\n').count() == text.matches("\r\n").count();
    if all_crlf && !old.contains('\n') {
        (old.to_string(), crlf(new))
    } else {
        (old.to_string(), new.to_string())
    }
}

/// Lines added and removed, and a unified diff cut to `DIFF_LINES` lines.
pub fn unified(old: &str, new: &str, name: &str) -> (usize, usize, String) {
    let diff = TextDiff::from_lines(old, new);
    let (mut added, mut removed) = (0, 0);
    for c in diff.iter_all_changes() {
        match c.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }
    let text = diff.unified_diff().context_radius(3).header(&format!("a/{name}"), &format!("b/{name}")).to_string();
    let lines: Vec<&str> = text.lines().collect();
    let shown = if lines.len() > DIFF_LINES {
        format!("{}\n… {} more diff lines", lines[..DIFF_LINES].join("\n"), lines.len() - DIFF_LINES)
    } else {
        text
    };
    (added, removed, shown)
}

/// Lines of context shown on each side of a change.
const CONTEXT: usize = 3;
/// Numbered lines shown at most for the changed regions of one result.
const SNIPPET_LINES: usize = 40;
/// Characters kept of a line in the snippet.
const SNIPPET_LINE_CHARS: usize = 200;

/// The whole of `text`, numbered as `read` numbers it, when it is short enough to be shown whole: at most
/// [`WHOLE_LINES`] lines, none cut. None otherwise. The agent uses it to know a file was shown whole.
pub fn whole(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() || lines.len() > WHOLE_LINES || lines.iter().any(|l| l.chars().count() > SNIPPET_LINE_CHARS) {
        return None;
    }
    Some(lines.iter().enumerate().map(|(i, l)| format!("{:>6}│ {l}\n", i + 1)).collect())
}

/// What a changed file shows of itself: all of it when short, else the changed regions with context.
/// Empty when nothing changed.
pub fn now_text(old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }
    whole(new).unwrap_or_else(|| around(old, new))
}

/// The file as it is now around what changed, numbered as `read` numbers it: each changed
/// region with `CONTEXT` lines either side, gaps and a cut at `SNIPPET_LINES` lines marked `…`.
/// Empty when nothing changed.
pub fn around(old: &str, new: &str) -> String {
    let lines: Vec<&str> = new.lines().collect();
    let diff = TextDiff::from_lines(old, new);
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for op in diff.ops().iter().filter(|o| o.tag() != similar::DiffTag::Equal) {
        let r = op.new_range();
        let (a, b) = (r.start.saturating_sub(CONTEXT), (r.end + CONTEXT).min(lines.len()));
        match spans.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => spans.push((a, b)),
        }
    }
    let (mut out, mut shown) = (String::new(), 0);
    for (k, (a, b)) in spans.into_iter().enumerate() {
        if shown >= SNIPPET_LINES {
            out.push_str("…\n");
            break;
        }
        if k > 0 {
            out.push_str("…\n");
        }
        let cut = b.min(a + (SNIPPET_LINES - shown));
        for (i, l) in lines.iter().enumerate().take(cut).skip(a) {
            out.push_str(&format!("{:>6}│ {}\n", i + 1, cut_line(l, SNIPPET_LINE_CHARS)));
        }
        shown += cut - a;
        if cut < b {
            out.push_str("…\n");
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_miss_only_in_whitespace_shows_the_files_own_text_and_where() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let file = "fn a() {\n\tlet x = 1;  \n\tx\n}\nfn b() {\n    let y = 2;\n}\n";
        std::fs::write(d.path().join("a.rs"), file).unwrap();
        // trailing spaces in the file
        let out = Edit.run(json!({"path": "a.rs", "old_string": "\tlet x = 1;\n\tx", "new_string": "z"}), &c).await;
        assert!(out.is_error);
        assert!(
            out.content.contains("lines 2-3 match it except for spaces or tabs at the ends of lines"),
            "{}",
            out.content
        );
        assert!(out.content.contains("The file has there:\n\tlet x = 1;  \n\tx\nCopy"), "{}", out.content);
        // tabs given where the file has spaces
        let out = Edit.run(json!({"path": "a.rs", "old_string": "\tlet y = 2;", "new_string": "z"}), &c).await;
        assert!(out.content.contains("as given; line 6 matches it"), "{}", out.content);
        assert!(out.content.contains("in the indentation"), "{}", out.content);
        // two places that match up to whitespace: no hint, so it never points at the wrong one
        std::fs::create_dir(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("sub/b.rs"), "  x\n\tx\n").unwrap();
        let out = Edit.run(json!({"path": "sub/b.rs", "old_string": "x ", "new_string": "z"}), &c).await;
        assert!(out.content.ends_with("read the file and copy the text exactly"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("a.rs")).unwrap(), file, "nothing written");
    }

    #[tokio::test]
    async fn replaces_exactly_one_occurrence_and_reports_a_diff() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::write(d.path().join("a.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
        let out = Edit.run(json!({"path": "a.rs", "old_string": "fn b() {}", "new_string": "fn b() { 1 }"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.starts_with("edited a.rs: +1 −1 lines, applied and verified on disk\n"), "{}", out.content);
        assert!(out.content.contains("-fn b() {}\n+fn b() { 1 }"));
        assert_eq!(std::fs::read_to_string(d.path().join("a.rs")).unwrap(), "fn a() {}\nfn b() { 1 }\nfn c() {}\n");
        assert!(matches!(out.display, ToolDisplay::Diff { start_line: 1, .. }));
    }

    #[tokio::test]
    async fn refuses_ambiguous_or_missing_text_unless_replace_all() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::write(d.path().join("a.txt"), "x\nx\n").unwrap();
        let out = Edit.run(json!({"path": "a.txt", "old_string": "x", "new_string": "y"}), &c).await;
        assert!(out.is_error && out.content.contains("occurs 2 times"), "{}", out.content);
        let out = Edit.run(json!({"path": "a.txt", "old_string": "z", "new_string": "y"}), &c).await;
        assert!(out.is_error && out.content.contains("not found"));
        let out =
            Edit.run(json!({"path": "a.txt", "old_string": "x", "new_string": "y", "replace_all": true}), &c).await;
        assert!(!out.is_error && out.content.contains("(2 occurrences)"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "y\ny\n");
        assert!(Edit
            .run(json!({"path": "nope", "old_string": "a", "new_string": "b"}), &c)
            .await
            .content
            .contains("does not exist"));
        assert!(Edit.run(json!({"path": "a.txt", "old_string": "a", "new_string": "a"}), &c).await.is_error);
        assert!(Edit.run(json!({"path": "a.txt", "old_string": "", "new_string": "a"}), &c).await.is_error);
        assert_eq!(Edit.summary(&json!({"path": "a.txt", "replace_all": true})), "edit a.txt (all occurrences)");
    }

    #[tokio::test]
    async fn edits_a_crlf_file_and_keeps_its_line_ends() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::write(d.path().join("w.txt"), "one\r\ntwo\r\nthree\r\n").unwrap();
        let out = Edit.run(json!({"path": "w.txt", "old_string": "two\nthree", "new_string": "2\n3"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("w.txt")).unwrap(), "one\r\n2\r\n3\r\n");
    }

    #[tokio::test]
    async fn refuses_a_file_that_is_not_utf8_and_leaves_it_alone() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        // Latin-1: "café = 1\n"
        let latin1 = b"caf\xe9 = 1\nx = 2\n".to_vec();
        std::fs::write(d.path().join("l.txt"), &latin1).unwrap();
        let out = Edit.run(json!({"path": "l.txt", "old_string": "x = 2", "new_string": "x = 3"}), &c).await;
        assert!(out.is_error && out.content.contains("not UTF-8"), "{}", out.content);
        assert_eq!(std::fs::read(d.path().join("l.txt")).unwrap(), latin1);
    }

    #[tokio::test]
    async fn a_mixed_line_end_file_keeps_every_line_end_it_had() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::write(d.path().join("m.txt"), "a\nb\r\nc\n").unwrap();
        let out = Edit.run(json!({"path": "m.txt", "old_string": "a", "new_string": "x"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("m.txt")).unwrap(), "x\nb\r\nc\n");
        // a CRLF string against the plain part of the file
        let out = Edit.run(json!({"path": "m.txt", "old_string": "x\r\nb", "new_string": "y\r\nb"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("m.txt")).unwrap(), "y\nb\r\nc\n");
        // a file all CRLF: new lines in `new_string` take its ends
        std::fs::write(d.path().join("w.txt"), "one\r\ntwo\r\n").unwrap();
        let out = Edit.run(json!({"path": "w.txt", "old_string": "two", "new_string": "2\n2b"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("w.txt")).unwrap(), "one\r\n2\r\n2b\r\n");
    }

    #[tokio::test]
    async fn a_file_over_the_limit_is_refused_before_it_is_read() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        // sparse and all NUL: read whole it would be called binary, not too large
        let f = std::fs::File::create(d.path().join("big.txt")).unwrap();
        f.set_len(MAX_FILE_BYTES + 1).unwrap();
        let out = Edit.run(json!({"path": "big.txt", "old_string": "a", "new_string": "b"}), &c).await;
        assert!(out.is_error && out.content.contains("too large"), "{}", out.content);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_edit_replaces_the_file_whole_keeping_its_mode_and_links() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let f = d.path().join("run.sh");
        std::fs::write(&f, "echo a\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o750)).unwrap();
        std::os::unix::fs::symlink(&f, d.path().join("link.sh")).unwrap();
        let out = Edit.run(json!({"path": "link.sh", "old_string": "a", "new_string": "b"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "echo b\n");
        assert!(std::fs::symlink_metadata(d.path().join("link.sh")).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o750);
        let names: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "no temporary file left: {names:?}");
        // a read-only file stays read-only and unchanged
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o444)).unwrap();
        let out = Edit.run(json!({"path": "run.sh", "old_string": "b", "new_string": "c"}), &c).await;
        assert!(out.is_error && out.content.contains("read-only"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "echo b\n");
    }
}

#[cfg(test)]
mod many_tests {
    use super::*;

    #[tokio::test]
    async fn several_changes_go_in_one_call_and_one_that_fails_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let f = d.path().join("a.rs");
        std::fs::write(&f, "fn a() {}\nfn b() {}\nlet x = 1; let x2 = x;\n").unwrap();
        let edits = json!([
            {"old_string": "fn a() {}", "new_string": "fn a() { 1 }"},
            // the second sees the text the first left
            {"old_string": "fn a() { 1 }\nfn b", "new_string": "fn a() { 1 }\nfn bee"},
            {"old_string": "x", "new_string": "y", "replace_all": true}
        ]);
        let out = Edit.run(json!({"path": "a.rs", "edits": edits}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.starts_with("edited a.rs (3 changes): +3 −3 lines"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "fn a() { 1 }\nfn bee() {}\nlet y = 1; let y2 = y;\n");
        assert_eq!(Edit.summary(&json!({"path": "a.rs", "edits": edits})), "edit a.rs (3 changes)");

        let before = std::fs::read_to_string(&f).unwrap();
        let bad = json!([{"old_string": "fn bee", "new_string": "fn b"}, {"old_string": "missing", "new_string": "z"}]);
        let out = Edit.run(json!({"path": "a.rs", "edits": bad}), &c).await;
        assert!(out.is_error);
        assert!(out.content.starts_with("edit: change 2 of 2: `old_string` not found in a.rs"), "{}", out.content);
        assert!(out.content.ends_with("nothing was written"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), before);

        for (args, want) in [
            (
                json!({"path": "a.rs", "edits": [{"old_string": "", "new_string": "z"}]}),
                "edit: change 1: `old_string` is empty",
            ),
            (json!({"path": "a.rs", "edits": "x"}), "edit: `edits` must be a list"),
            (
                json!({"path": "a.rs", "old_string": "a", "new_string": "b", "edits": [{"old_string": "a", "new_string": "b"}]}),
                "edit: give either",
            ),
            (json!({"path": "a.rs"}), "edit: `old_string` and `new_string` (strings) are required"),
        ] {
            let out = Edit.run(args, &c).await;
            assert!(out.is_error && out.content.starts_with(want), "{want}: {}", out.content);
        }
    }
}

#[cfg(test)]
mod around_tests {
    use super::*;

    #[tokio::test]
    async fn short_files_come_back_whole_after_an_edit() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let short: String = (1..=60).map(|i| format!("line{i}\n")).collect();
        std::fs::write(d.path().join("s.txt"), &short).unwrap();
        let out = Edit.run(json!({"path": "s.txt", "old_string": "line30\n", "new_string": "thirty\n"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.lines().next().unwrap().contains("applied and verified on disk"), "{}", out.content);
        for i in [1, 29, 30, 31, 60] {
            let want = if i == 30 { "thirty".to_string() } else { format!("line{i}") };
            assert!(out.content.contains(&format!("{i:>6}│ {want}\n")), "line {i}: {}", out.content);
        }
        assert!(!out.content.contains("…"), "{}", out.content);
        let long: String = (1..=200).map(|i| format!("line{i}\n")).collect();
        std::fs::write(d.path().join("l.txt"), &long).unwrap();
        let out = Edit.run(json!({"path": "l.txt", "old_string": "line100\n", "new_string": "hundred\n"}), &c).await;
        assert!(out.content.contains("   100│ hundred\n"), "{}", out.content);
        assert!(!out.content.contains("│ line1\n") && !out.content.contains("│ line200\n"), "{}", out.content);
    }

    #[tokio::test]
    async fn the_result_shows_the_numbered_lines_around_the_change() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let file: String = (1..=200).map(|i| format!("line{i}\n")).collect();
        std::fs::write(d.path().join("a.txt"), &file).unwrap();
        let out = Edit.run(json!({"path": "a.txt", "old_string": "line10\n", "new_string": "ten\nten b\n"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("\nfile now:\n     7│ line7\n"), "{}", out.content);
        assert!(out.content.contains("    10│ ten\n    11│ ten b\n    12│ line11\n"), "{}", out.content);
        assert!(out.content.ends_with("    14│ line13\n"), "{}", out.content);
        assert!(!out.content.contains("line6\n    7"), "{}", out.content);
    }

    #[test]
    fn the_snippet_is_capped_and_gaps_are_marked() {
        let old: String = (1..=200).map(|i| format!("l{i}\n")).collect();
        // a change at the top and one far below: two regions with a gap
        let two = old.replace("l2\n", "X\n").replace("l150\n", "Y\n");
        let s = around(&old, &two);
        assert!(s.contains("…\n") && s.contains("   150│ Y\n") && s.contains("     2│ X\n"), "{s}");
        // every line changed: cut at the cap
        let all: String = (1..=200).map(|i| format!("n{i}\n")).collect();
        let s = around(&old, &all);
        assert_eq!(s.lines().filter(|l| l.contains('│')).count(), SNIPPET_LINES, "{s}");
        assert!(s.ends_with("…\n"), "{s}");
        assert_eq!(around("a\n", "a\n"), "");
    }

    #[tokio::test]
    async fn edit_files_changes_several_files_and_stops_at_the_first_failure() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        for (n, t) in [("a.txt", "one\n"), ("b.txt", "two\n"), ("c.txt", "three\n")] {
            std::fs::write(d.path().join(n), t).unwrap();
        }
        let out = Edit
            .run(
                json!({"files": [
                    {"path": "a.txt", "edits": [{"old_string": "one", "new_string": "1"}]},
                    {"path": "b.txt", "old_string": "two", "new_string": "2"}
                ]}),
                &c,
            )
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.starts_with("edited 2 files: a.txt, b.txt"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("b.txt")).unwrap(), "2\n");
        // the second file fails: the first stays applied, the third is not tried, and the answer says so
        let out = Edit
            .run(
                json!({"files": [
                    {"path": "a.txt", "old_string": "1", "new_string": "uno"},
                    {"path": "b.txt", "old_string": "absent", "new_string": "x"},
                    {"path": "c.txt", "old_string": "three", "new_string": "3"}
                ]}),
                &c,
            )
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("applied before the failure: a.txt; not tried: c.txt"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "uno\n");
        assert_eq!(std::fs::read_to_string(d.path().join("c.txt")).unwrap(), "three\n");
        // a malformed entry is found before anything is written
        let out = Edit.run(json!({"files": [{"path": "a.txt", "old_string": "uno", "new_string": "x"}, {"old_string": "a", "new_string": "b"}]}), &c).await;
        assert!(out.is_error && out.content.contains("files[2]"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "uno\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edit_check_runs_the_command_and_reports_exit_and_output() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
        let out = Edit
            .run(json!({"path": "a.txt", "old_string": "one", "new_string": "two", "check": "cat a.txt; exit 3"}), &c)
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.contains("\ncheck: cat a.txt; exit 3 | exit 3 | 0.") && out.content.ends_with("\ntwo"),
            "{}",
            out.content
        );
        // a failed change runs no check
        let out =
            Edit.run(json!({"path": "a.txt", "old_string": "zzz", "new_string": "y", "check": "echo ran"}), &c).await;
        assert!(out.is_error && !out.content.contains("check:"), "{}", out.content);
    }
}

#[cfg(test)]
mod elsewhere_tests {
    use super::*;

    /// The H1 logs: two changes sent for `src/report.rs`, one of them for text in `src/pricing.rs`.
    #[tokio::test]
    async fn a_text_that_lives_in_another_file_names_that_file() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::create_dir(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/pricing.rs"), "use x;\n\npub fn margin(cost: f64) -> f64 {\n    cost\n}\n")
            .unwrap();
        std::fs::write(d.path().join("src/report.rs"), "pub fn format_row() {}\n").unwrap();
        let edits = json!([
            {"old_string": "pub fn format_row() {}", "new_string": "/// Row.\npub fn format_row() {}"},
            {"old_string": "pub fn margin(cost: f64) -> f64 {", "new_string": "/// Margin.\npub fn margin(cost: f64) -> f64 {"}
        ]);
        let out = Edit.run(json!({"path": "src/report.rs", "edits": edits}), &c).await;
        assert!(out.is_error && out.content.contains("change 2 of 2"), "{}", out.content);
        assert!(
            out.content.contains("it is in src/pricing.rs line 3") && out.content.contains("one file"),
            "{}",
            out.content
        );
        assert_eq!(std::fs::read_to_string(d.path().join("src/report.rs")).unwrap(), "pub fn format_row() {}\n");
        // in two files: no guess
        std::fs::write(d.path().join("src/other.rs"), "pub fn margin(cost: f64) -> f64 {\n").unwrap();
        let out = Edit.run(json!({"path": "src/report.rs", "edits": edits}), &c).await;
        assert!(out.is_error && !out.content.contains("it is in"), "{}", out.content);
    }
}
