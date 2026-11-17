//! `grep`: lines matching a regular expression, across a tree or in one file.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use globset::GlobBuilder;
use ignore::WalkState;
use nexo_core::{Compact, Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSpec};
use regex::{Regex, RegexBuilder};
use serde_json::{json, Value};

use crate::glob::walker;
use crate::program::{bin_call, bin_offered, callable, quoted, slash};
use crate::util::{bool_arg, cut_line, display_path, int_arg, is_binary, outside_note, resolve, str_arg};

/// Files larger than this are not searched (logs, bundles, data).
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// A search stops after this long and says so.
pub const TIME_CAP: Duration = Duration::from_secs(20);

pub const DEFAULT_MAX_RESULTS: u64 = 200;
/// One file shows at most this many matches by default: in recorded sessions 6 % of the files with hits
/// held more than 20, and one such file could fill the whole answer while the others got nothing.
pub const DEFAULT_MAX_PER_FILE: u64 = 20;
/// Files searched together before their answers are merged in path order.
const BATCH: usize = 128;
/// Once this many matches are left out of the answer, the rest of the files are not counted (the answer says so).
const COUNT_LEFT_UP_TO: usize = 2000;
const LINE_CHARS: usize = 300;
/// A multi-line match shows at most this many of its lines.
const MAX_SPAN_LINES: usize = 10;

pub struct Grep;

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &str {
        "grep"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".into(),
            description: "Search file contents with a regex (Rust syntax) under a \
                          directory or in one file; gitignored and binary files are skipped. Returns \
                          `path:line: text` per match (`context` lines around it), or \
                          only the matching files with `files_only`. At most `max_results` (200) matches, \
                          `max_per_file` (20) per file."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string", "description": "Default: the working directory."},
                    "glob": {"type": "string", "description": "Only files whose name matches, e.g. `*.rs`; with a `/` or `**` the path under `path` is matched."},
                    "case_insensitive": {"type": "boolean"},
                    "context": {"type": "integer", "minimum": 0},
                    "files_only": {"type": "boolean"},
                    "max_results": {"type": "integer", "minimum": 1},
                    "max_per_file": {"type": "integer"},
                    "multiline": {"type": "boolean", "description": "Match across lines (`\\n` in the pattern, `.` also matches a newline)."}
                },
                "required": ["pattern"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: "Regex search (Rust syntax) of file contents under `path` (default: working directory); returns `path:line: text`.",
            params: &[
                ("glob", "File name filter, e.g. `*.rs`"),
                ("context", "Lines around a match"),
                ("files_only", "List files only"),
                ("max_results", "Default 200"),
                ("max_per_file", "Default 20"),
                ("multiline", "Match across lines"),
            ],
        })
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
        let multiline = bool_arg(&args, "multiline");
        let crlf = multiline && !names_carriage_return(pattern);
        let re = match RegexBuilder::new(pattern)
            .case_insensitive(bool_arg(&args, "case_insensitive"))
            .multi_line(multiline)
            // CRLF files: without this `$` and `^` never see the line break because the `\r` sits before the `\n`
            .crlf(crlf)
            .dot_matches_new_line(multiline)
            .size_limit(2 << 20)
            .build()
        {
            Ok(r) => r,
            Err(e) => return ToolOutput::error(format!("grep: bad pattern `{pattern}`: {e}")),
        };
        // the same pattern over raw bytes: a compiled program is not text, yet holds names and strings
        let in_programs = regex::bytes::RegexBuilder::new(pattern)
            .case_insensitive(bool_arg(&args, "case_insensitive"))
            .multi_line(multiline)
            // as above: `$` and `^` must see CRLF line ends in the byte search too
            .crlf(crlf)
            .dot_matches_new_line(multiline)
            .size_limit(2 << 20)
            .build()
            .ok();
        let strings_grep = match bool_arg(&args, "case_insensitive") {
            true => format!("(?i){pattern}"),
            false => pattern.to_string(),
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
                    if let Some(kind) = nexo_binmagic::kind_at(&root) {
                        let file = display_path(&ctx.cwd, &root);
                        let facts = kind.facts(Some(m.len()));
                        return ToolOutput::error(match strings_call(&file, &strings_grep, bin_offered(ctx)) {
                            Some(call) => format!("grep: {shown_root} is not text: {facts}; {call} lists the matches"),
                            None => format!("grep: {shown_root} is not text: {facts}"),
                        });
                    }
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
        let max_per_file = int_arg(&args, "max_per_file").unwrap_or(DEFAULT_MAX_PER_FILE).max(1) as usize;
        let search = Search {
            re,
            in_programs,
            multiline,
            files_only,
            context,
            keep: max_per_file.min(max),
            cwd: ctx.cwd.clone(),
        };
        let root2 = root.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let deadline = Instant::now() + TIME_CAP;
            let mut cut = false;
            // files that hold keys are searched only when named as `path` (the permission asks then)
            let mut kept_out = 0usize;
            let mut files: Vec<PathBuf> = if root2.is_file() {
                vec![root2.clone()]
            } else {
                let found = Mutex::new((Vec::<PathBuf>::new(), 0usize));
                let timed_out = AtomicBool::new(false);
                walker(&root2).build_parallel().run(|| {
                    let (timed_out, root2, name_filter) = (&timed_out, &root2, &name_filter);
                    let mut hand = Hand { into: &found, files: Vec::new(), kept_out: 0 };
                    Box::new(move |entry| {
                        if Instant::now() > deadline {
                            timed_out.store(true, Ordering::Relaxed);
                            return WalkState::Quit;
                        }
                        let Ok(e) = entry else { return WalkState::Continue };
                        if !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                            return WalkState::Continue;
                        }
                        let p = e.into_path();
                        if nexo_core::paths::holds_keys(&p) {
                            hand.kept_out += 1;
                            return WalkState::Continue;
                        }
                        let hit = |g: &globset::GlobMatcher| {
                            if by_name {
                                p.file_name().map(|n| g.is_match(std::path::Path::new(n))).unwrap_or(false)
                            } else {
                                g.is_match(p.strip_prefix(root2).unwrap_or(&p))
                            }
                        };
                        if name_filter.as_ref().map(hit).unwrap_or(true) {
                            hand.files.push(p);
                        }
                        WalkState::Continue
                    })
                });
                cut = timed_out.load(Ordering::Relaxed);
                let (v, k) = found.into_inner().unwrap_or_default();
                kept_out = k;
                v
            };
            // the threads finish in any order; the answer is always in path order
            files.sort();
            let mut m = Merge::default();
            let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 8);
            let mut searched = 0usize;
            for batch in files.chunks(BATCH) {
                if Instant::now() > deadline {
                    cut = true;
                    break;
                }
                // a batch is searched by several threads and merged in path order, so the answer does not
                // depend on which thread was faster
                let next = AtomicUsize::new(0);
                let outs: Vec<Mutex<Option<FileOut>>> = batch.iter().map(|_| Mutex::new(None)).collect();
                std::thread::scope(|sc| {
                    for _ in 0..threads.min(batch.len()) {
                        sc.spawn(|| loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            if i >= batch.len() || Instant::now() > deadline {
                                break;
                            }
                            let out = search.file(&batch[i]);
                            if let Ok(mut slot) = outs[i].lock() {
                                *slot = Some(out);
                            }
                        });
                    }
                });
                for slot in outs {
                    match slot.into_inner().ok().flatten() {
                        Some(out) => m.add(out, max, max_per_file, files_only),
                        None => {
                            cut = true;
                            break;
                        }
                    }
                    searched += 1;
                }
                if cut {
                    break;
                }
                // Past `max_results` the rest of the search only counts. Enough has been counted for the
                // model to see how broad the pattern is: stop here, between batches, so that the answer
                // is the same on every run.
                if !files_only && m.left >= COUNT_LEFT_UP_TO && searched < files.len() {
                    m.counting_stopped = Some((searched, files.len()));
                    break;
                }
            }
            (m, cut, kept_out)
        })
        .await;
        let Ok((m, cut, kept_out)) = joined else {
            return ToolOutput::error("grep: the search failed unexpectedly");
        };
        let Merge {
            items, total, left, left_files, hit_files, too_big, unreadable, counting_stopped, programs, ..
        } = m;
        let mut content = if total == 0 {
            let mut s = format!("grep: no match for `{pattern}` in {shown_root}");
            if !multiline && (pattern.contains('\n') || pattern.contains("\\n")) {
                s.push_str(". Patterns are matched line by line; set `multiline: true` to match across lines.");
            }
            s
        } else if !files_only && left > 0 {
            let matches = if left == 1 { "match" } else { "matches" };
            let files = nexo_core::count(left_files, "file");
            match counting_stopped {
                // the counts are what the files searched so far hold
                Some((done, all)) => format!(
                    "{}\n… at least {left} more {matches} in at least {files} (at least {total} in all; counting stopped after {done} of {all} files); narrow the pattern or the path",
                    items.join("\n")
                ),
                None => format!(
                    "{}\n… {left} more {matches} in {files} ({total} in all); narrow the pattern or the path",
                    items.join("\n")
                ),
            }
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
        if let Some(line) = programs_line(&programs, &strings_grep, bin_offered(ctx)) {
            content.push('\n');
            content.push_str(&line);
        }
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

/// What every search of one file shares.
struct Search {
    re: Regex,
    /// `re` over bytes, for the compiled programs met (None if the pattern does not build so).
    in_programs: Option<regex::bytes::Regex>,
    multiline: bool,
    files_only: bool,
    context: usize,
    /// Matches of one file kept for the answer (`max_per_file`, and never more than `max_results`).
    keep: usize,
    cwd: PathBuf,
}

enum FileOut {
    /// Binary, or nothing to say.
    Skip,
    /// A compiled program whose bytes hold a match.
    Program(String, nexo_binmagic::Compiled),
    TooBig,
    Unreadable,
    Hits {
        shown: String,
        total: usize,
        /// The lines of each of the first `keep` matches, with their context.
        chunks: Vec<Vec<String>>,
    },
}

impl Search {
    fn file(&self, f: &Path) -> FileOut {
        match std::fs::metadata(f) {
            Ok(m) if m.len() > MAX_FILE_BYTES => return FileOut::TooBig,
            Ok(_) => {}
            Err(_) => return FileOut::Unreadable,
        }
        let Ok(bytes) = std::fs::read(f) else {
            return FileOut::Unreadable;
        };
        if is_binary(&bytes) {
            let held = |re: &regex::bytes::Regex| re.is_match(&bytes);
            return match nexo_binmagic::kind_of(&bytes) {
                Some(kind) if self.in_programs.as_ref().is_some_and(held) => {
                    FileOut::Program(display_path(&self.cwd, f), kind)
                }
                _ => FileOut::Skip,
            };
        }
        let text = String::from_utf8_lossy(&bytes);
        // the first and last line (0-based) of each match
        let mut spans: Vec<(usize, usize)> = Vec::new();
        let mut lines: Vec<&str> = Vec::new();
        if self.multiline {
            lines = text.lines().collect();
            let nl: Vec<usize> = text.match_indices('\n').map(|(i, _)| i).collect();
            let line_of = |off: usize| nl.partition_point(|&p| p < off);
            for m in self.re.find_iter(&text) {
                let a = line_of(m.start());
                let b = line_of(if m.end() > m.start() { m.end() - 1 } else { m.start() });
                if spans.last().is_some_and(|&(_, e)| a <= e) {
                    continue; // a second match on lines already shown
                }
                spans.push((a, b.max(a)));
            }
        } else if self.context > 0 && !self.files_only {
            lines = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if self.re.is_match(line) {
                    spans.push((i, i));
                }
            }
        } else {
            // no line is needed again afterwards: do not hold them all
            for (i, line) in text.lines().enumerate() {
                if self.re.is_match(line) {
                    spans.push((i, i));
                }
            }
        }
        if spans.is_empty() {
            return FileOut::Skip;
        }
        let shown = display_path(&self.cwd, f);
        let total = spans.len();
        let mut chunks: Vec<Vec<String>> = Vec::new();
        if !self.files_only {
            if lines.is_empty() {
                // plain search without context: the lines of the few matches kept are read again
                let wanted: std::collections::BTreeSet<usize> = spans.iter().take(self.keep).map(|&(a, _)| a).collect();
                let last = wanted.last().copied().unwrap_or(0);
                lines = vec![""; last + 1];
                for (i, l) in text.lines().enumerate().take(last + 1) {
                    if wanted.contains(&i) {
                        lines[i] = l;
                    }
                }
            }
            for &(a, b) in spans.iter().take(self.keep) {
                let mut items: Vec<String> = Vec::new();
                if self.context > 0 {
                    for (j, l) in lines.iter().enumerate().take(a).skip(a.saturating_sub(self.context)) {
                        items.push(format!("{shown}-{}- {}", j + 1, cut_line(l, LINE_CHARS)));
                    }
                }
                let last = b.min(a + MAX_SPAN_LINES - 1);
                for (j, l) in lines.iter().enumerate().take(last + 1).skip(a) {
                    items.push(format!("{shown}:{}: {}", j + 1, cut_line(l, LINE_CHARS)));
                }
                if b > last {
                    items.push(format!("{shown}:… {} more lines in this match", b - last));
                }
                if self.context > 0 {
                    for (j, l) in lines.iter().enumerate().skip(b + 1).take(self.context) {
                        items.push(format!("{shown}-{}- {}", j + 1, cut_line(l, LINE_CHARS)));
                    }
                    items.push("--".into());
                }
                chunks.push(items);
            }
        }
        FileOut::Hits { shown, total, chunks }
    }
}

/// The answer being put together from the files in path order.
#[derive(Default)]
struct Merge {
    items: Vec<String>,
    total: usize,
    shown_total: usize,
    /// Matches not shown (over `max_results` or `max_per_file`) and the files that hold them.
    left: usize,
    left_files: usize,
    hit_files: usize,
    too_big: usize,
    unreadable: usize,
    /// Files searched and files in all, when the counting was stopped early.
    counting_stopped: Option<(usize, usize)>,
    /// The compiled programs that hold a match: the first few, and how many in all.
    programs: (Vec<(String, nexo_binmagic::Compiled)>, usize),
}

impl Merge {
    fn add(&mut self, out: FileOut, max: usize, max_per_file: usize, files_only: bool) {
        match out {
            FileOut::Skip => {}
            FileOut::Program(shown, kind) => {
                if self.programs.0.len() < PROGRAMS_NAMED {
                    self.programs.0.push((shown, kind));
                }
                self.programs.1 += 1;
            }
            FileOut::TooBig => self.too_big += 1,
            FileOut::Unreadable => self.unreadable += 1,
            FileOut::Hits { shown, total, chunks } => {
                self.total += total;
                if files_only {
                    self.hit_files += 1;
                    if self.items.len() < max {
                        self.items.push(shown);
                    }
                    return;
                }
                let take = chunks.len().min(max.saturating_sub(self.shown_total)).min(max_per_file);
                for c in chunks.into_iter().take(take) {
                    self.items.extend(c);
                }
                self.shown_total += take;
                let file_left = total - take;
                if file_left > 0 {
                    self.left += file_left;
                    self.left_files += 1;
                    // a file past `max_results` shows nothing, so there is nothing to put the note under
                    if take > 0 {
                        self.items.push(format!("… {file_left} more in this file"));
                    }
                }
            }
        }
    }
}

/// True when the pattern names a carriage return itself (`\r`, a hex or unicode escape, or the raw character).
/// In CRLF mode `$` is false between the `\r` and the `\n`, so a pattern that already spells out the `\r` before its
/// `$` (like `foo\r$`) would stop matching; those patterns keep the plain line-feed semantics they were written for.
fn names_carriage_return(pattern: &str) -> bool {
    pattern.contains('\r')
        || pattern.contains("\\r")
        || pattern.contains("\\x")
        || pattern.contains("\\u")
        || pattern.contains("\\U")
}

/// The compiled programs a search names in its line about them.
const PROGRAMS_NAMED: usize = 3;

/// The call that lists the strings of `file` that hold `pattern`; without the pattern when it cannot be written
/// into a call, and None when the name cannot.
fn strings_call(file: &str, pattern: &str, tool: bool) -> Option<String> {
    let file = slash(file);
    if !callable(&file) {
        return None;
    }
    let file = quoted(&file);
    Some(match callable(pattern) {
        true => bin_call(&format!("strings {file} --grep {}", quoted(pattern)), tool),
        false => bin_call(&format!("strings {file}"), tool),
    })
}

/// One line on the compiled programs whose bytes hold a match: what the first is and the call that lists its
/// matching strings.
fn programs_line(
    programs: &(Vec<(String, nexo_binmagic::Compiled)>, usize),
    pattern: &str,
    tool: bool,
) -> Option<String> {
    let (named, all) = programs;
    let (first, kind) = named.first()?;
    let call = strings_call(first, pattern, tool)?;
    if *all == 1 {
        return Some(format!("[{first} holds a match and is not text: {}; {call} lists it]", kind.facts(None)));
    }
    let names: Vec<&str> = named.iter().map(|(f, _)| f.as_str()).collect();
    let more = all - names.len();
    let rest = if more > 0 { format!(" and {more} more") } else { String::new() };
    Some(format!(
        "[{all} compiled programs hold a match and are not text: {}{rest}; {call} lists it in {first} ({})]",
        names.join(", "),
        kind.facts(None)
    ))
}

/// What one walker thread found, handed in when the thread is done.
struct Hand<'a> {
    into: &'a Mutex<(Vec<PathBuf>, usize)>,
    files: Vec<PathBuf>,
    kept_out: usize,
}

impl Drop for Hand<'_> {
    fn drop(&mut self) {
        if let Ok(mut all) = self.into.lock() {
            all.0.append(&mut self.files);
            all.1 += self.kept_out;
        }
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
        assert!(out.content.contains("1 more match in 1 file (2 in all)"), "{}", out.content);
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
    async fn multiline_matches_across_lines_and_a_failed_newline_pattern_says_why() {
        let (_d, c) = fixture();
        for pat in ["alpha\\(\\) \\{\\}\\nlet", "alpha\\(\\) \\{\\}\nlet"] {
            let out = Grep.run(json!({"pattern": pat}), &c).await;
            assert!(out.content.contains("no match") && out.content.contains("multiline: true"), "{}", out.content);
        }
        let out = Grep.run(json!({"pattern": "zzz"}), &c).await;
        assert_eq!(out.content, "grep: no match for `zzz` in .");
        let out = Grep.run(json!({"pattern": "alpha\\(\\) \\{\\}\\nlet x", "multiline": true}), &c).await;
        assert_eq!(out.content, "src/a.rs:1: fn alpha() {}\nsrc/a.rs:2: let x = 1;");
        let out = Grep.run(json!({"pattern": "fn alpha.*beta", "multiline": true, "path": "src"}), &c).await;
        assert_eq!(out.content, "src/a.rs:1: fn alpha() {}\nsrc/a.rs:2: let x = 1;\nsrc/a.rs:3: fn beta() {}");
        let out = Grep.run(json!({"pattern": "^let", "multiline": true, "files_only": true}), &c).await;
        assert_eq!(out.content, "1 match in 1 file\nsrc/a.rs");
    }

    #[tokio::test]
    async fn multiline_anchors_match_lines_that_end_in_crlf() {
        let (d, c) = fixture();
        std::fs::write(d.path().join("win.txt"), "alpha\r\nfoo\r\nbar\r\n").unwrap();
        let out = Grep.run(json!({"pattern": "^foo$", "multiline": true, "glob": "win.txt"}), &c).await;
        assert_eq!(out.content, "win.txt:2: foo", "{}", out.content);
    }

    #[tokio::test]
    async fn patterns_that_spell_out_the_carriage_return_still_match_before_the_line_end() {
        let (d, c) = fixture();
        std::fs::write(d.path().join("win.txt"), "alpha\r\nfoo\r\nbar\r\n").unwrap();
        for pattern in ["foo\\r$", "foo\\x0d$", "foo\\r\\n"] {
            let out = Grep.run(json!({"pattern": pattern, "multiline": true, "glob": "win.txt"}), &c).await;
            assert_eq!(out.content, "win.txt:2: foo", "{pattern}: {}", out.content);
        }
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

    /// Three files with 30, 25 and 2 matching lines (`hit 0` ...), so one file can crowd out the others.
    fn crowded() -> (tempfile::TempDir, ToolContext) {
        let d = tempfile::tempdir().unwrap();
        for (name, n) in [("a.txt", 30), ("b.txt", 25), ("c.txt", 2)] {
            let body: String = (0..n).map(|i| format!("hit {i}\n")).collect();
            std::fs::write(d.path().join(name), body).unwrap();
        }
        let c = ToolContext::new(d.path().to_path_buf());
        (d, c)
    }

    #[tokio::test]
    async fn one_file_shows_at_most_twenty_matches_and_says_how_many_it_left() {
        let (_d, c) = crowded();
        let out = Grep.run(json!({"pattern": "hit"}), &c).await;
        let lines: Vec<&str> = out.content.lines().collect();
        assert_eq!(lines.iter().filter(|l| l.starts_with("a.txt:")).count(), 20, "{}", out.content);
        assert_eq!(lines.iter().filter(|l| l.starts_with("b.txt:")).count(), 20, "{}", out.content);
        assert_eq!(lines.iter().filter(|l| l.starts_with("c.txt:")).count(), 2, "{}", out.content);
        let a_note = lines.iter().position(|l| *l == "… 10 more in this file").unwrap();
        assert_eq!(lines[a_note - 1], "a.txt:20: hit 19", "the note sits under its own file");
        let b_note = lines.iter().position(|l| *l == "… 5 more in this file").unwrap();
        assert_eq!(lines[b_note - 1], "b.txt:20: hit 19");
        assert_eq!(
            *lines.last().unwrap(),
            "… 15 more matches in 2 files (57 in all); narrow the pattern or the path",
            "{}",
            out.content
        );
    }

    #[tokio::test]
    async fn max_per_file_raises_or_lowers_the_cap_per_file() {
        let (_d, c) = crowded();
        let out = Grep.run(json!({"pattern": "hit", "max_per_file": 30}), &c).await;
        assert_eq!(out.content.lines().filter(|l| l.starts_with("a.txt:")).count(), 30, "{}", out.content);
        assert!(!out.content.contains("more"), "{}", out.content);
        let out = Grep.run(json!({"pattern": "hit", "max_per_file": 100}), &c).await;
        assert!(!out.content.contains("more"), "nothing is left when the cap is above every file: {}", out.content);
        assert_eq!(out.content.lines().count(), 57);
        let out = Grep.run(json!({"pattern": "hit", "max_per_file": 1}), &c).await;
        assert_eq!(
            out.content,
            "a.txt:1: hit 0\n… 29 more in this file\nb.txt:1: hit 0\n… 24 more in this file\nc.txt:1: hit 0\n… 1 more in this file\n… 54 more matches in 3 files (57 in all); narrow the pattern or the path"
        );
    }

    #[tokio::test]
    async fn max_results_counts_the_files_it_left_without_a_note_under_a_file_it_did_not_show() {
        let (_d, c) = crowded();
        let out = Grep.run(json!({"pattern": "hit", "max_results": 22}), &c).await;
        let lines: Vec<&str> = out.content.lines().collect();
        assert_eq!(lines.iter().filter(|l| l.starts_with("a.txt:")).count(), 20, "{}", out.content);
        assert_eq!(lines.iter().filter(|l| l.starts_with("b.txt:")).count(), 2, "{}", out.content);
        assert!(!lines.iter().any(|l| l.starts_with("c.txt:")), "{}", out.content);
        assert_eq!(
            *lines.last().unwrap(),
            "… 35 more matches in 3 files (57 in all); narrow the pattern or the path",
            "{}",
            out.content
        );
        assert!(
            lines.contains(&"… 10 more in this file") && lines.contains(&"… 23 more in this file"),
            "{}",
            out.content
        );
    }

    #[tokio::test]
    async fn files_only_ignores_the_per_file_cap() {
        let (_d, c) = crowded();
        let out = Grep.run(json!({"pattern": "hit", "files_only": true, "max_per_file": 1}), &c).await;
        assert_eq!(out.content, "57 matches in 3 files\na.txt\nb.txt\nc.txt");
    }

    #[tokio::test]
    async fn files_searched_together_come_back_in_path_order_every_time() {
        let d = tempfile::tempdir().unwrap();
        // more files than one batch, in folders, written in the opposite order of their names
        for i in (0..300).rev() {
            let dir = d.path().join(format!("d{}", i % 7));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("f{i:03}.txt")), format!("needle {i}\nhay\n")).unwrap();
        }
        let c = ToolContext::new(d.path().to_path_buf());
        let first = Grep.run(json!({"pattern": "needle", "max_results": 300}), &c).await.content;
        let paths: Vec<&str> = first.lines().map(|l| l.split(':').next().unwrap()).collect();
        assert_eq!(paths.len(), 300, "{first}");
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted, "the answer is in path order");
        for _ in 0..3 {
            assert_eq!(Grep.run(json!({"pattern": "needle", "max_results": 300}), &c).await.content, first);
        }
    }

    #[tokio::test]
    async fn counting_stops_between_batches_once_enough_is_left_and_the_answer_says_so() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..300 {
            let body: String = (0..30).map(|j| format!("hit {j}\n")).collect();
            std::fs::write(d.path().join(format!("f{i:03}.txt")), body).unwrap();
        }
        let c = ToolContext::new(d.path().to_path_buf());
        let out = Grep.run(json!({"pattern": "hit"}), &c).await.content;
        // 10 files show 20 matches each; the first batch (128 files) holds 3840 matches, 3640 of them left
        assert_eq!(
            out.lines().last().unwrap(),
            "… at least 3640 more matches in at least 128 files (at least 3840 in all; counting stopped after 128 of 300 files); narrow the pattern or the path",
            "{out}"
        );
        assert_eq!(out.lines().filter(|l| l.contains(":")).count(), 200, "{out}");
        assert_eq!(Grep.run(json!({"pattern": "hit"}), &c).await.content, out, "the same on every run");
        // files_only counts every file
        let out = Grep.run(json!({"pattern": "hit", "files_only": true, "max_results": 1}), &c).await.content;
        assert!(out.starts_with("9000 matches in 300 files\n"), "{out}");
    }

    #[tokio::test]
    async fn a_compiled_program_that_holds_the_pattern_is_named_with_the_call_that_searches_it() {
        let (d, mut c) = fixture();
        std::fs::write(d.path().join("prog"), nexo_bin::fixtures::elf64(false).bytes).unwrap();
        // a name of its symbol table
        let pat = "helper";
        let out = Grep.run(json!({"pattern": pat}), &c).await;
        let what = nexo_binmagic::kind_at(&d.path().join("prog")).unwrap().facts(None);
        assert_eq!(
            out.content,
            format!("grep: no match for `{pat}` in .\n[prog holds a match and is not text: {what}; nexo bin strings prog --grep {pat} lists it]")
        );
        c.tool_names = std::sync::Arc::new(vec!["bin".into()]);
        let out = Grep.run(json!({"pattern": pat, "path": "prog"}), &c).await;
        assert!(
            out.content.ends_with(&format!("; bin {{\"command\": \"strings prog --grep {pat}\"}} lists it]")),
            "{}",
            out.content
        );
        // a pattern the program does not hold, and binary data that is no program, add nothing
        let out = Grep.run(json!({"pattern": "zq_not_in_any_file_zq"}), &c).await;
        assert_eq!(out.content, "grep: no match for `zq_not_in_any_file_zq` in .");
        let out = Grep.run(json!({"pattern": "alpha"}), &c).await;
        assert!(!out.content.contains("not text"), "{}", out.content);
    }
}
