//! What the tools share: argument reading, path resolution, text limits.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// A string argument, trimmed; `None` when absent, not a string, or empty.
pub fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// A non-negative integer argument (a number or a numeric string).
pub fn int_arg(args: &Value, key: &str) -> Option<u64> {
    match args.get(key) {
        Some(Value::Number(n)) => n.as_u64().or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
}

pub fn bool_arg(args: &Value, key: &str) -> bool {
    match args.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1"),
        _ => false,
    }
}

/// `p` against `cwd`: absolute paths as they are, `~` expanded, the rest joined to `cwd`.
pub fn resolve(cwd: &Path, p: &str) -> PathBuf {
    let p = p.trim();
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    if p == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    if cfg!(windows) {
        if let Some(native) = from_git_bash(p, &std::env::temp_dir()) {
            return normalize(&native);
        }
    }
    let path = Path::new(p);
    if path.is_absolute() {
        normalize(path)
    } else {
        normalize(&cwd.join(path))
    }
}

/// A path as Git Bash prints it, in Windows form: `/c/Users/x` is `C:/Users/x`, `/tmp/x` is
/// under `temp`. The model copies these from `pwd` and `ls` into the file tools.
pub fn from_git_bash(p: &str, temp: &Path) -> Option<PathBuf> {
    let rest = p.strip_prefix('/')?;
    let (head, tail) = rest.split_once('/').unwrap_or((rest, ""));
    if head == "tmp" {
        return Some(temp.join(tail));
    }
    match head.as_bytes() {
        [d] if d.is_ascii_alphabetic() => Some(PathBuf::from(format!("{}:/{tail}", d.to_ascii_uppercase() as char))),
        _ => None,
    }
}

/// Removes `.` and `..` components without touching the file system.
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// `path` relative to `cwd` when inside it, else absolute.
pub fn display_path(cwd: &Path, path: &Path) -> String {
    match path.strip_prefix(cwd) {
        Ok(rel) if !rel.as_os_str().is_empty() => nexo_core::shell::slashed(rel),
        Ok(_) => ".".to_string(),
        Err(_) => nexo_core::shell::slashed(path),
    }
}

/// `" (outside the project)"` when `path` is not under `cwd`, and
/// `" (a link outside the project)"` when it is, but the file it names is
/// not (a symbolic link in the tree). Nothing is blocked: the note is for
/// the person deciding whether to allow the call.
pub fn outside_note(cwd: &Path, path: &Path) -> &'static str {
    let inside = path.starts_with(cwd);
    let real_cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    // the file itself, or (for one being created) its directory
    let real = std::fs::canonicalize(path).or_else(|_| match path.parent() {
        Some(d) => std::fs::canonicalize(d).map(|d| d.join(path.file_name().unwrap_or_default())),
        None => Err(std::io::Error::other("no parent")),
    });
    let really_inside = match real {
        Ok(r) => r.starts_with(&real_cwd),
        Err(_) => inside,
    };
    match (inside, really_inside) {
        (true, true) => "",
        (true, false) => " (a link outside the project)",
        (false, _) => " (outside the project)",
    }
}

/// Bytes a file may have before the file tools refuse to load it whole.
pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// `s` cut to `max` characters: the head and the tail with a marker between.
pub fn cap_text(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let half = max / 2;
    let head: String = s.chars().take(half).collect();
    let tail: String = s.chars().skip(n - half).collect();
    format!("{head}\n\n… [{} characters omitted] …\n\n{tail}", n - 2 * half)
}

/// The first `max` characters of a line, with an ellipsis when cut.
pub fn cut_line(line: &str, max: usize) -> String {
    if line.chars().count() <= max {
        line.to_string()
    } else {
        let mut s: String = line.chars().take(max.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}

/// Whether `bytes` look like a binary file (a NUL in the first 8 KiB).
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|b| *b == 0)
}

/// Writes `data` to `path` whole or not at all: into a new file beside it, then renamed over it
/// (`rename` replaces an existing file on Windows too). A link is followed, so the file it points
/// to is the one replaced; an existing file keeps its permissions, and a read-only one is refused.
pub async fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::io::AsyncWriteExt;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    // a link is followed by hand first, so a dangling one (canonicalize fails) is not replaced
    let mut target = path.to_path_buf();
    for _ in 0..40 {
        match tokio::fs::symlink_metadata(&target).await {
            Ok(m) if m.file_type().is_symlink() => {
                let link = tokio::fs::read_link(&target).await?;
                target = target.parent().map(|d| d.join(&link)).unwrap_or(link);
            }
            _ => break,
        }
    }
    let target = tokio::fs::canonicalize(&target).await.unwrap_or(target);
    let perms = tokio::fs::metadata(&target).await.ok().map(|m| m.permissions());
    if perms.as_ref().is_some_and(|p| p.readonly()) {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "the file is read-only"));
    }
    let dir = target.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.nexo-{}-{n}.tmp", std::process::id()));
    let written = async {
        let mut f = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).await?;
        f.write_all(data).await?;
        f.sync_all().await?;
        drop(f);
        if let Some(p) = perms {
            tokio::fs::set_permissions(&tmp, p).await?;
        }
        tokio::fs::rename(&tmp, &target).await
    }
    .await;
    if written.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    written
}

/// The first line of a command, cut for a summary.
pub fn one_line(s: &str, max: usize) -> String {
    let first = s.trim().lines().next().unwrap_or("").trim();
    let more = s.trim().lines().count() > 1;
    let mut out = cut_line(first, max);
    if more && !out.ends_with('…') {
        out.push_str(" …");
    }
    out
}

/// Runs a test's async body on its own runtime with a time limit: past it the test fails with
/// its name instead of hanging. The runtime is then shut down without waiting for the blocking
/// pool: on Windows tokio reads a child's pipes in blocking threads, and a plain drop of the
/// runtime would wait for each of them until the child (or a grandchild that inherited the pipe)
/// ends, which for a language server kept in a static or a `sleep 60` is a hang, not a failure.
#[cfg(test)]
pub(crate) fn run_bounded<F: std::future::Future>(limit: std::time::Duration, name: &str, body: F) -> F::Output {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
    let out = rt.block_on(async { tokio::time::timeout(limit, body).await });
    rt.shutdown_background();
    out.unwrap_or_else(|_| panic!("{name}: still running after {} s", limit.as_secs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_against_cwd_and_normalises() {
        let cwd = Path::new("/w/p");
        assert_eq!(resolve(cwd, "a/b.rs"), PathBuf::from("/w/p/a/b.rs"));
        assert_eq!(resolve(cwd, "./a/../b.rs"), PathBuf::from("/w/p/b.rs"));
        assert_eq!(resolve(cwd, "/etc/hosts"), PathBuf::from("/etc/hosts"));
        assert_eq!(display_path(cwd, Path::new("/w/p/a/b.rs")), "a/b.rs");
        assert_eq!(display_path(cwd, Path::new("/etc/hosts")), "/etc/hosts");
        assert_eq!(outside_note(cwd, Path::new("/etc/hosts")), " (outside the project)");
        assert_eq!(outside_note(cwd, Path::new("/w/p/x")), "");
    }

    #[test]
    fn git_bash_paths_become_windows_paths() {
        let temp = Path::new("T:/temp");
        assert_eq!(from_git_bash("/c/Users/me/a.rs", temp), Some(PathBuf::from("C:/Users/me/a.rs")));
        assert_eq!(from_git_bash("/d", temp), Some(PathBuf::from("D:/")));
        assert_eq!(from_git_bash("/tmp/x/y", temp), Some(temp.join("x/y")));
        assert_eq!(from_git_bash("/etc/hosts", temp), None);
        assert_eq!(from_git_bash("src/a.rs", temp), None);
        assert_eq!(from_git_bash("C:/x", temp), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_that_leaves_the_project_is_said_so() {
        let proj = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "s").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), proj.path().join("link.txt")).unwrap();
        std::fs::write(proj.path().join("own.txt"), "o").unwrap();
        assert_eq!(outside_note(proj.path(), &proj.path().join("link.txt")), " (a link outside the project)");
        assert_eq!(outside_note(proj.path(), &proj.path().join("own.txt")), "");
        assert_eq!(outside_note(proj.path(), &proj.path().join("new.txt")), "", "a file to be created in the tree");
        assert_eq!(outside_note(proj.path(), &outside.path().join("secret.txt")), " (outside the project)");
    }

    #[test]
    fn caps_text_keeping_head_and_tail() {
        let s: String = (0..100).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
        let c = cap_text(&s, 20);
        assert!(c.starts_with("abcdefghij"));
        assert!(c.ends_with(&s[90..]));
        assert!(c.contains("80 characters omitted"));
        assert_eq!(cap_text("short", 20), "short");
        assert_eq!(cut_line("abcdef", 4), "abc…");
        assert_eq!(one_line("ls -la\ncd x", 80), "ls -la …");
    }

    #[test]
    fn reads_arguments_leniently() {
        let a = serde_json::json!({"path": " a.rs ", "limit": "5", "n": 3.0, "all": "yes", "b": true});
        assert_eq!(str_arg(&a, "path"), Some("a.rs"));
        assert_eq!(str_arg(&a, "missing"), None);
        assert_eq!(int_arg(&a, "limit"), Some(5));
        assert_eq!(int_arg(&a, "n"), Some(3));
        assert!(bool_arg(&a, "all") && bool_arg(&a, "b") && !bool_arg(&a, "path"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_atomic_keeps_a_dangling_link_and_writes_its_target() {
        let d = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("real.txt", d.path().join("link")).unwrap();
        write_atomic(&d.path().join("link"), b"hi").await.unwrap();
        assert!(std::fs::symlink_metadata(d.path().join("link")).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(d.path().join("real.txt")).unwrap(), "hi");
    }
}
