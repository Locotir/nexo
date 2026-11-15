//! The note the model reads when jobs ended, and the short forms it uses.

use super::*;

/// `text` within `max` characters: the head and the tail, the middle said to be cut.
pub(super) fn cap(text: &str, max: usize) -> String {
    let n = text.chars().count();
    if n <= max {
        return text.to_string();
    }
    let half = max / 2;
    let head: String = text.chars().take(half).collect();
    let tail: String = text.chars().skip(n - half).collect();
    format!("{head}\n… [{} characters omitted] …\n{tail}", n - 2 * half)
}

/// The note the model gets when jobs have ended.
pub fn ended_note(jobs: &[JobInfo]) -> String {
    note(jobs, &vec![None; jobs.len()], None)
}

/// Characters of a sub-agent's report the ended note carries (as many as one `jobs` read gives).
pub const REPORT_NOTE_CHARS: usize = READ_CHARS;

/// The least characters of one report the note keeps when the window is small.
const MIN_REPORT_CHARS: usize = 1_000;

/// With `room`, the reports share what one result may take of the window: each is cut to its part of it, and says so.
pub(super) fn note(jobs: &[JobInfo], reports: &[Option<String>], room: Option<crate::tool::ResultRoom>) -> String {
    // never less than a line or two of a report, however many jobs ended together
    let each = room.map(|r| (r.chars / reports.iter().flatten().count().max(1)).max(MIN_REPORT_CHARS));
    let lines: Vec<String> = jobs
        .iter()
        .map(|j| {
            let what = match j.kind {
                JobKind::Shell => format!("`{}`", one_line(&j.label)),
                JobKind::Agent => format!("sub-agent \"{}\"", one_line(&j.label)),
            };
            format!(
                "- job {} {what} {} after {}, {} lines of output",
                j.id,
                j.status.note_word(),
                duration(j.elapsed_ms),
                j.lines
            )
        })
        .collect();
    let mut out = format!("[background jobs ended]\n{}", lines.join("\n"));
    for (j, r) in jobs.iter().zip(reports) {
        if let Some(r) = r {
            let today = REPORT_NOTE_CHARS;
            let kept = match each {
                Some(e) if e < today => e.saturating_sub(crate::tool::CUT_LINE_CHARS),
                _ => today,
            };
            out.push_str(&format!("\n\n--- report of job {} ---\n{}", j.id, cap(r, kept)));
            let whole = r.chars().count();
            if let (Some(room), true) = (room, kept < today && whole > kept) {
                out.push_str(&format!(
                    "\n[cut to {kept} of {whole} characters for a {}-token window; read the whole report with the \
                     `jobs` tool (action \"output\", job {})]",
                    room.window, j.id
                ));
            }
        }
    }
    // a job whose report is not here is read with the tool
    if reports.iter().any(Option::is_none) {
        out.push_str("\nRead the output with the `jobs` tool (action \"output\", the job's id).");
    }
    out
}

pub(super) fn one_line(s: &str) -> String {
    let first = s.lines().next().unwrap_or("");
    let short: String = first.chars().take(80).collect();
    if short.len() < s.len() {
        format!("{short}…")
    } else {
        short
    }
}

/// `850 ms`, `12 s`, `3 min 5 s`.
pub fn duration(ms: u64) -> String {
    match ms {
        0..=999 => format!("{ms} ms"),
        1000..=59_999 => format!("{} s", ms / 1000),
        _ => format!("{} min {} s", ms / 60_000, ms % 60_000 / 1000),
    }
}
