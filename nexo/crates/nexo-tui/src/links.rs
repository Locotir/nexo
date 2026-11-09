//! Links in the transcript: finding the one under a cell, and opening it in the default browser.

use unicode_segmentation::UnicodeSegmentation;

use crate::render::wrap::width;

/// The link under display column `col` of `text`: `http://`, `https://`, `file://` or a `www.` address
/// (which opens as `https://`). It ends at a space, a quote, an angle bracket or a box line; a trailing
/// `.,;:!?` and a closing bracket without its opening one are not part of it.
pub fn url_at(text: &str, col: usize) -> Option<String> {
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
                        return Some(normal(&text[i..end]));
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

/// Opens `url` in the default browser: detached, the output dropped, nothing waited for.
#[cfg(not(test))]
pub fn open(url: &str) {
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
    if let Ok(mut child) = c.spawn() {
        // reaped off the UI thread
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Under test nothing is launched: the links asked for are kept for the test to read.
#[cfg(test)]
pub fn open(url: &str) {
    OPENED.with(|o| o.borrow_mut().push(url.to_string()));
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
