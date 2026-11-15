//! The `debug` call for an `strace` or `ltrace` of a program of the project.

use std::path::Path;

use super::detect::{input_for, run_text};
use super::{LTRACE_VALUED, STRACE_VALUED};
use crate::hint_cmd::{self, Seg};

/// `strace`/`ltrace` of a program of the project as the debugger's own: the trace on at the entry, the run, the log.
pub(super) fn trace_call(segs: &[Seg], k: usize, cwd: &Path) -> Option<String> {
    let s = &segs[k];
    let tool = hint_cmd::program(s.words.first()?);
    let valued: &[&str] = if tool == "strace" { &STRACE_VALUED } else { &LTRACE_VALUED };
    let mut filter: Option<String> = None;
    let mut i = 1;
    while i < s.words.len() {
        let a = s.words[i].as_str();
        if !a.starts_with('-') || a == "-" {
            break;
        }
        i += 1;
        if a == "--" {
            break;
        }
        let numeric = |w: Option<&String>| w.is_some_and(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()));
        if a == "-p" || (!a.starts_with("--") && a.ends_with('p') && numeric(s.words.get(i))) {
            return None;
        }
        if a == "-e" {
            filter = s.words.get(i).cloned();
            i += 1;
        } else if let Some(v) = a
            .strip_prefix("--trace=")
            .or_else(|| a.strip_prefix("-e").filter(|v| !a.starts_with("--") && !v.is_empty()))
        {
            filter = Some(v.to_string());
        } else if valued.contains(&a) {
            i += 1;
        }
    }
    let prog = s.words.get(i).filter(|w| hint_cmd::opens(&s.dir, w))?;
    let run = run_text(cwd, s, prog, &s.words[i + 1..], input_for(segs, k).as_deref())?;
    let names = filter.and_then(|f| {
        let f =
            f.trim_start_matches("trace=").trim_start_matches("t=").trim_start_matches("syscall=").replace('+', ",");
        let ok = !f.is_empty()
            && f.len() <= 80
            && f.split(',').all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        ok.then_some(f)
    });
    let on = match &names {
        Some(n) => format!("{tool} {n}"),
        None => tool.clone(),
    };
    Some(format!("{run}; {on}; cont; {tool} show"))
}
