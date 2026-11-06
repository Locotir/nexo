//! `grep`: lines matching a regular expression, across a tree or in one file.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use globset::GlobBuilder;
use nexo_core::{Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSpec};
use regex::RegexBuilder;
use serde_json::{json, Value};

use crate::glob::walker;
use crate::util::{bool_arg, cut_line, display_path, int_arg, is_binary, outside_note, resolve, str_arg};

/// Files larger than this are not searched (logs, bundles, data).
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// A search stops after this long and says so.
pub const TIME_CAP: Duration = Duration::from_secs(20);

pub const DEFAULT_MAX_RESULTS: u64 = 200;
const LINE_CHARS: usize = 300;

pub struct Grep;

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &str {
        "grep"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".into(),
            description: "Search file contents with a regular expression (Rust regex syntax) under a \
                          directory or in one file; gitignored and binary files are skipped. Returns \
                          `path:line: text` per match (with `context` lines around each when asked), or \
                          only the matching files with `files_only`. At most `max_results` matches \
                          (default 200). Use it to find definitions, usages and error strings before \
                          reading."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "The regular expression, e.g. `fn \\w+\\(`."},
                    "path": {"type": "string", "description": "A directory or a file. Default: the working directory."},
                    "glob": {"type": "string", "description": "Only files whose name matches, e.g. `*.rs`; with a `/` or `**` the path under `path` is matched, e.g. `src/**/*.ts`."},
                    "case_insensitive": {"type": "boolean", "description": "Ignore case. Default false."},
                    "context": {"type": "integer", "minimum": 0, "description": "Lines shown before and after each match. Default 0."},
                    "files_only": {"type": "boolean", "description": "List the matching files instead of the lines."},
                    "max_results": {"type": "integer", "minimum": 1, "description": "Matches returned at most. Default 200."}
                },
                "required": ["pattern"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Search
    }

    fn summary(&self, args: &Value) -> String {
        match (str_arg(args, "pattern"), str_arg(args, "path")) {
            (Some(p), Some(d)) => format!("grep /{}/ in {d}", cut_line(p, 60)),
            (Some(p), None) => format!("grep /{}/", cut_line(p, 60)),
            _ => "grep (no pattern)".into(),
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(pattern) = str_arg(&args, "pattern") else {
            return ToolOutput::error("grep: `pattern` is required");
        };
        // a bounded compiled size: a pattern like `(a{1000}){1000}` is refused, not built
        let re = match RegexBuilder::new(pattern)
            .case_insensitive(bool_arg(&args, "case_insensitive"))
            .size_limit(2 << 20)
            .build()
        {
            Ok(r) => r,
            Err(e) => return ToolOutput::error(format!("grep: bad pattern `{pattern}`: {e}")),
        };
        // as in `glob`: a pattern with a directory part matches the path under `path`, else the name
        let glob_arg = str_arg(&args, "glob");
        let by_name = glob_arg.map(|g| !g.contains('/') && !g.contains("**")).unwrap_or(true);
        let name_filter = match glob_arg {
            Some(g) => match GlobBuilder::new(g).literal_separator(!by_name).build() {
                Ok(g) => Some(g.compile_matcher()),
                Err(e) => return ToolOutput::error(format!("grep: bad glob `{g}`: {e}")),
            },
            None => None,
        };
        let root = resolve(&ctx.cwd, str_arg(&args, "path").unwrap_or("."));
        let shown_root = format!("{}{}", display_path(&ctx.cwd, &root), outside_note(&ctx.cwd, &root));
        if !root.exists() {
            return ToolOutput::error(format!("grep: {shown_root} does not exist"));
        }
        if root.is_file() {
            if let Ok(m) = std::fs::metadata(&root) {
                if m.len() > MAX_FILE_BYTES {
                    return ToolOutput::error(format!(
                        "grep: {shown_root} is too large to search here ({} MiB); use `bash` with grep",
                        m.len() >> 20
                    ));
                }
            }
        }
        let context = int_arg(&args, "context").unwrap_or(0) as usize;
        let files_only = bool_arg(&args, "files_only");
        let max = int_arg(&args, "max_results").unwrap_or(DEFAULT_MAX_RESULTS).max(1) as usize;
        let cwd = ctx.cwd.clone();
        let root2 = root.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let deadline = Instant::now() + TIME_CAP;
            let mut cut = false;
            // files that hold keys are searched only when named as `path` (the permission asks then)
            let mut kept_out = 0usize;
            let files: Vec<PathBuf> = if root2.is_file() {
                vec![root2.clone()]
            } else {
                let mut v: Vec<PathBuf> = Vec::new();
                for e in walker(&root2).build().flatten() {
                    if Instant::now() > deadline {
                        cut = true;
                        break;
                    }
                    if !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                        continue;
                    }
                    let p = e.into_path();
                    if nexo_core::paths::holds_keys(&p) {
                        kept_out += 1;
                        continue;
                    }
                    let hit = |g: &globset::GlobMatcher| {
                        if by_name {
                            p.file_name().map(|n| g.is_match(std::path::Path::new(n))).unwrap_or(false)
                        } else {
                            g.is_match(p.strip_prefix(&root2).unwrap_or(&p))
                        }
                    };
                    if name_filter.as_ref().map(hit).unwrap_or(true) {
                        v.push(p);
                    }
                }
                v.sort();
                v
            };
            let mut items: Vec<String> = Vec::new();
            let mut total = 0usize;
            let mut hit_files = 0usize;
            let mut too_big = 0usize;
            let mut unreadable = 0usize;
            for f in files {
                if Instant::now() > deadline {
                    cut = true;
                    break;
                }
                match std::fs::metadata(&f) {
                    Ok(m) if m.len() > MAX_FILE_BYTES => {
                        too_big += 1;
                        continue;
                    }
                    Ok(_) => {}
                    Err(_) => {
                        unreadable += 1;
                        continue;
                    }
                }
                let Ok(bytes) = std::fs::read(&f) else {
                    unreadable += 1;
                    continue;
                };
                if is_binary(&bytes) {
                    continue;
                }
                let text = String::from_utf8_lossy(&bytes);
                let lines: Vec<&str> = text.lines().collect();
                let shown = display_path(&cwd, &f);
                let mut file_hit = false;
                for (i, line) in lines.iter().enumerate() {
                    if !re.is_match(line) {
                        continue;
                    }
                    total += 1;
                    if files_only {
                        if !file_hit {
                            hit_files += 1;
                            if items.len() < max {
                                items.push(shown.clone());
                            }
                            file_hit = true;
                        }
                        continue;
                    }
                    if total > max {
                        continue;
                    }
                    if context > 0 {
                        let from = i.saturating_sub(context);
                        for (j, l) in lines.iter().enumerate().take(i).skip(from) {
                            items.push(format!("{shown}-{}- {}", j + 1, cut_line(l, LINE_CHARS)));
                        }
                    }
                    items.push(format!("{shown}:{}: {}", i + 1, cut_line(line, LINE_CHARS)));
                    if context > 0 {
                        for (j, l) in lines.iter().enumerate().skip(i + 1).take(context) {
                            items.push(format!("{shown}-{}- {}", j + 1, cut_line(l, LINE_CHARS)));
                        }
                        items.push("--".into());
                    }
                }
            }
            (items, total, cut, kept_out, hit_files, too_big, unreadable)
        })
        .await;
        let Ok((items, total, cut, kept_out, hit_files, too_big, unreadable)) = joined else {
            return ToolOutput::error("grep: the search failed unexpectedly");
        };
        let mut content = if total == 0 {
            format!("grep: no match for `{pattern}` in {shown_root}")
        } else if !files_only && total > max {
            format!(
                "{}\n… {} more {} ({total} in all); narrow the pattern or the path",
                items.join("\n"),
                total - max,
                if total - max == 1 { "match" } else { "matches" }
            )
        } else if files_only {
            let mut s = format!(
                "{} in {}\n{}",
                nexo_core::count(total, "match"),
                nexo_core::count(hit_files, "file"),
                items.join("\n")
            );
            if hit_files > items.len() {
                let more = hit_files - items.len();
                s.push_str(&format!("\n… {more} more {}", if more == 1 { "file" } else { "files" }));
            }
            s
        } else {
            items.join("\n")
        };
        if too_big > 0 {
            content.push_str(&format!(
                "\n[{} over {} MiB not searched; use `bash` with grep for those]",
                nexo_core::count(too_big, "file"),
                MAX_FILE_BYTES >> 20
            ));
        }
        if unreadable > 0 {
            content.push_str(&format!("\n[{} could not be read]", nexo_core::count(unreadable, "file")));
        }
        if kept_out > 0 {
            content.push_str(&format!(
                "\n[{} that may hold keys (.env, private keys, credentials) not searched; name one as `path` to search it]",
                nexo_core::count(kept_out, "file")
            ));
        }
        if cut {
            content
                .push_str(&format!("\n[search stopped after {} s; narrow the path or the glob]", TIME_CAP.as_secs()));
        }
        ToolOutput::ok(content).with_display(ToolDisplay::List { items })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, ToolContext) {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/a.rs"), "fn alpha() {}\nlet x = 1;\nfn beta() {}\n").unwrap();
        std::fs::write(d.path().join("notes.md"), "Alpha and omega\n").unwrap();
        std::fs::write(d.path().join("blob.bin"), b"fn \x00alpha").unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        (d, c)
    }

    #[tokio::test]
    async fn files_that_hold_keys_are_searched_only_when_named() {
        let (d, c) = fixture();
        std::fs::write(d.path().join(".env"), "TOKEN=alpha-secret\n").unwrap();
        std::fs::create_dir_all(d.path().join("certs")).unwrap();
        std::fs::write(d.path().join("certs/server.key"), "alpha-key\n").unwrap();
        let out = Grep.run(json!({"pattern": "alpha"}), &c).await;
        assert!(!out.content.contains("secret") && !out.content.contains("alpha-key"), "{}", out.content);
        assert!(out.content.contains("src/a.rs:1:"), "{}", out.content);
        assert!(out.content.contains("[2 files that may hold keys"), "{}", out.content);
        let out = Grep.run(json!({"pattern": "alpha", "path": ".env"}), &c).await;
        assert_eq!(out.content, ".env:1: TOKEN=alpha-secret");
    }

    #[tokio::test]
    async fn finds_lines_with_options() {
        let (_d, c) = fixture();
        let out = Grep.run(json!({"pattern": "fn \\w+"}), &c).await;
        assert_eq!(out.content, "src/a.rs:1: fn alpha() {}\nsrc/a.rs:3: fn beta() {}");
        let out = Grep.run(json!({"pattern": "alpha", "case_insensitive": true}), &c).await;
        assert_eq!(out.content.lines().count(), 2, "{}", out.content);
        let out = Grep.run(json!({"pattern": "alpha", "glob": "*.md"}), &c).await;
        assert!(out.content.contains("no match"));
        let out = Grep.run(json!({"pattern": "alpha", "glob": "*.md", "case_insensitive": true}), &c).await;
        assert_eq!(out.content, "notes.md:1: Alpha and omega");
        let out = Grep.run(json!({"pattern": "x = 1", "context": 1}), &c).await;
        assert_eq!(out.content, "src/a.rs-1- fn alpha() {}\nsrc/a.rs:2: let x = 1;\nsrc/a.rs-3- fn beta() {}\n--");
        let out = Grep.run(json!({"pattern": "fn", "files_only": true, "case_insensitive": true}), &c).await;
        assert_eq!(out.content, "2 matches in 1 file\nsrc/a.rs");
        let out = Grep.run(json!({"pattern": "fn", "max_results": 1}), &c).await;
        assert!(out.content.contains("1 more match ("), "{}", out.content);
        let out = Grep.run(json!({"pattern": "beta", "path": "src/a.rs"}), &c).await;
        assert_eq!(out.content, "src/a.rs:3: fn beta() {}");
        assert!(Grep.run(json!({"pattern": "("}), &c).await.is_error);
        let out = Grep.run(json!({"pattern": "(a{1000}){1000}"}), &c).await;
        assert!(
            out.is_error && out.content.contains("bad pattern"),
            "a pattern too large to compile is refused: {}",
            out.content
        );
        let big = std::fs::File::create(_d.path().join("huge.log")).unwrap();
        big.set_len(MAX_FILE_BYTES + 1).unwrap(); // sparse
        let out = Grep.run(json!({"pattern": "alpha", "path": "huge.log"}), &c).await;
        assert!(out.is_error && out.content.contains("too large"), "a huge file named is refused: {}", out.content);
        let out = Grep.run(json!({"pattern": "alpha"}), &c).await;
        assert!(out.content.contains("[1 file over 8 MiB not searched"), "{}", out.content);
        assert!(Grep.run(json!({"pattern": "a", "path": "nope"}), &c).await.is_error);
        assert_eq!(Grep.summary(&json!({"pattern": "fn", "path": "src"})), "grep /fn/ in src");
    }

    #[tokio::test]
    async fn a_glob_with_a_directory_part_matches_the_path() {
        let (_d, c) = fixture();
        let out = Grep.run(json!({"pattern": "beta", "glob": "src/**/*.rs"}), &c).await;
        assert_eq!(out.content, "src/a.rs:3: fn beta() {}");
        let out = Grep.run(json!({"pattern": "beta", "glob": "**/*.rs"}), &c).await;
        assert_eq!(out.content, "src/a.rs:3: fn beta() {}");
        let out = Grep.run(json!({"pattern": "beta", "glob": "lib/**/*.rs"}), &c).await;
        assert!(out.content.contains("no match"), "{}", out.content);
    }

    #[tokio::test]
    async fn files_only_lists_at_most_max_results_files() {
        let (d, c) = fixture();
        for i in 0..5 {
            std::fs::write(d.path().join(format!("f{i}.txt")), "hit\n").unwrap();
        }
        let out = Grep.run(json!({"pattern": "hit", "files_only": true, "max_results": 2}), &c).await;
        assert_eq!(out.content, "5 matches in 5 files\nf0.txt\nf1.txt\n… 3 more files", "{}", out.content);
    }
}
