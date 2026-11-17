//! Paths as the rules see them: written and resolved forms, sensitive files, guarded folders.

use std::path::{Path, PathBuf};

use nexo_core::paths::real_path;
use nexo_core::tool::ToolKind;
use serde_json::Value;

use super::rules::target_of;

/// The path as the rules see it: relative to `cwd` when under it, else absolute; always with
/// `/` between the parts, as rules are written, on Windows too.
pub(super) fn path_forms(p: &str, cwd: &Path) -> Vec<String> {
    let abs = lexical(p, cwd);
    let slash = |s: String| s.replace('\\', "/");
    let mut out = vec![slash(abs.to_string_lossy().to_string())];
    if let Ok(rel) = abs.strip_prefix(normalise(cwd)) {
        out.push(slash(rel.to_string_lossy().to_string()));
    }
    // as written, unless it holds `..`: a glob crosses it, so `src/../.env` would pass as `src/**`
    if !Path::new(p.trim()).components().any(|c| c == std::path::Component::ParentDir) {
        out.push(slash(p.trim().to_string()));
    }
    out
}

/// The forms of the path with its links resolved, relative to the resolved `cwd` when under it.
pub(super) fn real_forms(p: &str, cwd: &Path) -> Vec<String> {
    let real = real_path(&lexical(p, cwd));
    let root = real_path(&normalise(cwd));
    let mut out = vec![rule_slash(&real)];
    if let Ok(rel) = real.strip_prefix(&root) {
        out.push(rule_slash(rel));
    }
    out
}

/// A path as rules are written: `/` between the parts, and without the `\\?\C:\...` (or
/// `\\?\UNC\host\...`) prefix Windows adds when it resolves one.
pub(super) fn rule_slash(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    match s.strip_prefix("//?/") {
        Some(rest) => rest.strip_prefix("UNC/").map(|r| format!("//{r}")).unwrap_or_else(|| rest.to_string()),
        None => s,
    }
}

/// An absolute pattern with the fixed folder it starts with resolved (links, and on Windows
/// 8.3 short names such as `RUNNER~1`), for matching a path's resolved forms; `None` when that
/// changes nothing. Only the folder the rule itself names is resolved: a link under it still
/// does not carry the rule to where it points.
pub(super) fn resolved_pattern(pattern: &str) -> Option<String> {
    let fixed = pattern.find(['*', '?', '[', '{']).unwrap_or(pattern.len());
    let cut = pattern[..fixed].rfind('/')?;
    let (dir, rest) = pattern.split_at(cut);
    if dir.is_empty() || !Path::new(dir).is_absolute() {
        return None;
    }
    let real = format!("{}{rest}", rule_slash(&real_path(Path::new(dir))));
    (real != pattern).then_some(real)
}

/// The path as the file tools resolve it: trimmed, `~` expanded, joined to `cwd`, `..` folded.
pub(super) fn lexical(p: &str, cwd: &Path) -> PathBuf {
    let p = p.trim();
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    let abs = match (p, home) {
        ("~", Some(h)) => h,
        (_, Some(h)) if p.starts_with("~/") => h.join(&p[2..]),
        _ if Path::new(p).is_absolute() => PathBuf::from(p),
        _ => cwd.join(p),
    };
    normalise(&abs)
}

/// A file that holds keys or credentials (`nexo_core::paths::holds_keys`: `.ssh`, `.aws`, `.env`
/// files, private keys, the macOS keychains and their kind, and nexo's own credential store).
/// Checked as written and with links resolved.
pub fn is_sensitive(p: &str, cwd: &Path) -> bool {
    let lex = lexical(p, cwd);
    let real = real_path(&lex);
    [&lex, &real].iter().any(|f| nexo_core::paths::holds_keys(f))
}

/// Folders whose files run something here later or change what an agent may do.
pub(super) const GUARDED_DIRS: &[&str] = &[".git", ".nexo", ".claude", ".codex", ".vscode", ".idea", ".husky"];
/// Shell start-up files and other single files with the same effect.
pub(super) const GUARDED_FILES: &[&str] = &[
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".bash_logout",
    ".profile",
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".mcp.json",
    ".envrc",
    ".gitconfig",
];

/// Whether `rel` has a guarded folder in it or is a guarded file; `fold` compares names without case, as a
/// case-insensitive disk (APFS by default, NTFS) does: `.GIT/hooks` is the folder `.git/hooks` there.
pub(super) fn guarded_in(rel: &Path, fold: bool) -> bool {
    let named = |list: &[&str], n: &str| list.iter().any(|g| if fold { g.eq_ignore_ascii_case(n) } else { *g == n });
    rel.components().any(|c| c.as_os_str().to_str().is_some_and(|n| named(GUARDED_DIRS, n)))
        || rel.file_name().and_then(|n| n.to_str()).is_some_and(|n| named(GUARDED_FILES, n))
}

/// accept-edits writes without asking only inside the project (links resolved), and never into
/// the guarded folders and files above or files that hold keys.
pub(super) fn editable(args: &Value, cwd: &Path) -> bool {
    let Some(p) = target_of(ToolKind::Write, args) else { return false };
    let (lex, root) = (lexical(&p, cwd), normalise(cwd));
    let (real, real_root) = (real_path(&lex), real_path(&root));
    let (Ok(rel), Ok(real_rel)) = (lex.strip_prefix(&root), real.strip_prefix(&real_root)) else { return false };
    let guarded = |r: &Path| guarded_in(r, nexo_core::paths::FOLD_CASE);
    !rel.as_os_str().is_empty() && !guarded(rel) && !guarded(real_rel) && !is_sensitive(&p, cwd)
}

pub(super) fn normalise(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// True when no link inside the project bends the path: it sits at the same place under the
/// resolved project as under the project as written (only the project's own folder may be a link).
pub(super) fn no_link_inside(p: &str, cwd: &Path) -> bool {
    let (lex, root) = (lexical(p, cwd), normalise(cwd));
    let (real, real_root) = (real_path(&lex), real_path(&root));
    matches!((lex.strip_prefix(&root), real.strip_prefix(&real_root)), (Ok(a), Ok(b)) if a == b)
}
