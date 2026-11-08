//! Image files dropped onto the terminal: the terminal types their path, plain, quoted, with
//! escaped spaces or as a `file://` URL. Such a path is attached as an image, as `@path` would be.

use std::path::{Path, PathBuf};

/// The extensions sent as images (as the agent attaches them).
const IMAGES: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// The existing image files a request names by absolute (or `~/`) path, in order, once each.
pub fn image_paths(text: &str, home: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for word in words(text) {
        let Some(p) = image_path(&word, home) else { continue };
        if p.is_file() && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// The path a word spells, when it is an absolute path to an image.
fn image_path(word: &str, home: &Path) -> Option<PathBuf> {
    let word = word.trim_end_matches([',', ';', ')']);
    let word = match word.strip_prefix("file://") {
        Some(rest) => percent_decoded(rest),
        None => word.to_string(),
    };
    let p = match word.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(&word),
    };
    let ext = p.extension()?.to_str()?.to_ascii_lowercase();
    (p.is_absolute() && IMAGES.contains(&ext.as_str())).then_some(p)
}

/// The words of a text: whitespace splits them except inside quotes and, off Windows, after a
/// backslash (`/My\ Shots/a.png`).
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') if cur.is_empty() => quote = Some(c),
            (None, '\\') if !cfg!(windows) => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `%20` and the other escapes of a URL path, decoded; a broken escape stays as typed.
fn percent_decoded(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_image_path_is_found_plain_quoted_escaped_or_as_a_url() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("My Shots");
        std::fs::create_dir(&dir).unwrap();
        let png = dir.join("a b.png");
        std::fs::write(&png, b"x").unwrap();
        let plain = d.path().join("c.JPG");
        std::fs::write(&plain, b"x").unwrap();
        std::fs::write(d.path().join("notes.txt"), b"x").unwrap();
        let home = d.path();
        let shown = png.display().to_string();
        let found = |t: &str| image_paths(t, home);

        assert_eq!(found(&format!("what is wrong here? {}", plain.display())), vec![plain.clone()]);
        assert_eq!(found(&format!("see '{shown}' please")), vec![png.clone()]);
        assert_eq!(found(&format!("see \"{shown}\"")), vec![png.clone()]);
        assert_eq!(found("look at ~/c.JPG and ~/c.JPG"), vec![plain.clone()], "once each");
        assert!(found(&format!("{} and src/x.png", d.path().join("notes.txt").display())).is_empty());
        assert!(found(&format!("{}", d.path().join("gone.png").display())).is_empty());
        if !cfg!(windows) {
            let escaped = shown.replace(' ', "\\ ");
            assert_eq!(found(&format!("fix {escaped}")), vec![png.clone()]);
            let url = format!("file://{}", shown.replace(' ', "%20"));
            assert_eq!(found(&url), vec![png.clone()]);
        }
    }
}
