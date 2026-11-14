//! The text the model reads back: snapshots, console, network, tabs and found elements.

use super::*;

/// The snapshot as the model reads it, cut at [`MAX_ELEMENTS`] lines.
pub(super) fn show_snapshot(snap: &Snapshot) -> String {
    let text = snap.render();
    if text.is_empty() {
        return "(no actionable elements on the page; try read_text, or wait and snapshot again)".into();
    }
    let total = text.lines().count();
    if total <= MAX_ELEMENTS {
        return text;
    }
    let head: Vec<&str> = text.lines().take(MAX_ELEMENTS).collect();
    format!(
        "{}\n… {} more elements; scroll and snapshot again, or target one by its text",
        head.join("\n"),
        total - MAX_ELEMENTS
    )
}

/// The tree of `read_page`; an empty one says so, so the model does not take it for a failure.
pub(super) fn show_tree(text: &str) -> String {
    if text.is_empty() {
        "(nothing on the page; it may still be loading: wait, then read_page again)".into()
    } else {
        text.to_string()
    }
}

/// The text of `read_text`; when the page's main part was picked, a line says so and how to get the rest.
pub(super) fn show_text(t: &PageText) -> String {
    if t.text.trim().is_empty() {
        return "(no text)".into();
    }
    let text = cap_text(&t.text, MAX_TEXT);
    match t.source.as_str() {
        "selector" | "body" => text,
        main => format!("{text}\n\n[main content only ({main}); selector \"body\" reads the whole page]"),
    }
}

pub(super) fn fill_report(r: &FillReport) -> String {
    let mut out = format!("filled {}: {}", r.filled.len(), r.filled.join(", "));
    if !r.missing.is_empty() {
        out.push_str(&format!("\nNOT FOUND ({}): {}", r.missing.len(), r.missing.join(", ")));
    }
    if !r.failed.is_empty() {
        let items: Vec<String> = r.failed.iter().map(|(t, why)| format!("{t} ({why})")).collect();
        out.push_str(&format!("\nFAILED ({}): {}", r.failed.len(), items.join("; ")));
    }
    out
}

/// `level  text`, the latest [`MAX_ENTRIES`].
pub(super) fn show_console(list: &[ConsoleEntry]) -> String {
    show_lines(list, |e| format!("{}  {}", e.level, one_line(&e.text, 300)))
}

/// ` with ctrl+shift held`, or nothing without modifiers.
pub(super) fn held(m: Modifiers) -> String {
    if m.is_none() {
        String::new()
    } else {
        format!(" with {m} held")
    }
}

/// `number method status url`, the latest [`MAX_ENTRIES`]; `-` while no response has come. The number is
/// what `network` takes in `to` for the full request.
pub(super) fn show_network(list: &[NetEntry]) -> String {
    show_lines(list, |e| {
        let status = e.status.map_or_else(|| "-".to_string(), |s| s.to_string());
        format!("{} {} {status} {}", e.seq, e.method, one_line(&e.url, 300))
    })
}

/// One request in full: status, timing, headers both ways and the start of the body.
pub(super) fn show_request(d: &NetDetail) -> String {
    let e = &d.entry;
    let status = e.status.map_or_else(|| "no response".to_string(), |s| format!("{s} {}", e.status_text));
    let mut out = format!("{} {}\n{}", e.method, one_line(&e.url, 600), status.trim());
    if !e.mime.is_empty() {
        out.push_str(&format!("  {}", e.mime));
    }
    let ms = |v: f64| format!("{} ms", v.round());
    match (e.first_byte_ms, e.duration_ms) {
        (Some(first), Some(total)) => {
            out.push_str(&format!("\ntime: {} total, first byte after {}", ms(total), ms(first)))
        }
        (None, Some(total)) => out.push_str(&format!("\ntime: {}", ms(total))),
        (Some(first), None) => out.push_str(&format!("\ntime: first byte after {}, still loading", ms(first))),
        (None, None) => {}
    }
    if let Some(err) = &e.error {
        out.push_str(&format!("\nFAILED: {err}"));
    }
    for (title, list) in [("request headers", &e.request_headers), ("response headers", &e.response_headers)] {
        if !list.is_empty() {
            out.push_str(&format!("\n{title}:"));
            for (k, v) in list {
                out.push_str(&format!("\n  {k}: {}", one_line(v, MAX_HEADER)));
            }
        }
    }
    match (&d.body, &d.body_note) {
        (Some(body), note) => {
            let note = note.as_ref().map(|n| format!(" ({n})")).unwrap_or_default();
            out.push_str(&format!("\nbody{note}:\n{body}"));
        }
        (None, Some(note)) => out.push_str(&format!("\nbody: {note}")),
        (None, None) => out.push_str("\nbody: (none)"),
    }
    cap_text(&out, MAX_DETAIL)
}

/// The matches of `find` as snapshot lines, best first.
pub(super) fn show_found(query: &str, hits: &[nexo_browser::Element], total: usize) -> String {
    if hits.is_empty() {
        return format!("(nothing on the page matches {query:?}; try other words, snapshot, or read_text)");
    }
    let snap = Snapshot { elements: hits.to_vec() };
    match total.saturating_sub(hits.len()) {
        0 => snap.render(),
        more => format!("{}\n… {more} weaker matches; use more specific words", snap.render()),
    }
}

pub(super) fn show_lines<T>(list: &[T], line: impl Fn(&T) -> String) -> String {
    if list.is_empty() {
        return "(none)".into();
    }
    let skip = list.len().saturating_sub(MAX_ENTRIES);
    let mut lines: Vec<String> = list[skip..].iter().map(line).collect();
    if skip > 0 {
        lines.insert(0, format!("… {skip} earlier entries left out"));
    }
    cap_text(&lines.join("\n"), MAX_LIST)
}

/// `index* title  url`; the star marks the tab the actions work on.
pub(super) fn show_tabs(tabs: &[TabInfo]) -> String {
    let lines: Vec<String> = tabs
        .iter()
        .map(|t| {
            format!(
                "{}{} {}  {}",
                t.index,
                if t.active { "*" } else { "" },
                one_line(&t.title, 80),
                one_line(&t.url, 200)
            )
        })
        .collect();
    lines.join("\n")
}
