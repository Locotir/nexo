//! Globs, the home folder and paths: reading the paths and command out of a call, and telling whether a name leaves the session folder.

use std::path::{Component, Path, PathBuf};

use globset::GlobBuilder;
use serde_json::Value;

pub(super) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

/// `separators` marks a file name pattern: on macOS and Windows the volume folds its case. A command or tool pattern
/// never folds, so `git branch -d *` approved by the person does not cover `-D`.
pub(super) fn glob(pattern: &str, separators: bool) -> Option<globset::GlobMatcher> {
    GlobBuilder::new(pattern)
        .literal_separator(separators)
        .case_insensitive(separators && nexo_core::paths::FOLD_CASE)
        .build()
        .ok()
        .map(|g| g.compile_matcher())
}

pub(super) fn matches(pattern: &str, text: &str, separators: bool) -> bool {
    glob(pattern, separators).is_some_and(|g| g.is_match(text))
}

pub(super) fn expand_home(pattern: &str, home: Option<&Path>) -> String {
    match (pattern.strip_prefix("~/"), home) {
        (Some(rest), Some(h)) => format!("{}/{rest}", h.to_string_lossy().trim_end_matches('/')),
        _ => pattern.to_string(),
    }
}

// ---- reading a call ----

pub(super) fn bash_command<'a>(tool: &str, args: &'a Value) -> Option<&'a str> {
    let t = tool.to_ascii_lowercase();
    if !matches!(t.as_str(), "bash" | "shell" | "exec" | "run_command" | "powershell") {
        return None;
    }
    ["command", "cmd", "script"].iter().find_map(|k| args.get(*k).and_then(Value::as_str))
}

pub(super) fn path_args(args: &Value) -> Vec<&str> {
    let many = args.get("paths").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str);
    ["file_path", "path", "notebook_path", "directory", "dir", "file"]
        .iter()
        .filter_map(|k| args.get(*k).and_then(Value::as_str))
        .chain(many)
        .collect()
}

pub(super) fn is_write_tool(tool: &str) -> bool {
    matches!(
        tool.to_ascii_lowercase().as_str(),
        "write" | "edit" | "multiedit" | "notebookedit" | "write_file" | "edit_file" | "apply_patch"
    )
}

// ---- paths ----

/// `p` with `.` and `..` folded away, no file system involved. A relative `p` is taken from `base`.
pub(super) fn normalize(p: &Path, base: &Path) -> PathBuf {
    let joined = if p.is_absolute() { p.to_path_buf() } else { base.join(p) };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `text` as an absolute path with the home folder and `..` resolved.
pub(super) fn resolve(text: &str, cwd: &Path, home: Option<&Path>) -> String {
    let text = expand_home(text, home);
    let text = match (text.as_str(), home) {
        ("~", Some(h)) => h.to_string_lossy().into_owned(),
        _ => text,
    };
    normalize(Path::new(&text), cwd).to_string_lossy().into_owned()
}

/// Whether `text` names somewhere outside `cwd`. A name built from a variable cannot be known: outside.
pub(super) fn outside(text: &str, cwd: &Path, home: Option<&Path>) -> bool {
    if matches!(text, "/dev/null" | "/dev/stdout" | "/dev/stderr") {
        return false;
    }
    if text.contains('$') || text.contains('`') {
        return true;
    }
    let path = PathBuf::from(resolve(text, cwd, home));
    if path.starts_with(cwd) {
        return false;
    }
    // One folder can have two names: on macOS `/tmp` and `/var` are links into `/private`, so the
    // session folder comes back as `/private/tmp/p` while a file in it is written `/tmp/p/a`.
    // Compared again with the folders' links resolved, as far as they exist. The last name stays as
    // written: a link there is the thing named (`rm -rf ~/link` removes the link, wherever it points).
    let real = nexo_core::paths::real_path;
    let named = match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) => real(dir).join(name),
        _ => real(&path),
    };
    !named.starts_with(real(cwd))
}
