//! A failure drawn as a card of three rows at most: what happened (the mark and the title), the likely
//! cause (left out when it is not known), and the first thing to do, one command or one key. Each row is
//! cut to `COLS` columns. The other steps and the provider's own text open on demand (the block's fold:
//! click the card or press Enter on it); the whole text stays in the block (`Block::copy_text`).

use nexo_core::Problem;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::wrap::{clip_spans, width, wrap_runs_ex};
use crate::regions::Cont;
use crate::theme::Theme;

/// A row of the card is under this many columns.
const COLS: usize = 71;
/// Rows of the provider's own text once the card is open.
const DETAIL_ROWS: usize = 8;

/// `text` as styled runs: what sits between backticks in the code style, the rest in `base`.
fn runs(text: &str, base: Style, theme: &Theme) -> Vec<(String, Style)> {
    text.split('`')
        .enumerate()
        .filter(|(_, s)| !s.is_empty())
        .map(|(i, s)| (super::wrap::clean(s).into_owned(), if i % 2 == 1 { theme.code_inline() } else { base }))
        .collect()
}

/// One row: `head` then `text`, cut to the row's width.
fn row(head: Vec<Span<'static>>, text: Vec<(String, Style)>, tail: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let mut spans = head;
    spans.extend(text.into_iter().map(|(t, s)| Span::styled(t, s)));
    let room = w.min(COLS);
    let tail_w: usize = tail.iter().map(|s| width(&s.content)).sum();
    let head_w: usize = spans.iter().map(|s| width(&s.content)).sum();
    // the tail (the count, the chevron) keeps its place; the words give way
    if head_w + tail_w > room {
        spans = clip_spans(spans, room.saturating_sub(tail_w));
    }
    spans.extend(tail);
    Line::from(spans)
}

/// The card at `w` columns, `count` times seen, with its other steps and raw text when `open`: its
/// rows, and which of them continue the one above (only in the open part, where text wraps).
pub fn card(p: &Problem, count: u32, open: bool, w: usize, theme: &Theme) -> (Vec<Line<'static>>, Vec<Cont>) {
    let plain = Style::default().fg(theme.fg);
    let more = p.detail.as_deref().is_some_and(|d| !d.trim().is_empty()) || p.steps.len() > 1;
    let mut tail = Vec::new();
    if count > 1 {
        tail.push(Span::styled(format!(" ×{count}"), theme.dim()));
    }
    if more {
        tail.push(Span::styled(if open { " ▾" } else { " ▸" }, theme.dim()));
    }
    let mut out = vec![row(
        vec![Span::styled("✗ ", theme.bold(theme.error))],
        vec![(super::wrap::clean(&p.title).into_owned(), theme.bold(theme.fg))],
        tail,
        w,
    )];
    let cause = p.why.lines().next().unwrap_or("").trim();
    if !cause.is_empty() {
        out.push(row(vec![Span::raw("  ")], runs(cause, theme.dim(), theme), Vec::new(), w));
    }
    if let Some(step) = p.steps.first() {
        out.push(row(vec![Span::raw("  ")], runs(step, plain, theme), Vec::new(), w));
    }
    let mut conts = Vec::new();
    if open {
        let room = w.min(COLS);
        for step in p.steps.iter().skip(1) {
            out.push(row(vec![Span::raw("  ")], runs(step, plain, theme), Vec::new(), w));
        }
        if let Some(d) = p.detail.as_deref().filter(|d| !d.trim().is_empty()) {
            let (lines, cs) =
                wrap_runs_ex(&[(d.trim().to_string(), theme.dim())], room, &[Span::raw("  ")], &[Span::raw("  ")]);
            let at = out.len();
            conts.extend(cs.into_iter().map(|c| Cont { line: c.line + at as u32, ..c }));
            out.extend(lines);
            if out.len() - at > DETAIL_ROWS {
                out.truncate(at + DETAIL_ROWS);
                conts.retain(|c| (c.line as usize) < out.len());
                let last = out.pop().unwrap_or_default();
                let mut spans = clip_spans(last.spans, room.saturating_sub(1));
                spans.push(Span::styled("…", theme.dim()));
                out.push(Line::from(spans));
            }
        }
    }
    (out, conts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ascii::fold_char;
    use nexo_core::{ErrorKind, ProblemCtx, ProviderError};

    fn rows(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    fn ctx() -> ProblemCtx {
        ProblemCtx {
            provider: "openai".into(),
            model: "gpt-5".into(),
            base_url: "https://api.openai.com/v1".into(),
            key_env: Some("OPENAI_API_KEY".into()),
            login: false,
            window: None,
        }
    }

    fn sample() -> Problem {
        let body = r#"{"error":{"message":"Incorrect API key provided: sk-proj-abc. You can find your API key at https://platform.openai.com/account/api-keys.","type":"invalid_request_error","code":"invalid_api_key"}}"#;
        Problem::of(&ProviderError::from_http(401, body), &ctx())
    }

    #[test]
    fn the_card_is_three_rows_at_120_and_at_80_columns_with_the_error_colour_on_the_mark_only() {
        let theme = Theme::nexo();
        for cols in [120, 80] {
            let (lines, conts) = card(&sample(), 1, false, cols, &theme);
            let r = rows(&lines);
            assert_eq!(r.len(), 3, "{cols}: {r:?}");
            assert!(r.iter().all(|l| !l.trim().is_empty() && width(l) < 72), "{cols}: {r:?}");
            assert_eq!(r[0], "✗ openai rejected the key ▸", "the chevron says there is more");
            assert_eq!(r[1], "  The key was not accepted; it may be expired or revoked.");
            assert_eq!(r[2], "  Run `nexo auth login openai --key`.".replace('`', ""));
            assert!(conts.is_empty());
            // the error colour is on the mark; the title is bold in the normal colour
            assert_eq!(lines[0].spans[0].style.fg, Some(theme.error));
            assert_eq!(lines[0].spans[1].style.fg, Some(theme.fg));
            assert!(lines[1..].iter().flat_map(|l| &l.spans).all(|s| s.style.fg != Some(theme.error)));
            // a command is in the code style
            assert!(lines[2].spans.iter().any(|s| s.style == theme.code_inline() && s.content.contains("nexo auth")));
            // the provider's own words are not on screen until it is opened
            assert!(!r.join("\n").contains("HTTP 401"), "{r:?}");
        }
    }

    #[test]
    fn the_cause_row_is_left_out_when_it_is_not_known() {
        let theme = Theme::nexo();
        let p = Problem::of(&ProviderError::new(ErrorKind::Other, "boom"), &ctx());
        let r = rows(&card(&p, 1, false, 100, &theme).0);
        assert_eq!(r, ["✗ The request to openai failed ▸", "  Send your message again."]);
        // a card with only a title is one row
        assert_eq!(rows(&card(&Problem::from_text("cannot read session abc"), 1, false, 100, &theme).0).len(), 1);
    }

    #[test]
    fn a_repeat_shows_its_count_and_a_long_title_gives_way_to_it() {
        let theme = Theme::nexo();
        let r = rows(&card(&sample(), 3, false, 100, &theme).0);
        assert_eq!(r[0], "✗ openai rejected the key ×3 ▸");
        let long = Problem::new("word ".repeat(30), "cause").step("Do it.");
        let (lines, _) = card(&long, 12, false, 100, &theme);
        let r = rows(&lines);
        assert!(width(&r[0]) < 72 && r[0].contains('…') && r[0].ends_with(" ×12"), "{r:?}");
    }

    #[test]
    fn opened_the_card_adds_the_other_steps_and_the_raw_text_then_closes_again() {
        let theme = Theme::nexo();
        let p = sample();
        let (lines, _) = card(&p, 1, true, 100, &theme);
        let r = rows(&lines);
        assert!(r[0].ends_with(" ▾"), "{r:?}");
        assert!(r.len() > 3 && r[3].contains("OPENAI_API_KEY"), "{r:?}");
        assert!(r[5].contains("HTTP 401 from api.openai.com"), "the raw text follows the steps: {r:?}");
        assert!(r.iter().all(|l| width(l) < 72));
        // a long raw text is cut with a mark
        let big = Problem::new("It broke", "A thing failed.").step("Try again.").detail("word ".repeat(300));
        let (lines, conts) = card(&big, 1, true, 100, &theme);
        let r = rows(&lines);
        assert_eq!(r.len(), 3 + DETAIL_ROWS, "{r:?}");
        assert!(r.last().unwrap().ends_with('…'));
        assert!(conts.iter().all(|c| (c.line as usize) < r.len()));
        // closed again, a card with nothing more has no chevron
        let r = rows(&card(&Problem::new("t", "w").step("s"), 1, false, 100, &theme).0);
        assert_eq!(r[0], "✗ t");
    }

    #[test]
    fn at_40_columns_every_row_fits() {
        let theme = Theme::nexo();
        for open in [false, true] {
            let (lines, conts) = card(&sample(), 2, open, 40, &theme);
            let r = rows(&lines);
            assert!(r.iter().all(|l| width(l) <= 40), "{r:?}");
            assert!(conts.iter().all(|c| (c.line as usize) < r.len()));
            assert!(r[0].ends_with("×2 ▸") || r[0].ends_with("×2 ▾"), "the count and chevron keep their place: {r:?}");
        }
        assert_eq!(rows(&card(&sample(), 1, false, 40, &theme).0).len(), 3);
    }

    #[test]
    fn it_works_in_the_mono_theme_and_in_ascii() {
        let mono = Theme::mono();
        let net = ProviderError::new(ErrorKind::Network, "connection refused");
        let c = ProblemCtx {
            provider: "ollama".into(),
            model: "llama3".into(),
            base_url: "http://localhost:11434/v1".into(),
            ..Default::default()
        };
        let p = Problem::of(&net, &c);
        let (lines, _) = card(&p, 2, true, 40, &mono);
        // no colour is needed to tell the title from the rest: it is bold
        assert!(lines[0].spans.iter().any(|s| s.style.add_modifier.contains(ratatui::style::Modifier::BOLD)));
        assert!(rows(&lines).iter().any(|l| l.contains("ollama serve")));
        // every symbol of the card folds to plain ASCII, one column each
        for l in rows(&lines) {
            for ch in l.chars() {
                assert!(ch.is_ascii() || fold_char(ch).is_some_and(|a| a.is_ascii()), "{ch:?} has no ASCII form");
            }
        }
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;

    fn rows(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    #[test]
    fn a_plain_text_of_many_lines_is_one_closed_row_and_all_of_it_open() {
        let theme = Theme::nexo();
        let text = "tool failed: the command exited with a long and unhelpful message that goes past the row\nsecond line\nthird line";
        let p = Problem::from_text(text);
        for cols in [100, 72, 40] {
            let (closed, _) = card(&p, 1, false, cols, &theme);
            let r = rows(&closed);
            assert!(r.len() <= 3 && r.iter().all(|l| width(l) <= cols.min(71)), "{cols}: {r:?}");
            assert!(r[0].ends_with('▸'), "{cols}: the chevron says there is more: {r:?}");
            let (open, _) = card(&p, 1, true, cols, &theme);
            let all = rows(&open).join(" ");
            assert!(all.contains("second line") && all.contains("third line"), "{cols}: {all}");
        }
    }

    #[test]
    fn rows_fit_at_72_columns_and_an_open_card_at_40_is_bounded() {
        let theme = Theme::nexo();
        let p = Problem::new("word ".repeat(30), "cause ".repeat(20))
            .step("Do `this` ".repeat(10))
            .step("Then that.")
            .detail("raw ".repeat(500));
        let (closed, _) = card(&p, 4, false, 72, &theme);
        let r = rows(&closed);
        assert_eq!(r.len(), 3);
        assert!(r.iter().all(|l| width(l) < 72), "{r:?}");
        let (open, conts) = card(&p, 4, true, 40, &theme);
        let r = rows(&open);
        assert!(r.len() <= 3 + 1 + DETAIL_ROWS, "{} rows: {r:?}", r.len());
        assert!(r.iter().all(|l| width(l) <= 40), "{r:?}");
        assert!(conts.iter().all(|c| (c.line as usize) < r.len()));
    }

    #[test]
    fn escape_bytes_in_a_card_never_reach_the_screen() {
        let theme = Theme::nexo();
        let mut p = Problem::new("bad", "cause");
        p.title = "\x1b[31mred\x1b]0;owned\x07 title".into();
        p.why = "why\x1b[2J".into();
        p.steps.push("Run `a\x1b[0mb`.".into());
        p.detail = Some("raw\x1b[H text".into());
        let (lines, _) = card(&p, 1, true, 80, &theme);
        let all = rows(&lines).join("\n");
        assert!(!all.contains('\x1b') && all.contains("red title") && all.contains("raw text"), "{all:?}");
    }

    #[test]
    fn every_slash_command_a_card_names_exists() {
        use nexo_core::{ErrorKind, ProblemCtx, ProviderError};
        let ctx = ProblemCtx {
            provider: "openai".into(),
            model: "gpt-5".into(),
            base_url: "https://api.openai.com/v1".into(),
            window: Some(128000),
            ..Default::default()
        };
        let mut texts = Vec::new();
        for (status, body) in [
            (401, "x"),
            (402, ""),
            (429, "x"),
            (503, "x"),
            (400, "context_length_exceeded"),
            (404, "model"),
            (500, "x"),
        ] {
            texts.push(Problem::of(&ProviderError::from_http(status, body), &ctx).plain());
        }
        for kind in [ErrorKind::Network, ErrorKind::Parse, ErrorKind::Auth, ErrorKind::Other, ErrorKind::BadRequest] {
            for msg in ["timed out", "connection reset", "something odd", "no key found", "content_filter"] {
                texts.push(Problem::of(&ProviderError::new(kind, msg), &ctx).plain());
            }
        }
        texts.push(
            Problem::of(&ProviderError::from_http(400, "context_length_exceeded"), &ctx)
                .after_compaction_failed()
                .plain(),
        );
        let mut seen = 0;
        for t in texts {
            for cmd in t.split('`').skip(1).step_by(2).filter(|c| c.starts_with('/') && !c.starts_with("/v1")) {
                seen += 1;
                let name = cmd.trim_start_matches('/').split_whitespace().next().unwrap_or("");
                assert!(crate::commands::lookup(name).is_some(), "`{cmd}` is not a command");
            }
        }
        assert!(seen >= 4, "the cards name commands: {seen}");
    }
}
