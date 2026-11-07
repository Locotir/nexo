//! The file tools, reading and writing through the editor.
//!
//! When the client says it can `fs/read_text_file` and `fs/write_text_file`, the model's
//! `read`, `write` and `edit` go through it, so an unsaved buffer is what the model sees and
//! an edit lands in the buffer. Each wrapper keeps the spec, kind and summary of the built-in
//! tool it stands in for; only where the text comes from and goes to changes. A direction the
//! client did not offer uses the disk.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use nexo_core::{Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSet, ToolSpec};
use nexo_tools::util::{bool_arg, cut_line, display_path, int_arg, outside_note, resolve, str_arg};
use serde_json::{json, Value};

use super::rpc::Rpc;

/// What the client can do with files.
#[derive(Clone, Copy, Debug, Default)]
pub struct FsCaps {
    pub read: bool,
    pub write: bool,
}

/// The connection to the editor's files for one session.
pub struct ClientFs {
    rpc: Arc<Rpc>,
    /// The session id, known once the agent has started.
    sid: Arc<OnceLock<String>>,
    caps: FsCaps,
}

impl ClientFs {
    async fn read(&self, path: &Path) -> Result<String, String> {
        if self.caps.read {
            let sid = self.sid.get().cloned().unwrap_or_default();
            let r = self
                .rpc
                .request("fs/read_text_file", json!({"sessionId": sid, "path": path.display().to_string()}))
                .await
                .map_err(|e| e.message)?;
            return Ok(r.get("content").and_then(Value::as_str).unwrap_or_default().to_string());
        }
        let bytes = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
        if nexo_tools::util::is_binary(&bytes) {
            return Err("a binary file".into());
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn write(&self, path: &Path, content: &str) -> Result<(), String> {
        if self.caps.write {
            let sid = self.sid.get().cloned().unwrap_or_default();
            self.rpc
                .request(
                    "fs/write_text_file",
                    json!({"sessionId": sid, "path": path.display().to_string(), "content": content}),
                )
                .await
                .map(|_| ())
                .map_err(|e| e.message)
        } else {
            nexo_tools::util::write_atomic(path, content.as_bytes()).await.map_err(|e| e.to_string())
        }
    }
}

/// Puts the editor-backed `read`, `write` and `edit` in `tools` in place of the built-in ones.
/// Returns the cell to fill with the session id once it is known.
pub fn install(tools: &mut ToolSet, rpc: Arc<Rpc>, caps: FsCaps) -> Arc<OnceLock<String>> {
    let sid = Arc::new(OnceLock::new());
    if !caps.read && !caps.write {
        return sid;
    }
    let fs = Arc::new(ClientFs { rpc, sid: sid.clone(), caps });
    tools.add(Arc::new(ReadTool(fs.clone())));
    tools.add(Arc::new(WriteTool(fs.clone())));
    tools.add(Arc::new(EditTool(fs)));
    sid
}

/// Runs `f` unless the turn is interrupted first.
async fn cancellable(ctx: &ToolContext, f: impl std::future::Future<Output = ToolOutput>) -> ToolOutput {
    tokio::select! {
        o = f => o,
        _ = ctx.cancel.cancelled() => ToolOutput::error("interrupted"),
    }
}

struct ReadTool(Arc<ClientFs>);

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }
    fn spec(&self) -> ToolSpec {
        nexo_tools::read::Read.spec()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn summary(&self, args: &Value) -> String {
        nexo_tools::read::Read.summary(args)
    }
    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        cancellable(ctx, async {
            let Some(p) = str_arg(&args, "path") else {
                return ToolOutput::error("read: `path` is required");
            };
            let path = resolve(&ctx.cwd, p);
            let shown = format!("{}{}", display_path(&ctx.cwd, &path), outside_note(&ctx.cwd, &path));
            let text = match self.0.read(&path).await {
                Ok(t) => t,
                Err(e) => return ToolOutput::error(format!("read: cannot read {shown}: {e}")),
            };
            if text.is_empty() {
                return ToolOutput::ok(format!("{shown}: (empty file)")).with_display(ToolDisplay::File {
                    path: shown,
                    text: String::new(),
                    start_line: 1,
                });
            }
            let offset = int_arg(&args, "offset").unwrap_or(1).max(1) as usize;
            let limit = int_arg(&args, "limit").unwrap_or(nexo_tools::read::DEFAULT_LIMIT).max(1) as usize;
            let total = text.lines().count();
            if offset > total {
                return ToolOutput::error(format!("read: {shown} has {total} lines; offset {offset} is past the end"));
            }
            let (mut out, mut raw, mut n) = (String::new(), String::new(), 0usize);
            for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
                let l = cut_line(line, nexo_tools::read::LINE_CHARS);
                out.push_str(&format!("{:>6}│ {}\n", i + 1, l));
                raw.push_str(&l);
                raw.push('\n');
                n = i + 1;
            }
            if n < total {
                out.push_str(&format!(
                    "… {} more lines (of {total}); read with offset {} to continue\n",
                    total - n,
                    n + 1
                ));
            }
            ToolOutput::ok(out).with_display(ToolDisplay::File { path: shown, text: raw, start_line: offset })
        })
        .await
    }
}

struct WriteTool(Arc<ClientFs>);

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }
    fn spec(&self) -> ToolSpec {
        nexo_tools::write::Write.spec()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Write
    }
    fn summary(&self, args: &Value) -> String {
        nexo_tools::write::Write.summary(args)
    }
    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        cancellable(ctx, async {
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
            // a file that cannot be read is a new one
            let old = self.0.read(&path).await.ok();
            if let Some(parent) = path.parent() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    return ToolOutput::error(format!("write: cannot create {}: {e}", parent.display()));
                }
            }
            if let Err(e) = self.0.write(&path, content).await {
                return ToolOutput::error(format!("write: cannot write {shown}: {e}"));
            }
            let what = if old.is_some() { "replaced" } else { "created" };
            let lines = content.lines().count();
            ToolOutput::ok(format!("{what} {shown} ({}, {} bytes)", nexo_core::count(lines, "line"), content.len()))
                .with_display(ToolDisplay::Diff {
                    path: shown,
                    old: old.unwrap_or_default(),
                    new: content.to_string(),
                    start_line: 1,
                })
        })
        .await
    }
}

struct EditTool(Arc<ClientFs>);

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }
    fn spec(&self) -> ToolSpec {
        nexo_tools::edit::Edit.spec()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Write
    }
    fn summary(&self, args: &Value) -> String {
        nexo_tools::edit::Edit.summary(args)
    }
    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        cancellable(ctx, async {
            let Some(p) = str_arg(&args, "path") else {
                return ToolOutput::error("edit: `path` is required");
            };
            let (Some(old), Some(new)) =
                (args.get("old_string").and_then(Value::as_str), args.get("new_string").and_then(Value::as_str))
            else {
                return ToolOutput::error("edit: `old_string` and `new_string` (strings) are required");
            };
            if old.is_empty() {
                return ToolOutput::error("edit: `old_string` is empty; use `write` to create a file");
            }
            if old == new {
                return ToolOutput::error("edit: `old_string` and `new_string` are the same");
            }
            let path = resolve(&ctx.cwd, p);
            let shown = format!("{}{}", display_path(&ctx.cwd, &path), outside_note(&ctx.cwd, &path));
            let text = match self.0.read(&path).await {
                Ok(t) => t,
                Err(e) => return ToolOutput::error(format!("edit: cannot read {shown}: {e}")),
            };
            let (old, new) = nexo_tools::edit::match_line_ends(&text, old, new);
            let (old, new) = (old.as_str(), new.as_str());
            let count = text.matches(old).count();
            let replace_all = bool_arg(&args, "replace_all");
            let edited = match count {
                0 => {
                    return ToolOutput::error(format!(
                        "edit: `old_string` not found in {shown}; read the file and copy the text exactly"
                    ))
                }
                1 => text.replacen(old, new, 1),
                _ if replace_all => text.replace(old, new),
                n => {
                    return ToolOutput::error(format!(
                        "edit: `old_string` occurs {n} times in {shown}; include more surrounding lines to make it unique, or set replace_all"
                    ))
                }
            };
            if let Err(e) = self.0.write(&path, &edited).await {
                return ToolOutput::error(format!("edit: cannot write {shown}: {e}"));
            }
            let (added, removed, diff) = nexo_tools::edit::unified(&text, &edited, &shown);
            let n = if replace_all { format!(" ({count} occurrences)") } else { String::new() };
            ToolOutput::ok(format!("edited {shown}{n}: +{added} −{removed} lines\n{diff}"))
                .with_display(ToolDisplay::Diff { path: shown, old: text, new: edited, start_line: 1 })
        })
        .await
    }
}
