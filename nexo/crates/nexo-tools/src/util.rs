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
    if let Some(rest) = home_prefix_rest(p, cfg!(windows)) {
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

/// Text saved as UTF-16, which `is_binary` takes for a binary file because every other byte is a NUL
/// (Windows `.rc` and `.reg` files, PowerShell output, some `.csv`). Returns the decoded text, whether
/// the units are little-endian, and whether the file began with a byte order mark (not part of the text).
/// A file without a mark counts only when the first 8 KiB have a NUL in every high byte, so Latin-1
/// range text in UTF-16 is found and a real binary is not. Anything with a control character other than
/// whitespace in the first 8 KiB, an odd length or an unpaired surrogate is not text, and is None.
pub fn utf16_text(bytes: &[u8]) -> Option<(String, bool, bool)> {
    let (le, bom) = match bytes {
        [0xFF, 0xFE, ..] => (true, true),
        [0xFE, 0xFF, ..] => (false, true),
        _ => {
            let head = &bytes[..bytes.len().min(8192) & !1];
            let high = |k: usize| head.iter().skip(k).step_by(2).all(|b| *b == 0);
            match (head.len() >= 2, high(1), high(0)) {
                (true, true, false) => (true, false),
                (true, false, true) => (false, false),
                _ => return None,
            }
        }
    };
    let body = &bytes[if bom { 2 } else { 0 }..];
    if !body.len().is_multiple_of(2) {
        return None;
    }
    let unit = |c: &[u8; 2]| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) };
    // a NUL or another control byte means a binary that happens to start like UTF-16 (UTF-32 does)
    let control = |u: u16| u < 0x20 && !matches!(u, 9 | 10 | 12 | 13);
    if body.as_chunks::<2>().0.iter().take(4096).map(unit).any(control) {
        return None;
    }
    let text: String = char::decode_utf16(body.as_chunks::<2>().0.iter().map(unit)).collect::<Result<_, _>>().ok()?;
    Some((text, le, bom))
}

/// The lines of a file that were cut to a line length: the first, and how many in all.
#[derive(Default)]
pub struct Cuts {
    /// Line number and length in characters of the first line cut.
    first: Option<(usize, usize)>,
    count: usize,
}

impl Cuts {
    /// Notes that line `number` (1-based) was shown through `cut_line(line, max)`; call it only for
    /// lines that were put in the answer.
    pub fn seen(&mut self, number: usize, line: &str, max: usize) {
        // `nth` stops after `max + 1` characters, so a long line is counted in full only the first time
        if line.chars().nth(max).is_none() {
            return;
        }
        self.count += 1;
        self.first.get_or_insert_with(|| (number, line.chars().count()));
    }

    /// The fact for the end of the answer, when a line was cut.
    pub fn note(&self, max: usize) -> Option<String> {
        let (number, len) = self.first?;
        let all = if self.count > 1 { format!(" ({} lines cut in all)", self.count) } else { String::new() };
        Some(format!(
            "line {number} cut at {max} of {len} chars{all}; `bash` with `grep -o`, `cut -c` or `sed -n` shows the rest"
        ))
    }
}

/// What follows the home prefix of `p`: `~/` everywhere, and `~\` on Windows only, where a
/// backslash separates directories (on Linux it is a legal file-name character).
fn home_prefix_rest(p: &str, windows: bool) -> Option<&str> {
    p.strip_prefix("~/").or_else(|| if windows { p.strip_prefix("~\\") } else { None })
}

/// Whether a failed rename looks like Windows holding the target open for a moment: access denied
/// (5), a sharing violation (32) or a lock violation (33). An editor, an indexer or an antivirus
/// scan releases it soon after.
pub fn rename_busy(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(5 | 32 | 33))
}

/// `tmp` renamed over `target`. On Windows a target another process has open (without delete
/// sharing) refuses the rename, so a busy one is retried a few times with short waits; when it
/// stays busy the error keeps its kind and says the file may be open in another program.
async fn rename_over(tmp: &Path, target: &Path) -> std::io::Result<()> {
    let tries: u64 = if cfg!(windows) { 5 } else { 1 };
    let mut attempt = 1;
    loop {
        match tokio::fs::rename(tmp, target).await {
            Err(e) if cfg!(windows) && rename_busy(&e) => {
                if attempt >= tries {
                    return Err(std::io::Error::new(e.kind(), format!("{e}; the file may be open in another program")));
                }
                tokio::time::sleep(std::time::Duration::from_millis(20 * attempt)).await;
                attempt += 1;
            }
            other => return other,
        }
    }
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
        rename_over(&tmp, &target).await
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

    #[test]
    fn a_backslash_home_prefix_is_home_only_on_windows() {
        assert_eq!(home_prefix_rest("~\\notes.txt", true), Some("notes.txt"));
        assert_eq!(home_prefix_rest("~/notes.txt", true), Some("notes.txt"));
        assert_eq!(home_prefix_rest("~/notes.txt", false), Some("notes.txt"));
        assert_eq!(home_prefix_rest("~\\notes.txt", false), None, "a backslash is a file-name character on Linux");
        assert_eq!(home_prefix_rest("~", true), None);
    }

    #[test]
    fn a_busy_target_is_told_from_a_missing_one() {
        use std::io::{Error, ErrorKind};
        assert!(rename_busy(&Error::from_raw_os_error(32)));
        assert!(rename_busy(&Error::from_raw_os_error(5)));
        assert!(rename_busy(&Error::from_raw_os_error(33)));
        assert!(rename_busy(&Error::new(ErrorKind::PermissionDenied, "denied")));
        assert!(!rename_busy(&Error::new(ErrorKind::NotFound, "gone")));
        assert!(!rename_busy(&Error::from_raw_os_error(2)));
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

    fn utf16(text: &str, le: bool, bom: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if bom {
            out.extend(if le { [0xFF, 0xFE] } else { [0xFE, 0xFF] });
        }
        for u in text.encode_utf16() {
            out.extend(if le { u.to_le_bytes() } else { u.to_be_bytes() });
        }
        out
    }

    #[test]
    fn utf16_text_is_found_with_or_without_a_mark_in_either_order() {
        let text = "[Section]\r\nname=caf\u{e9}\r\n";
        assert_eq!(utf16_text(&utf16(text, true, true)), Some((text.into(), true, true)));
        assert_eq!(utf16_text(&utf16(text, false, true)), Some((text.into(), false, true)));
        assert_eq!(utf16_text(&utf16(text, false, false)), Some((text.into(), false, false)));
        assert_eq!(utf16_text(&utf16(text, true, false)), Some((text.into(), true, false)));
        // a pair of surrogates is one character
        assert_eq!(utf16_text(&utf16("a\u{1F600}", true, true)).map(|t| t.0), Some("a\u{1F600}".into()));
    }

    #[test]
    fn utf16_text_refuses_what_is_not_text() {
        // a real binary with NULs, an odd length, an unpaired surrogate and UTF-32 are not UTF-16 text
        assert_eq!(utf16_text(b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00"), None);
        assert_eq!(utf16_text(&[0; 64]), None);
        let mut odd = utf16("abc", true, true);
        odd.push(b'x');
        assert_eq!(utf16_text(&odd), None);
        assert_eq!(utf16_text(&[0xFF, 0xFE, 0x00, 0xD8, b'a', 0x00]), None);
        assert_eq!(utf16_text(&[0xFF, 0xFE, 0x00, 0x00, b'a', 0x00, 0x00, 0x00]), None);
        // a NUL inside the text of a file with a mark is a binary, not a line
        assert_eq!(utf16_text(&utf16("ab\0cd", true, true)), None);
        assert_eq!(utf16_text(b""), None);
        assert_eq!(utf16_text(b"plain text"), None);
    }

    #[test]
    fn cuts_count_the_long_lines_and_say_where_the_first_is() {
        let mut cuts = Cuts::default();
        assert_eq!(cuts.note(10), None);
        cuts.seen(1, &"x".repeat(10), 10);
        assert_eq!(cuts.note(10), None, "a line of exactly the limit is not cut");
        cuts.seen(4, &"x".repeat(11), 10);
        cuts.seen(9, &"x".repeat(50), 10);
        let note = cuts.note(10).unwrap();
        assert!(note.starts_with("line 4 cut at 10 of 11 chars (2 lines cut in all);"), "{note}");
    }
}
