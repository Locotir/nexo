//! `ls`: one directory.

use async_trait::async_trait;
use nexo_core::{Compact, Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

use crate::program::{listed_kind, may_be_program, KINDS_UP_TO};
use crate::util::{display_path, outside_note, resolve, str_arg};

/// Entries listed at most.
const MAX_ENTRIES: usize = 1000;

pub struct Ls;

#[async_trait]
impl Tool for Ls {
    fn name(&self) -> &str {
        "ls"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ls".into(),
            description: "List one directory: subdirectories first (with a trailing `/`), then files with \
                          their sizes."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Default: the working directory."}
                }
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: "List a directory (default: working directory); subdirectories first, then files with sizes.",
            params: &[],
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Search
    }

    fn summary(&self, args: &Value) -> String {
        format!("ls {}", str_arg(args, "path").unwrap_or("."))
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let path = resolve(&ctx.cwd, str_arg(&args, "path").unwrap_or("."));
        let shown = format!("{}{}", display_path(&ctx.cwd, &path), outside_note(&ctx.cwd, &path));
        let mut rd = match tokio::fs::read_dir(&path).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolOutput::error(format!("ls: {shown} does not exist"))
            }
            Err(e) if path.is_file() => {
                let _ = e;
                return ToolOutput::error(format!("ls: {shown} is a file; use `read`"));
            }
            Err(e) => return ToolOutput::error(format!("ls: cannot list {shown}: {e}")),
        };
        let mut dirs = Vec::new();
        // name, size (None for a broken link), may be a program, where a symlink points
        let mut files: Vec<(String, Option<u64>, bool, Option<String>)> = Vec::new();
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            match entry.metadata().await {
                // `DirEntry::metadata` does not follow symlinks, so a link is looked through here: the
                // model sees what it points to instead of a file the size of the link text.
                Ok(m) if m.file_type().is_symlink() => {
                    let target = tokio::fs::read_link(entry.path())
                        .await
                        .map(|t| t.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    match tokio::fs::metadata(entry.path()).await {
                        Ok(t) if t.is_dir() => dirs.push(format!("{name}/ -> {target}")),
                        Ok(t) => {
                            let program = may_be_program(&name, &t);
                            files.push((name, Some(t.len()), program, Some(target)))
                        }
                        Err(_) => files.push((name, None, false, Some(target))),
                    }
                }
                Ok(m) if m.is_dir() => dirs.push(format!("{name}/")),
                Ok(m) => {
                    let program = may_be_program(&name, &m);
                    files.push((name, Some(m.len()), program, None))
                }
                Err(_) => files.push((name, Some(0), false, None)),
            }
        }
        dirs.sort();
        files.sort();
        let total = dirs.len() + files.len();
        let kinds = files.len() <= KINDS_UP_TO;
        let mut items: Vec<String> = dirs;
        items.extend(files.into_iter().map(|(n, size, program, link)| {
            let Some(size) = size else {
                return format!("{n} -> {} (broken link)", link.unwrap_or_default());
            };
            let to = link.map(|t| format!(" -> {t}")).unwrap_or_default();
            match (kinds && program).then(|| listed_kind(&path.join(&n))).flatten() {
                Some(kind) => format!("{n}  ({}, {kind}){to}", human(size)),
                None => format!("{n}  ({}){to}", human(size)),
            }
        }));
        let more = total.saturating_sub(MAX_ENTRIES);
        items.truncate(MAX_ENTRIES);
        let mut content = format!("{shown}: {}\n{}", nexo_core::count(total, "entry"), items.join("\n"));
        if more > 0 {
            content.push_str(&format!("\n… {more} more entries"));
        }
        if total == 0 {
            content = format!("{shown}: (empty directory)");
        }
        ToolOutput::ok(content).with_display(ToolDisplay::List { items })
    }
}

fn human(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lists_dirs_first_then_files_with_sizes() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::create_dir(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("b.txt"), "12345").unwrap();
        std::fs::write(d.path().join("a.txt"), vec![0u8; 2048]).unwrap();
        let out = Ls.run(json!({}), &c).await;
        assert!(!out.is_error);
        assert_eq!(out.content, ".: 3 entries\nsub/\na.txt  (2.0 KiB)\nb.txt  (5 B)");
        assert!(Ls.run(json!({"path": "nope"}), &c).await.content.contains("does not exist"));
        assert!(Ls.run(json!({"path": "b.txt"}), &c).await.content.contains("is a file"));
        std::fs::create_dir(d.path().join("empty")).unwrap();
        assert!(Ls.run(json!({"path": "empty"}), &c).await.content.contains("empty directory"));
        assert_eq!(Ls.summary(&json!({"path": "src"})), "ls src");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_is_listed_with_what_it_points_to() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::create_dir(d.path().join("real")).unwrap();
        std::fs::write(d.path().join("real.txt"), "12345").unwrap();
        std::os::unix::fs::symlink("real", d.path().join("dirlink")).unwrap();
        std::os::unix::fs::symlink("real.txt", d.path().join("filelink")).unwrap();
        std::os::unix::fs::symlink("gone", d.path().join("dangling")).unwrap();
        let out = Ls.run(json!({}), &c).await;
        assert_eq!(
            out.content,
            ".: 5 entries\ndirlink/ -> real\nreal/\ndangling -> gone (broken link)\nfilelink  (5 B) -> real.txt\nreal.txt  (5 B)"
        );
    }

    #[tokio::test]
    async fn a_compiled_program_is_marked_with_what_it_is() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let prog = d.path().join(if cfg!(windows) { "prog.exe" } else { "prog" });
        std::fs::copy(std::env::current_exe().unwrap(), &prog).unwrap();
        std::fs::write(d.path().join("notes.txt"), "text").unwrap();
        let out = Ls.run(json!({}), &c).await;
        let what = nexo_binmagic::kind_at(&prog).unwrap().facts(None);
        let size = human(std::fs::metadata(&prog).unwrap().len());
        let name = prog.file_name().unwrap().to_string_lossy();
        assert_eq!(out.content, format!(".: 2 entries\nnotes.txt  (4 B)\n{name}  ({size}, {what})"));
    }
}
