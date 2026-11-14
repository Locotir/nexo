//! Globs, the home folder and paths: reading the paths and command out of a call, and telling whether a name leaves the session folder.

use std::path::{Component, Path, PathBuf};

use globset::GlobBuilder;
use serde_json::Value;

pub(super) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

pub(super) fn glob(pattern: &str, separators: bool) -> Option<globset::GlobMatcher> {
    GlobBuilder::new(pattern).literal_separator(separators).build().ok().map(|g| g.compile_matcher())
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
    !Path::new(&resolve(text, cwd, home)).starts_with(cwd)
}
