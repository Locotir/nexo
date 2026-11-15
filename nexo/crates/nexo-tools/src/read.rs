//! `read`: a file, as numbered lines; a picture, as an image the model looks at.
use async_trait::async_trait;
use nexo_core::tool::{call_paths, paths_label};
use nexo_core::{Compact, Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

use crate::util::int_arg;

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

use many::read_many;
use one::read_one;

pub struct Read;

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str {
        "read"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read".into(),
            description: "Read numbered text lines (`   12│ text`): `offset` is 1-based, `limit` defaults to 2000. \
                          Lines are cut at 2000 characters, calls at 64000. `paths` reads files under `=== path ===` headers; failures appear in place. \
                          PNG, JPEG, GIF, WebP, PPM, PGM, BMP show as images (max 5 MB, 2000 px); Jupyter notebooks show cells and outputs. \
                          Directories and other binaries are refused (use `ls` for directories). \
                          `symbol` reads a Rust item; ambiguity reports ranges. Cannot combine with `offset` or `limit`."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "paths": {"type": "array", "items": {"type": "string"}, "description": "Max 20, about 100000 characters total; offset and limit apply to each."},
                    "symbol": {"type": "string", "description": "Exact Rust item or method name; one file."},
                    "offset": {"type": "integer", "minimum": 1},
                    "limit": {"type": "integer", "minimum": 1}
                }
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: "Read text files as numbered lines; shows images. Not for directories: use `ls`.",
            params: &[
                ("paths", "Several files, at most 20"),
                ("symbol", "Exact Rust item or method name, one file"),
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
        if let Some(symbol) = args.get("symbol").and_then(Value::as_str) {
            return format!("read {} symbol {symbol}", paths_label(&files));
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
        if let Some(symbol) = args.get("symbol") {
            if !symbol.as_str().is_some_and(|s| !s.trim().is_empty()) {
                return ToolOutput::error("read: `symbol` must be a non-empty string");
            }
            if files.len() != 1 || args.get("offset").is_some() || args.get("limit").is_some() {
                return ToolOutput::error(
                    "read: `symbol` requires one file and cannot be combined with `offset` or `limit`",
                );
            }
        }
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

mod ipynb;
mod many;
mod one;
mod show;
mod symbol;
#[cfg(test)]
mod tests;
