//! The card for background jobs that ended.

use super::super::wrap::wrap_runs;
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
    let how = if ok || how == "killed" || how == "failed" || how.starts_with("exit ") {
        how
    } else {
        format!("failed: {how}")
    };
    Some(JobRow { id, what, ok, facts: format!("{how} · {dur} · {n} {unit}") })
}

/// A quiet card for rows followed by full Markdown reports. The model keeps the original text.
pub(super) fn job_card(text: &str, w: usize, theme: &Theme) -> Vec<Line<'static>> {
    let dim = theme.dim();
    let title = " jobs ended ";
    let rule = format!("──{title}{}", "─".repeat(w.saturating_sub(2 + width(title))));
    let mut lines = vec![Line::from(Span::styled(truncate(&rule, w), theme.style(theme.dim2)))];
    let body = text.strip_prefix(JOB_NOTE).unwrap_or(text);
    let mut offset = 0;
    for raw in body.split_inclusive('\n') {
        let row = raw.trim_end_matches('\n');
        if let Some(r) = parse_job_row(row) {
            let mark = if r.ok { "✓" } else { "✗" };
            let mstyle = if r.ok { theme.style(theme.success) } else { theme.style(theme.error) };
            let runs = vec![
                (format!("⚙ job {}  ", r.id), dim),
                (format!("{}  ", r.what), Style::default()),
                (format!("{mark} {}", r.facts), if r.ok { dim } else { mstyle }),
            ];
            lines.extend(wrap_runs(&runs, w, &[], &[]));
        } else if !row.trim().is_empty() {
            break;
        }
        offset += raw.len();
    }
    let report = body[offset..].trim();
    // The producer appends this instruction when at least one output is missing.
    let instruction = "Read the output with the `jobs` tool (action \"output\", the job's id).";
    let (report, missing_output) = match report.strip_suffix(instruction) {
        Some(report) => (report.trim_end(), true),
        None => (report, false),
    };
    if !report.is_empty() {
        lines.push(Line::default());
        lines.extend(markdown::render(report, w, theme));
    }
    if missing_output {
        lines.push(Line::default());
        lines.extend(markdown::render(&format!("Some job output is not included. {instruction}"), w, theme));
    }
    lines
}
