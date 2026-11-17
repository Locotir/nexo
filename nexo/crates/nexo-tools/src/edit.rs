//! `edit`: replace one exact occurrence of a text in a file.

use async_trait::async_trait;
use nexo_core::{Compact, Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

use crate::util::{bool_arg, str_arg};

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
        // One change as `changes()` reads it. Spelled out because a bare object or an array without
        // `items` tells the model nothing about the keys and strict validators refuse it.
        let pair = json!({
            "old_string": {"type": "string"},
            "new_string": {"type": "string", "description": "Empty to delete."},
            "replace_all": {"type": "boolean"}
        });
        let change = json!({"type": "object", "properties": pair, "required": ["old_string", "new_string"]});
        // A file entry takes `edits`, or a single pair like the top level; only `path` is required.
        let mut file_props = pair.as_object().cloned().unwrap_or_default();
        file_props.insert("path".into(), json!({"type": "string"}));
        file_props.insert("edits".into(), json!({"type": "array", "items": change}));
        let file = json!({"type": "object", "properties": file_props, "required": ["path"]});
        ToolSpec {
            name: "edit".into(),
            description: "Replace `old_string` in a file with `new_string`. It must match \
                          exactly (indentation included) and occur once; add surrounding lines to make it \
                          unique, or set `replace_all` to change every occurrence (renames). Several changes to one file go in one call as \
                          `edits`, applied in order; if one fails nothing is written. Several files: `files`. `check` runs a test or build after."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_string": {"type": "string"},
                    "new_string": {"type": "string", "description": "Empty to delete."},
                    "replace_all": {"type": "boolean"},
                    "edits": {"type": "array", "items": change},
                    "files": {"type": "array", "description": "Instead of `path`: [{path, edits}]; stops at the first failure.", "items": file},
                    "check": {"type": "string", "description": "Short command to run after; exit and tail come back."}
                }
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: "Replace `old_string` with `new_string` in a file; it must match exactly and occur once, or set `replace_all`. `edits` or `files` for several.",
            params: &[
                ("edits", "List of changes"),
                ("files", "Instead of `path`: [{path, edits}]"),
                ("check", "Command to run after"),
            ],
        })
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

mod apply;
mod miss;
mod run;
mod show;
#[cfg(test)]
mod tests;

pub use apply::{apply, changes, match_line_ends, Change};
use run::{run_files, run_one};
pub use show::{around, now_text, unified, whole};
