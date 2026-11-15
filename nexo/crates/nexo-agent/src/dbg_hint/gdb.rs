//! The `debug` call for a `gdb` command: its program, its `-ex` lines and its script.

use std::path::Path;

use super::detect::{input_text, plain, run_text, script_lines, stdin_text};
use super::translate::translate;
use super::{GDB_VALUED, MAX_CALL, MAX_CMDS};
use crate::hint_cmd::{self, Seg};

/// The debug call for the `gdb` command at `k`.
pub(super) fn gdb_call(segs: &[Seg], k: usize, cwd: &Path) -> Option<String> {
    let s = &segs[k];
    let args = &s.words[1..];
    let mut target: Option<String> = None;
    let mut core: Option<String> = None;
    let mut shell_args: Vec<String> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;
        match a {
            "--args" | "-args" => {
                target = args.get(i).cloned();
                shell_args = args.get(i + 1..).map(<[String]>::to_vec).unwrap_or_default();
                break;
            }
            "-ex" | "--ex" | "-eval-command" | "--eval-command" => {
                lines.extend(args.get(i).cloned());
                i += 1;
            }
            "-x" | "--command" | "-command" => {
                if let Some(f) = args.get(i) {
                    lines.extend(script_lines(segs, k, f));
                }
                i += 1;
            }
            // attaching to a process is not a program of the project
            "-p" | "-pid" | "--pid" => return None,
            _ if a.starts_with("--eval-command=") => lines.push(a["--eval-command=".len()..].to_string()),
            _ if a.starts_with("--command=") => lines.extend(script_lines(segs, k, &a["--command=".len()..])),
            _ if a.starts_with("--core=") => core = Some(a["--core=".len()..].to_string()),
            _ if GDB_VALUED.contains(&a) => i += 1,
            _ if a.starts_with('-') => {}
            _ => {
                if target.is_none() {
                    target = Some(a.to_string());
                } else if core.is_none() {
                    core = Some(a.to_string());
                }
            }
        }
    }
    let target = target.filter(|t| hint_cmd::opens(&s.dir, t))?;
    let target = &target;
    // the second word is a core file only when it is one; a process id or a missing name is no program of ours
    let core = core.filter(|c| std::fs::metadata(s.dir.join(c)).is_ok_and(|m| m.is_file()));
    let core = core.as_ref();
    if let Some(t) = stdin_text(segs, k) {
        lines.extend(t.lines().map(str::to_string));
    }
    let plan = translate(&lines, &s.dir);
    // every breakpoint, watch and catch of the script was left out: the call would stop nowhere the script did
    if plan.stops_wanted > 0 && plan.stops_kept == 0 {
        return None;
    }
    let (run_args, input) = plan.args.unwrap_or((shell_args, None));
    let input = input.as_ref().and_then(input_text);
    let mut calls: Vec<String> = Vec::new();
    match core {
        Some(c) => {
            let shown = hint_cmd::shown(cwd, &s.dir, c);
            let exe = hint_cmd::shown(cwd, &s.dir, target);
            if !plain(&shown) || !plain(&exe) {
                return None;
            }
            calls.push(format!("core {} --exe {}", hint_cmd::quoted(&shown), hint_cmd::quoted(&exe)));
            calls.extend(plan.cmds.iter().chain(&plan.hit).cloned());
            if plan.cmds.is_empty() && plan.hit.is_empty() {
                calls.push("bt".into());
            }
        }
        None => {
            calls.push(run_text(cwd, s, target, &run_args, input.as_deref())?);
            // breakpoints come after `run`, which stops at the entry: gdb's `run` is the `cont` that follows them
            // no `run` in the script: the breakpoints and watches still go before it
            let at = plan.run_at.unwrap_or_else(|| {
                plan.cmds
                    .iter()
                    .rposition(|c| ["break", "tbreak", "watch", "catch"].iter().any(|v| c.split(' ').next() == Some(v)))
                    .map_or(0, |i| i + 1)
            });
            let (pre, post) = plan.cmds.split_at(at);
            calls.extend(pre.iter().cloned());
            calls.push("cont".into());
            calls.extend(post.iter().skip(post.first().is_some_and(|c| c == "cont") as usize).cloned());
            calls.extend(plan.hit.iter().cloned());
        }
    }
    // `next; next` is two steps; the same word twice in a row is otherwise said once
    calls.dedup_by(|a, b| a == b && !(a.starts_with("next") || a.starts_with("step")));
    fit(&mut calls).then(|| calls.join("; "))
}

/// Keeps the call within `MAX_CMDS` commands and `MAX_CALL` characters without losing the `cont` that runs the
/// program: what follows it goes first, then the last of the breakpoints before it. False when `run` and `cont`
/// alone do not fit.
pub(super) fn fit(calls: &mut Vec<String>) -> bool {
    let too_big = |c: &[String]| c.len() > MAX_CMDS || c.join("; ").len() > MAX_CALL;
    let mut cont = calls.iter().position(|c| c == "cont");
    while too_big(calls) {
        match cont {
            Some(at) if calls.len() > at + 1 => {
                calls.pop();
            }
            Some(at) if at >= 2 => {
                calls.remove(at - 1);
                cont = Some(at - 1);
            }
            None if calls.len() > 2 => {
                calls.pop();
            }
            _ => return false,
        }
    }
    true
}
