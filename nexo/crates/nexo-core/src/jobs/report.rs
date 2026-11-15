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
    note(jobs, &vec![None; jobs.len()])
}

/// Characters of a sub-agent's report the ended note carries (as many as one `jobs` read gives).
pub const REPORT_NOTE_CHARS: usize = READ_CHARS;

pub(super) fn note(jobs: &[JobInfo], reports: &[Option<String>]) -> String {
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
            out.push_str(&format!("\n\n--- report of job {} ---\n{}", j.id, cap(r, REPORT_NOTE_CHARS)));
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
