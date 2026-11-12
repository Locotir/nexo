//! The line of a group of tool calls: `✓ Made 1 edit +29 −3, read 2 files, ran 3 shell commands  4.2s ▸`.
//! It is built from the calls it stands for, with the same kind classification as the turn's
//! summary (`summary::classify`). While a kind is still running its phrase is in the present
//! tense with an ellipsis; finished, in the past.

use std::time::Instant;

use nexo_core::tool::ToolDisplay;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::blocks::{fmt_ms, Block, ToolStatus, SPINNER};
use super::diff;
use super::summary::{classify, What};
use super::wrap::{truncate, width};
use crate::theme::Theme;

/// One phrase of the line: all the calls that do the same thing.
struct Row {
    what: What,
    /// The kind word the turn's summary counts it under (`read`, `edit`, `run`...).
    kind: &'static str,
    /// What an `Other` call is named by (the server of an MCP tool, else the tool).
    name: String,
    n: usize,
    /// Some call of the row has not finished.
    live: bool,
    added: usize,
    removed: usize,
    diffs: bool,
}

/// The name a tool is counted under: `mcp__slack__post` is `slack`.
fn call_name(tool: &str) -> String {
    match tool.strip_prefix("mcp__").and_then(|r| r.split_once("__")) {
        Some((server, t)) if !server.is_empty() && !t.is_empty() => server.to_string(),
        _ => tool.to_string(),
    }
}

thread_local! {
    /// The line totals of each edit call, by call id: a live group is drawn every tick and its
    /// diffs are not worth running again each time.
    static STATS: std::cell::RefCell<std::collections::HashMap<String, (usize, usize)>> = Default::default();
}

fn stats_of(id: &str, old: &str, new: &str) -> (usize, usize) {
    if let Some(s) = STATS.with(|m| m.borrow().get(id).copied()) {
        return s;
    }
    let s = diff::stats(old, new);
    STATS.with(|m| m.borrow_mut().insert(id.to_string(), s));
    s
}

/// The phrases of the calls in `members`, in the order each kind first appears.
fn rows(members: &[Block]) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    for b in members {
        let Block::Tool { id, name, status, output, .. } = b else { continue };
        let class = classify(name);
        let what = class.what;
        let label = if what == What::Other { call_name(name) } else { String::new() };
        let at = match out.iter().position(|r| r.what == what && r.name == label) {
            Some(i) => i,
            None => {
                out.push(Row {
                    what,
                    kind: class.kind,
                    name: label,
                    n: 0,
                    live: false,
                    added: 0,
                    removed: 0,
                    diffs: false,
                });
                out.len() - 1
            }
        };
        let r = &mut out[at];
        r.n += 1;
        r.live |= matches!(status, ToolStatus::Running | ToolStatus::Pending);
        if what == What::Edit {
            if let Some(o) = output {
                // the totals come from what the tool returned, never from the disk
                match &o.display {
                    ToolDisplay::Diff { old, new, .. } => {
                        let (a, d) = stats_of(id, old, new);
                        r.added += a;
                        r.removed += d;
                        r.diffs = true;
                    }
                    _ if !o.is_error => {
                        if let Some((a, d)) = crate::app::parse_counts(&o.content) {
                            r.added += a;
                            r.removed += d;
                            r.diffs = true;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

/// One phrase, `read 2 files` or `reading 2 files…` (lower case: the first one is capitalised later).
fn phrase(r: &Row) -> String {
    let n = r.n;
    let (past, pres, object) = match r.what {
        What::File => ("read", "reading", nexo_core::count(n, "file")),
        What::Page => ("fetched", "fetching", nexo_core::count(n, "page")),
        What::Edit => ("made", "making", nexo_core::count(n, "edit")),
        What::Shell => ("ran", "running", nexo_core::count(n, "shell command")),
        What::Pattern => ("searched", "searching", nexo_core::count(n, "pattern")),
        What::Dir => ("listed", "listing", nexo_core::count(n, "directory")),
        What::Web => ("searched the web", "searching the web", nexo_core::count(n, "time")),
        What::Agent => ("ran", "running", nexo_core::count(n, "agent")),
        What::Other => ("called", "calling", format!("{} {}", r.name, nexo_core::count(n, "time"))),
    };
    let mut s = format!("{} {object}", if r.live { pres } else { past });
    if r.what == What::Edit && r.diffs {
        s.push_str(&format!(" +{} −{}", r.added, r.removed));
    }
    if r.live {
        s.push('…');
    }
    s
}

/// The sentence for `members`: phrases joined by commas, the first with a capital.
pub fn sentence(members: &[Block]) -> String {
    let mut s = rows(members).iter().map(phrase).collect::<Vec<_>>().join(", ");
    if let Some(c) = s.chars().next() {
        let up: String = c.to_uppercase().collect();
        s.replace_range(..c.len_utf8(), &up);
    }
    s
}

/// The quiet sentence: counts only, `todo, 1 read, 1 edit +1 -1, 1 run`. Calls of one kind are one
/// count (a file and a page are both a read); a tool with no kind of its own is named, not counted.
pub fn sentence_quiet(members: &[Block]) -> String {
    // (is a named tool, the kind word or the name, calls, lines added, lines removed, has a diff)
    let mut parts: Vec<(bool, String, usize, usize, usize, bool)> = Vec::new();
    for r in rows(members) {
        let named = r.what == What::Other;
        let word = if named { r.name.clone() } else { r.kind.to_string() };
        match parts.iter_mut().find(|p| p.0 == named && p.1 == word) {
            Some(p) => {
                p.2 += r.n;
                p.3 += r.added;
                p.4 += r.removed;
                p.5 |= r.diffs;
            }
            None => parts.push((named, word, r.n, r.added, r.removed, r.diffs)),
        }
    }
    let mut out = Vec::new();
    for (named, word, n, added, removed, diffs) in parts {
        let mut s = if named { word } else { format!("{n} {word}") };
        if diffs {
            s.push_str(&format!(" +{added} -{removed}"));
        }
        out.push(s);
    }
    out.join(", ")
}

/// The time the calls took, as the span of the wall clock they cover (parallel calls are not added
/// up): from the first start to now while one runs, to the last end when all are done. None when
/// no call carries a time (a replayed session).
pub fn elapsed_ms(members: &[Block]) -> Option<u64> {
    let mut first: Option<Instant> = None;
    let mut live = false;
    for b in members {
        if let Block::Tool { started, status, .. } = b {
            if let Some(s) = started {
                first = Some(first.map_or(*s, |f| f.min(*s)));
            }
            live |= matches!(status, ToolStatus::Running | ToolStatus::Pending);
        }
    }
    let first = first?;
    if live {
        return Some(first.elapsed().as_millis() as u64);
    }
    members
        .iter()
        .filter_map(|b| match b {
            Block::Tool { started: Some(s), elapsed_ms: Some(e), .. } => {
                Some(s.saturating_duration_since(first).as_millis() as u64 + e)
            }
            _ => None,
        })
        .max()
}

/// The first line of the summary of the call that started last among the live ones.
fn live_call(members: &[Block]) -> Option<String> {
    let mut best: Option<(&Option<Instant>, &str)> = None;
    for b in members {
        if let Block::Tool { status: ToolStatus::Running | ToolStatus::Pending, started, summary, .. } = b {
            if best.is_none_or(|(s, _)| started >= s) {
                best = Some((started, summary));
            }
        }
    }
    let first = best?.1.lines().next().unwrap_or("").trim();
    (!first.is_empty()).then(|| first.to_string())
}

/// The head line of a group of `members`, `w` cells wide.
pub fn header(members: &[Block], folded: bool, spinner: usize, w: usize, theme: &Theme) -> Line<'static> {
    header_for(members, folded, spinner, w, theme, nexo_core::config::Detail::Full)
}

/// The same line; quiet says the calls as counts (`1 read, 1 run`), not as a sentence.
pub fn header_for(
    members: &[Block],
    folded: bool,
    spinner: usize,
    w: usize,
    theme: &Theme,
    detail: nexo_core::config::Detail,
) -> Line<'static> {
    let quiet = detail == nexo_core::config::Detail::Quiet;
    let (mut failed, mut denied, mut live) = (0usize, 0usize, false);
    for b in members {
        if let Block::Tool { status, .. } = b {
            match status {
                ToolStatus::Failed => failed += 1,
                ToolStatus::Denied => denied += 1,
                ToolStatus::Running | ToolStatus::Pending => live = true,
                ToolStatus::Done => {}
            }
        }
    }
    let (icon, istyle): (&str, Style) = if live {
        (SPINNER[spinner % SPINNER.len()], theme.style(theme.accent))
    } else if failed > 0 {
        ("✗", theme.style(theme.error))
    } else if denied > 0 {
        ("⊘", theme.style(theme.warn))
    } else {
        ("✓", theme.style(theme.success))
    };
    let text = if quiet { sentence_quiet(members) } else { sentence(members) };
    let chevron = if folded { " ▸" } else { " ▾" };
    let long = |f: usize, d: usize| {
        (
            if f > 0 { format!(" · {f} failed") } else { String::new() },
            if d > 0 { format!(" · {d} denied") } else { String::new() },
        )
    };
    let (mut fails, mut denies) = long(failed, denied);
    // in a very narrow pane the markers shrink to a symbol and a count so the chevron still fits
    if w < 2 + width(&fails) + width(&denies) + width(chevron) + 4 {
        fails = if failed > 0 { format!(" ✗{failed}") } else { String::new() };
        denies = if denied > 0 { format!(" ⊘{denied}") } else { String::new() };
    }
    let gap = if quiet { " · " } else { "  " };
    let stamp = elapsed_ms(members).map(|ms| format!("{gap}{}", fmt_ms(ms))).unwrap_or_default();
    let fixed = |stamp: &str| 2 + width(&fails) + width(&denies) + width(stamp) + width(chevron);
    // the failures and the chevron are the last to go; the time goes first when the line is tight
    let stamp = if w.saturating_sub(fixed(&stamp)) < width(&text).min(24) { String::new() } else { stamp };
    let room = w.saturating_sub(fixed(&stamp)).max(4);
    let mut spans = vec![Span::styled(format!("{icon} "), istyle), Span::raw(truncate(&text, room))];
    // the call running now takes what the sentence leaves, and is the first thing to go
    if live {
        if let Some(call) = live_call(members) {
            let avail = room.saturating_sub(width(&text) + 3);
            if width(&text) <= room && avail >= 8 {
                spans.push(Span::styled(format!(" · {}", truncate(&call, avail)), theme.dim()));
            }
        }
    }
    if !fails.is_empty() {
        spans.push(Span::styled(fails, theme.style(theme.error)));
    }
    if !denies.is_empty() {
        spans.push(Span::styled(denies, theme.style(theme.warn)));
    }
    if !stamp.is_empty() {
        spans.push(Span::styled(stamp, theme.dim()));
    }
    spans.push(Span::styled(chevron, theme.dim()));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::tool::ToolOutput;
    use std::time::Duration;

    fn call(name: &str, status: ToolStatus) -> Block {
        Block::Tool {
            id: "c".into(),
            name: name.into(),
            summary: name.into(),
            args: serde_json::Value::Null,
            status,
            progress: vec![],
            output: None,
            started: None,
            elapsed_ms: None,
            folded: true,
        }
    }

    fn done(name: &str) -> Block {
        call(name, ToolStatus::Done)
    }

    fn text(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn phrases_are_plural_aware_and_ordered_by_first_use() {
        let m = [done("read"), done("edit"), done("read"), done("ls"), done("bash"), done("bash"), done("bash")];
        assert_eq!(sentence(&m), "Read 2 files, made 1 edit, listed 1 directory, ran 3 shell commands");
        let m =
            [done("ls"), done("ls"), done("grep"), done("glob"), done("web_search"), done("web_fetch"), done("task")];
        assert_eq!(
            sentence(&m),
            "Listed 2 directories, searched 2 patterns, searched the web 1 time, fetched 1 page, ran 1 agent"
        );
    }

    #[test]
    fn a_running_kind_reads_in_the_present_and_the_others_in_the_past() {
        let m = [done("read"), call("read", ToolStatus::Running), done("bash"), call("bash", ToolStatus::Pending)];
        assert_eq!(sentence(&m), "Reading 2 files…, running 2 shell commands…");
        let m = [done("read"), done("read"), call("bash", ToolStatus::Running)];
        assert_eq!(sentence(&m), "Read 2 files, running 1 shell command…");
    }

    #[test]
    fn edit_totals_come_from_the_diff_display_only() {
        let mut e = done("edit");
        if let Block::Tool { output, .. } = &mut e {
            *output = Some(ToolOutput::ok("edited a.rs").with_display(ToolDisplay::Diff {
                path: "a.rs".into(),
                old: "a\nb\nc\n".into(),
                new: "a\nx\ny\nz\n".into(),
                start_line: 1,
            }));
        }
        assert_eq!(sentence(&[e.clone(), done("read")]), "Made 1 edit +3 −2, read 1 file");
        // a call that returned no counts shows none: nothing is read from the disk
        assert_eq!(sentence(&[done("edit"), done("read")]), "Made 1 edit, read 1 file");
        // a replayed result says it in its text
        let mut r = done("write");
        if let Block::Tool { output, .. } = &mut r {
            *output = Some(ToolOutput::ok("wrote b.rs: +29 −3 lines"));
        }
        assert_eq!(sentence(&[e, r]), "Made 2 edits +32 −5");
    }

    #[test]
    fn mcp_calls_read_called_x_n_times() {
        let m = [done("mcp__slack__post"), done("mcp__slack__read"), done("mcp__slack__post"), done("todo")];
        assert_eq!(sentence(&m), "Called slack 3 times, called todo 1 time");
        let m = [done("todo"), call("todo", ToolStatus::Running)];
        assert_eq!(sentence(&m), "Calling todo 2 times…");
    }

    #[test]
    fn elapsed_is_wall_span_not_sum() {
        let t0 = Instant::now() - Duration::from_secs(60);
        let mk = |start: u64, took: u64| {
            let mut b = done("bash");
            if let Block::Tool { started, elapsed_ms, .. } = &mut b {
                *started = Some(t0 + Duration::from_secs(start));
                *elapsed_ms = Some(took * 1000);
            }
            b
        };
        // two parallel calls of 4 s that overlap, then one later: 0..4, 1..5, 6..7
        let m = [mk(0, 4), mk(1, 4), mk(6, 1)];
        assert_eq!(elapsed_ms(&m), Some(7000));
        // none timed (a replay): no clock
        assert_eq!(elapsed_ms(&[done("bash")]), None);
        // running: from the first start to now
        let mut r = mk(0, 1);
        if let Block::Tool { status, elapsed_ms, .. } = &mut r {
            *status = ToolStatus::Running;
            *elapsed_ms = None;
        }
        let e = elapsed_ms(&[mk(0, 1), r]).unwrap();
        assert!((59_000..70_000).contains(&e), "{e}");
    }

    #[test]
    fn failed_count_in_error_colour_never_cut_at_narrow_width() {
        let theme = Theme::dark();
        let m = [done("read"), call("bash", ToolStatus::Failed), done("edit"), call("bash", ToolStatus::Denied)];
        let h = header(&m, true, 0, 80, &theme);
        let t = text(&h);
        assert!(t.starts_with("✗ Read 1 file, ran 2 shell commands, made 1 edit"), "{t}");
        assert!(t.contains(" · 1 failed") && t.contains(" · 1 denied") && t.ends_with('▸'), "{t}");
        let red = h.spans.iter().find(|s| s.content.contains("1 failed")).unwrap();
        assert_eq!(red.style.fg, Some(theme.error));
        for w in [30usize, 40, 50] {
            let t = text(&header(&m, true, 0, w, &theme));
            assert!(width(&t) <= w, "{w}: {t:?} is {}", width(&t));
            assert!(t.contains("1 failed") && t.contains("1 denied"), "{w}: {t}");
        }
    }

    #[test]
    fn both_markers_fit_a_very_narrow_pane() {
        let theme = Theme::dark();
        let m = [done("read"), call("bash", ToolStatus::Failed), call("bash", ToolStatus::Denied)];
        for w in [20usize, 24, 26] {
            let t = text(&header(&m, true, 0, w, &theme));
            assert!(width(&t) <= w, "{w}: {t:?}");
            assert!(t.contains("✗1") && t.contains("⊘1") && t.ends_with('▸'), "{w}: {t}");
        }
    }

    #[test]
    fn the_icon_follows_the_state_and_the_chevron_the_fold() {
        let theme = Theme::dark();
        let t = text(&header(&[done("read"), done("read")], false, 0, 60, &theme));
        assert_eq!(t, "✓ Read 2 files ▾");
        let t = text(&header(&[done("read"), call("bash", ToolStatus::Running)], true, 0, 60, &theme));
        assert!(t.starts_with(&format!("{} Read 1 file, running", SPINNER[0])), "{t}");
    }

    fn running(summary: &str, start_ago: u64) -> Block {
        let mut b = call("bash", ToolStatus::Running);
        if let Block::Tool { summary: s, started, .. } = &mut b {
            *s = summary.into();
            *started = Some(Instant::now() - Duration::from_secs(start_ago));
        }
        b
    }

    #[test]
    fn a_live_group_shows_the_call_running_now_dim() {
        let theme = Theme::dark();
        let m = [done("bash"), running("old call", 30), running("ssh minipc docker ps", 5)];
        let h = header(&m, true, 0, 100, &theme);
        let t = text(&h);
        assert!(t.contains("Running 3 shell commands… · ssh minipc docker ps  "), "{t}");
        assert!(t.ends_with('▸'), "{t}");
        let dim = h.spans.iter().find(|s| s.content.contains("docker ps")).unwrap();
        assert_eq!(dim.style, theme.dim());
        // open and quiet too
        assert!(text(&header(&m, false, 0, 100, &theme)).contains("· ssh minipc docker ps"));
        let q = text(&header_for(&m, true, 0, 100, &theme, nexo_core::config::Detail::Quiet));
        assert!(q.contains("· ssh minipc docker ps"), "{q}");
    }

    #[test]
    fn a_finished_group_is_unchanged() {
        let theme = Theme::dark();
        let m = [done("bash"), done("bash"), done("read")];
        assert_eq!(text(&header(&m, true, 0, 80, &theme)), "✓ Ran 2 shell commands, read 1 file ▸");
    }

    #[test]
    fn a_narrow_width_drops_the_call_before_the_time_and_the_chevron() {
        let theme = Theme::dark();
        let mut a = running("ssh minipc docker ps --format json", 400);
        if let Block::Tool { summary, .. } = &mut a {
            summary.push_str(" and more");
        }
        let m = [a];
        let full = text(&header(&m, true, 0, 200, &theme));
        let base = full.split(" · ").next().unwrap().to_string();
        for w in 20..90usize {
            let t = text(&header(&m, true, 0, w, &theme));
            assert!(width(&t) <= w, "{w}: {t:?}");
            if !t.contains(" · ") {
                assert!(t.ends_with('▸'), "{w}: {t}");
            }
            if t.contains("docker") {
                assert!(t.contains(&base) && t.contains("6m4") && t.ends_with('▸'), "{w}: {t}");
            }
        }
        // some width keeps the time and chevron with the call already gone
        let t = text(&header(&m, true, 0, 40, &theme));
        assert!(!t.contains("ssh") && t.contains("6m4") && t.ends_with('▸'), "{t}");
    }

    #[test]
    fn a_multi_line_summary_shows_its_first_line_only() {
        let theme = Theme::dark();
        let t = text(&header(&[running("echo one\necho two", 3)], true, 0, 100, &theme));
        assert!(t.contains("· echo one") && !t.contains("two"), "{t}");
    }
}
