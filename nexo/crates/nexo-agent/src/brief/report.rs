//! The report coming back from a sub-agent, compacted.

use super::*;

/// The report as it enters the main conversation: lines that repeat the
/// briefing's excerpts word for word left out (the main agent has them), and
/// at most `cap` tokens.
pub fn compact_report(report: &str, brief: Option<&str>, cap: usize) -> String {
    let known: BTreeSet<&str> =
        brief.map(|b| b.lines().map(str::trim).filter(|l| l.len() >= 40).collect()).unwrap_or_default();
    let mut out = String::new();
    let mut dropped = 0usize;
    let flush = |out: &mut String, dropped: &mut usize| {
        if *dropped > 0 {
            out.push_str(&format!("(… {} line{} you already have)\n", *dropped, if *dropped == 1 { "" } else { "s" }));
            *dropped = 0;
        }
    };
    for line in report.lines() {
        let t = line.trim().trim_start_matches(['>', '-', '*', ' ']);
        let body = numbered(t).map(|(_, r)| r.trim()).unwrap_or(t);
        if t.len() >= 40 && (known.contains(t) || known.iter().any(|k| k.ends_with(body) && body.len() >= 40)) {
            dropped += 1;
            continue;
        }
        flush(&mut out, &mut dropped);
        out.push_str(line);
        out.push('\n');
    }
    flush(&mut out, &mut dropped);
    let out = out.trim_end().to_string();
    if tokens(&out) <= cap {
        return out;
    }
    let mut cut = String::new();
    for line in out.lines() {
        if tokens(&cut) + tokens(line) > cap.saturating_sub(20) {
            break;
        }
        cut.push_str(line);
        cut.push('\n');
    }
    format!("{}\n(… the report went on; cut at about {cap} tokens)", cut.trim_end())
}
