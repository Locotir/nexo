//! The fixed lists of calls that are never approved, and the stricter one that no mode lets through.

use std::path::{Path, PathBuf};

use super::paths::{home_dir, normalize, outside};
use super::shell::{program, segments};

/// Why a command would wipe the machine beyond doubt: a recursive delete, permission change or `find -delete`
/// of the root, the home folder, a folder above it or a system folder, a file system made or raw bytes written
/// over a disk, a fork bomb. No mode, rule or approval runs these; everything less certain is left to the
/// person's own rules.
///
/// It reads one line as written. A caller that wants the commands behind wrappers, `bash -lc '..'`, `eval` and
/// `$(..)` too passes each of those forms in turn. The floor is best effort: a script written to a file and
/// run later, or `python -c`, is out of its sight, and the real wall there is the OS sandbox.
pub fn catastrophic(cmd: &str) -> Option<&'static str> {
    catastrophic_in(cmd, home_dir().as_deref())
}

/// `catastrophic` with the home folder given, so the answer does not depend on who runs it.
pub(super) fn catastrophic_in(cmd: &str, home: Option<&Path>) -> Option<&'static str> {
    let packed: String = cmd.chars().filter(|c| !c.is_whitespace()).collect();
    if packed.contains(":(){:|:&};:") {
        return Some("a fork bomb");
    }
    let whole = |a: &String| is_whole(a, home);
    for seg in segments(cmd) {
        // `> /dev/sda`, `2>/dev/sdb`, `&> /dev/nvme0n1`: whatever writes, the redirect writes over the disk
        for (k, word) in seg.words.iter().enumerate() {
            let Some(at) = word.rfind('>') else { continue };
            let target = word[at + 1..].trim_start_matches('&');
            let target =
                if target.is_empty() { seg.words.get(k + 1).map(String::as_str).unwrap_or("") } else { target };
            if disk(target) {
                return Some("raw bytes written over a disk");
            }
        }
        let (prog, args) = program(&seg.words);
        let flags = || args.iter().filter(|a| a.starts_with('-') && !a.starts_with("--"));
        let operands = || args.iter().filter(|a| !a.starts_with('-'));
        let recursive = args.iter().any(|a| a == "--recursive") || flags().any(|f| f.contains(['r', 'R']));
        match prog {
            "rm" if recursive && operands().any(whole) => {
                return Some("a recursive delete of the root or home folder");
            }
            "find" => {
                // the start folders are the words before the first test or action (`find ~ -name x -delete`)
                let starts = args.iter().skip_while(|a| matches!(a.as_str(), "-H" | "-L" | "-P"));
                let starts = starts.take_while(|a| !a.starts_with('-') && !matches!(a.as_str(), "(" | "!"));
                let deletes = args.iter().any(|a| a == "-delete")
                    || args.windows(2).any(|w| {
                        matches!(w[0].as_str(), "-exec" | "-execdir" | "-ok" | "-okdir")
                            && matches!(w[1].rsplit('/').next(), Some("rm" | "shred" | "unlink"))
                    });
                if deletes && starts.into_iter().any(whole) {
                    return Some("a recursive delete of the root or home folder");
                }
            }
            "chmod" | "chown" | "chgrp" if recursive && operands().any(whole) => {
                return Some("a recursive permission change over the root or home folder");
            }
            "shred" if operands().any(whole) => return Some("a recursive delete of the root or home folder"),
            "shred" | "truncate" | "tee" | "blkdiscard" if operands().any(|a| disk(a)) => {
                return Some("raw bytes written over a disk");
            }
            // `cp image.iso /dev/sda`: the last word is the target; a disk read as the source is harmless
            "cp" if operands().last().is_some_and(|a| disk(a)) => return Some("raw bytes written over a disk"),
            // listing signatures is harmless; `-a` and `-o` erase them
            "wipefs"
                if operands().any(|a| disk(a))
                    && (flags().any(|f| f.contains(['a', 'o']))
                        || args.iter().any(|a| a == "--all" || a.starts_with("--offset"))) =>
            {
                return Some("a disk erased");
            }
            // `sfdisk -l` and `-d` only read the table
            "sfdisk"
                if operands().any(|a| disk(a))
                    && !flags().any(|f| f.contains(['l', 'd', 's', 'F', 'J', 'V']))
                    && !args.iter().any(|a| {
                        matches!(
                            a.as_str(),
                            "--list" | "--dump" | "--show-size" | "--list-free" | "--json" | "--verify"
                        )
                    }) =>
            {
                return Some("a disk repartitioned");
            }
            "sgdisk"
                if operands().any(|a| disk(a))
                    && !args.iter().filter(|a| a.starts_with('-')).all(|a| sgdisk_reads(a)) =>
            {
                return Some("a disk repartitioned");
            }
            "parted"
                if operands().any(|a| disk(a))
                    && args.iter().any(|a| {
                        matches!(
                            a.as_str(),
                            "mklabel"
                                | "mktable"
                                | "mkpart"
                                | "mkpartfs"
                                | "mkfs"
                                | "rm"
                                | "resizepart"
                                | "resize"
                                | "move"
                                | "rescue"
                                | "name"
                        )
                    }) =>
            {
                return Some("a disk repartitioned");
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
                // `-c`, or a group of short flags that holds it (`-lc`, `-ec`), then the script
                let at = args.iter().position(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('c'));
                let inner = at.and_then(|at| args.get(at + 1));
                if let Some(why) = inner.and_then(|c| catastrophic_in(c, home)) {
                    return Some(why);
                }
            }
            // `osascript -e 'do shell script "diskutil eraseDisk ..."'` hands its text to a shell
            "osascript" => {
                if let Some(why) = shell_texts(args).iter().find_map(|t| catastrophic_in(t, home)) {
                    return Some(why);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether `a` is a disk or partition node. The harmless nodes under `/dev` (the null and zero devices, the
/// terminal, the standard streams, `fd/`, `pts/`, `shm/`, the sockets) are not.
fn disk(a: &str) -> bool {
    let Some(name) = a.strip_prefix("/dev/") else { return false };
    let plain = matches!(
        name,
        "" | "null"
            | "zero"
            | "full"
            | "random"
            | "urandom"
            | "stdin"
            | "stdout"
            | "stderr"
            | "tty"
            | "ptmx"
            | "console"
    );
    !plain && !["fd/", "pts/", "shm/", "tcp/", "udp/"].iter().any(|p| name.starts_with(p))
}

/// The `sgdisk` options that only print.
fn sgdisk_reads(a: &str) -> bool {
    ["-p", "--print", "-i", "--info", "-v", "--verify", "-E", "--end-of-largest", "-F", "--first-in-largest"]
        .iter()
        .any(|p| a.starts_with(p))
}

/// Folders whose loss ends the machine. `/home` and `/Users` hold every account.
const SYSTEM_FOLDERS: &[&str] = &[
    "/etc", "/usr", "/bin", "/sbin", "/lib", "/lib64", "/boot", "/var", "/opt", "/root", "/home", "/Users", "/System",
    "/Library",
];

/// Whether `arg` names the root, the home folder, a folder above it, a system folder, or everything in one of
/// them (`~/*`, `$HOME/.*`, `/usr/*`). `~`, `$HOME` and `${HOME}` are replaced and `.` and `..` folded first, so
/// `$HOME/.`, `/home/me` and `~/x/..` all name the home folder. A name built from any other variable, or a
/// relative one, cannot be told apart from a build folder and is not judged.
fn is_whole(arg: &str, home: Option<&Path>) -> bool {
    // without a home folder the stand-in still catches `~`, `~/*` and `$HOME`
    let home: PathBuf = home.map(Path::to_path_buf).filter(|h| h.has_root()).unwrap_or_else(|| PathBuf::from("/~"));
    let mut text = arg.to_string();
    for prefix in ["${HOME}", "$HOME", "~"] {
        if let Some(rest) = arg.strip_prefix(prefix).filter(|r| r.is_empty() || r.starts_with('/')) {
            text = format!("{}{rest}", home.to_string_lossy().trim_end_matches('/'));
            break;
        }
    }
    if !text.starts_with('/') || text.contains(['$', '`']) {
        return false;
    }
    let mut target = normalize(Path::new(&text), Path::new("/"));
    // `*` and `.*` stand for everything in the folder before them (`.*` also matches `..`)
    if matches!(target.file_name().and_then(|n| n.to_str()), Some("*" | ".*")) {
        target.pop();
    }
    target.parent().is_none()
        || target == home
        || home.starts_with(&target)
        || SYSTEM_FOLDERS.iter().any(|s| target == Path::new(s))
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
