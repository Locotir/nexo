//! The machine map: where the person's programs keep their config, known before the model has to look.
//!
//! When the newest message of the person names a program or its config ("add a CPU temperature module to my
//! quickshell"), the first call of the request already carries where its files are, and the small ones whole, so the
//! model reads or edits at once instead of running `find` and `ls` first. The match is mechanical (no model call) and
//! the lines are data only.
//!
//! The index is built once per process in the background, never written to disk: the entry names at depth 1 of the
//! config and data folders and of the home folder's dotfiles, the programs in `PATH` and, where the system has a
//! package tool, the packages the person installed by hand. A request that starts before it is ready carries nothing
//! from it and never waits for it. Nothing private is listed, followed or attached (see
//! [`nexo_core::paths::is_private`]), and a file whose name looks secret is listed by name only.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use nexo_core::paths::is_private;

mod list;
mod sys;
#[cfg(test)]
mod tests;

use list::{Group, Row};

/// The most matches one message gets.
const MAX_HITS: usize = 3;
/// The shortest word that can match.
const MIN_WORD: usize = 3;
/// The most files one match lists before "and N more".
const MAX_ROWS: usize = 20;
/// The most characters of listing lines one request carries.
const LINE_CHARS: usize = 1_500;
/// The most bytes of files attached whole for one match.
const HIT_BYTES: u64 = 12 * 1024;
/// The most files one match reads while it picks what to attach.
const LOOKED_AT: usize = 60;
/// The room kept in the line cap for the "and N more" line.
const MORE_LINE: usize = 20;

/// Names too ordinary to say a program: a word of the person's that equals one is no hit.
const GENERIC: &[&str] = &[
    "config",
    "local",
    "share",
    "cache",
    "state",
    "data",
    "bin",
    "lib",
    "applications",
    "icons",
    "fonts",
    "themes",
    "mime",
    "trash",
    "test",
    "tests",
    "backup",
    "backups",
    "default",
    "user",
    "users",
    "home",
    "desktop",
    "templates",
    "public",
    "documents",
    "downloads",
    "pictures",
    "music",
    "videos",
    "history",
];

/// Home folder entries that are no program's: the folders the others live in, and files of the session or its history.
fn skipped_in_home(lower: &str) -> bool {
    matches!(
        lower,
        ".config" | ".local" | ".cache" | ".var" | ".trash" | ".xauthority" | ".iceauthority" | ".viminfo" | ".lesshst"
    ) || lower.starts_with(".xsession")
        || lower.ends_with("history")
        || lower.ends_with("_hist")
}

/// Where the index looks: the one place a test points at a folder of its own.
pub struct Roots {
    pub home: PathBuf,
    /// Folders whose entries (depth 1) are programs' config or data: `~/.config`, `~/.local/share`, ...
    pub config_dirs: Vec<PathBuf>,
    /// The folders of `PATH`.
    pub path_dirs: Vec<PathBuf>,
    /// The packages the person installed by hand (lowercase), when the system can say.
    pub packages: Option<Vec<String>>,
    /// Whether the package that owns a program is asked for.
    pub owners: bool,
}

impl Roots {
    /// The roots of this machine, with the package list asked for now (two seconds at most).
    pub fn system() -> Option<Roots> {
        let home = nexo_core::paths::own_home()?;
        let env_dir = |v: &str| std::env::var_os(v).map(PathBuf::from).filter(|p| p.is_absolute());
        let mut config_dirs = vec![
            env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config")),
            env_dir("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local").join("share")),
        ];
        if cfg!(target_os = "macos") {
            config_dirs.push(home.join("Library").join("Application Support"));
        }
        if cfg!(windows) {
            config_dirs.extend(["APPDATA", "LOCALAPPDATA"].iter().filter_map(|v| env_dir(v)));
        }
        let mut seen = HashSet::new();
        config_dirs.retain(|d| seen.insert(d.canonicalize().unwrap_or_else(|_| d.clone())));
        let path_dirs: Vec<PathBuf> =
            std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        let packages = sys::installed(&path_dirs);
        let owners = sys::tool(&path_dirs).is_some();
        Some(Roots { home, config_dirs, path_dirs, packages, owners })
    }
}

/// The names the machine has, to match the words of a message against.
pub struct Index {
    home: PathBuf,
    /// The config folders and dotfiles, by every name a word can match them with.
    by_key: HashMap<String, Vec<PathBuf>>,
    /// The programs in `PATH`: name (lowercase, no `.exe`) to the first path.
    programs: HashMap<String, PathBuf>,
    packages: Option<HashSet<String>>,
    owner: Option<sys::Tool>,
}

/// The names a word can match an entry with: its own, without the dot of a dotfile, without a trailing `rc` or
/// `config` of a dotfile (`zsh` for `.zshrc`, `git` for `.gitconfig`), and a file without its extension.
fn keys_of(name: &str, file: bool) -> Vec<String> {
    let lower = name.to_lowercase();
    let mut keys = vec![lower.clone()];
    let dotted = lower.strip_prefix('.').filter(|b| !b.is_empty());
    let base = dotted.unwrap_or(&lower).to_string();
    if dotted.is_some() {
        keys.push(base.clone());
    }
    let mut bases = vec![base.clone()];
    if file {
        if let Some((stem, _)) = base.rsplit_once('.').filter(|(s, _)| !s.is_empty()) {
            keys.push(stem.to_string());
            bases.push(stem.to_string());
        }
    }
    if dotted.is_some() {
        for b in &bases {
            for tail in ["rc", "config"] {
                if let Some(s) = b.strip_suffix(tail).filter(|s| !s.is_empty()) {
                    keys.push(s.to_string());
                }
            }
        }
    }
    keys.retain(|k| k.chars().count() >= MIN_WORD && !GENERIC.contains(&k.as_str()));
    let mut seen = HashSet::new();
    keys.retain(|k| seen.insert(k.clone()));
    keys
}

/// Whether `path` is a program the system runs.
fn runs(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        path.extension()
            .is_some_and(|e| ["exe", "cmd", "bat", "com"].contains(&e.to_string_lossy().to_lowercase().as_str()))
    }
}

impl Index {
    /// The index of `roots`: a few thousand names, in about 200 ms on a big machine.
    pub fn build(roots: &Roots) -> Index {
        let mut index = Index {
            home: roots.home.clone(),
            by_key: HashMap::new(),
            programs: HashMap::new(),
            packages: roots.packages.as_ref().map(|p| p.iter().cloned().collect()),
            owner: if roots.owners { sys::tool(&roots.path_dirs) } else { None },
        };
        index.add_root(&roots.home, true);
        for d in &roots.config_dirs {
            index.add_root(d, false);
        }
        for dir in &roots.path_dirs {
            let Ok(read) = std::fs::read_dir(dir) else { continue };
            for e in read.flatten() {
                let path = e.path();
                if !runs(&path) {
                    continue;
                }
                let mut name = e.file_name().to_string_lossy().to_lowercase();
                if let Some(bare) = [".exe", ".cmd", ".bat", ".com"].iter().find_map(|x| name.strip_suffix(x)) {
                    name = bare.to_string();
                }
                index.programs.entry(name).or_insert(path);
            }
        }
        index
    }

    /// The entries (depth 1) of `root`: of the home folder only its dotfiles and dot-folders.
    fn add_root(&mut self, root: &Path, dots_only: bool) {
        let Ok(read) = std::fs::read_dir(root) else { return };
        for e in read.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            if (dots_only && (!name.starts_with('.') || skipped_in_home(&lower))) || name.is_empty() {
                continue;
            }
            let path = root.join(&name);
            if is_private(&path) {
                continue;
            }
            if let Ok(link) = std::fs::symlink_metadata(&path) {
                if link.file_type().is_symlink() && !path.canonicalize().is_ok_and(|r| !is_private(&r)) {
                    continue;
                }
            }
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            if !meta.is_file() && !meta.is_dir() {
                continue;
            }
            for key in keys_of(&name, meta.is_file()) {
                self.by_key.entry(key).or_default().push(path.clone());
            }
        }
    }

    /// How many names the index holds (config entries and programs).
    pub fn len(&self) -> usize {
        self.by_key.len() + self.programs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// What the machine has for the words of `text`: at most [`MAX_HITS`] matches, the longest name first.
    pub fn find(&self, text: &str) -> Vec<Found> {
        let mut hits: Vec<(String, Vec<PathBuf>, Option<PathBuf>)> = Vec::new();
        for word in candidates(text) {
            let entries = self.by_key.get(&word).cloned().unwrap_or_default();
            let program = self.programs.get(&word).cloned();
            // a program alone says nothing (`sort`, `find`, `make`) unless the person installed a package of that name
            if entries.is_empty() && !(program.is_some() && self.packages.as_ref().is_some_and(|p| p.contains(&word))) {
                continue;
            }
            hits.push((word, entries, program));
        }
        hits.sort_by(|a, b| b.0.chars().count().cmp(&a.0.chars().count()).then_with(|| a.0.cmp(&b.0)));
        let mut used: HashSet<PathBuf> = HashSet::new();
        let mut out = Vec::new();
        for (word, entries, program) in hits {
            // the same files under another word (`zsh` and `zshrc`) are one match
            let fresh = entries.iter().any(|e| !used.contains(e))
                || (entries.is_empty() && program.as_ref().is_some_and(|p| !used.contains(p)));
            if !fresh {
                continue;
            }
            used.extend(entries.iter().cloned());
            used.extend(program.iter().cloned());
            if let Some(f) = self.found(&word, &entries, program.as_deref()) {
                out.push(f);
            }
            if out.len() == MAX_HITS {
                break;
            }
        }
        out
    }

    fn found(&self, label: &str, entries: &[PathBuf], program: Option<&Path>) -> Option<Found> {
        let groups: Vec<Group> = entries.iter().filter_map(|e| list::group(e)).collect();
        if groups.is_empty() && program.is_none() {
            return None;
        }
        let owner = program.and_then(|p| self.owner.and_then(|t| sys::owner(t, p)));
        let shown = |p: &Path| nexo_core::paths::shown_from(p, Some(&self.home));
        let mut head = format!("{label}:");
        if let Some(p) = program {
            head.push(' ');
            head.push_str(&shown(p));
            if let Some(o) = &owner {
                head.push_str(&format!(" (package {o})"));
            }
        }
        let mut lines = vec![head];
        let mut left = MAX_ROWS;
        let mut more = 0;
        for g in &groups {
            if g.dir {
                lines.push(format!("{}/", shown(&g.path)));
            }
            for row in &g.rows {
                if left == 0 {
                    more += 1;
                    continue;
                }
                left -= 1;
                let name = if g.dir { format!("  {}", row.rel) } else { shown(&g.path) };
                lines.push(match (row.secret, row.modified) {
                    (true, _) => name,
                    (false, Some(t)) => format!("{name}  {}  {}", list::size(row.len), list::date(t)),
                    (false, None) => format!("{name}  {}", list::size(row.len)),
                });
            }
            more += g.unseen;
            if g.backups > 0 {
                lines.push(format!("  {}", nexo_core::count(g.backups, "backup")));
            }
        }
        if more > 0 {
            lines.push(format!("  and {more} more"));
        }
        let mut files = Vec::new();
        for g in &groups {
            for r in g.rows.iter().filter(|r| !r.secret && r.len > 0 && r.len <= HIT_BYTES) {
                files.push(Candidate { path: r.path.clone(), real: r.real.clone(), len: r.len });
            }
        }
        files.sort_by(|a, b| a.len.cmp(&b.len).then_with(|| a.path.cmp(&b.path)));
        // two names for one file (a link inside the folder) carry it once
        let mut seen_real = HashSet::new();
        files.retain(|c| seen_real.insert(c.real.clone()));
        Some(Found { key: label.to_string(), lines, files, print: print(&groups, program) })
    }
}

/// The words of `text`, lowercase, split on anything but letters, digits, `-`, `_` and `.`, and each pair of
/// neighbours joined (`quick shell` gives `quickshell`): the names a message can say, at least [`MIN_WORD`] long.
fn candidates(text: &str) -> Vec<String> {
    let words: Vec<String> = text
        .to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .map(|w| w.trim_matches(['-', '_', '.']).to_string())
        .filter(|w| !w.is_empty())
        .collect();
    let mut all = words.clone();
    all.extend(words.windows(2).map(|w| format!("{}{}", w[0], w[1])));
    let mut seen = HashSet::new();
    all.retain(|w| w.chars().count() >= MIN_WORD && seen.insert(w.clone()));
    all
}

/// What the files of a match were: names, sizes and times, so a change shows.
fn print(groups: &[Group], program: Option<&Path>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    program.hash(&mut h);
    for g in groups {
        g.path.hash(&mut h);
        g.backups.hash(&mut h);
        g.unseen.hash(&mut h);
        for Row { rel, len, modified, .. } in &g.rows {
            (rel, len, modified).hash(&mut h);
        }
    }
    h.finish()
}

/// A file of a match that may be attached whole.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub path: PathBuf,
    pub real: PathBuf,
    pub len: u64,
}

/// One match: what the request says of it, and the files it may carry.
#[derive(Clone, Debug)]
pub struct Found {
    /// The word that matched.
    pub key: String,
    /// The listing, the first line naming the match.
    pub lines: Vec<String>,
    /// The small files, smallest first.
    pub files: Vec<Candidate>,
    /// What the files were when the listing was made.
    print: u64,
}

/// What a request carries from the map.
#[derive(Debug, Default)]
pub struct Mapped {
    /// One block of lines per match shown.
    pub lines: Vec<String>,
    /// The files to attach whole.
    pub files: Vec<PathBuf>,
}

/// The tokens a file of `len` bytes takes attached whole: the estimate of the first-context budget (four bytes a
/// token, plus its header and fence).
fn file_tokens(len: u64) -> u64 {
    len / 4 + 12
}

/// The map of a session: the shared index, and what this session showed already.
pub struct MachineMap {
    index: Arc<OnceLock<Index>>,
    /// What each match looked like when it was shown (see [`Found::print`]).
    shown: HashMap<String, u64>,
}

impl MachineMap {
    /// The map of this process: its index is built in the background the first time, and shared after. `on` is false
    /// where the request carries nothing of the machine (no attached files, a hermetic run); `NEXO_MACHINE_MAP=0`
    /// turns it off too. Unit tests build their own (a test never reads the real home).
    pub fn start(on: bool) -> MachineMap {
        #[cfg(test)]
        {
            let _ = on;
            MachineMap::pending()
        }
        #[cfg(not(test))]
        {
            if !on || std::env::var_os("NEXO_MACHINE_MAP").is_some_and(|v| v == "0" || v == "off") {
                return MachineMap::pending();
            }
            static PROCESS: OnceLock<Arc<OnceLock<Index>>> = OnceLock::new();
            let cell = PROCESS.get_or_init(|| {
                let cell = Arc::new(OnceLock::new());
                let into = cell.clone();
                let _ = std::thread::Builder::new().name("machine-map".into()).spawn(move || {
                    if let Some(roots) = Roots::system() {
                        let _ = into.set(Index::build(&roots));
                    }
                });
                cell
            });
            MachineMap { index: cell.clone(), shown: HashMap::new() }
        }
    }

    /// A map whose index is not there (yet): it finds nothing.
    pub fn pending() -> MachineMap {
        MachineMap { index: Arc::new(OnceLock::new()), shown: HashMap::new() }
    }

    /// A map over a built index.
    pub fn with(index: Index) -> MachineMap {
        let cell = OnceLock::new();
        let _ = cell.set(index);
        MachineMap { index: Arc::new(cell), shown: HashMap::new() }
    }

    /// The index, to find on a thread that may block.
    pub fn cell(&self) -> Arc<OnceLock<Index>> {
        self.index.clone()
    }

    /// What the index has for `text`; nothing, at once, while it is not built.
    pub fn found(&self, text: &str) -> Vec<Found> {
        self.index.get().map(|i| i.find(text)).unwrap_or_default()
    }

    /// Takes back what was shown: the conversation it was shown in is gone (cleared, rewound, compacted).
    pub fn forget(&mut self) {
        self.shown.clear();
    }

    /// What the request carries of `found`, within `tokens` for the lines and the files together: the matches not
    /// shown before, or changed since, as lines of at most [`LINE_CHARS`] characters in all, and the small text
    /// files of each, smallest first, up to [`HIT_BYTES`] a match. `allowed` is asked about each file (its path as
    /// the person has it, and as it really is): one the model has already, or the policy keeps out, is not attached.
    pub fn attach(&mut self, found: Vec<Found>, tokens: u64, allowed: &dyn Fn(&Path, &Path) -> bool) -> Mapped {
        let mut out = Mapped::default();
        let mut chars = 0usize;
        let mut spent = 0u64;
        for f in found {
            if self.shown.get(&f.key) == Some(&f.print) {
                continue;
            }
            let block = fit(&f.lines, LINE_CHARS.saturating_sub(chars));
            let cost = block.len() as u64 / 4 + 1;
            if block.is_empty() || spent + cost > tokens {
                break;
            }
            chars += block.len() + 1;
            spent += cost;
            let mut bytes = 0u64;
            for c in f.files.iter().take(LOOKED_AT) {
                if bytes + c.len > HIT_BYTES || spent + file_tokens(c.len) > tokens {
                    break;
                }
                if !allowed(&c.path, &c.real) || is_private(&c.real) {
                    continue;
                }
                let Ok(data) = std::fs::read(&c.real) else { continue };
                if data.len() as u64 != c.len || list::text_of(&data).is_none() {
                    continue;
                }
                bytes += c.len;
                spent += file_tokens(c.len);
                out.files.push(c.path.clone());
            }
            out.lines.push(block);
            self.shown.insert(f.key, f.print);
        }
        out
    }
}

/// `lines` joined within `room` characters: the first line always (when it fits), then as many as leave room for
/// "and N more".
fn fit(lines: &[String], room: usize) -> String {
    let mut used = 0usize;
    let mut kept: Vec<&str> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let reserve = if i + 1 < lines.len() { MORE_LINE } else { 0 };
        if used + l.len() + 1 + reserve > room {
            break;
        }
        used += l.len() + 1;
        kept.push(l);
    }
    if kept.is_empty() {
        return String::new();
    }
    let mut text = kept.join("\n");
    if kept.len() < lines.len() {
        text.push_str(&format!("\n  and {} more", lines.len() - kept.len()));
    }
    text
}
