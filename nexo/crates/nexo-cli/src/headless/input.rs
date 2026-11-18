//! What goes in: the images, the piped stdin, the input stream and the extra folders.

use super::*;

/// The `--image` files, made absolute: each must be a PNG, JPEG, GIF or WebP file that is there.
pub fn images(cwd: &Path, files: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    const KINDS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];
    let mut out: Vec<PathBuf> = Vec::new();
    for f in files {
        let path = cwd.join(f);
        if !path.is_file() {
            return Err(format!("--image {}: no such file", f.display()));
        }
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
        if !ext.is_some_and(|e| KINDS.contains(&e.as_str())) {
            return Err(format!("--image {}: not a PNG, JPEG, GIF or WebP image", f.display()));
        }
        if !out.contains(&path) {
            out.push(path);
        }
    }
    Ok(out)
}

/// The first request of the interface with the `--image` files added as quoted paths, which it attaches.
pub fn with_images(first: Option<String>, images: &[PathBuf]) -> Option<String> {
    if images.is_empty() {
        return first;
    }
    let paths: Vec<String> = images.iter().map(|p| format!("'{}'", p.display())).collect();
    Some(match first {
        Some(t) if !t.trim().is_empty() => format!("{} {}", t.trim_end(), paths.join(" ")),
        _ => paths.join(" "),
    })
}
/// The stdin of a run: what was piped in. With `first_byte`, a stdin that gives nothing in that time
/// (a caller that left it open) is given up on, and one line on stderr says so.
pub(super) fn read_stdin(first_byte: Option<Duration>) -> String {
    if std::io::stdin().is_terminal() {
        return String::new();
    }
    let (text, gave_up) = read_piped(std::io::stdin(), first_byte);
    if gave_up {
        eprintln!(
            "nexo: nothing came on stdin in {} s; going on with the -p prompt alone (pipe the input, or add < /dev/null)",
            first_byte.map_or(0, |d| d.as_secs())
        );
    }
    text
}

/// Reads `r` to EOF on a thread. With `first_byte`, waits at most that long for the first chunk and
/// then reads on without a limit. Returns the text and whether it gave up (the thread is left behind).
pub(super) fn read_piped<R: Read + Send + 'static>(mut r: R, first_byte: Option<Duration>) -> (String, bool) {
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut bytes: Vec<u8> = Vec::new();
    if let Some(wait) = first_byte {
        match rx.recv_timeout(wait) {
            Ok(chunk) => bytes.extend(chunk),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return (String::new(), true),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return (String::new(), false),
        }
    }
    while let Ok(chunk) = rx.recv() {
        bytes.extend(chunk);
    }
    (String::from_utf8_lossy(&bytes).into_owned(), false)
}

/// The text of one user message of the input stream. `Ok(None)`: a line that is not a user message.
pub(super) fn user_text(line: &str) -> Result<Option<String>, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("not JSON: {e}"))?;
    if v.get("type").and_then(Value::as_str).is_some_and(|t| t != "user") {
        return Ok(None);
    }
    let content = v
        .pointer("/message/content")
        .or_else(|| v.get("content"))
        .or_else(|| v.get("text"))
        .or_else(|| v.get("prompt"))
        .ok_or("a user message needs message.content (or content, text)")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.as_str().or_else(|| p.get("text").and_then(Value::as_str)))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return Err("the message content must be a string or a list of text blocks".into()),
    };
    Ok(Some(text))
}

/// Lets the tools read and write in each of `dirs` (relative to `cwd`) besides the project, and
/// lets a sandboxed command write there; the folders as resolved, or why one is not a folder.
pub fn add_dirs(
    config: &mut nexo_core::Config,
    cwd: &std::path::Path,
    dirs: &[PathBuf],
) -> Result<Vec<String>, String> {
    dirs.iter()
        .map(|d| {
            config.add_dir(cwd, d).map(|(real, _)| real.display().to_string()).map_err(|e| format!("--add-dir {e}"))
        })
        .collect()
}
/// Reads stdin lines on a thread, so the run can wait on them and on the agent at once.
pub(super) fn stdin_lines() -> tokio::sync::mpsc::UnboundedReceiver<String> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for l in std::io::stdin().lock().lines() {
            match l {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_shapes_of_a_user_message() {
        let t = |s| user_text(s).unwrap();
        assert_eq!(t(r#"{"type":"user","message":{"role":"user","content":"hi"}}"#), Some("hi".into()));
        assert_eq!(
            t(r#"{"message":{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}}"#),
            Some("a\nb".into())
        );
        assert_eq!(t(r#"{"content":"x"}"#), Some("x".into()));
        assert_eq!(t(r#"{"type":"control","x":1}"#), None);
        assert!(user_text("nope").is_err());
        assert!(user_text(r#"{"type":"user"}"#).is_err());
    }
}

#[cfg(test)]
mod add_dir_tests {
    use super::*;

    #[test]
    fn a_folder_added_gets_the_file_tools_and_the_sandbox() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("other")).unwrap();
        let mut config = nexo_core::Config::default();
        let roots = add_dirs(&mut config, d.path(), &[PathBuf::from("other")]).unwrap();
        // The folder is reported as the person writes it, so on Windows the expected form has no verbatim prefix either.
        let real = nexo_core::paths::canonical(&d.path().join("other")).unwrap();
        assert_eq!(roots, vec![real.display().to_string()]);
        let p = nexo_core::config::rule_path(&real);
        for tool in ["read", "write", "edit"] {
            assert!(
                config.permissions.allow.contains(&format!("{tool}({p}/**)")),
                "{tool}: {:?}",
                config.permissions.allow
            );
        }
        assert_eq!(config.permissions.sandbox_write_dirs, vec![real]);
        let e = add_dirs(&mut config, d.path(), &[PathBuf::from("missing")]).unwrap_err();
        assert_eq!(e, "--add-dir missing: not a folder");
    }
}

#[cfg(test)]
mod image_tests {
    use super::{images, with_images};
    use std::path::PathBuf;

    #[test]
    fn image_files_are_checked_made_absolute_and_added_to_the_first_request() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("shot.PNG"), b"png").unwrap();
        std::fs::write(d.path().join("notes.txt"), b"x").unwrap();
        let shot = d.path().join("shot.PNG");
        let got = images(d.path(), &[PathBuf::from("shot.PNG"), shot.clone()]).unwrap();
        assert_eq!(got, vec![shot.clone()], "relative to the folder, once");
        assert!(images(d.path(), &[PathBuf::from("gone.png")]).unwrap_err().contains("no such file"));
        assert!(images(d.path(), &[PathBuf::from("notes.txt")]).unwrap_err().contains("not a PNG"));
        let quoted = format!("'{}'", shot.display());
        assert_eq!(with_images(Some("why red?".into()), &got), Some(format!("why red? {quoted}")));
        assert_eq!(with_images(None, &got), Some(quoted));
        assert_eq!(with_images(Some("hi".into()), &[]), Some("hi".into()));
    }
}

#[cfg(test)]
mod piped_tests {
    use super::read_piped;
    use std::io::Read;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// Gives each `(delay, text)` in turn, then EOF.
    struct Slow(Vec<(u64, &'static str)>);
    impl Read for Slow {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.0.is_empty() {
                return Ok(0);
            }
            let (ms, s) = self.0.remove(0);
            std::thread::sleep(Duration::from_millis(ms));
            buf[..s.len()].copy_from_slice(s.as_bytes());
            Ok(s.len())
        }
    }

    /// Blocks until the sender goes away, which the test never does.
    struct Never(mpsc::Receiver<()>);
    impl Read for Never {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            let _ = self.0.recv();
            Ok(0)
        }
    }

    #[test]
    fn a_stdin_nobody_writes_to_is_given_up_on() {
        let (_keep, rx) = mpsc::channel();
        let t = Instant::now();
        let (text, gave_up) = read_piped(Never(rx), Some(Duration::from_millis(100)));
        assert!(text.is_empty() && gave_up);
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn the_wait_covers_only_the_first_byte() {
        let r = Slow(vec![(50, "abc"), (300, "def")]);
        assert_eq!(read_piped(r, Some(Duration::from_millis(100))), ("abcdef".to_string(), false));
    }

    #[test]
    fn without_a_wait_slow_text_is_read_in_full() {
        let r = Slow(vec![(300, "late text")]);
        assert_eq!(read_piped(r, None), ("late text".to_string(), false));
    }

    #[test]
    fn an_empty_stdin_ends_at_once() {
        let t = Instant::now();
        assert_eq!(read_piped(std::io::empty(), Some(Duration::from_secs(3))), (String::new(), false));
        assert!(t.elapsed() < Duration::from_secs(1));
        assert_eq!(read_piped(std::io::empty(), None), (String::new(), false));
    }
}
