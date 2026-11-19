//! Part of the agent loop; see the parent module.
//!
//! What a request names. The words of a message that are paths, file names or identifiers are looked up on disk
//! (the project, the folders the message names, the top level of the home folder), and what they find reaches the
//! model as data: a small file whole, a large or ambiguous one as its path, size and head, a folder as its listing,
//! an identifier as the line that defines it. The same lookup says whether the message is about code at all, which
//! is what lets the project's check run at the start. Nothing here knows a language, a task or a benchmark: only
//! words, paths, sizes and counts.
//!
//! [`find`] touches the file system only and holds no lock; [`admit`] applies the conversation's state (what the
//! model holds already, the read policy, the budget) and runs under the lock.

use super::*;

use nexo_tools::postcheck::is_source;

/// The largest file a request attaches by naming it.
pub(super) const NAMED_FILE_BYTES: u64 = 24_000;
/// The most files one request attaches whole by naming them (what defines a named identifier counts too).
pub(super) const NAMED_FILES: usize = 3;
/// How long the walks and the definition search of one request may take.
pub(super) const FIND_TIME: Duration = Duration::from_millis(250);
/// How long [`find`] may take for a message that joins a running turn (it walks nothing).
pub(super) const DIRECT_TIME: Duration = Duration::from_millis(50);

/// The path and file-name words resolved per message.
const WORDS: usize = 6;
/// The identifiers looked up per message.
const IDENTS: usize = 4;
/// The matches kept per file name.
const MATCHES: usize = 5;
/// The entries of a folder listing, and its characters.
const FOLDER_ENTRIES: usize = 40;
const FOLDER_CHARS: usize = 1_500;
/// What a walk under a named folder looks at: entries and depth.
const NAMED_WALK: usize = 5_000;
const NAMED_DEPTH: usize = 3;
/// The head of a file: lines and bytes; the lines of a file are counted up to `COUNT_BYTES`.
const HEAD_LINES: usize = 20;
const HEAD_BYTES: usize = 1_200;
const COUNT_BYTES: u64 = 1_000_000;
/// The definition search: files read, bytes read in all, definitions kept per word.
const DEF_FILE_BYTES: u64 = 256_000;
const DEF_READ_BYTES: u64 = 32_000_000;
const DEFS_PER_WORD: usize = 3;
/// The excerpt at a definition: lines and bytes.
const EXCERPT_LINES: usize = 40;
const EXCERPT_BYTES: usize = 2_000;
/// All the lines and heads of one message, in characters.
const LINES_CHARS: usize = 6_000;
/// What a source count says when it was stopped or the project has more.
const SOURCES_PAST: usize = 201;
/// The deepest a project folder is looked at for a folder named by an identifier.
const IDENT_DEPTH: usize = 3;
/// The most entries one listing reads.
const LISTING_READ: usize = 20_000;

/// How far a lookup goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Reach {
    /// Walks, folder listings and the definition search.
    Search,
    /// The paths the message writes and nothing else: no walk, no listing, no search (a message that joins a turn).
    Direct,
}

/// How a file was found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum How {
    /// The message wrote its path.
    Path,
    /// The message wrote its name and it matched exactly.
    Exact,
    /// The message wrote its name and it matched without case.
    NoCase,
}

/// The first lines of a file.
#[derive(Clone, Debug)]
struct Head {
    /// Its lines, when it is small enough to count them.
    lines: Option<usize>,
    text: String,
    /// The head stops before the end of the file.
    cut: bool,
}

#[derive(Clone, Debug)]
struct FoundFile {
    shown: String,
    real: PathBuf,
    size: u64,
    how: How,
    /// How many files the name matched (1 for a path).
    matches: usize,
    project: bool,
    head: Option<Head>,
}

impl FoundFile {
    /// Whether the message pointed at this file alone: the whole file may come.
    fn single(&self) -> bool {
        self.how == How::Path || self.matches == 1
    }
}

#[derive(Clone, Debug)]
struct FoundFolder {
    shown: String,
    /// Every entry, folders first, as the listing shows them.
    entries: Vec<String>,
}

#[derive(Clone, Debug)]
struct Def {
    shown: String,
    size: u64,
    line: usize,
}

#[derive(Clone, Debug)]
struct FoundDef {
    word: String,
    real: PathBuf,
    defs: Vec<Def>,
    /// Lines a to b of the first definition's file, from the definition line.
    excerpt: Option<(usize, usize, String)>,
}

/// What a message names.
#[derive(Default, Debug)]
pub(super) struct Found {
    files: Vec<FoundFile>,
    folders: Vec<FoundFolder>,
    defs: Vec<FoundDef>,
    /// The lines for the words that matched nothing.
    missing: Vec<String>,
    /// The message is about code (see `find`).
    pub(super) code: bool,
    /// Source files in the project, counted up to 201; 0 when `code` is false.
    pub(super) sources: usize,
}

/// What [`admit`] lets into the request.
#[derive(Default, Debug)]
pub(super) struct Admitted {
    /// Files included whole, under nexo's header.
    pub whole: Vec<Attachment>,
    /// Lines for the start note: heads, listings, definitions, files the model holds already.
    pub lines: Vec<String>,
    /// The tokens the lines take (the whole files are counted where they are included).
    pub spent: u64,
}

// ---- words ----

enum Word {
    Path(String),
    Name(String),
}

struct Ident {
    name: String,
    /// A name with `-`: a folder is looked up, never a definition.
    dash: bool,
}

struct Parsed {
    words: Vec<Word>,
    idents: Vec<Ident>,
    /// Every word of the message, lowercased and stripped of what is not a letter or a digit.
    tokens: Vec<String>,
}

fn has_alnum(s: &str) -> bool {
    s.chars().any(char::is_alphanumeric)
}

/// `w` without a trailing compiler or panic location (`:12` or `:12:5`).
fn without_location(w: &str) -> &str {
    let mut s = w;
    for _ in 0..2 {
        match s.rsplit_once(':') {
            Some((head, tail)) if !head.is_empty() && !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => {
                s = head
            }
            _ => break,
        }
    }
    s
}

/// A word of the message as a path or a name: trimmed as the words of a message are, a location removed.
fn cleaned(raw: &str) -> String {
    let strip =
        |c: char| matches!(c, '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';' | ':' | '!' | '?');
    // a closing bracket may stand before a sentence's full stop: `(src/lib.rs:12).`
    let w = raw.trim_matches(strip).trim_end_matches('.').trim_end_matches(strip);
    let w = without_location(w);
    w.trim_end_matches('.').to_string()
}

/// Whether `w` is a path word: it has a separator and a letter or a digit.
fn is_path_word(w: &str) -> bool {
    !w.is_empty()
        && !w.starts_with('@')
        && !w.contains("://")
        && !w.contains('*')
        && (w.contains('/') || cfg!(windows) && w.contains('\\'))
        && has_alnum(w)
}

/// Whether `w` is a file-name word: a stem, a dot and an extension of letters and digits with a letter in it.
fn is_name_word(w: &str) -> bool {
    if w.is_empty() || w.starts_with('@') || w.starts_with('.') || w.contains("://") || w.contains('*') {
        return false;
    }
    if w.contains('/') || cfg!(windows) && w.contains('\\') {
        return false;
    }
    match w.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && !ext.is_empty()
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && ext.chars().any(|c| c.is_ascii_alphabetic())
        }
        None => false,
    }
}

/// The identifier a word is, when it is a distinctive one (never a plain word).
fn ident_of(raw: &str) -> Option<Ident> {
    let core =
        raw.trim_matches(|c: char| matches!(c, '"' | '\'' | '[' | ']' | '<' | '>' | ',' | ';' | ':' | '!' | '?' | '.'));
    let mut core = core.trim_start_matches('(');
    while core.ends_with(')') && core.matches(')').count() > core.matches('(').count() {
        core = &core[..core.len() - 1];
    }
    let written = core.chars().count();
    let ticked = core.len() >= 3 && core.starts_with('`') && core.ends_with('`');
    let inner = core.trim_matches('`');
    let call = inner.ends_with("()");
    let inner = inner.strip_suffix("()").unwrap_or(inner);
    let colons = inner.contains("::");
    let name = inner.rsplit("::").next().unwrap_or(inner);
    if written < 4 || name.is_empty() || !name.chars().any(char::is_alphabetic) {
        return None;
    }
    if !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    if !name.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let dash = name.contains('-');
    let inner_underscore = name.trim_matches('_').contains('_');
    let camel = name.chars().zip(name.chars().skip(1)).any(|(a, b)| a.is_lowercase() && b.is_uppercase());
    if !(inner_underscore || camel || colons || call || ticked || dash) {
        return None;
    }
    Some(Ident { name: name.to_string(), dash })
}

fn parse(text: &str) -> Parsed {
    let mut words: Vec<Word> = Vec::new();
    let mut idents: Vec<Ident> = Vec::new();
    let mut tokens = Vec::new();
    for raw in text.split_whitespace() {
        let token = raw.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
        if !token.is_empty() {
            tokens.push(token);
        }
        let w = cleaned(raw);
        if w.is_empty() || w.starts_with('@') || w.contains("://") || w.contains('*') {
            continue;
        }
        let word = if is_path_word(&w) {
            Some(Word::Path(w))
        } else if is_name_word(&w) {
            Some(Word::Name(w))
        } else {
            None
        };
        match word {
            Some(word) => {
                let same = |x: &Word| match (x, &word) {
                    (Word::Path(a), Word::Path(b)) | (Word::Name(a), Word::Name(b)) => a == b,
                    _ => false,
                };
                if words.len() < WORDS && !words.iter().any(same) {
                    words.push(word);
                }
            }
            None => {
                if let Some(id) = ident_of(raw) {
                    if idents.len() < IDENTS && !idents.iter().any(|i| i.name == id.name) {
                        idents.push(id);
                    }
                }
            }
        }
    }
    Parsed { words, idents, tokens }
}

/// Whether the words of the project's check command (`cargo test`) come in a row in the message, without case.
fn check_hit(tokens: &[String], check_words: &[String]) -> bool {
    let want: Vec<String> = check_words.iter().map(|w| w.to_lowercase()).collect();
    match want.len() {
        0 => false,
        // a lone word counts only when it is not a word of plain speech (`pytest`, not `make`)
        1 => want[0].chars().count() >= 6 && tokens.iter().any(|t| *t == want[0]),
        n => tokens.windows(n).any(|w| w == want.as_slice()),
    }
}

// ---- paths that are never read, listed or followed ----

/// Whether `p` is a path nothing here lists or reads, written or resolved: keys and credentials, other programs'
/// account folders, shell histories, `.git` (its config can hold a token), and the system's secret files.
pub(super) fn refused(p: &Path) -> bool {
    if nexo_core::paths::is_private(p) {
        return true;
    }
    let parts: Vec<String> = p.components().map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase()).collect();
    if parts.iter().any(|c| c == ".git") {
        return true;
    }
    if let Some(name) = parts.last() {
        if name.starts_with(".env") || name.ends_with("_history") || name == ".histfile" {
            return true;
        }
    }
    const FILES: &[&str] = &["/etc/environment", "/etc/shadow", "/etc/gshadow", "/etc/sudoers"];
    const TREES: &[&str] = &["/etc/sudoers.d", "/proc", "/sys", "/dev"];
    FILES.iter().any(|f| p == Path::new(f)) || TREES.iter().any(|t| p.starts_with(t))
}

// ---- where a word points ----

struct Ctx {
    cwd: PathBuf,
    /// The project's real root; None when the working folder is the home folder or a filesystem root.
    root: Option<PathBuf>,
    home: Option<PathBuf>,
    home_real: Option<PathBuf>,
    deadline: Instant,
}

struct Hit {
    real: PathBuf,
    project: bool,
    dir: bool,
    size: u64,
}

enum Res {
    Hit(Hit),
    /// A path that is never read or listed: no line at all.
    Refused,
    Nothing,
}

impl Ctx {
    fn new(cwd: &Path, home: Option<&Path>, deadline: Instant) -> Ctx {
        let root = if orient::outside_project(cwd, home) {
            None
        } else {
            Some(cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()))
        };
        Ctx {
            cwd: cwd.to_path_buf(),
            root,
            home: home.map(Path::to_path_buf),
            home_real: home.map(|h| h.canonicalize().unwrap_or_else(|_| h.to_path_buf())),
            deadline,
        }
    }

    fn in_project(&self, real: &Path) -> bool {
        self.root.as_ref().is_some_and(|r| real.starts_with(r))
    }

    /// The path as the request shows it: relative to the project, or the real path.
    fn shown(&self, real: &Path) -> String {
        match self.root.as_ref().and_then(|r| real.strip_prefix(r).ok()) {
            Some(rel) => {
                let s = rel.to_string_lossy().replace('\\', "/");
                if s.is_empty() {
                    ".".to_string()
                } else {
                    s
                }
            }
            None => real.display().to_string(),
        }
    }

    /// The folder a refusal of a path inside a walk is read against: the project's root, so that where the project
    /// lives says nothing about its files.
    fn base(&self, project: bool) -> Option<&Path> {
        if project {
            self.root.as_deref()
        } else {
            None
        }
    }

    /// Where a path word points: the project first, then the home folder (`~`, or a relative word), then an
    /// absolute path as it is.
    fn resolve(&self, word: &str) -> Res {
        enum Via {
            Cwd,
            Home,
            Abs,
        }
        let tilde = word == "~" || word.starts_with("~/");
        let cands: Vec<(PathBuf, Via)> = if tilde {
            match &self.home {
                Some(h) => vec![(h.join(word[1..].trim_start_matches('/')), Via::Abs)],
                None => Vec::new(),
            }
        } else if Path::new(word).is_absolute() {
            vec![(PathBuf::from(word), Via::Abs)]
        } else {
            let mut v = vec![(self.cwd.join(word), Via::Cwd)];
            if let Some(h) = &self.home {
                v.push((h.join(word), Via::Home));
            }
            v
        };
        for (cand, via) in cands {
            // the path as written: a link with a plain name is judged by where it leads below
            let written: &Path = if matches!(via, Via::Cwd) { Path::new(word) } else { cand.as_path() };
            if refused(written) {
                return Res::Refused;
            }
            let Ok(real) = cand.canonicalize() else { continue };
            let project = self.in_project(&real);
            match via {
                Via::Cwd if !project => continue,
                Via::Home if !self.home_real.as_ref().is_some_and(|h| real.starts_with(h)) => continue,
                _ => {}
            }
            let judged: &Path = match self.root.as_ref().filter(|_| project).and_then(|r| real.strip_prefix(r).ok()) {
                Some(rel) => rel,
                None => real.as_path(),
            };
            if refused(judged) {
                return Res::Refused;
            }
            let Ok(meta) = std::fs::metadata(&real) else { continue };
            return Res::Hit(Hit { real, project, dir: meta.is_dir(), size: meta.len() });
        }
        Res::Nothing
    }
}

// ---- walks ----

struct Entry {
    path: PathBuf,
    name: String,
    /// 1 for an entry of the walk's root.
    depth: usize,
    size: u64,
    dir: bool,
}

#[derive(Default)]
struct Tree {
    entries: Vec<Entry>,
    /// The walk stopped at its entry cap.
    capped: bool,
    /// The walk stopped at the deadline.
    stopped: bool,
}

impl Tree {
    fn files(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| !e.dir)
    }

    fn incomplete(&self) -> bool {
        self.capped || self.stopped
    }

    fn has_source(&self) -> bool {
        self.files().any(|e| is_source(&e.path))
    }
}

/// The entries under `root`, breadth first with each folder's entries sorted by name, to `max_depth` and at most
/// `cap` entries. Hidden entries, links (never followed, so a loop cannot recurse) and refused paths are left out;
/// the folders of build output and dependencies are listed and never entered. `base` is what refusals are read
/// against (the project's root), else the whole path.
fn bfs(root: &Path, base: Option<&Path>, max_depth: usize, cap: usize, deadline: Instant) -> Tree {
    let mut tree = Tree::default();
    let mut level = vec![root.to_path_buf()];
    let mut depth = 1;
    let mut seen = 0usize;
    while !level.is_empty() && depth <= max_depth {
        let mut next = Vec::new();
        for dir in level {
            if Instant::now() >= deadline {
                tree.stopped = true;
                return tree;
            }
            let Ok(read) = std::fs::read_dir(&dir) else { continue };
            let mut items: Vec<(String, PathBuf, std::fs::FileType, u64)> = read
                .flatten()
                .filter_map(|e| {
                    let ft = e.file_type().ok()?;
                    let size = if ft.is_file() { e.metadata().map_or(0, |m| m.len()) } else { 0 };
                    Some((e.file_name().to_string_lossy().into_owned(), e.path(), ft, size))
                })
                .collect();
            items.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, path, ft, size) in items {
                if name.starts_with('.') || ft.is_symlink() {
                    continue;
                }
                let judged: &Path = base.and_then(|b| path.strip_prefix(b).ok()).unwrap_or(path.as_path());
                if refused(judged) {
                    continue;
                }
                if !ft.is_dir() && !ft.is_file() {
                    continue;
                }
                seen += 1;
                if seen > cap {
                    tree.capped = true;
                    return tree;
                }
                if ft.is_dir() {
                    if depth < max_depth && !orient::SKIP_DIRS.contains(&name.to_ascii_lowercase().as_str()) {
                        next.push(path.clone());
                    }
                    tree.entries.push(Entry { path, name, depth, size: 0, dir: true });
                } else {
                    tree.entries.push(Entry { path, name, depth, size, dir: false });
                }
            }
        }
        level = next;
        depth += 1;
    }
    tree
}

/// The project's own walk: like [`bfs`] (same entries, same order, hidden entries and links left out, build output
/// and dependency folders listed and never entered) but what the project's `.gitignore` files ignore is not there,
/// so generated output and vendored trees neither use up the entry cap nor match a word of the message.
fn ignore_walk(root: &Path, cap: usize, deadline: Instant) -> Tree {
    let mut tree = Tree::default();
    let kept: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>> = Default::default();
    let note = kept.clone();
    let walk = ignore::WalkBuilder::new(root)
        .require_git(false)
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(move |e| {
            let is_dir = e.file_type().is_some_and(|t| t.is_dir());
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            if e.depth() > 0 && is_dir && orient::SKIP_DIRS.contains(&name.as_str()) {
                // listed by name, never entered
                note.lock().unwrap_or_else(|p| p.into_inner()).push(e.path().to_path_buf());
                return false;
            }
            true
        })
        .build();
    let mut seen = 0usize;
    for e in walk.flatten() {
        if e.depth() == 0 {
            continue;
        }
        if Instant::now() >= deadline {
            tree.stopped = true;
            break;
        }
        let Some(ft) = e.file_type() else { continue };
        let Ok(rel) = e.path().strip_prefix(root) else { continue };
        if ft.is_symlink() || refused(rel) || !(ft.is_dir() || ft.is_file()) {
            continue;
        }
        seen += 1;
        if seen > cap {
            tree.capped = true;
            break;
        }
        let size = if ft.is_file() { e.metadata().map_or(0, |m| m.len()) } else { 0 };
        tree.entries.push(Entry {
            path: e.path().to_path_buf(),
            name: e.file_name().to_string_lossy().into_owned(),
            depth: e.depth(),
            size,
            dir: ft.is_dir(),
        });
    }
    let skipped = std::mem::take(&mut *kept.lock().unwrap_or_else(|p| p.into_inner()));
    for path in skipped {
        let Ok(rel) = path.strip_prefix(root) else { continue };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.starts_with('.') || refused(rel) {
            continue;
        }
        tree.entries.push(Entry { depth: rel.components().count(), path, name, size: 0, dir: true });
    }
    // level by level, each folder's entries by name, as the breadth first walk gives them
    tree.entries.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.path.cmp(&b.path)));
    tree
}

/// The project's walk, once: every file and folder to the walk cap.
fn project_tree<'a>(cell: &'a mut Option<Tree>, ctx: &Ctx) -> &'a Tree {
    cell.get_or_insert_with(|| match &ctx.root {
        Some(root) => ignore_walk(root, orient::WALK_CAP, ctx.deadline),
        None => Tree::default(),
    })
}

/// The source files of a walk, counted up to 201: 201 too when the walk did not reach the end.
fn count_sources(tree: &Tree) -> usize {
    let n = tree.files().filter(|e| is_source(&e.path)).count();
    if n >= SOURCES_PAST || tree.incomplete() {
        SOURCES_PAST
    } else {
        n
    }
}

/// The entries of a folder as a listing shows them: folders (with a trailing `/`) first, then files with their size.
/// Hidden entries, links and refused paths are left out; the folders of build output are shown by name.
fn listing(dir: &Path, base: Option<&Path>) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for e in read.flatten().take(LISTING_READ) {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let path = e.path();
        let judged: &Path = base.and_then(|b| path.strip_prefix(b).ok()).unwrap_or(path.as_path());
        if refused(judged) {
            continue;
        }
        if ft.is_dir() {
            dirs.push(format!("{name}/"));
        } else if ft.is_file() {
            files.push((name, e.metadata().map_or(0, |m| m.len())));
        }
    }
    dirs.sort();
    files.sort();
    dirs.into_iter().chain(files.into_iter().map(|(n, s)| format!("{n} {}", size_text(s)))).collect()
}

// ---- reading ----

fn size_text(n: u64) -> String {
    if n < 1_000 {
        format!("{n} B")
    } else if n < 10_000 {
        format!("{:.1} KB", n as f64 / 1_000.0)
    } else if n < 1_000_000 {
        format!("{} KB", (n + 500) / 1_000)
    } else {
        format!("{:.1} MB", n as f64 / 1_000_000.0)
    }
}

/// The longest prefix of `s` of at most `max` bytes that ends at a character.
fn cut(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The first lines of the text file at `path` (`size` bytes): None for a file that is no text or cannot be read.
fn head_of(path: &Path, size: u64) -> Option<Head> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(COUNT_BYTES).read_to_end(&mut buf).ok()?;
    if buf.contains(&0) {
        return None;
    }
    let text = match std::str::from_utf8(&buf) {
        Ok(t) => t,
        // the read stopped inside a character: not an error
        Err(e) if size > COUNT_BYTES && e.error_len().is_none() => std::str::from_utf8(&buf[..e.valid_up_to()]).ok()?,
        Err(_) => return None,
    };
    let lines = if size > COUNT_BYTES { None } else { Some(text.lines().count()) };
    let mut head = String::new();
    let mut cut_here = lines.is_none_or(|n| n > HEAD_LINES);
    for line in text.lines().take(HEAD_LINES) {
        let room = HEAD_BYTES.saturating_sub(head.len());
        if line.len() + 1 > room {
            head.push_str(cut(line, room.saturating_sub(1)));
            head.push('\n');
            cut_here = true;
            break;
        }
        head.push_str(line);
        head.push('\n');
    }
    Some(Head { lines, text: head, cut: cut_here })
}

// ---- definitions ----

/// Whether `rest` starts with `name` and the name ends there.
fn names(rest: &str, name: &str) -> bool {
    rest.strip_prefix(name)
        .is_some_and(|after| !after.starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '$'))
}

/// Whether `line` defines `name`: after any modifiers, a keyword that introduces a definition, then the name.
fn is_def(line: &str, name: &str) -> bool {
    const KEYS: &[&str] = &[
        "fn",
        "struct",
        "enum",
        "trait",
        "type",
        "union",
        "const",
        "static",
        "mod",
        "module",
        "def",
        "class",
        "function",
        "fun",
        "func",
        "interface",
    ];
    const MODS: &[&str] = &[
        "pub",
        "export",
        "default",
        "async",
        "static",
        "unsafe",
        "extern",
        "const",
        "abstract",
        "final",
        "public",
        "private",
        "protected",
        "internal",
        "open",
        "override",
        "inline",
        "data",
        "sealed",
    ];
    let mut rest = line.trim_start();
    for _ in 0..8 {
        let end = rest.find(|c: char| !(c.is_ascii_alphabetic() || c == '_')).unwrap_or(rest.len());
        let (word, after) = rest.split_at(end);
        if word.is_empty() {
            return false;
        }
        if KEYS.contains(&word) && after.starts_with(char::is_whitespace) {
            let mut tail = after.trim_start();
            // a Go method: `func (s *Srv) Handle`
            if word == "func" && tail.starts_with('(') {
                match tail.find(')') {
                    Some(i) => tail = tail[i + 1..].trim_start(),
                    None => return false,
                }
            }
            if names(tail, name) {
                return true;
            }
        }
        if !MODS.contains(&word) {
            return false;
        }
        let mut next = after;
        if word == "pub" && next.starts_with('(') {
            match next.find(')') {
                Some(i) => next = &next[i + 1..],
                None => return false,
            }
        } else if word == "extern" {
            let t = next.trim_start();
            if let Some(q) = t.strip_prefix('"') {
                match q.find('"') {
                    Some(i) => next = &q[i + 1..],
                    None => return false,
                }
            }
        }
        if !next.is_empty() && !next.starts_with(char::is_whitespace) {
            return false;
        }
        rest = next.trim_start();
    }
    false
}

/// The files the definition search reads: source code, not the data formats the source extensions also hold.
fn scans_for_defs(p: &Path) -> bool {
    is_source(p)
        && !p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e, "toml" | "json" | "yaml" | "yml" | "csv" | "tsv"))
}

/// Where each of `words` is defined in the project's source files: one pass over the files for all of them, each
/// file read only when it holds one of the words, at most [`DEFS_PER_WORD`] definitions per word.
fn find_defs(ctx: &Ctx, tree: &Tree, words: &[String]) -> Vec<FoundDef> {
    let mut found: Vec<Vec<(PathBuf, u64, usize)>> = vec![Vec::new(); words.len()];
    let mut read = 0u64;
    for e in tree.files() {
        if found.iter().all(|f| f.len() >= DEFS_PER_WORD) || Instant::now() >= ctx.deadline {
            break;
        }
        if e.size == 0 || e.size > DEF_FILE_BYTES || !scans_for_defs(&e.path) {
            continue;
        }
        read += e.size;
        if read > DEF_READ_BYTES {
            break;
        }
        let Ok(text) = std::fs::read_to_string(&e.path) else { continue };
        if !words.iter().any(|w| text.contains(w.as_str())) {
            continue;
        }
        for (i, line) in text.lines().enumerate() {
            for (k, w) in words.iter().enumerate() {
                if found[k].len() < DEFS_PER_WORD && line.contains(w.as_str()) && is_def(line, w) {
                    found[k].push((e.path.clone(), e.size, i + 1));
                }
            }
        }
    }
    let mut out = Vec::new();
    for (word, hits) in words.iter().zip(found) {
        let Some((real, _, line)) = hits.first().cloned() else { continue };
        let excerpt = excerpt_at(&real, line);
        let defs = hits.into_iter().map(|(p, size, line)| Def { shown: ctx.shown(&p), size, line }).collect();
        out.push(FoundDef { word: word.clone(), real, defs, excerpt });
    }
    out
}

/// Lines `line..` of the file at `path`: at most 40 lines and 2 KB, with the numbers of the first and last.
fn excerpt_at(path: &Path, line: usize) -> Option<(usize, usize, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut out = String::new();
    let mut last = line;
    for (i, l) in text.lines().enumerate().skip(line.saturating_sub(1)).take(EXCERPT_LINES) {
        let room = EXCERPT_BYTES.saturating_sub(out.len());
        if l.len() + 1 > room {
            if out.is_empty() {
                out.push_str(cut(l, room.saturating_sub(1)));
                out.push('\n');
                last = i + 1;
            }
            break;
        }
        out.push_str(l);
        out.push('\n');
        last = i + 1;
    }
    (!out.is_empty()).then_some((line, last, out))
}

// ---- find ----

fn extension_of(p: &Path) -> Option<String> {
    p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase())
}

fn file_of(ctx: &Ctx, real: PathBuf, size: u64, how: How, matches: usize, search: bool) -> FoundFile {
    let project = ctx.in_project(&real);
    let head = if search { head_of(&real, size) } else { None };
    FoundFile { shown: ctx.shown(&real), real, size, how, matches, project, head }
}

/// What `text` names, looked up on disk. `attachments` are the files the request carries already; `check_words` the
/// words of the project's check command (`cargo test`); `home` the person's home folder. The walks and the
/// definition search stop at `deadline`.
///
/// A message with no path word, file-name word, identifier or check-command words costs nothing: no walk.
///
/// `code` is the evidence that the message is about code, computed from the files and never from verbs: a named
/// project file of a source language (or one not created yet, in a folder that exists), a project folder that holds
/// source files, an attached source file, an identifier the project defines, or the check command's own words.
/// Paths outside the project never give it. `sources` counts the project's source files (up to 201) when `code`.
pub(super) fn find(
    text: &str,
    cwd: &Path,
    home: Option<&Path>,
    attachments: &[Attachment],
    check_words: &[String],
    reach: Reach,
    deadline: Instant,
) -> Found {
    let parsed = parse(text);
    let ctx = Ctx::new(cwd, home, deadline);
    let mut f = Found::default();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for a in attachments {
        let p = if a.path.is_absolute() { a.path.clone() } else { cwd.join(&a.path) };
        let real = p.canonicalize().unwrap_or(p);
        if ctx.in_project(&real) && is_source(&real) {
            f.code = true;
        }
        seen.insert(real);
    }
    if reach == Reach::Direct {
        direct(&ctx, &parsed, &mut seen, &mut f);
        return f;
    }
    if check_hit(&parsed.tokens, check_words) {
        f.code = true;
    }
    let mut project: Option<Tree> = None;
    let mut trees: Vec<Tree> = Vec::new();
    let mut folders_seen: HashSet<PathBuf> = HashSet::new();
    let mut names: Vec<String> = Vec::new();
    let mut absent: Vec<String> = Vec::new();

    // the paths the message writes
    for word in &parsed.words {
        match word {
            Word::Name(n) => names.push(n.clone()),
            Word::Path(p) => match ctx.resolve(p) {
                Res::Refused => {}
                Res::Nothing => absent.push(p.clone()),
                Res::Hit(h) if h.dir => {
                    if !folders_seen.insert(h.real.clone()) {
                        continue;
                    }
                    let base = ctx.base(h.project);
                    let tree = bfs(&h.real, base, NAMED_DEPTH, NAMED_WALK, deadline);
                    if h.project && tree.has_source() {
                        f.code = true;
                    }
                    f.folders.push(FoundFolder { shown: ctx.shown(&h.real), entries: listing(&h.real, base) });
                    trees.push(tree);
                }
                Res::Hit(h) => {
                    if image_type(&h.real).is_some() || !seen.insert(h.real.clone()) {
                        continue;
                    }
                    if h.project && is_source(&h.real) {
                        f.code = true;
                    }
                    f.files.push(file_of(&ctx, h.real, h.size, How::Path, 1, true));
                }
            },
        }
    }

    // the names the message writes: the project, the folders it names, the top level of the home folder
    let mut absent_names: Vec<String> = Vec::new();
    let mut exts: HashSet<String> = HashSet::new();
    let mut home_tree = Tree::default();
    if !names.is_empty() {
        if let Some(h) = &ctx.home_real {
            home_tree = bfs(h, None, 1, NAMED_WALK, deadline);
        }
        let tree = project_tree(&mut project, &ctx);
        let pool: Vec<&Tree> = std::iter::once(tree).chain(trees.iter()).chain(std::iter::once(&home_tree)).collect();
        for t in &pool {
            exts.extend(t.files().filter_map(|e| extension_of(&e.path)));
        }
        for n in &names {
            let mut picked: Vec<(PathBuf, u64)> = Vec::new();
            let mut how = How::Exact;
            for pass in [How::Exact, How::NoCase] {
                for t in &pool {
                    for e in t.files() {
                        let hit = match pass {
                            How::NoCase => e.name.eq_ignore_ascii_case(n),
                            _ => e.name == *n,
                        };
                        let fresh = !seen.contains(&e.path) && !picked.iter().any(|(p, _)| *p == e.path);
                        if hit && fresh && image_type(&e.path).is_none() && picked.len() < MATCHES {
                            picked.push((e.path.clone(), e.size));
                        }
                    }
                }
                if !picked.is_empty() {
                    how = pass;
                    break;
                }
            }
            if picked.is_empty() {
                absent_names.push(n.clone());
                continue;
            }
            let count = picked.len();
            for (p, size) in picked {
                seen.insert(p.clone());
                if ctx.in_project(&p) && is_source(&p) {
                    f.code = true;
                }
                f.files.push(file_of(&ctx, p, size, how, count, true));
            }
        }
    }

    // the identifiers: a project folder of that name, else where the project defines it
    if ctx.root.is_some() && !parsed.idents.is_empty() {
        let tree = project_tree(&mut project, &ctx);
        let mut to_define: Vec<String> = Vec::new();
        let mut folders: Vec<(PathBuf, Tree)> = Vec::new();
        for id in &parsed.idents {
            let dir = tree.entries.iter().find(|e| e.dir && e.depth <= IDENT_DEPTH && e.name == id.name);
            match dir {
                Some(d) => {
                    if folders_seen.insert(d.path.clone()) {
                        let sub = bfs(&d.path, ctx.base(true), NAMED_DEPTH, NAMED_WALK, deadline);
                        folders.push((d.path.clone(), sub));
                    }
                }
                None if !id.dash => to_define.push(id.name.clone()),
                None => {}
            }
        }
        for (path, sub) in folders {
            if sub.has_source() {
                f.code = true;
            }
            f.folders.push(FoundFolder { shown: ctx.shown(&path), entries: listing(&path, ctx.base(true)) });
        }
        if !to_define.is_empty() {
            let defs = find_defs(&ctx, tree, &to_define);
            if !defs.is_empty() {
                f.code = true;
            }
            f.defs = defs;
        }
    }

    // what matched nothing: a line only when the word can be told from noise, and only when the walks reached the end
    let complete = !project.as_ref().is_some_and(Tree::incomplete)
        && !trees.iter().any(Tree::incomplete)
        && !home_tree.incomplete()
        && Instant::now() < deadline;
    for p in &absent {
        let segments = p.trim_end_matches('/').split('/').filter(|s| !s.is_empty()).count();
        if segments < 2 {
            continue;
        }
        let parent = match p.trim_end_matches('/').rsplit_once('/') {
            Some(("", _)) => "/".to_string(),
            Some((head, _)) => head.to_string(),
            None => continue,
        };
        if let Res::Hit(h) = ctx.resolve(&parent) {
            if !h.dir {
                continue;
            }
            // a source file that does not exist yet, in a project folder that does
            if h.project && is_source(Path::new(p)) {
                f.code = true;
            }
            if complete {
                f.missing.push(format!("{p}: no such file or folder"));
            }
        }
    }
    if complete {
        for n in &absent_names {
            let known =
                Path::new(n).extension().is_some_and(|e| exts.contains(&e.to_string_lossy().to_ascii_lowercase()));
            if known {
                f.missing.push(format!(
                    "{n}: no file of that name in the project, the folders this message names or the top level of the \
                     home folder"
                ));
            }
        }
    }

    if f.code && ctx.root.is_some() {
        f.sources = count_sources(project_tree(&mut project, &ctx));
    }
    f
}

/// [`Reach::Direct`]: the project files the message writes a path or a name for, and nothing else.
fn direct(ctx: &Ctx, parsed: &Parsed, seen: &mut HashSet<PathBuf>, f: &mut Found) {
    for word in &parsed.words {
        let (w, how) = match word {
            Word::Path(p) => (p, How::Path),
            Word::Name(n) => (n, How::Exact),
        };
        if let Res::Hit(h) = ctx.resolve(w) {
            if h.dir || !h.project || image_type(&h.real).is_some() || !seen.insert(h.real.clone()) {
                continue;
            }
            f.files.push(file_of(ctx, h.real, h.size, how, 1, false));
        }
    }
}

// ---- admit ----

/// Whether `block` is in a message of the person's already: said once, it is not said again.
fn repeated(texts: &[String], block: &str) -> bool {
    texts.iter().any(|t| t.contains(block))
}

/// What the lines of one request may still take.
struct Room {
    left: u64,
    chars: usize,
    /// No budget to attach with: no line is admitted.
    legacy: bool,
}

impl Room {
    /// Adds `block` to `out.lines` when it fits the characters and the tokens left and the conversation has not said
    /// it yet.
    fn say(&mut self, out: &mut Admitted, texts: &[String], block: String) -> bool {
        let tokens = orient::attached_tokens(block.len());
        if self.legacy || self.chars + block.len() > LINES_CHARS || tokens > self.left || repeated(texts, &block) {
            return false;
        }
        self.chars += block.len();
        self.left -= tokens;
        out.spent += tokens;
        out.lines.push(block);
        true
    }
}

/// The line of a folder: its path, how many entries it has and the first of them (at most [`FOLDER_ENTRIES`] and
/// [`FOLDER_CHARS`] characters), then how many more there are.
fn folder_line(shown: &str, entries: &[String]) -> String {
    let total = entries.len();
    let mut body = String::new();
    let mut count = 0usize;
    for e in entries.iter().take(FOLDER_ENTRIES) {
        let add = if body.is_empty() { e.len() } else { e.len() + 2 };
        if body.len() + add > FOLDER_CHARS {
            break;
        }
        if !body.is_empty() {
            body.push_str(", ");
        }
        body.push_str(e);
        count += 1;
    }
    if total == 0 {
        body.push_str("(empty)");
    } else if count < total {
        body.push_str(&format!(", … and {} more", total - count));
    }
    format!("{shown}/ (folder, {}): {body}", nexo_core::count(total, "entry"))
}

/// What of `found` goes into the request: the files whole (at most [`NAMED_FILES`], the model not holding them
/// already), and the lines about the rest, together within `budget` tokens and 6,000 characters. Lines that are
/// already in the conversation are not said again; a path the read policy does not let the model read is left out.
///
/// With no budget to attach with (`agent.attach_budget` of 0), only the project files the message wrote a path or
/// a bare name for come, whole, as before: no heads, listings, definitions or missing lines.
pub(super) fn admit(st: &State, found: Found, budget: u64) -> Admitted {
    let mut out = Admitted::default();
    if !st.config.agent.attach_named_files {
        return out;
    }
    let legacy = st.config.agent.attach_budget == 0;
    let said = orient::carried(&st.history);
    let texts: Vec<String> = st.history.iter().filter(|m| m.role == Role::User).map(|m| m.text()).collect();
    let allowed = |shown: &str| {
        st.perm.decide("read", ToolKind::Read, &serde_json::json!({ "path": shown }), &st.cwd) == Decision::Allow
    };
    let mut room = Room { left: budget, chars: 0, legacy };
    let mut taken = 0usize;
    for file in &found.files {
        let direct = file.project && (file.how == How::Path || (file.how == How::Exact && !file.shown.contains('/')));
        if (legacy && !direct) || said.contains(&file.shown) || !allowed(&file.shown) {
            continue;
        }
        let tokens = orient::attached_tokens(file.size as usize);
        let whole = (file.single() || legacy)
            && file.size <= NAMED_FILE_BYTES
            && taken < NAMED_FILES
            && (legacy || tokens <= room.left);
        if whole {
            let Ok(bytes) = std::fs::read(&file.real) else { continue };
            if std::str::from_utf8(&bytes).is_err() {
                continue;
            }
            // the model holds the whole file (it wrote it, or its command made it and printed it) and it is unchanged
            let made = orient::made_in_work(&st.worked, &file.real);
            taken += 1;
            if let Some(how) = st.reads.held_whole(&file.real, &bytes, made) {
                out.lines.push(format!(
                    "{}: {how}, unchanged since; not attached again. Read it if you need its lines.",
                    file.shown
                ));
                continue;
            }
            if !legacy {
                room.left -= tokens;
            }
            out.whole.push(Attachment { path: PathBuf::from(&file.shown) });
            continue;
        }
        if legacy {
            continue;
        }
        let Some(head) = &file.head else { continue };
        let shown_lines = head.text.lines().count();
        let count = match head.lines {
            Some(n) if !head.cut => nexo_core::count(n, "line"),
            Some(n) if shown_lines < n => format!("{}; first {shown_lines} lines", nexo_core::count(n, "line")),
            Some(n) => format!("{}; cut at {HEAD_BYTES} bytes", nexo_core::count(n, "line")),
            None => format!("over {}; first {shown_lines} lines", size_text(COUNT_BYTES)),
        };
        let header = format!("{} ({}, {count}):", file.shown, size_text(file.size));
        let block = if head.text.is_empty() { header } else { fenced(&header, &file.real, &head.text) };
        room.say(&mut out, &texts, block);
    }
    if legacy {
        return out;
    }

    for folder in &found.folders {
        if !allowed(&folder.shown) {
            continue;
        }
        room.say(&mut out, &texts, folder_line(&folder.shown, &folder.entries));
    }

    for def in &found.defs {
        let Some(first) = def.defs.first() else { continue };
        if !allowed(&first.shown) {
            continue;
        }
        let at: Vec<String> = def.defs.iter().map(|d| format!("{}:{}", d.shown, d.line)).collect();
        if !room.say(&mut out, &texts, format!("{}: defined at {}", def.word, at.join(", "))) {
            continue;
        }
        if said.contains(&first.shown) {
            continue;
        }
        let tokens = orient::attached_tokens(first.size as usize);
        let already = out.whole.iter().any(|a| a.path == Path::new(&first.shown));
        if def.defs.len() == 1
            && first.size <= NAMED_FILE_BYTES
            && taken < NAMED_FILES
            && tokens <= room.left
            && !already
        {
            let Ok(bytes) = std::fs::read(&def.real) else { continue };
            if std::str::from_utf8(&bytes).is_err() {
                continue;
            }
            let made = orient::made_in_work(&st.worked, &def.real);
            if st.reads.held_whole(&def.real, &bytes, made).is_some() {
                continue;
            }
            taken += 1;
            room.left -= tokens;
            out.whole.push(Attachment { path: PathBuf::from(&first.shown) });
        } else if let Some((a, b, text)) = &def.excerpt {
            if !already {
                let header = format!("{} ({}; lines {a}-{b}):", first.shown, size_text(first.size));
                room.say(&mut out, &texts, fenced(&header, &def.real, text));
            }
        }
    }

    for line in found.missing {
        room.say(&mut out, &texts, line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn later() -> Instant {
        Instant::now() + Duration::from_secs(20)
    }

    /// A project folder and a home folder, apart.
    fn dirs() -> (tempfile::TempDir, tempfile::TempDir) {
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap())
    }

    fn put(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn look(text: &str, cwd: &Path, home: &Path) -> Found {
        find(text, cwd, Some(home), &[], &[], Reach::Search, later())
    }

    fn shown(f: &Found) -> Vec<String> {
        f.files.iter().map(|x| x.shown.clone()).collect()
    }

    #[test]
    fn home_relative_tilde_and_absolute_paths_resolve() {
        let (project, home) = dirs();
        put(home.path(), "notes/a.md", "a\n");
        put(home.path(), "b.md", "b\n");
        let real_home = home.path().canonicalize().unwrap();
        let abs = real_home.join("notes/a.md");
        let f = look("see ~/b.md and notes/a.md", project.path(), home.path());
        let mut got = shown(&f);
        got.sort();
        assert_eq!(got, [real_home.join("b.md").display().to_string(), abs.display().to_string()].map(String::from));
        // an absolute path stays as it is
        let f = look(&format!("see {}", abs.display()), project.path(), home.path());
        assert_eq!(shown(&f), [abs.display().to_string()]);
        // a path that does not exist and a link out of the home folder give nothing
        let f = look("see ~/nothing/here.md", project.path(), home.path());
        assert!(f.files.is_empty() && f.folders.is_empty());
    }

    #[test]
    fn a_named_folder_gives_a_bounded_listing_without_hidden_entries() {
        let (project, home) = dirs();
        let dir = home.path().join("Store");
        for i in 0..45 {
            put(&dir, &format!("f{i:02}.txt"), "x\n");
        }
        put(&dir, ".hidden/x", "x\n");
        put(&dir, ".git/config", "[remote]\n");
        put(&dir, "target/debug/out", "x\n");
        let f = look(&format!("look at {}/", dir.display()), project.path(), home.path());
        assert_eq!(f.folders.len(), 1);
        let entries = &f.folders[0].entries;
        assert_eq!(entries.len(), 46, "{entries:?}");
        assert!(entries.iter().all(|e| !e.starts_with('.')), "{entries:?}");
        assert!(entries.contains(&"target/".to_string()));
        // nothing under target/ was walked
        assert!(f.files.is_empty());
    }

    #[test]
    fn a_file_inside_a_build_folder_is_not_found() {
        let (project, home) = dirs();
        put(project.path(), "target/debug/GENERATED.md", "g\n");
        put(project.path(), "src/lib.rs", "pub fn a() {}\n");
        let f = look("update GENERATED.md", project.path(), home.path());
        assert!(f.files.is_empty(), "{:?}", shown(&f));
        let (f2, h2) = dirs();
        put(h2.path(), "Store/target/debug/GENERATED.md", "g\n");
        put(h2.path(), "Store/ok.txt", "x\n");
        let f = look(&format!("look at {}/Store/", h2.path().display()), f2.path(), h2.path());
        assert!(f.files.is_empty());
        assert_eq!(f.folders.len(), 1);
        assert!(f.folders[0].entries.contains(&"target/".to_string()));
    }

    #[test]
    fn a_gitignored_folder_is_neither_walked_nor_matched() {
        let (project, home) = dirs();
        put(project.path(), ".gitignore", "generated/\n*.log\n");
        put(project.path(), "generated/OUTPUT.md", "g\n");
        put(project.path(), "src/OUTPUT.log", "l\n");
        put(project.path(), "docs/NOTES.md", "n\n");
        let f = look("read OUTPUT.md and OUTPUT.log", project.path(), home.path());
        assert!(f.files.is_empty(), "{:?}", shown(&f));
        // a file the ignore file does not name is still found
        let f = look("read NOTES.md", project.path(), home.path());
        assert_eq!(shown(&f), ["docs/NOTES.md"]);
    }

    #[test]
    fn a_folder_line_is_cut_at_forty_entries_and_at_its_characters() {
        let many: Vec<String> = (0..45).map(|i| format!("f{i:02}.txt")).collect();
        let line = folder_line("/h/Store", &many);
        assert!(line.starts_with("/h/Store/ (folder, 45 entries): f00.txt, f01.txt"), "{line}");
        assert!(line.ends_with(", … and 5 more"), "{line}");
        assert!(!line.contains("f40.txt"), "{line}");
        let long: Vec<String> = (0..30).map(|i| format!("{i:02}{}", "n".repeat(98))).collect();
        let line = folder_line("/h/L", &long);
        let body = line.split_once("): ").unwrap().1;
        let listed = body.split(", … and ").next().unwrap();
        assert!(listed.len() <= FOLDER_CHARS, "{}", listed.len());
        assert!(body.contains(" more"), "{body}");
        assert_eq!(folder_line("/h/E", &[]), "/h/E/ (folder, 0 entries): (empty)");
    }
    #[test]
    fn a_file_name_gives_every_match_up_to_five() {
        let (project, home) = dirs();
        for i in 0..7 {
            put(project.path(), &format!("d{i}/NOTES.md"), "n\n");
        }
        let f = look("update NOTES.md", project.path(), home.path());
        assert_eq!(f.files.len(), MATCHES, "{:?}", shown(&f));
        assert!(f.files.iter().all(|x| x.matches == MATCHES && !x.single()));
        // one match is a single file
        put(project.path(), "only/ONE.md", "n\n");
        let f = look("update ONE.md", project.path(), home.path());
        assert_eq!(shown(&f), ["only/ONE.md"]);
        assert!(f.files[0].single());
    }

    #[test]
    fn a_file_at_depth_three_of_a_large_named_folder_is_found() {
        let (project, home) = dirs();
        let dir = home.path().join("Big");
        for d in 0..50 {
            for f in 0..49 {
                put(&dir, &format!("a{d:02}/f{f:02}.txt"), "x\n");
            }
        }
        put(&dir, "research/ideas/IDEAS.md", "# ideas\n");
        let f = look(&format!("ideas in {}/ and IDEAS.md", dir.display()), project.path(), home.path());
        assert!(
            shown(&f).iter().any(|s| std::path::Path::new(s).ends_with("research/ideas/IDEAS.md")),
            "{:?}",
            shown(&f)
        );
    }

    #[test]
    fn a_name_differing_in_case_is_found_when_no_exact_match_exists() {
        let (project, home) = dirs();
        put(project.path(), "IDEAS.md", "# one\n");
        put(home.path(), "docs/IDEAS.md", "# two\n");
        let f = look("mira ideas.md", project.path(), home.path());
        assert_eq!(shown(&f), ["IDEAS.md"]);
        assert!(f.files.iter().all(|x| x.how == How::NoCase));
        // an exact match wins over one without case
        put(project.path(), "ideas.md", "# three\n");
        // on a case-insensitive file system (macOS and Windows by default) that write went into IDEAS.md: there is
        // no exact match to win, and the file is rightly still found without case
        let names = std::fs::read_dir(project.path()).unwrap().filter_map(|e| e.ok()?.file_name().into_string().ok());
        if names.filter(|n| n.eq_ignore_ascii_case("ideas.md")).count() < 2 {
            eprintln!(
                "this file system folds case, so IDEAS.md and ideas.md are one file: the exact-match check is skipped"
            );
            return;
        }
        let f = look("mira ideas.md", project.path(), home.path());
        assert!(f.files.iter().all(|x| x.how == How::Exact), "{:?}", f.files);
    }

    #[test]
    fn private_paths_are_never_listed_or_read() {
        let (project, home) = dirs();
        let h = home.path();
        put(h, ".ssh/id_ed25519", "secret\n");
        put(h, "x.key", "secret\n");
        put(h, ".env", "TOKEN=1\n");
        put(h, ".envrc", "export TOKEN=1\n");
        put(h, ".nexo/auth.json", "{}\n");
        put(h, ".config/gh/hosts.yml", "token: x\n");
        put(h, ".zsh_history", "ls\n");
        put(project.path(), ".git/config", "[remote]\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink(h.join(".ssh/id_ed25519"), project.path().join("plain.txt")).unwrap();
        let words =
            "~/.ssh/id_ed25519 ~/x.key ~/.env ~/.envrc ~/.nexo/auth.json ~/.config/gh/hosts.yml ~/.zsh_history \
                     .git/config plain.txt";
        let f = look(words, project.path(), home.path());
        assert!(f.files.is_empty() && f.folders.is_empty() && f.missing.is_empty(), "{f:?}");
        // and a listing of the folder that holds them leaves them out
        let f = look(&format!("see {}/", h.display()), project.path(), home.path());
        assert!(f.folders[0].entries.iter().all(|e| !e.contains("key") && !e.contains("env")), "{:?}", f.folders);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_loop_in_a_named_folder_ends() {
        let (project, home) = dirs();
        let dir = home.path().join("Loop");
        put(&dir, "a/file.txt", "x\n");
        std::os::unix::fs::symlink(&dir, dir.join("a/back")).unwrap();
        let f = look(&format!("see {}/ and file.txt", dir.display()), project.path(), home.path());
        assert_eq!(f.folders.len(), 1);
        assert_eq!(f.files.len(), 1, "{:?}", shown(&f));
    }

    #[test]
    fn a_url_and_plain_words_find_nothing() {
        let (project, home) = dirs();
        put(project.path(), "src/lib.rs", "pub fn add() {}\n");
        let f = look(
            "https://apps.microsoft.com/detail/9P9QS9MCXTJZ how does the curator work",
            project.path(),
            home.path(),
        );
        assert!(f.files.is_empty() && f.folders.is_empty() && f.defs.is_empty() && f.missing.is_empty() && !f.code);
        let f = look("hola", project.path(), home.path());
        assert!(f.files.is_empty() && !f.code && f.sources == 0);
        // stem words are not evidence
        let f = look("render the tests", project.path(), home.path());
        assert!(f.files.is_empty() && f.defs.is_empty() && !f.code);
    }

    #[test]
    fn a_spanish_request_naming_a_crate_gives_its_folder() {
        let (project, home) = dirs();
        put(project.path(), "crates/nexo-tools/src/lib.rs", "pub fn a() {}\n");
        put(project.path(), "crates/nexo-tools/Cargo.toml", "[package]\n");
        let f = look("arregla el fallo en nexo-tools", project.path(), home.path());
        assert_eq!(f.folders.len(), 1);
        assert_eq!(f.folders[0].shown, "crates/nexo-tools");
        assert!(f.code);
        assert_eq!(f.sources, 2);
    }

    #[test]
    fn a_folder_with_sources_and_a_new_source_file_are_code_evidence() {
        let (project, home) = dirs();
        put(project.path(), "web/index.php", "<?php\n");
        put(project.path(), "docs/readme.md", "x\n");
        put(project.path(), "tests/old.rs", "fn a() {}\n");
        assert!(look("fix web/", project.path(), home.path()).code);
        assert!(!look("read docs/", project.path(), home.path()).code);
        let f = look("add tests/new.rs", project.path(), home.path());
        assert!(f.code);
        assert_eq!(f.missing, ["tests/new.rs: no such file or folder"]);
        assert!(!look("add nothing/new.rs", project.path(), home.path()).code);
    }

    #[test]
    fn the_check_command_words_are_code_evidence() {
        let (project, home) = dirs();
        put(project.path(), "src/lib.rs", "pub fn add() {}\n");
        let words = vec!["cargo".to_string(), "test".to_string()];
        let with = |t: &str| find(t, project.path(), Some(home.path()), &[], &words, Reach::Search, later());
        assert!(with("`cargo test` fails").code);
        assert!(with("Cargo Test fails").code);
        assert!(!with("the test fails").code);
        assert!(!find("cargo test fails", project.path(), Some(home.path()), &[], &[], Reach::Search, later()).code);
    }

    #[test]
    fn common_identifiers_are_not_distinctive() {
        for plain in ["ideas", "tests", "render", "add", "readme", "curator"] {
            assert!(ident_of(plain).is_none(), "{plain}");
        }
        for ident in ["split_added", "pickIn", "StorePrep", "Foo::bar", "run()", "`add`", "nexo-tools"] {
            assert!(ident_of(ident).is_some(), "{ident}");
        }
        assert_eq!(ident_of("Foo::bar").unwrap().name, "bar");
        assert!(ident_of("nexo-tools").unwrap().dash);
        assert!(ident_of("e.g").is_none() && ident_of("0.2.0").is_none() && ident_of("a_b").is_none());
    }

    #[test]
    fn definitions_are_found_after_any_modifiers_and_in_go_methods() {
        assert!(is_def("pub(crate) async unsafe fn split_added(x: u8) {", "split_added"));
        assert!(is_def("    pub const LIMIT: u64 = 3;", "LIMIT"));
        assert!(is_def("export default function render() {", "render"));
        assert!(is_def("func (s *Srv) Handle(w http.ResponseWriter) {", "Handle"));
        assert!(is_def("data class Point(val x: Int)", "Point"));
        assert!(is_def("extern \"C\" fn start() {}", "start"));
        assert!(!is_def("let split_added = 1;", "split_added"));
        assert!(!is_def("// fn split_added() {}", "split_added"));
        assert!(!is_def("pub fn split_added_all() {}", "split_added"));
        let (project, home) = dirs();
        put(project.path(), "src/parse.rs", "use a;\n\npub(crate) async fn split_added() {}\n");
        let f = look("arregla split_added", project.path(), home.path());
        assert_eq!(f.defs.len(), 1);
        assert_eq!(f.defs[0].defs[0].shown, "src/parse.rs");
        assert_eq!(f.defs[0].defs[0].line, 3);
        assert!(f.code);
    }

    #[test]
    fn the_home_folder_as_project_is_not_walked_deep() {
        let (_, home) = dirs();
        put(home.path(), "deep/er/IDEAS.md", "x\n");
        put(home.path(), "top.md", "x\n");
        let f = find("update IDEAS.md and top.md", home.path(), Some(home.path()), &[], &[], Reach::Search, later());
        let real = home.path().canonicalize().unwrap();
        assert_eq!(shown(&f), [real.join("top.md").display().to_string()]);
        assert!(!f.code);
    }

    #[test]
    fn a_walk_stops_at_the_deadline() {
        let (project, home) = dirs();
        put(project.path(), "src/lib.rs", "pub fn add() {}\n");
        let words = vec!["cargo".to_string(), "test".to_string()];
        let passed = Instant::now() - Duration::from_secs(1);
        let f = find(
            "`cargo test` fails in lib.rs and nothere.md",
            project.path(),
            Some(home.path()),
            &[],
            &words,
            Reach::Search,
            passed,
        );
        assert!(f.files.is_empty());
        assert!(f.missing.is_empty());
        assert!(f.code);
        assert_eq!(f.sources, SOURCES_PAST);
    }

    #[test]
    fn a_panic_location_is_removed_from_a_path() {
        assert_eq!(cleaned("src/lib.rs:2:5"), "src/lib.rs");
        assert_eq!(cleaned("(src/lib.rs:12)."), "src/lib.rs");
        assert_eq!(cleaned("C:\\x"), "C:\\x");
    }

    #[test]
    fn noise_words_give_no_missing_line() {
        let (project, home) = dirs();
        put(project.path(), "README.md", "x\n");
        let f = look("and/or e.g. update 0.2.0 like Node.js does, see /goal", project.path(), home.path());
        assert!(f.missing.is_empty(), "{:?}", f.missing);
        let f = look("update nothere.md", project.path(), home.path());
        assert_eq!(f.missing.len(), 1);
        assert!(f.missing[0].starts_with("nothere.md: no file of that name"));
    }
}
