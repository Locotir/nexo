//! The kept closed part must give the very lines and boxes a render from the first byte gives.

use super::super::stream_tests::{long_reply, random_doc, samples, Rng};
use super::*;

fn kept_bytes() -> usize {
    KEPT.with(|k| k.borrow().iter().map(|e| e.text.len()).max().unwrap_or(0))
}

fn same(md: &str, w: usize, theme: &Theme, interpreted: bool) {
    let (cl, cm) = render(md, w, theme, interpreted, 0);
    let (fl, fm) = render_whole(md, w, theme, interpreted);
    assert_eq!(cl, fl, "lines differ at {} bytes, width {w}, interpreted {interpreted}:\n{md:?}", md.len());
    assert_eq!(format!("{cm:?}"), format!("{fm:?}"), "boxes differ at {} bytes, width {w}:\n{md:?}", md.len());
}

/// Grows `md` in random chunks of characters and compares every step.
fn stream(md: &str, seed: u64, theme: &Theme) -> usize {
    let mut rng = Rng(seed);
    let chars: Vec<(usize, char)> = md.char_indices().collect();
    let mut at = 0;
    let mut most = 0;
    while at < chars.len() {
        at = (at + 1 + (rng.next() % 45) as usize).min(chars.len());
        let end = chars.get(at).map_or(md.len(), |(i, _)| *i);
        let w = [30, 56, 100][(rng.next() % 3) as usize];
        same(&md[..end], w, theme, rng.next().is_multiple_of(2));
        most = most.max(kept_bytes());
    }
    most
}

#[test]
fn a_streamed_reply_gives_the_same_lines_at_every_step_as_a_full_render() {
    let theme = Theme::dark();
    let mut cached = 0;
    for (n, md) in samples().iter().enumerate() {
        for seed in 1..=12u64 {
            cached += stream(md, seed * 7919 + n as u64, &theme);
        }
    }
    assert!(cached > 0, "no sample ever had a closed part, so nothing was compared");
}

#[test]
fn a_long_reply_keeps_most_of_itself_closed_and_ends_the_same() {
    let theme = Theme::dark();
    let md = long_reply(400);
    let most = stream(&md, 99, &theme);
    assert!(most > md.len() / 2, "kept {most} of {} bytes", md.len());
    // the default threshold and a width change in the middle of a stream
    let (a, am) = super::super::render_as_meta(&md, 80, &theme, false);
    let (b, bm) = super::super::render_as_meta(&md, 80, &theme, false);
    let (w, wm) = render_whole(&md, 80, &theme, false);
    assert_eq!((&a, format!("{am:?}")), (&w, format!("{wm:?}")));
    assert_eq!((&b, format!("{bm:?}")), (&w, format!("{wm:?}")));
    same(&md, 47, &theme, false);
}

#[test]
fn a_text_that_is_not_the_one_kept_renders_from_its_first_byte() {
    let theme = Theme::dark();
    let md = long_reply(80);
    same(&md, 60, &theme, false);
    // the same start with another ending, and another theme
    let edited = format!("{}\n\nsomething else\n\n{}", &md[..md.len() / 2], &md[md.len() / 3..]);
    same(&edited, 60, &theme, false);
    same(&md, 60, &Theme::light(), false);
}

#[test]
fn where_the_closed_part_ends() {
    // a paragraph and a blank line, then a block of its own
    assert_eq!(closed_part("a\n\nb\n"), Some(3));
    assert_eq!(closed_part("a\n\nb\n\n## c\n\nd"), Some(12));
    // a list goes on past a blank line, so nothing closes while the next line may be an item or indented
    assert_eq!(closed_part("- a\n\n- b\n"), Some(0));
    assert_eq!(closed_part("- a\n\n  more\n"), Some(0));
    assert_eq!(closed_part("1. a\n\n2. b\n"), Some(0));
    // after a list, a line that cannot be an item closes it
    assert_eq!(closed_part("- a\n\nthen text\n"), Some(5));
    // a blank line inside a code fence closes nothing
    assert_eq!(closed_part("```\na\n\nb\n```\n"), Some(0));
    assert_eq!(closed_part("~~~~\n```\n\nb\n~~~~\n\nc\n"), Some(18));
    // a line that cannot be told from a continuation, an underline or a quote
    assert_eq!(closed_part("a\n\n> q\n"), Some(0));
    assert_eq!(closed_part("a\n\n    code\n"), Some(0));
    assert_eq!(closed_part("a\n\n---\n"), Some(0));
    // the whole text decides links, and HTML reaches into the next block
    assert_eq!(closed_part("[a][r]\n\nb\n\n[r]: https://x.y\n"), None);
    assert_eq!(closed_part("<div>\n\ntext\n\nmore\n"), Some(0));
    assert_eq!(closed_part("a\n\nb\n\n<div>\n\ntext\n"), Some(3));
    // tabs and escapes are rewritten by the cleaning
    assert_eq!(closed_part("a\n\nb\tc\n"), None);
    // a fence the scan cannot follow stops the cuts after it
    assert_eq!(closed_part("a\n\nc\n\n- ```\n  x\n\ny\n  ```\n\nz\n"), Some(3));
}

#[test]
fn random_documents_streamed_in_random_chunks_match_a_full_render() {
    let theme = Theme::dark();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut cached = 0;
    for n in 0..120u64 {
        let md = random_doc(&mut rng);
        cached += stream(&md, n * 31 + 5, &theme);
    }
    assert!(cached > 0);
}
