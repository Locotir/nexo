//! The data of the diff viewer: a small unified-diff parser, the git calls that feed it and the
//! session's own edits for a folder that is no repository. Nothing here draws.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use similar::TextDiff;

/// Files over this many bytes are listed but not read (untracked ones).
pub const MAX_UNTRACKED_BYTES: u64 = 1_000_000;
/// The most untracked files read; a build folder that is not ignored must not stall the view.
pub const MAX_UNTRACKED_FILES: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Area {
    Staged,
    Unstaged,
    Untracked,
    /// An edit made in this session, outside any repository.
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
}

impl Status {
    pub fn letter(self) -> char {
        match self {
            Status::Modified => 'M',
            Status::Added => 'A',
            Status::Deleted => 'D',
            Status::Renamed => 'R',
            Status::Untracked => '?',
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Context,
    Add,
    Del,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DLine {
    pub kind: Kind,
    pub text: String,
    /// Zero-based line numbers in the old and the new file.
    pub old: Option<usize>,
    pub new: Option<usize>,
    /// "\ No newline at end of file" followed this line.
    pub no_newline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// The `@@ -a,b +c,d @@ section` line.
    pub header: String,
    pub lines: Vec<DLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub status: Status,
    pub area: Area,
    pub binary: bool,
    /// Why there is no body: binary, too large, mode change.
    pub note: Option<String>,
    pub added: usize,
    pub removed: usize,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    pub fn new(path: &str, area: Area) -> FileDiff {
        FileDiff {
            path: path.to_string(),
            old_path: None,
            status: Status::Modified,
            area,
            binary: false,
            note: None,
            added: 0,
            removed: 0,
            hunks: Vec::new(),
        }
    }

    fn count(&mut self) {
        self.added = self.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == Kind::Add).count();
        self.removed = self.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == Kind::Del).count();
    }

    pub fn line_count(&self) -> usize {
        self.hunks.iter().map(|h| h.lines.len()).sum()
    }

    /// The file's diff as `git diff` would print it (for the clipboard).
    pub fn as_patch(&self) -> String {
        let old = self.old_path.as_deref().unwrap_or(&self.path);
        let mut s = format!("diff --git a/{old} b/{}\n", self.path);
        if self.hunks.is_empty() {
            if let Some(n) = &self.note {
                s.push_str(&format!("# {n}\n"));
            }
            return s;
        }
        s.push_str(&format!("--- a/{old}\n+++ b/{}\n", self.path));
        for h in &self.hunks {
            s.push_str(&h.header);
            s.push('\n');
            for l in &h.lines {
                s.push(match l.kind {
                    Kind::Context => ' ',
                    Kind::Add => '+',
                    Kind::Del => '-',
                });
                s.push_str(&l.text);
                s.push('\n');
                if l.no_newline {
                    s.push_str("\\ No newline at end of file\n");
                }
            }
        }
        s
    }
}

/// What a collection brings back.
#[derive(Debug, Clone, Default)]
pub struct Collected {
    pub files: Vec<FileDiff>,
    /// A line shown under the header (the folder is no repository, a list was cut).
    pub note: Option<String>,
    /// A short reason when nothing could be read.
    pub error: Option<String>,
    /// The folder the paths are relative to.
    pub root: PathBuf,
}

fn unquote(p: &str) -> String {
    let p = p.trim_end_matches(['\t', '\r']);
    p.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(p).to_string()
}

fn strip_side(p: &str) -> Option<String> {
    let p = unquote(p);
    if p == "/dev/null" {
        return None;
    }
    Some(p.strip_prefix("a/").or_else(|| p.strip_prefix("b/")).unwrap_or(&p).to_string())
}

/// The path of a `diff --git a/x b/y` line (the new side).
fn header_path(rest: &str) -> String {
    let split = rest.rfind(" \"b/").or_else(|| rest.rfind(" b/")).map(|i| i + 1);
    match split {
        Some(i) => strip_side(&rest[i..]).unwrap_or_default(),
        None => rest.to_string(),
    }
}

/// `@@ -a,b +c,d @@`: the first lines and the counts of both sides.
fn parse_hunk_header(h: &str) -> Option<(usize, usize, usize, usize)> {
    let inner = h.strip_prefix("@@ ")?;
    let end = inner.find(" @@")?;
    let mut parts = inner[..end].split(' ');
    let side = |s: Option<&str>, sign: char| -> Option<(usize, usize)> {
        let s = s?.strip_prefix(sign)?;
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (a, b) = side(parts.next(), '-')?;
    let (c, d) = side(parts.next(), '+')?;
    Some((a, b, c, d))
}

fn close(cur: &mut Option<FileDiff>, hunk: &mut Option<Hunk>) {
    if let (Some(f), Some(h)) = (cur.as_mut(), hunk.take()) {
        f.hunks.push(h);
    }
}

/// Parses the output of `git diff` (no colour) into files. Unknown lines are ignored.
pub fn parse_unified(text: &str, area: Area) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut cur: Option<FileDiff> = None;
    let mut hunk: Option<Hunk> = None;
    // the next line number of each side (zero-based) and how many lines the hunk still owes
    let (mut o, mut n, mut o_left, mut n_left) = (0usize, 0usize, 0usize, 0usize);
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        // inside a hunk the counts say how many lines are body: "--- x" can be a removed line
        if let Some(h) = hunk.as_mut() {
            if line.starts_with('\\') {
                if let Some(last) = h.lines.last_mut() {
                    last.no_newline = true;
                }
                continue;
            }
            let body_line = matches!(line.chars().next(), Some('+' | '-' | ' ') | None);
            if (o_left > 0 || n_left > 0) && body_line {
                let (kind, body) = match line.chars().next() {
                    Some('+') => (Kind::Add, &line[1..]),
                    Some('-') => (Kind::Del, &line[1..]),
                    Some(' ') => (Kind::Context, &line[1..]),
                    _ => (Kind::Context, ""),
                };
                let (old, new) = match kind {
                    Kind::Add => {
                        n += 1;
                        n_left = n_left.saturating_sub(1);
                        (None, Some(n - 1))
                    }
                    Kind::Del => {
                        o += 1;
                        o_left = o_left.saturating_sub(1);
                        (Some(o - 1), None)
                    }
                    Kind::Context => {
                        o += 1;
                        n += 1;
                        o_left = o_left.saturating_sub(1);
                        n_left = n_left.saturating_sub(1);
                        (Some(o - 1), Some(n - 1))
                    }
                };
                h.lines.push(DLine { kind, text: body.to_string(), old, new, no_newline: false });
                continue;
            }
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            close(&mut cur, &mut hunk);
            if let Some(mut f) = cur.take() {
                f.count();
                files.push(f);
            }
            cur = Some(FileDiff::new(&header_path(rest), area));
            continue;
        }
        let Some(f) = cur.as_mut() else { continue };
        if line.starts_with("@@ ") {
            close(&mut cur, &mut hunk);
            if let Some((a, b, c, d)) = parse_hunk_header(line) {
                // git numbers an empty side "one before" its place: -0,0 means nothing precedes
                o = if b == 0 { a } else { a.saturating_sub(1) };
                n = if d == 0 { c } else { c.saturating_sub(1) };
                (o_left, n_left) = (b, d);
                hunk = Some(Hunk { header: line.to_string(), lines: Vec::new() });
            }
            continue;
        }
        if line.starts_with("new file mode") {
            f.status = Status::Added;
        } else if line.starts_with("deleted file mode") {
            f.status = Status::Deleted;
        } else if let Some(p) = line.strip_prefix("rename from ") {
            f.status = Status::Renamed;
            f.old_path = Some(unquote(p));
        } else if let Some(p) = line.strip_prefix("rename to ") {
            f.path = unquote(p);
        } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            f.binary = true;
            f.note = Some("binary file".to_string());
        } else if line.starts_with("old mode") || line.starts_with("new mode") {
            f.note.get_or_insert_with(|| "mode changed".to_string());
        } else if let Some(p) = line.strip_prefix("--- ") {
            if let Some(p) = strip_side(p) {
                if f.status == Status::Deleted {
                    f.path = p;
                }
            }
        } else if let Some(p) = line.strip_prefix("+++ ") {
            if let Some(p) = strip_side(p) {
                f.path = p;
            }
        }
    }
    close(&mut cur, &mut hunk);
    if let Some(mut f) = cur.take() {
        f.count();
        files.push(f);
    }
    for f in &mut files {
        if f.status == Status::Renamed && f.hunks.is_empty() && f.note.is_none() {
            f.note = Some("renamed, no content change".to_string());
        }
    }
    files
}

/// Hunks (three lines of context) between two texts, the numbers starting at `start_line`.
pub fn hunks_between(old: &str, new: &str, start_line: usize) -> Vec<Hunk> {
    let skip = start_line.saturating_sub(1);
    let diff = TextDiff::from_lines(old, new);
    let mut out = Vec::new();
    for group in diff.grouped_ops(3) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else { continue };
        let (a0, b0) = (first.old_range().start, first.new_range().start);
        let (la, lb) = (last.old_range().end - a0, last.new_range().end - b0);
        let mut lines = Vec::new();
        for op in &group {
            for ch in diff.iter_changes(op) {
                let kind = match ch.tag() {
                    similar::ChangeTag::Delete => Kind::Del,
                    similar::ChangeTag::Insert => Kind::Add,
                    similar::ChangeTag::Equal => Kind::Context,
                };
                lines.push(DLine {
                    kind,
                    text: ch.value().trim_end_matches('\n').to_string(),
                    old: ch.old_index().map(|i| i + skip),
                    new: ch.new_index().map(|i| i + skip),
                    no_newline: false,
                });
            }
        }
        out.push(Hunk { header: format!("@@ -{},{la} +{},{lb} @@", a0 + 1 + skip, b0 + 1 + skip), lines });
    }
    out
}

/// The files the session edited, from the diffs of its edit cards: `(path, old, new, start_line)`.
///
/// Edits of one region (same path and start, or a new text that the next edit takes as its old)
/// collapse to first-old versus last-new; a region edited back to its first text is dropped.
pub fn session_files(edits: &[(String, String, String, usize)]) -> Vec<FileDiff> {
    // (path, first old, last new, start)
    let mut regions: Vec<(String, String, String, usize)> = Vec::new();
    for (path, old, new, start) in edits {
        let chained = regions.iter().position(|r| &r.0 == path && (r.3 == *start || (!r.2.is_empty() && &r.2 == old)));
        match chained {
            Some(i) => regions[i].2 = new.clone(),
            None => regions.push((path.clone(), old.clone(), new.clone(), *start)),
        }
    }
    regions.retain(|r| r.1 != r.2);
    regions.sort_by_key(|r| r.3);
    let mut files: Vec<FileDiff> = Vec::new();
    for (path, old, new, start) in &regions {
        let hunks = hunks_between(old, new, *start);
        if hunks.is_empty() {
            continue;
        }
        let pos = files.iter().position(|f| &f.path == path).unwrap_or_else(|| {
            let mut f = FileDiff::new(path, Area::Session);
            if old.is_empty() {
                f.status = Status::Added;
            }
            files.push(f);
            files.len() - 1
        });
        let f = &mut files[pos];
        f.hunks.extend(hunks);
        f.count();
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// A whole new file as one hunk of added lines.
pub fn all_added(path: &str, text: &str, area: Area, status: Status) -> FileDiff {
    let mut f = FileDiff::new(path, area);
    f.status = status;
    let lines: Vec<DLine> = text
        .split_terminator('\n')
        .enumerate()
        .map(|(i, l)| DLine {
            kind: Kind::Add,
            text: l.strip_suffix('\r').unwrap_or(l).to_string(),
            old: None,
            new: Some(i),
            no_newline: false,
        })
        .collect();
    if !lines.is_empty() {
        f.hunks.push(Hunk { header: format!("@@ -0,0 +1,{} @@", lines.len()), lines });
    }
    f.count();
    f
}

/// The longest a single git call may run before it is killed.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Kills the child when dropped, so a cancelled or timed-out collection never leaves git behind.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    git_timed(root, args, GIT_TIMEOUT)
}

fn git_timed(root: &Path, args: &[&str], timeout: Duration) -> Result<Vec<u8>, String> {
    let child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotepath=false", "--no-pager"])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "git is not available".to_string())?;
    let mut guard = KillOnDrop(child);
    let (Some(mut so), Some(mut se)) = (guard.0.stdout.take(), guard.0.stderr.take()) else {
        return Err("git failed".to_string());
    };
    let out_t = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = so.read_to_end(&mut v);
        v
    });
    let err_t = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = se.read_to_end(&mut v);
        v
    });
    let started = Instant::now();
    let status = loop {
        match guard.0.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if started.elapsed() >= timeout => {
                drop(guard);
                return Err(format!("git timed out after {} s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => return Err("git failed".to_string()),
        }
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    if status.success() {
        Ok(stdout)
    } else {
        let e = String::from_utf8_lossy(&stderr);
        Err(e.lines().next().unwrap_or("git failed").chars().take(120).collect())
    }
}

fn diff_text(root: &Path, cached: bool) -> Result<String, String> {
    let mut a = vec![
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--ignore-submodules=all",
        "-M",
        "--src-prefix=a/",
        "--dst-prefix=b/",
    ];
    if cached {
        a.push("--cached");
    }
    Ok(String::from_utf8_lossy(&git(root, &a)?).into_owned())
}

/// Reads staged, unstaged and untracked changes of the repository around `cwd`; outside one,
/// `session` (this session's edits) with a note. Meant for a thread of its own.
pub fn collect(cwd: &Path, session: Vec<FileDiff>) -> Collected {
    let top = match git(cwd, &["rev-parse", "--show-toplevel"]) {
        Ok(t) => PathBuf::from(String::from_utf8_lossy(&t).trim()),
        Err(_) => {
            return Collected {
                files: session,
                note: Some("not a git repo: showing this session's edits".to_string()),
                error: None,
                root: cwd.to_path_buf(),
            };
        }
    };
    let mut files = Vec::new();
    let mut problem = None;
    for (cached, area) in [(true, Area::Staged), (false, Area::Unstaged)] {
        match diff_text(&top, cached) {
            Ok(t) => files.extend(parse_unified(&t, area)),
            Err(e) => problem = Some(e),
        }
    }
    match git(&top, &["ls-files", "--others", "--exclude-standard", "-z"]) {
        Ok(raw) => {
            let paths: Vec<&[u8]> = raw.split(|b| *b == 0).filter(|p| !p.is_empty()).collect();
            for p in paths.iter().take(MAX_UNTRACKED_FILES) {
                files.push(read_untracked(&top, &String::from_utf8_lossy(p)));
            }
            if paths.len() > MAX_UNTRACKED_FILES {
                problem = Some(format!("{} untracked files, the first {MAX_UNTRACKED_FILES} shown", paths.len()));
            }
        }
        Err(e) => problem = Some(e),
    }
    if files.is_empty() {
        Collected { files, note: None, error: problem, root: top }
    } else {
        Collected { files, note: problem, error: None, root: top }
    }
}

fn read_untracked(root: &Path, path: &str) -> FileDiff {
    let full = root.join(path);
    let mut f = FileDiff::new(path, Area::Untracked);
    f.status = Status::Untracked;
    let Ok(meta) = std::fs::metadata(&full) else {
        f.note = Some("unreadable".to_string());
        return f;
    };
    if !meta.is_file() {
        f.note = Some("not a regular file".to_string());
        return f;
    }
    if meta.len() > MAX_UNTRACKED_BYTES {
        f.note = Some(format!("too large to show ({} KB)", meta.len() / 1000));
        return f;
    }
    match std::fs::read(&full) {
        Ok(bytes) if bytes.iter().take(8000).any(|b| *b == 0) => {
            f.binary = true;
            f.note = Some("binary file".to_string());
            f
        }
        Ok(bytes) => all_added(path, &String::from_utf8_lossy(&bytes), Area::Untracked, Status::Untracked),
        Err(_) => {
            f.note = Some("unreadable".to_string());
            f
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 111..222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,4 +1,5 @@ fn main
 one
-two
+TWO
+two and a half
 three
 four
@@ -20,3 +21,3 @@
 x
--- not a header
+++ also body
 y
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..333
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
\\ No newline at end of file
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 444..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/old name.md b/new name.md
similarity index 90%
rename from old name.md
rename to new name.md
index 555..666 100644
--- a/old name.md
+++ b/new name.md
@@ -1 +1 @@
-a
+b
diff --git a/pic.png b/pic.png
index 777..888 100644
Binary files a/pic.png and b/pic.png differ
diff --git a/same.md b/moved.md
similarity index 100%
rename from same.md
rename to moved.md
";

    #[test]
    fn parses_hunks_counts_and_numbers() {
        let f = parse_unified(SAMPLE, Area::Unstaged);
        assert_eq!(f.len(), 6);
        let a = &f[0];
        assert_eq!((a.path.as_str(), a.status, a.hunks.len()), ("src/a.rs", Status::Modified, 2));
        assert_eq!((a.added, a.removed), (3, 2));
        let l = &a.hunks[0].lines;
        assert_eq!((l[0].old, l[0].new), (Some(0), Some(0)));
        assert_eq!((l[1].kind, l[1].old, l[1].new), (Kind::Del, Some(1), None));
        assert_eq!((l[2].kind, l[2].old, l[2].new), (Kind::Add, None, Some(1)));
        assert_eq!((l[4].old, l[4].new), (Some(2), Some(3)));
        assert_eq!(a.hunks[0].header, "@@ -1,4 +1,5 @@ fn main");
    }

    #[test]
    fn a_removed_line_that_looks_like_a_header_stays_body() {
        let f = parse_unified(SAMPLE, Area::Unstaged);
        let h = &f[0].hunks[1];
        assert_eq!(h.lines.len(), 4);
        assert_eq!((h.lines[1].kind, h.lines[1].text.as_str()), (Kind::Del, "-- not a header"));
        assert_eq!((h.lines[2].kind, h.lines[2].text.as_str()), (Kind::Add, "++ also body"));
        assert_eq!(h.lines[0].old, Some(19));
        assert_eq!(h.lines[0].new, Some(20));
    }

    #[test]
    fn new_deleted_and_no_newline() {
        let f = parse_unified(SAMPLE, Area::Staged);
        assert_eq!((f[1].path.as_str(), f[1].status, f[1].added), ("new.txt", Status::Added, 2));
        assert!(f[1].hunks[0].lines[1].no_newline);
        assert!(!f[1].hunks[0].lines[0].no_newline);
        assert_eq!(f[1].hunks[0].lines[0].new, Some(0));
        assert_eq!((f[2].path.as_str(), f[2].status, f[2].removed), ("gone.txt", Status::Deleted, 1));
        assert!(f.iter().all(|x| x.area == Area::Staged));
    }

    #[test]
    fn renames_and_binary() {
        let f = parse_unified(SAMPLE, Area::Unstaged);
        assert_eq!(f[3].status, Status::Renamed);
        assert_eq!((f[3].path.as_str(), f[3].old_path.as_deref()), ("new name.md", Some("old name.md")));
        assert_eq!((f[3].added, f[3].removed), (1, 1));
        assert!(f[4].binary && f[4].hunks.is_empty());
        assert_eq!(f[4].note.as_deref(), Some("binary file"));
        assert_eq!((f[5].path.as_str(), f[5].status), ("moved.md", Status::Renamed));
        assert_eq!(f[5].note.as_deref(), Some("renamed, no content change"));
    }

    #[test]
    fn empty_and_garbage_input() {
        assert!(parse_unified("", Area::Staged).is_empty());
        assert!(parse_unified("nothing to see\n@@ -1 +1 @@\n+x\n", Area::Staged).is_empty());
    }

    #[test]
    fn patch_round_trips() {
        let f = parse_unified(SAMPLE, Area::Unstaged);
        let again = parse_unified(&f[0].as_patch(), Area::Unstaged);
        assert_eq!(again[0].hunks, f[0].hunks);
    }

    #[test]
    fn session_edits_make_hunks() {
        let f = session_files(&[
            ("a.rs".into(), "a\nb\n".into(), "a\nc\n".into(), 1),
            ("n.rs".into(), String::new(), "x\n".into(), 1),
        ]);
        assert_eq!(f.len(), 2);
        assert_eq!((f[0].added, f[0].removed, f[0].area), (1, 1, Area::Session));
        assert_eq!(f[1].status, Status::Added);
    }

    fn sh(dir: &Path, args: &[&str]) {
        let st = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(st.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&st.stderr));
    }

    #[test]
    fn collects_staged_unstaged_and_untracked_from_a_real_repo() {
        // the clipboard tests swap PATH for a moment: git must not be looked for then
        let _env = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        sh(d, &["init", "-q"]);
        sh(d, &["config", "user.email", "t@example.com"]);
        sh(d, &["config", "user.name", "t"]);
        std::fs::write(d.join("a.txt"), "1\n2\n3\n").unwrap();
        std::fs::write(d.join("b.txt"), "x\ny\n").unwrap();
        sh(d, &["add", "."]);
        sh(d, &["-c", "core.hooksPath=/dev/null", "commit", "-q", "-m", "first"]);
        std::fs::write(d.join("a.txt"), "1\ntwo\n3\n").unwrap();
        sh(d, &["add", "a.txt"]);
        std::fs::write(d.join("b.txt"), "x\ny\nz\n").unwrap();
        std::fs::write(d.join("c.txt"), "new\nfile\n").unwrap();
        std::fs::write(d.join("blob.bin"), [0u8, 1, 2, 3]).unwrap();
        let c = collect(d, Vec::new());
        assert!(c.error.is_none() && c.note.is_none(), "{:?} {:?}", c.error, c.note);
        let by = |p: &str, a: Area| {
            c.files.iter().find(|f| f.path == p && f.area == a).unwrap_or_else(|| panic!("{p} {a:?}"))
        };
        let a = by("a.txt", Area::Staged);
        assert_eq!((a.added, a.removed), (1, 1));
        assert_eq!(by("b.txt", Area::Unstaged).added, 1);
        let n = by("c.txt", Area::Untracked);
        assert_eq!((n.status, n.added), (Status::Untracked, 2));
        assert!(by("blob.bin", Area::Untracked).binary);
        assert_eq!(c.files.len(), 4);
    }

    #[test]
    fn outside_a_repo_the_session_edits_are_shown() {
        // git is looked up on PATH, which the clipboard tests point elsewhere while they hold this lock
        let _env = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let t = tempfile::tempdir().unwrap();
        let s = session_files(&[("a.rs".into(), "a\n".into(), "b\n".into(), 1)]);
        let c = collect(t.path(), s);
        assert_eq!(c.files.len(), 1);
        assert_eq!(c.note.as_deref(), Some("not a git repo: showing this session's edits"));
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;

    #[test]
    fn session_keeps_first_old_against_last_new_and_drops_reverts() {
        let f = session_files(&[
            ("a.rs".into(), "a\nb\n".into(), "a\nc\n".into(), 1),
            ("a.rs".into(), "a\nc\n".into(), "a\nd\n".into(), 1),
            ("r.rs".into(), "x\n".into(), "y\n".into(), 1),
            ("r.rs".into(), "y\n".into(), "x\n".into(), 1),
        ]);
        assert_eq!(f.len(), 1, "{:?}", f.iter().map(|f| &f.path).collect::<Vec<_>>());
        assert_eq!((f[0].path.as_str(), f[0].added, f[0].removed), ("a.rs", 1, 1));
        let text: Vec<&str> = f[0].hunks[0].lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(text, ["a", "b", "d"]);
    }

    #[test]
    fn a_slow_git_is_killed_after_the_timeout() {
        let _env = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let t = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let r = git_timed(t.path(), &["--exec-path"], Duration::from_millis(0));
        assert!(r.as_ref().map_or_else(|e| e.starts_with("git timed out"), |_| true), "{r:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(git_timed(t.path(), &["--version"], Duration::from_secs(10)).is_ok());
    }
}
