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
        // name, size, may be a program, where a symlink points
        let mut files: Vec<(String, Size, bool, Option<String>)> = Vec::new();
        let mut stopped: Option<String> = None;
        loop {
            let entry = match rd.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(e) => {
                    stopped = Some(stopped_note(&e));
                    break;
                }
            };
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
                            files.push((name, Size::Known(t.len()), program, Some(target)))
                        }
                        Err(_) => files.push((name, Size::BrokenLink, false, Some(target))),
                    }
                }
                Ok(m) if m.is_dir() => dirs.push(format!("{name}/")),
                Ok(m) => {
                    let program = may_be_program(&name, &m);
                    files.push((name, Size::Known(m.len()), program, None))
                }
                // the entry's own type needs no stat, so a folder is still told from a file
                Err(e) => match entry.file_type().await {
                    Ok(t) if t.is_dir() => dirs.push(format!("{name}/ (cannot stat: {e})")),
                    _ => files.push((name, Size::Unknown(e.to_string()), false, None)),
                },
            }
        }
        dirs.sort();
        files.sort();
        let total = dirs.len() + files.len();
        let kinds = files.len() <= KINDS_UP_TO;
        let mut items: Vec<String> = dirs;
        items.extend(files.into_iter().map(|(n, size, program, link)| {
            let size = match size {
                Size::Known(size) => size,
                Size::BrokenLink => return format!("{n} -> {} (broken link)", link.unwrap_or_default()),
                Size::Unknown(why) => return format!("{n} (size unknown: {why})"),
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
            content = match stopped {
                Some(_) => format!("{shown}: no entries read"),
                None => format!("{shown}: (empty directory)"),
            };
        }
        if let Some(note) = stopped {
            content.push('\n');
            content.push_str(&note);
        }
        ToolOutput::ok(content).with_display(ToolDisplay::List { items })
    }
}

/// What a listed file's size slot holds.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Size {
    Known(u64),
    /// A symlink whose target does not exist.
    BrokenLink,
    /// The entry could not be stat'ed; the error says why.
    Unknown(String),
}

/// The line that ends a listing the OS stopped reading.
fn stopped_note(e: &std::io::Error) -> String {
    format!("[listing stopped: {e}; entries after this point are not shown]")
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

    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_that_cannot_be_statted_is_not_shown_as_empty() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let sub = d.path().join("d");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("f"), "12345").unwrap();
        std::fs::create_dir(sub.join("s")).unwrap();
        // read without search: the names can be listed, the entries cannot be stat'ed
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o600)).unwrap();
        let can_stat = std::fs::metadata(sub.join("f")).is_ok();
        let out = Ls.run(json!({"path": "d"}), &c).await;
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).unwrap();
        if can_stat {
            return; // root: the mode does not stop the stat
        }
        assert!(out.content.contains("f (size unknown:"), "{}", out.content);
        assert!(out.content.contains("s/ (cannot stat:"), "{}", out.content);
        assert!(!out.content.contains("(0 B)"), "{}", out.content);
    }

    #[test]
    fn stopped_note_names_the_error() {
        let note = stopped_note(&std::io::Error::other("boom"));
        assert!(note.contains("listing stopped: boom"), "{note}");
        assert!(note.contains("entries after this point are not shown"), "{note}");
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
