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

/// The two walk limits, a value so a test can use small ones instead of 50,000 files and 20 seconds.
#[derive(Clone, Copy)]
struct Limits {
    max_candidates: usize,
    time_cap: Duration,
}

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
        Glob::find(args, ctx, Limits { max_candidates: MAX_CANDIDATES, time_cap: TIME_CAP }).await
    }
}

impl Glob {
    /// The search itself, with the limits as an argument so a test can use small ones.
    async fn find(args: Value, ctx: &ToolContext, limits: Limits) -> ToolOutput {
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
            let deadline = Instant::now() + limits.time_cap;
            let mut cut = false;
            let mut found: Vec<(SystemTime, PathBuf, bool)> = Vec::new();
            let mut links = Links::default();
            for entry in walker(&root2).build().flatten() {
                // `>=` so a zero cap stops at once even on a clock coarse enough to read the same instant twice
                if Instant::now() >= deadline {
                    cut = true;
                    break;
                }
                let kind = entry.file_type();
                let is_link = kind.is_some_and(|t| t.is_symlink());
                if !is_link && !kind.is_some_and(|t| t.is_file()) {
                    continue;
                }
                let path = entry.path();
                let rel = path.strip_prefix(&root2).unwrap_or(path);
                let hit = if by_name {
                    path.file_name().map(|n| glob.is_match(Path::new(n))).unwrap_or(false)
                } else {
                    glob.is_match(rel)
                };
                if is_link {
                    links.count(path, hit);
                    continue;
                }
                if hit {
                    // checked here, not per entry: a tree with exactly the cap in matches lost nothing
                    if found.len() >= limits.max_candidates {
                        cut = true;
                        break;
                    }
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
            (found.collect::<Vec<_>>(), total, cut, links)
        })
        .await;
        let Ok((found, total, cut, links)) = joined else {
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
        // what a link leads to is missing from any answer, not only an empty one
        if let Some(note) = links.note() {
            content.push('\n');
            content.push_str(&note);
        }
        if cut {
            content
                .push_str("\n[the walk was stopped early: too many files or too long; narrow the path or the pattern]");
        }
        ToolOutput::ok(content).with_display(ToolDisplay::List { items })
    }
}

/// Symlinks the walkers met and did not follow (they do not, so that a link loop or a link to a key file
/// cannot widen a search). Only links that could have changed the answer are counted: a link to a directory,
/// or a link to a file that passes the name filter. Used to say what an answer may be missing.
#[derive(Debug, Default)]
pub struct Links {
    pub files: usize,
    pub dirs: usize,
}

impl Links {
    /// Count the link at `path`; `name_hit` says whether its name or path passes the search's filter.
    pub fn count(&mut self, path: &Path, name_hit: bool) {
        match std::fs::metadata(path) {
            Ok(m) if m.is_dir() => self.dirs += 1,
            Ok(m) if m.is_file() && name_hit => self.files += 1,
            _ => {}
        }
    }

    /// One factual line for the answer, None when no link was skipped.
    pub fn note(&self) -> Option<String> {
        let all = self.files + self.dirs;
        if all == 0 {
            return None;
        }
        let dirs = match self.dirs {
            0 => String::new(),
            1 => " (1 to a directory)".into(),
            n => format!(" ({n} to directories)"),
        };
        Some(format!(
            "[{}{dirs} not followed; give the link's target as `path` to search it]",
            nexo_core::count(all, "symlink")
        ))
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

    /// A `Glob` run with small limits, so the caps need a handful of files and no wait.
    async fn found(dir: &Path, pattern: &str, max_candidates: usize, time_cap: Duration) -> String {
        let c = ToolContext::new(dir.to_path_buf());
        Glob::find(json!({"pattern": pattern}), &c, Limits { max_candidates, time_cap }).await.content
    }

    const STOPPED: &str = "[the walk was stopped early: too many files or too long; narrow the path or the pattern]";

    #[tokio::test]
    async fn more_than_500_matches_show_500_and_say_how_many_were_left_out() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..=MAX_RESULTS {
            std::fs::write(d.path().join(format!("f{i:03}.txt")), "").unwrap();
        }
        let c = ToolContext::new(d.path().to_path_buf());
        let out = Glob.run(json!({"pattern": "*.txt"}), &c).await;
        let lines: Vec<&str> = out.content.lines().collect();
        assert_eq!(lines.len(), MAX_RESULTS + 1, "500 files and the note");
        assert_eq!(lines[MAX_RESULTS], "… 1 more files (501 match); narrow the pattern");
        assert!(lines[..MAX_RESULTS].iter().all(|l| l.ends_with(".txt")));
        // exactly 500 matches is complete: no note
        std::fs::remove_file(d.path().join("f500.txt")).unwrap();
        let out = Glob.run(json!({"pattern": "*.txt"}), &c).await;
        assert_eq!(out.content.lines().count(), MAX_RESULTS);
        assert!(!out.content.contains("narrow the pattern"), "{}", out.content);
    }

    #[tokio::test]
    async fn the_newest_file_comes_first_and_equal_times_fall_back_to_the_path() {
        let d = tempfile::tempdir().unwrap();
        let at = |secs: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs);
        for (name, secs) in [("old.txt", 0), ("new.txt", 200), ("mid.txt", 100), ("b.log", 50), ("a.log", 50)] {
            let f = std::fs::File::create(d.path().join(name)).unwrap();
            f.set_modified(at(secs)).unwrap();
        }
        let out = found(d.path(), "*.txt", MAX_CANDIDATES, TIME_CAP).await;
        assert_eq!(out, "new.txt\nmid.txt\nold.txt");
        let out = found(d.path(), "*.log", MAX_CANDIDATES, TIME_CAP).await;
        assert_eq!(out, "a.log\nb.log");
    }

    #[tokio::test]
    async fn a_walk_over_the_candidate_cap_says_it_stopped_and_one_at_the_cap_does_not() {
        let d = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(d.path().join(name), "").unwrap();
        }
        // a file that does not match, however late the walk reaches it, loses nothing
        std::fs::write(d.path().join("z.md"), "").unwrap();
        let out = found(d.path(), "*.txt", 3, TIME_CAP).await;
        assert_eq!(out.lines().count(), 3, "{out}");
        assert!(!out.contains("stopped early"), "{out}");
        // a fourth match is lost, and the note says so
        std::fs::write(d.path().join("d.txt"), "").unwrap();
        let out = found(d.path(), "*.txt", 3, TIME_CAP).await;
        assert_eq!(out.lines().count(), 4, "3 files and the note: {out}");
        assert!(out.ends_with(&format!("\n{STOPPED}")), "{out}");
    }

    #[tokio::test]
    async fn a_walk_that_runs_out_of_time_says_it_stopped() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "").unwrap();
        let out = found(d.path(), "*.txt", MAX_CANDIDATES, Duration::ZERO).await;
        assert!(out.ends_with(STOPPED), "{out}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_empty_answer_says_how_many_symlinks_were_not_followed() {
        // the targets live outside the searched tree, as a shared folder or a vendored checkout does
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(outside.path().join("shared")).unwrap();
        std::fs::write(outside.path().join("shared/lib.rs"), "").unwrap();
        std::fs::write(outside.path().join("real.md"), "").unwrap();
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::os::unix::fs::symlink(outside.path().join("shared"), d.path().join("vendor")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("real.md"), d.path().join("notes.rs")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("real.md"), d.path().join("other.txt")).unwrap();
        let note = "[2 symlinks (1 to a directory) not followed; give the link's target as `path` to search it]";
        let out = Glob.run(json!({"pattern": "*.rs"}), &c).await;
        assert!(out.content.starts_with("glob: no files match"), "{}", out.content);
        assert!(out.content.ends_with(note), "{}", out.content);
        let out = Glob.run(json!({"pattern": "*.rs", "path": "vendor"}), &c).await;
        assert_eq!(out.content, "vendor/lib.rs");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_non_empty_answer_also_says_how_many_symlinks_were_not_followed() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(outside.path().join("shared")).unwrap();
        std::fs::write(outside.path().join("shared/lib.txt"), "").unwrap();
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::write(d.path().join("a.txt"), "").unwrap();
        std::os::unix::fs::symlink(outside.path().join("shared"), d.path().join("l")).unwrap();
        let out = Glob.run(json!({"pattern": "*.txt"}), &c).await;
        assert!(out.content.starts_with("a.txt\n"), "{}", out.content);
        assert!(out.content.contains("symlink") && out.content.contains("not followed"), "{}", out.content);
        assert!(!out.content.contains("lib.txt"), "{}", out.content);
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
