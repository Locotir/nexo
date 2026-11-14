//! Links in the transcript: finding the one under a cell, and opening it in the default browser.

use unicode_segmentation::UnicodeSegmentation;

use crate::render::wrap::width;

/// The link under display column `col` of `text`: `http://`, `https://`, `file://` or a `www.` address
/// (which opens as `https://`). It ends at a space, a quote, an angle bracket or a box line; a trailing
/// `.,;:!?` and a closing bracket without its opening one are not part of it.
pub fn url_at(text: &str, col: usize) -> Option<String> {
    url_span(text, col).map(|(_, _, u)| u)
}

/// [`url_at`] with where the link sits: its first and one past its last display column.
pub fn url_span(text: &str, col: usize) -> Option<(usize, usize, String)> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let before_ok = text[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        if before_ok {
            if let Some(len) = prefix_len(&text[i..]) {
                let end = i + link_end(&text[i..], len);
                if end > i + len {
                    let (from, to) = (width(&text[..i]), width(&text[..end]));
                    if (from..to).contains(&col) {
                        return Some((from, to, normal(&text[i..end])));
                    }
                    i = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    None
}

/// The length of the scheme (or `www.`) that `s` starts with.
fn prefix_len(s: &str) -> Option<usize> {
    ["https://", "http://", "file://", "www."]
        .iter()
        .find(|p| s.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p)))
        .map(|p| p.len())
}

/// Where the link in `s` (whose prefix is `skip` bytes long) ends, in bytes.
fn link_end(s: &str, skip: usize) -> usize {
    let stop = |g: &str| {
        g.chars().next().is_some_and(|c| {
            c.is_whitespace()
                || c.is_control()
                || matches!(c, '"' | '\'' | '`' | '<' | '>')
                || ('\u{2500}'..='\u{257f}').contains(&c)
        })
    };
    let mut end = skip;
    for (k, g) in s[skip..].grapheme_indices(true) {
        if stop(g) {
            break;
        }
        end = skip + k + g.len();
    }
    let mut url = &s[..end];
    while let Some(last) = url.chars().next_back() {
        let closes = |open: char| url.matches(last).count() > url.matches(open).count();
        let trim = match last {
            '.' | ',' | ';' | ':' | '!' | '?' => true,
            ')' => closes('('),
            ']' => closes('['),
            '}' => closes('{'),
            _ => false,
        };
        if !trim {
            break;
        }
        url = &url[..url.len() - last.len_utf8()];
    }
    url.len()
}

fn normal(url: &str) -> String {
    if url.len() >= 4 && url[..4].eq_ignore_ascii_case("www.") {
        format!("https://{url}")
    } else {
        url.to_string()
    }
}

/// What a click on a link does. A link comes from model output, so a click never runs a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Open `url` with the default app; `label` names it in the notice.
    Open { url: String, label: String },
    /// `url` is a program: its folder opens instead.
    Folder { url: String, label: String },
    /// Nothing opens; the URL is copied and `why` says so.
    Copy { url: String, why: String },
}

/// Decides what a click on `url` does: web links and files open, a program's folder opens in its
/// place, and any other scheme (or a file that is not there) is only copied.
pub fn plan(url: &str) -> Plan {
    let copy = |why: &str| Plan::Copy { url: url.to_string(), why: why.to_string() };
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        let host = url.split("://").nth(1).unwrap_or("").split(['/', '?', '#']).next().unwrap_or("");
        return Plan::Open { url: url.to_string(), label: short(host) };
    }
    if !lower.starts_with("file://") {
        let scheme = url.split(':').next().unwrap_or("").chars().take(12).collect::<String>();
        return copy(&format!("only web and file links open (not {scheme}:)"));
    }
    let Some(path) = file_path(&url[7..]) else { return copy("only files on this computer open") };
    let path = std::path::Path::new(&path);
    let Ok(meta) = std::fs::metadata(path) else { return copy("that file is not there") };
    let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    if !meta.is_dir() && is_program(path, &meta) {
        let folder = path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        if folder.is_empty() {
            return copy("that file is a program");
        }
        return Plan::Folder { url: crate::render::blocks::file_url(&folder), label: short(&name) };
    }
    Plan::Open { url: url.to_string(), label: short(&name) }
}

fn short(s: &str) -> String {
    crate::render::wrap::truncate(s, 40)
}

/// The local path of what follows `file://`: no other host, `%XX` decoded, a Windows drive without
/// the slash in front of it.
fn file_path(rest: &str) -> Option<String> {
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let mut bytes = Vec::with_capacity(rest.len());
    let raw = rest.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if raw[i] == b'%' && i + 2 < raw.len() && hex(raw[i + 1]).is_some() && hex(raw[i + 2]).is_some() {
            bytes.push((hex(raw[i + 1])? * 16 + hex(raw[i + 2])?) as u8);
            i += 3;
        } else {
            bytes.push(raw[i]);
            i += 1;
        }
    }
    let path = String::from_utf8_lossy(&bytes).into_owned();
    let b = path.as_bytes();
    if cfg!(windows) && b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        return Some(path[1..].to_string());
    }
    Some(path)
}

/// True when opening `path` could run it: the executable bit on Linux and macOS, a program or
/// script extension on Windows.
fn is_program(path: &std::path::Path, meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 != 0 {
            return true;
        }
    }
    let _ = meta;
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy().to_ascii_lowercase()));
    cfg!(windows) && ext.is_some_and(|e| program_ext(&e))
}

/// A Windows extension (with its dot, lower case) that launches code: PATHEXT plus the usual scripts.
pub fn program_ext(ext: &str) -> bool {
    const ALWAYS: [&str; 17] = [
        ".exe", ".com", ".bat", ".cmd", ".lnk", ".msi", ".ps1", ".scr", ".vbs", ".vbe", ".js", ".jse", ".wsf", ".wsh",
        ".hta", ".cpl", ".msc",
    ];
    ALWAYS.contains(&ext)
        || std::env::var("PATHEXT").is_ok_and(|p| p.split(';').any(|x| !x.is_empty() && x.eq_ignore_ascii_case(ext)))
}

/// Opens `url` with the system's default app: detached, the output dropped, nothing waited for.
/// False when the opener could not be started.
#[cfg(not(test))]
pub fn open(url: &str) -> bool {
    use std::process::{Command, Stdio};
    #[cfg(target_os = "macos")]
    let mut c = Command::new("open");
    #[cfg(windows)]
    let mut c = {
        // not `cmd /c start`: cmd would read `&` and `|` in the address as its own syntax
        let mut c = Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let mut c = Command::new("xdg-open");
    c.arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    match c.spawn() {
        Ok(mut child) => {
            // reaped off the UI thread
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            true
        }
        Err(_) => false,
    }
}

/// Under test nothing is launched: the links asked for are kept for the test to read.
#[cfg(test)]
pub fn open(url: &str) -> bool {
    OPENED.with(|o| o.borrow_mut().push(url.to_string()));
    true
}

#[cfg(test)]
thread_local! {
    static OPENED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The links "opened" so far on this thread, emptied.
#[cfg(test)]
pub fn take_opened() -> Vec<String> {
    OPENED.with(|o| std::mem::take(&mut *o.borrow_mut()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_link_is_found_across_its_cells() {
        let t = "see https://example.com/a?b=1 now";
        assert_eq!(url_at(t, 4).as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(url_at(t, 28).as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(url_at(t, 29), None);
        assert_eq!(url_at(t, 3), None);
    }

    #[test]
    fn brackets_quotes_and_dots_are_not_part_of_it() {
        assert_eq!(url_at("(https://a.io/x)", 5).as_deref(), Some("https://a.io/x"));
        assert_eq!(url_at("(https://a.io/x)", 15), None);
        assert_eq!(url_at("go to http://a.io/x.", 8).as_deref(), Some("http://a.io/x"));
        assert_eq!(url_at("a http://a.io/x, b", 4).as_deref(), Some("http://a.io/x"));
        assert_eq!(url_at("\"file:///tmp/a b\"", 3).as_deref(), Some("file:///tmp/a"));
        // a bracket with its opening one stays
        assert_eq!(
            url_at("https://en.wikipedia.org/wiki/Foo_(bar)", 10).as_deref(),
            Some("https://en.wikipedia.org/wiki/Foo_(bar)")
        );
        assert_eq!(url_at("[x](https://a.io/p)", 6).as_deref(), Some("https://a.io/p"));
    }

    #[test]
    fn two_links_on_one_line_are_told_apart() {
        let t = "https://a.io and www.b.io/z.";
        assert_eq!(url_at(t, 2).as_deref(), Some("https://a.io"));
        assert_eq!(url_at(t, 12), None);
        assert_eq!(url_at(t, 18).as_deref(), Some("https://www.b.io/z"));
    }

    #[test]
    fn a_click_outside_any_link_finds_nothing() {
        assert_eq!(url_at("no links here", 3), None);
        assert_eq!(url_at("https://", 2), None);
        assert_eq!(url_at("xhttps://a.io", 5), None);
        assert_eq!(url_at("", 0), None);
    }

    #[test]
    fn wide_and_unicode_cells_before_the_link_count_by_width() {
        // 漢字 are two cells each: the link starts at column 5
        let t = "漢字 https://a.io";
        assert_eq!(url_at(t, 4), None);
        assert_eq!(url_at(t, 5).as_deref(), Some("https://a.io"));
        assert_eq!(url_at(t, 16).as_deref(), Some("https://a.io"));
        assert_eq!(url_at(t, 17), None);
        let t = "é▌ │ http://x.org/ñ";
        assert_eq!(url_at(t, 5).as_deref(), Some("http://x.org/ñ"));
        assert_eq!(url_at(t, 3), None);
    }
}
