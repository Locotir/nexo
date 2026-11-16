//! The fixed lists of calls that are never approved, and the stricter one that no mode lets through.

use std::path::Path;

use super::paths::outside;
use super::shell::{program, segments};

/// Why a command would wipe the machine beyond doubt: a recursive delete of the root or home folder,
/// a file system made or raw bytes written over a disk, a fork bomb. No mode, rule or approval runs
/// these; everything less certain is left to the person's own rules.
pub fn catastrophic(cmd: &str) -> Option<&'static str> {
    let packed: String = cmd.chars().filter(|c| !c.is_whitespace()).collect();
    if packed.contains(":(){:|:&};:") {
        return Some("a fork bomb");
    }
    let disk =
        |a: &str| a.starts_with("/dev/") && !matches!(a, "/dev/null" | "/dev/zero" | "/dev/stdout" | "/dev/stderr");
    for seg in segments(cmd) {
        let (prog, args) = program(&seg.words);
        let flags = || args.iter().filter(|a| a.starts_with('-') && !a.starts_with("--"));
        let operands = || args.iter().filter(|a| !a.starts_with('-'));
        match prog {
            "rm" => {
                let recursive = args.iter().any(|a| a == "--recursive") || flags().any(|f| f.contains(['r', 'R']));
                let whole = |a: &String| {
                    matches!(
                        a.trim_end_matches('/'),
                        "" | "/*" | "~" | "$HOME" | "${HOME}" | "~/*" | "$HOME/*" | "/home" | "/Users" | "/Users/*"
                    )
                };
                if recursive && operands().any(whole) {
                    return Some("a recursive delete of the root or home folder");
                }
            }
            p if p.starts_with("mkfs") && operands().any(|a| disk(a)) => return Some("a new file system over a disk"),
            // macOS: `newfs_apfs /dev/disk3`, `diskutil eraseDisk ...`, `diskutil apfs deleteContainer ...`
            p if p.starts_with("newfs_") && operands().any(|a| disk(a)) => {
                return Some("a new file system over a disk")
            }
            "diskutil" if wipes_disk(args) => return Some("a disk erased"),
            "dd" if args.iter().any(|a| a.strip_prefix("of=").is_some_and(disk)) => {
                return Some("raw bytes written over a disk");
            }
            "sh" | "bash" | "zsh" | "dash" | "ksh" => {
                let inner = args.iter().position(|a| a == "-c").and_then(|at| args.get(at + 1));
                if let Some(why) = inner.and_then(|c| catastrophic(c)) {
                    return Some(why);
                }
            }
            // `osascript -e 'do shell script "diskutil eraseDisk ..."'` hands its text to a shell
            "osascript" => {
                if let Some(why) = shell_texts(args).iter().find_map(|t| catastrophic(t)) {
                    return Some(why);
                }
            }
            _ => {}
        }
    }
    None
}

/// The shell texts of every AppleScript `do shell script "..."` among an `osascript` call's arguments.
fn shell_texts(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for a in args {
        nexo_core::shell::osascript_scripts(a, &mut out);
    }
    out
}

/// Whether `diskutil`'s arguments erase or repartition a disk or APFS container.
fn wipes_disk(args: &[String]) -> bool {
    let first = args.first().map(|a| a.to_ascii_lowercase());
    match first.as_deref() {
        Some(
            "erasedisk" | "erasevolume" | "partitiondisk" | "secureerase" | "reformat" | "zerodisk" | "randomdisk",
        ) => true,
        Some("apfs" | "ap" | "corestorage") => args
            .get(1)
            .is_some_and(|a| a.eq_ignore_ascii_case("deleteContainer") || a.eq_ignore_ascii_case("eraseVolume")),
        _ => false,
    }
}

/// Why a command is never approved, when it is one of the fixed dangerous calls.
pub(super) fn dangerous(cmd: &str, cwd: &Path, home: Option<&Path>) -> Option<&'static str> {
    let segs = segments(cmd);
    for (i, seg) in segs.iter().enumerate() {
        let (prog, args) = program(&seg.words);
        let flags: Vec<&str> = args.iter().map(String::as_str).filter(|a| a.starts_with('-')).collect();
        let operands: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
        match prog {
            "rm" => {
                let recursive = flags
                    .iter()
                    .any(|f| *f == "--recursive" || (!f.starts_with("--") && f.chars().any(|c| c == 'r' || c == 'R')));
                if recursive {
                    let broad = |a: &str| {
                        matches!(
                            a.trim_end_matches('/'),
                            "" | "/*" | "~" | "$HOME" | "~/*" | "$HOME/*" | "/home" | "/Users" | "/Users/*"
                        )
                    };
                    if operands.iter().any(|a| broad(a)) {
                        return Some("rm -rf on the root or home folder");
                    }
                    if operands.iter().any(|a| outside(a, cwd, home)) {
                        return Some("recursive delete outside the session folder");
                    }
                }
            }
            "git" => {
                let sub = operands.first().map(|s| s.as_str());
                if sub == Some("push")
                    && args.iter().any(|a| a == "-f" || a.starts_with("--force") || (a.starts_with('+') && a.len() > 1))
                {
                    return Some("git push --force");
                }
                if sub == Some("reset") && args.iter().any(|a| a == "--hard") {
                    return Some("git reset --hard");
                }
            }
            "dd" => return Some("dd"),
            p if p.starts_with("mkfs") => return Some("mkfs"),
            "sh" | "bash" | "zsh" | "dash" | "ksh" => {
                if let Some(at) = args.iter().position(|a| a == "-c") {
                    if let Some(inner) = args.get(at + 1) {
                        if let Some(why) = dangerous(inner, cwd, home) {
                            return Some(why);
                        }
                    }
                }
            }
            "osascript" => {
                if let Some(why) = shell_texts(args).iter().find_map(|t| dangerous(t, cwd, home)) {
                    return Some(why);
                }
            }
            "tee" if operands.iter().any(|a| outside(a, cwd, home)) => {
                return Some("writes outside the session folder");
            }
            _ => {}
        }
        if seg.piped && matches!(prog, "curl" | "wget") {
            if let Some(next) = segs.get(i + 1) {
                if matches!(program(&next.words).0, "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish") {
                    return Some("a download piped into a shell");
                }
            }
        }
        // Redirections: `> file`, `>> file`, `2>file`.
        let w = &seg.words;
        for (k, word) in w.iter().enumerate() {
            let Some(at) = word.rfind('>') else { continue };
            let target = word[at + 1..].trim_start_matches('&');
            let target = if target.is_empty() { w.get(k + 1).map(String::as_str).unwrap_or("") } else { target };
            if !target.is_empty() && !target.chars().all(|c| c.is_ascii_digit()) && outside(target, cwd, home) {
                return Some("writes outside the session folder");
            }
        }
    }
    None
}
