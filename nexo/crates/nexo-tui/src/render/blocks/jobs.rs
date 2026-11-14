//! The card for background jobs that ended.

use super::*;

pub(super) const JOB_NOTE: &str = "[background jobs ended]\n";

/// Whether a user-role message is the note nexo-core sends when background jobs end.
pub fn is_job_note(text: &str) -> bool {
    text.starts_with(JOB_NOTE)
}

/// One ended job from the note: its id, what ran, whether it went well, and the facts.
pub(super) struct JobRow<'a> {
    id: &'a str,
    what: &'a str,
    ok: bool,
    facts: String,
}

pub(super) fn parse_job_row(line: &str) -> Option<JobRow<'_>> {
    let rest = line.strip_prefix("- job ")?;
    let (id, rest) = rest.split_once(' ')?;
    let (rest, lines) = rest.rsplit_once(", ")?;
    let n = lines.strip_suffix(" lines of output").or_else(|| lines.strip_suffix(" line of output"))?;
    let (rest, dur) = rest.rsplit_once(" after ")?;
    // `done (how)` went well, `failed (how)` did not; notes written before that split say `done` for both,
    // and only a clean exit or a report was a success then. The last marker is the state, whatever the label says.
    let (what, how, ok) = if let Some(w) = rest.strip_suffix(" killed") {
        (w, "killed".to_string(), false)
    } else if let Some(w) = rest.strip_suffix(" failed") {
        (w, "failed".to_string(), false)
    } else {
        let (d, f) = (rest.rfind(" done ("), rest.rfind(" failed ("));
        if f > d {
            let i = f?;
            (&rest[..i], rest[i + 9..].strip_suffix(')')?.to_string(), false)
        } else {
            let i = d?;
            let how = rest[i + 7..].strip_suffix(')')?;
            (&rest[..i], how.to_string(), how == "exit 0" || how == "reported")
        }
    };
    let what = what.strip_prefix('`').and_then(|w| w.strip_suffix('`')).unwrap_or(what);
    let unit = if n == "1" { "line" } else { "lines" };
    // a failure says why in words, not a bare mark
    let how = if ok || how == "killed" || how == "failed" || how.starts_with("exit ") {
        how
    } else {
        format!("failed: {how}")
    };
    Some(JobRow { id, what, ok, facts: format!("{how} · {dur} · {n} {unit}") })
}

/// The note as a quiet system card: a dim rule, then one row per job. Only the screen changes; the
/// model keeps the exact text.
pub(super) fn job_card(text: &str, w: usize, theme: &Theme) -> Vec<Line<'static>> {
    let dim = theme.dim();
    let title = " jobs ended ";
    let rule = format!("──{title}{}", "─".repeat(w.saturating_sub(2 + width(title))));
    let mut lines = vec![Line::from(Span::styled(truncate(&rule, w), theme.style(theme.dim2)))];
    for raw in text.lines().skip(1) {
        // the note ends at its last line; a message glued after it is not a job row
        if raw.starts_with("Read the output with") {
            break;
        }
        if raw.trim().is_empty() {
            continue;
        }
        let Some(r) = parse_job_row(raw) else {
            lines.push(Line::from(Span::styled(truncate(raw, w), dim)));
            continue;
        };
        let mark = if r.ok { "✓" } else { "✗" };
        let mstyle = if r.ok { theme.style(theme.success) } else { theme.style(theme.error) };
        let head = format!("⚙ job {}  ", r.id);
        let tail = format!("{mark} {}", r.facts);
        let room = w.saturating_sub(width(&head) + width(&tail) + 2);
        let what = truncate(r.what, room);
        let gap = w.saturating_sub(width(&head) + width(&what) + width(&tail)).max(2);
        lines.push(Line::from(vec![
            Span::styled(head, dim),
            Span::raw(what),
            Span::raw(" ".repeat(gap)),
            Span::styled(mark.to_string(), mstyle),
            Span::styled(format!(" {}", r.facts), if r.ok { dim } else { mstyle }),
        ]));
    }
    lines
}
