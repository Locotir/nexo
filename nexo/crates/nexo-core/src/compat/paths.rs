//! Where a project starts and where the other tools keep their files.

use std::path::{Path, PathBuf};

/// The nearest ancestor of `cwd` with a `.git` entry below the person's home, else `cwd` itself.
pub fn project_root(cwd: &Path) -> PathBuf {
    project_root_with(cwd, dirs::home_dir().as_deref())
}

/// Where scratch folders go: `dir` (on disk, under a build's target dir) unless it lies inside a git
/// repository, else the system temp dir. A test folder inside the checkout it was built from would be taken
/// for that project: its git line, its worktrees, its instruction files.
pub fn scratch_base(dir: &Path) -> PathBuf {
    match project_root(dir).join(".git").exists() {
        true => std::env::temp_dir(),
        false => dir.to_path_buf(),
    }
}

/// [`project_root`] below the person's home `own_home`.
pub(super) fn project_root_with(cwd: &Path, own_home: Option<&Path>) -> PathBuf {
    for d in cwd.ancestors() {
        if home_stop(d, cwd, own_home) {
            break;
        }
        if d.join(".git").exists() {
            return d.to_path_buf();
        }
    }
    cwd.to_path_buf()
}

/// Whether the walk up from `cwd` stops at `d` as the person's home (`own_home`): it does, kept in git or
/// not (as for skills), unless the person works in the home itself and it is a real repository (a
/// container whose home is the project); a stray `.git` folder makes none.
pub(super) fn home_stop(d: &Path, cwd: &Path, own_home: Option<&Path>) -> bool {
    let repo = d.join(".git").is_file() || d.join(".git/HEAD").is_file();
    crate::paths::is_home(d, own_home) && !(d == cwd && repo)
}

/// Where `~/.claude` and `~/.codex` are looked for: `NEXO_COMPAT_HOME` when set (empty turns the
/// lookup off), else the person's home. A custom `NEXO_CONFIG_DIR` marks an isolated setup (tests,
/// containers): then the other tools' user folders are not read unless `NEXO_COMPAT_HOME` says so.
pub fn foreign_home() -> Option<PathBuf> {
    match std::env::var_os("NEXO_COMPAT_HOME") {
        Some(v) if v.is_empty() => None,
        Some(v) => Some(PathBuf::from(v)),
        None if std::env::var_os("NEXO_CONFIG_DIR").is_some_and(|v| !v.is_empty()) => None,
        None => dirs::home_dir(),
    }
}
