//! The files of one match: a config folder to depth 2 (or a dotfile), as rows with size and date, backups counted
//! once, and what must never be listed left out.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use nexo_core::paths::is_private;

/// The most children of one folder looked at.
const PER_DIR: usize = 300;

/// One file of a match.
pub struct Row {
    /// Its path below the matched folder (forward slashes), or its name for a dotfile.
    pub rel: String,
    /// Where the person has it.
    pub path: PathBuf,
    /// Where it really is (a link resolved).
    pub real: PathBuf,
    pub len: u64,
    pub modified: Option<SystemTime>,
    /// Its name looks secret: the row says the name only and the file is never attached.
    pub secret: bool,
    pub depth: usize,
}

/// A matched folder or dotfile with what is in it.
pub struct Group {
    pub path: PathBuf,
    pub dir: bool,
    pub rows: Vec<Row>,
    /// Backup files and folders, counted and not listed.
    pub backups: usize,
    /// Children past the limit of what is looked at.
    pub unseen: usize,
}

/// Whether `name` is a copy an editor or the person left behind.
pub fn backup(name: &str, dir: bool) -> bool {
    let n = name.to_ascii_lowercase();
    let copy = n.ends_with(".bak")
        || n.contains(".bak-")
        || n.ends_with(".orig")
        || n.ends_with(".old")
        || n.ends_with('~')
        || n.ends_with(".swp");
    copy || (dir && n.starts_with("backup"))
}

/// Whether a file of this name is taken for one that holds a secret.
pub fn secret_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "auth.json"
        || n.starts_with("credentials")
        || n.contains("token")
        || n.contains("secret")
        || [".key", ".pem", ".pfx", ".p12"].iter().any(|e| n.ends_with(e))
        || n == ".env"
        || n.starts_with(".env.")
        || n == ".netrc"
        || n == ".git-credentials"
}

/// The text of `bytes` when it is one: no NUL byte in the first 8 KB and valid UTF-8.
pub fn text_of(bytes: &[u8]) -> Option<&str> {
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

/// What `entry` holds: None when it is private, gone, or neither a file nor a folder.
pub fn group(entry: &Path) -> Option<Group> {
    let real = entry.canonicalize().ok()?;
    if is_private(&real) || is_private(entry) {
        return None;
    }
    let meta = std::fs::metadata(entry).ok()?;
    if meta.is_file() {
        let name = entry.file_name()?.to_string_lossy().into_owned();
        let row = row(name.clone(), entry, &real, &meta, &name, 1);
        return Some(Group { path: entry.to_path_buf(), dir: false, rows: vec![row], backups: 0, unseen: 0 });
    }
    if !meta.is_dir() {
        return None;
    }
    let mut g = Group { path: entry.to_path_buf(), dir: true, rows: Vec::new(), backups: 0, unseen: 0 };
    walk(entry, &real, "", 1, &mut g);
    g.rows.sort_by(|a, b| (a.depth, &a.rel).cmp(&(b.depth, &b.rel)));
    Some(g)
}

fn row(rel: String, path: &Path, real: &Path, meta: &std::fs::Metadata, name: &str, depth: usize) -> Row {
    Row {
        rel,
        path: path.to_path_buf(),
        real: real.to_path_buf(),
        len: meta.len(),
        modified: meta.modified().ok(),
        secret: secret_name(name) || is_private(path) || is_private(real),
        depth,
    }
}

/// The children of `dir` (at `depth` 1 the matched folder, 2 a folder in it): files become rows, backups are
/// counted, a link that leaves `base` (the real matched folder) or points at something private is left out, and
/// no link to a folder is followed.
fn walk(dir: &Path, base: &Path, prefix: &str, depth: usize, g: &mut Group) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut names: Vec<String> = read.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    if names.len() > PER_DIR {
        g.unseen += names.len() - PER_DIR;
        names.truncate(PER_DIR);
    }
    for name in names {
        let path = dir.join(&name);
        let Ok(link) = std::fs::symlink_metadata(&path) else { continue };
        let mut real = path.clone();
        if link.file_type().is_symlink() {
            let Ok(target) = path.canonicalize() else { continue };
            if !target.starts_with(base) || is_private(&target) {
                continue;
            }
            real = target;
        }
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        if meta.is_dir() {
            if is_private(&path) {
                continue;
            }
            if backup(&name, true) {
                g.backups += 1;
            } else if depth < 2 && !link.file_type().is_symlink() {
                walk(&path, base, &rel, depth + 1, g);
            }
        } else if meta.is_file() {
            if backup(&name, false) {
                g.backups += 1;
            } else {
                g.rows.push(row(rel, &path, &real, &meta, &name, depth));
            }
        }
    }
}

/// A size as a person reads it.
pub fn size(len: u64) -> String {
    match len {
        0..=1023 => format!("{len} B"),
        1024..=1_048_575 => format!("{:.1} KB", len as f64 / 1024.0),
        _ => format!("{:.1} MB", len as f64 / 1_048_576.0),
    }
}

/// A modified time as a date.
pub fn date(t: SystemTime) -> String {
    let d = time::OffsetDateTime::from(t);
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}
