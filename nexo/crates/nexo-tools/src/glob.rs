//! `glob`: files by name pattern, newest first.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use globset::GlobBuilder;
use ignore::WalkBuilder;
use nexo_core::{Compact, Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

use crate::program::{listed_kind, may_be_program, KINDS_UP_TO};
use crate::util::{display_path, outside_note, resolve, str_arg};

pub const MAX_RESULTS: usize = 500;
/// Matches collected at most before the newest are picked (bounded memory on a wide pattern).
pub const MAX_CANDIDATES: usize = 50_000;
/// A walk stops after this long and says so.
pub const TIME_CAP: Duration = Duration::from_secs(20);

pub struct Glob;

#[async_trait]
impl Tool for Glob {
    fn name(&self) -> &str {
        "glob"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "glob".into(),
            description: "Find files by name pattern under a directory (gitignored files are skipped), \
                          newest first. A pattern without a `/` matches file names anywhere in the tree \
                          (`*.rs`); one with a `/` or `**` matches the path relative to the directory \
                          (`src/**/*.rs`). At most 500 results."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string", "description": "Default: the working directory."}
                },
                "required": ["pattern"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: "Find files by name under `path` (default: working directory), newest first. `*.rs` matches names anywhere; with `/` or `**`, the path.",
            params: &[],
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Search
    }

    fn summary(&self, args: &Value) -> String {
        match (str_arg(args, "pattern"), str_arg(args, "path")) {
            (Some(p), Some(d)) => format!("glob {p} in {d}"),
            (Some(p), None) => format!("glob {p}"),
            _ => "glob (no pattern)".into(),
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(pattern) = str_arg(&args, "pattern") else {
            return ToolOutput::error("glob: `pattern` is required");
        };
        let root = resolve(&ctx.cwd, str_arg(&args, "path").unwrap_or("."));
        let shown_root = format!("{}{}", display_path(&ctx.cwd, &root), outside_note(&ctx.cwd, &root));
        if !root.is_dir() {
            return ToolOutput::error(format!("glob: {shown_root} is not a directory"));
        }
        let by_name = !pattern.contains('/') && !pattern.contains("**");
        let glob = match GlobBuilder::new(pattern).literal_separator(!by_name).build() {
            Ok(g) => g.compile_matcher(),
            Err(e) => return ToolOutput::error(format!("glob: bad pattern `{pattern}`: {e}")),
        };
        let cwd = ctx.cwd.clone();
        let root2 = root.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let deadline = Instant::now() + TIME_CAP;
            let mut cut = false;
            let mut found: Vec<(SystemTime, PathBuf, bool)> = Vec::new();
            for entry in walker(&root2).build().flatten() {
                if found.len() >= MAX_CANDIDATES || Instant::now() > deadline {
                    cut = true;
                    break;
                }
                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let path = entry.path();
                let rel = path.strip_prefix(&root2).unwrap_or(path);
                let hit = if by_name {
                    path.file_name().map(|n| glob.is_match(Path::new(n))).unwrap_or(false)
                } else {
                    glob.is_match(rel)
                };
                if hit {
                    let meta = entry.metadata().ok();
                    let mtime = meta.as_ref().and_then(|m| m.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH);
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    let program = meta.is_some_and(|m| may_be_program(&name, &m));
                    found.push((mtime, path.to_path_buf(), program));
                }
            }
            found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let total = found.len();
            found.truncate(MAX_RESULTS);
            let kinds = found.len() <= KINDS_UP_TO;
            let found = found.into_iter().map(|(_, p, program)| {
                let kind = (kinds && program).then(|| listed_kind(&p)).flatten();
                (display_path(&cwd, &p), kind)
            });
            (found.collect::<Vec<_>>(), total, cut)
        })
        .await;
        let Ok((found, total, cut)) = joined else {
            return ToolOutput::error("glob: the search failed unexpectedly");
        };
        let lines: Vec<String> = found
            .iter()
            .map(|(p, kind)| match kind {
                Some(kind) => format!("{p}  ({kind})"),
                None => p.clone(),
            })
            .collect();
        let items: Vec<String> = found.into_iter().map(|(p, _)| p).collect();
        let mut content = if total == 0 {
            format!("glob: no files match `{pattern}` in {shown_root}")
        } else if total > MAX_RESULTS {
            format!("{}\n… {} more files ({total} match); narrow the pattern", lines.join("\n"), total - MAX_RESULTS)
        } else {
            lines.join("\n")
        };
        if cut {
            content
                .push_str("\n[the walk was stopped early: too many files or too long; narrow the path or the pattern]");
        }
        ToolOutput::ok(content).with_display(ToolDisplay::List { items })
    }
}

/// The walker the search tools share: gitignore-aware, hidden files included, `.git` skipped.
pub fn walker(root: &Path) -> WalkBuilder {
    let mut w = WalkBuilder::new(root);
    w.hidden(false).git_ignore(true).git_global(true).git_exclude(true).follow_links(false);
    w.filter_entry(|e| e.file_name() != ".git");
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn matches_names_anywhere_and_paths_when_asked() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::create_dir_all(d.path().join("src/deep")).unwrap();
        std::fs::create_dir_all(d.path().join("target")).unwrap();
        std::fs::write(d.path().join("src/a.rs"), "").unwrap();
        std::fs::write(d.path().join("src/deep/b.rs"), "").unwrap();
        std::fs::write(d.path().join("c.txt"), "").unwrap();
        std::fs::write(d.path().join("target/ignored.rs"), "").unwrap();
        std::fs::write(d.path().join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let out = Glob.run(json!({"pattern": "*.rs"}), &c).await;
        let mut items: Vec<&str> = out.content.lines().collect();
        items.sort();
        assert_eq!(items, vec!["src/a.rs", "src/deep/b.rs"]);
        let out = Glob.run(json!({"pattern": "src/*.rs"}), &c).await;
        assert_eq!(out.content, "src/a.rs");
        let out = Glob.run(json!({"pattern": "**/*.rs", "path": "src"}), &c).await;
        assert_eq!(out.content.lines().count(), 2);
        let out = Glob.run(json!({"pattern": "*.nope"}), &c).await;
        assert!(!out.is_error && out.content.contains("no files match"));
        assert!(Glob.run(json!({"pattern": "[", }), &c).await.is_error);
        assert!(Glob.run(json!({"pattern": "*", "path": "c.txt"}), &c).await.is_error);
        assert_eq!(Glob.summary(&json!({"pattern": "*.rs", "path": "src"})), "glob *.rs in src");
    }

    #[tokio::test]
    async fn a_compiled_program_found_is_marked_with_what_it_is() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let prog = d.path().join("prog.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &prog).unwrap();
        std::fs::write(d.path().join("readme.exe"), "text").unwrap();
        let out = Glob.run(json!({"pattern": "*.exe"}), &c).await;
        let what = nexo_binmagic::kind_at(&prog).unwrap().facts(None);
        let mut lines: Vec<&str> = out.content.lines().collect();
        lines.sort();
        assert_eq!(lines, [format!("prog.exe  ({what})").as_str(), "readme.exe"]);
    }
}
