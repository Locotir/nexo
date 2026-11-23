//! The fixed lists of calls that are never approved, and the stricter one that no mode lets through.

use std::path::{Path, PathBuf};

use super::paths::{home_dir, normalize, outside};
use super::shell::{program, segments};

/// Why a command would wipe the machine beyond doubt: a recursive delete, permission change or unfiltered
/// `find -delete` of the root, the home folder, a folder above it or a system folder, a file system made or raw
/// bytes written over a disk, a fork bomb. No mode, rule or approval runs these; everything less certain (a
/// `find` that only matches some names, an owner change of the home folder to oneself, a change inside a
/// system folder) is left to the person's own rules.
///
/// It reads one line as written. A caller that wants the commands behind wrappers, `bash -lc '..'`, `eval` and
/// `$(..)` too passes each of those forms in turn. The floor is best effort: a script written to a file and
/// run later, or `python -c`, is out of its sight, and the real wall there is the OS sandbox.
pub fn catastrophic(cmd: &str) -> Option<&'static str> {
    catastrophic_at(cmd, home_dir().as_deref(), None)
}

/// `catastrophic` for a command that runs in `cwd`: a relative name (`rm -rf .`, `rm -rf *`, `rm -rf ..`) is
/// judged by the folder it stands for, and a `cd` earlier in the line moves where the next command runs.
pub fn catastrophic_from(cmd: &str, cwd: &Path) -> Option<&'static str> {
    catastrophic_at(cmd, home_dir().as_deref(), Some(cwd))
}

/// `catastrophic` with the home folder given, so the answer does not depend on who runs it.
#[cfg(test)]
pub(super) fn catastrophic_in(cmd: &str, home: Option<&Path>) -> Option<&'static str> {
    catastrophic_at(cmd, home, None)
}

/// `catastrophic` with the home folder and the folder the command runs in given. Without the folder a relative
/// name cannot be told apart from a build folder and is not judged.
pub(super) fn catastrophic_at(cmd: &str, home: Option<&Path>, cwd: Option<&Path>) -> Option<&'static str> {
    let packed: String = cmd.chars().filter(|c| !c.is_whitespace()).collect();
    if packed.contains(":(){:|:&};:") {
        return Some("a fork bomb");
    }
    // Use the shared lexer while quotes and escapes still distinguish shell syntax from argument text.
    // In particular an SSH script is one argument, not a local redirect whose filename is the whole script.
    if output_targets(cmd).iter().any(|target| disk(target)) {
        return Some("raw bytes written over a disk");
    }
    // where the next command runs: `cd ~ && rm -rf *` deletes the home folder; `None` once a `cd` goes
    // somewhere that cannot be told
    let mut here: Option<PathBuf> = cwd.filter(|c| c.has_root()).map(Path::to_path_buf);
    for seg in segments(cmd) {
        let (prog, args) = program(&seg.words);
        // `(cd ~; rm -rf *)`: the opening parenthesis belongs to the group, not to the program
        let prog = prog.trim_start_matches('(');
        if matches!(prog, "cd" | "pushd") {
            let dir = args.iter().find(|a| !a.starts_with('-'));
            here = if args.iter().any(|a| a == "-") {
                None
            } else {
                match dir {
                    // `cd` alone goes home
                    None => home.filter(|h| h.has_root()).map(Path::to_path_buf),
                    Some(d) => resolve_name(d, home, here.as_deref()),
                }
            };
            continue;
        }
        let whole = |a: &String| whole_kind(a.trim_end_matches(')'), home, here.as_deref());
        let flags = || args.iter().filter(|a| a.starts_with('-') && !a.starts_with("--"));
        let operands = || args.iter().filter(|a| !a.starts_with('-'));
        let recursive = args.iter().any(|a| a == "--recursive") || flags().any(|f| f.contains(['r', 'R']));
        match prog {
            "rm" if recursive && operands().any(|a| whole(a).is_some()) => {
                return Some("a recursive delete of the root or home folder");
            }
            "find" => {
                // the start folders are the words before the first test or action (`find ~ -name x -delete`);
                // GNU `find -delete` with none starts at `.`
                let starts = args.iter().skip_while(|a| matches!(a.as_str(), "-H" | "-L" | "-P"));
                let starts: Vec<&String> =
                    starts.take_while(|a| !a.starts_with('-') && !matches!(a.as_str(), "(" | "!")).collect();
                let deletes = args.iter().any(|a| a == "-delete")
                    || args.windows(2).any(|w| {
                        matches!(w[0].as_str(), "-exec" | "-execdir" | "-ok" | "-okdir")
                            && matches!(w[1].rsplit('/').next(), Some("rm" | "shred" | "unlink"))
                    });
                let dot = String::from(".");
                let at_whole =
                    if starts.is_empty() { whole(&dot).is_some() } else { starts.iter().any(|a| whole(a).is_some()) };
                if deletes && at_whole && !find_narrows(args) {
                    return Some("a recursive delete of the root or home folder");
                }
            }
            "chmod" | "chown" | "chgrp" => {
                if recursive {
                    // changing the home folder to oneself (`chown -R $USER ~`) or only adding permissions to it
                    // (`chmod -R u+rwX ~`) repairs it; anything else, and anything over the root, a folder above
                    // the home folder or a core system folder, can lock the person out
                    let mine = if prog == "chmod" {
                        operands().any(|a| adds_only(a))
                    } else {
                        operands().next().is_some_and(|a| is_user(a, home))
                    };
                    let breaks = operands().filter_map(&whole).any(|kind| match kind {
                        Whole::Home => !mine,
                        Whole::Root | Whole::Above | Whole::Core => true,
                        Whole::Other => false,
                    });
                    if breaks {
                        return Some("a recursive permission change over the root or home folder");
                    }
                }
            }
            "shred" if operands().any(|a| whole(a).is_some()) => {
                return Some("a recursive delete of the root or home folder");
            }
            // `truncate -r /dev/sda file` only reads the size of the reference
            "truncate" if without_values(args, &["-r", "--reference", "-s", "--size"]).iter().any(|a| disk(a)) => {
                return Some("raw bytes written over a disk");
            }
            "shred" | "tee" | "blkdiscard" if operands().any(|a| disk(a)) => {
                return Some("raw bytes written over a disk");
            }
            // `cp image.iso /dev/sda`: the last word is the target; a disk read as the source is harmless
            "cp" if operands().next_back().is_some_and(|a| disk(a)) => return Some("raw bytes written over a disk"),
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
                if let Some(why) = inner.and_then(|c| catastrophic_at(c, home, here.as_deref())) {
                    return Some(why);
                }
            }
            // `osascript -e 'do shell script "diskutil eraseDisk ..."'` hands its text to a shell
            "osascript" => {
                if let Some(why) = shell_texts(args).iter().find_map(|t| catastrophic_at(t, home, here.as_deref())) {
                    return Some(why);
                }
            }
            _ => {}
        }
    }
    None
}

/// Actual output filenames, not `>` characters in quoted or escaped argument text. The lexer also handles
/// append, clobber, combined stdout/stderr and read/write redirects, preserving quoted target filenames.
fn output_targets(cmd: &str) -> Vec<String> {
    nexo_core::shell::lex(cmd, true)
        .cmds
        .into_iter()
        .flat_map(|c| c.redirects)
        .filter(|r| r.writes() && !r.dup)
        .filter_map(|r| r.target)
        .collect()
}

/// The operands of a command (the words that are not options), without the values of the options that take one.
fn without_values<'a>(args: &'a [String], takes: &[&str]) -> Vec<&'a String> {
    let mut skip = false;
    let mut out = Vec::new();
    for a in args {
        if skip {
            skip = false;
        } else if takes.contains(&a.as_str()) {
            skip = true;
        } else if !a.starts_with('-') {
            out.push(a);
        }
    }
    out
}

/// Device names that are block devices or whole folders of volumes: `sda1`, `nvme0n1p1`, `disk3`, `loop0`, `dm-0`.
const DISK_LETTER_FAMILIES: &[&str] = &["sd", "hd", "vd", "xvd", "ubd"];
const DISK_NUMBER_FAMILIES: &[&str] = &[
    "nvme", "mmcblk", "disk", "rdisk", "loop", "md", "dm-", "sr", "nbd", "zd", "zram", "ram", "rbd", "mtdblock",
    "pmem", "ubi",
];
/// The folders under `/dev` that hold terminals, sockets, input and other devices, not volumes. Any other folder
/// (`mapper/`, `disk/by-id/`, an LVM volume group `vg0/`) holds volumes.
const OTHER_DEVICE_FOLDERS: &[&str] = &[
    "fd",
    "pts",
    "shm",
    "tcp",
    "udp",
    "input",
    "snd",
    "dri",
    "bus",
    "net",
    "serial",
    "usb",
    "vfio",
    "mqueue",
    "hugepages",
    "cpu",
    "char",
];

/// Whether `a` is a disk or partition node, a volume of a volume group or the kernel memory. Everything else
/// under `/dev` (the null and zero devices, terminals and serial ports, `kmsg`, the standard streams, `shm`,
/// `fd/`, `pts/`, the sockets) is not.
fn disk(a: &str) -> bool {
    let Some(name) = a.strip_prefix("/dev/") else { return false };
    if let Some((dir, rest)) = name.split_once('/') {
        return !rest.is_empty() && !OTHER_DEVICE_FOLDERS.contains(&dir);
    }
    matches!(name, "mem" | "kmem")
        || DISK_LETTER_FAMILIES
            .iter()
            .any(|p| name.strip_prefix(*p).is_some_and(|r| r.starts_with(|c: char| c.is_ascii_lowercase())))
        || DISK_NUMBER_FAMILIES
            .iter()
            .any(|p| name.strip_prefix(*p).is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit())))
}

/// The `sgdisk` options that only print.
fn sgdisk_reads(a: &str) -> bool {
    ["-p", "--print", "-i", "--info", "-v", "--verify", "-E", "--end-of-largest", "-F", "--first-in-largest"]
        .iter()
        .any(|p| a.starts_with(p))
}

/// What a name stands for when it is a folder whose loss or change can end the machine or lock the person out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Whole {
    /// `/`
    Root,
    /// A folder that holds the home folder: `/home`, `/Users`.
    Above,
    /// The home folder.
    Home,
    /// A folder the system runs from: `/etc`, `/usr`, `/boot`.
    Core,
    /// A system folder that is not needed to boot: `/var`, `/opt`, `/root`.
    Other,
}

const CORE_FOLDERS: &[&str] = &["/etc", "/usr", "/bin", "/sbin", "/lib", "/lib64", "/boot", "/System", "/Library"];
const OTHER_FOLDERS: &[&str] = &["/var", "/opt", "/root"];

/// The home folder, or a stand-in that still catches `~`, `~/*` and `$HOME` when it is unknown.
fn standin(home: Option<&Path>) -> PathBuf {
    home.map(Path::to_path_buf).filter(|h| h.has_root()).unwrap_or_else(|| PathBuf::from("/~"))
}

/// The file or folder `arg` names, with `~`, `$HOME` and `${HOME}` replaced and `.` and `..` folded away. `None`
/// for a name built from any other variable or substitution, for `~user`, and for a relative name when the
/// folder the command runs in is unknown: it cannot be told apart from a build folder.
fn resolve_name(arg: &str, home: Option<&Path>, cwd: Option<&Path>) -> Option<PathBuf> {
    if arg.is_empty() {
        return None;
    }
    let home = standin(home);
    let mut text = arg.to_string();
    for prefix in ["${HOME}", "$HOME", "~"] {
        if let Some(rest) = arg.strip_prefix(prefix).filter(|r| r.is_empty() || r.starts_with('/')) {
            let home = home.to_string_lossy();
            text = format!("{}{rest}", home.trim_end_matches(['/', '\\']));
            if text.is_empty() {
                text.push('/');
            }
            break;
        }
    }
    if text.contains(['$', '`']) {
        return None;
    }
    // a name from the top of the file system, or of a drive on Windows, or one relative to the folder it runs in
    let base = if text.starts_with('/') || Path::new(&text).has_root() {
        PathBuf::from("/")
    } else if text.starts_with('~') {
        return None;
    } else {
        cwd.filter(|c| c.has_root())?.to_path_buf()
    };
    Some(normalize(Path::new(&text), &base))
}

/// Whether `arg` names the root, the home folder, a folder above it, a system folder, or everything in one of
/// them (`~/*`, `$HOME/.*`, `/usr/*`), and which of those. `~`, `$HOME` and `${HOME}` are replaced and `.` and
/// `..` folded first, so `$HOME/.`, `/home/me` and `~/x/..` all name the home folder; a relative name (`.`, `*`,
/// `..`) stands for the folder the command runs in when `cwd` is given.
fn whole_kind(arg: &str, home: Option<&Path>, cwd: Option<&Path>) -> Option<Whole> {
    let mut target = resolve_name(arg, home, cwd)?;
    // `*` and `.*` stand for everything in the folder before them (`.*` also matches `..`)
    if matches!(target.file_name().and_then(|n| n.to_str()), Some("*" | ".*")) {
        target.pop();
    }
    let home = standin(home);
    if target.parent().is_none() {
        Some(Whole::Root)
    } else if target == home {
        Some(Whole::Home)
    } else if home.starts_with(&target) || ["/home", "/Users"].iter().any(|s| target == Path::new(*s)) {
        Some(Whole::Above)
    } else if CORE_FOLDERS.iter().any(|s| target == Path::new(*s)) {
        Some(Whole::Core)
    } else if OTHER_FOLDERS.iter().any(|s| target == Path::new(*s)) {
        Some(Whole::Other)
    } else {
        None
    }
}

/// Whether a `find` expression keeps only some of what it visits, so its `-delete` or `-exec rm` removes less than
/// the whole folder: a name, path, age, size, owner or permission test. A name test every name passes
/// (`-name '*'`) narrows nothing, and `!`, `-not` and `-o` turn a test around or let another branch through.
fn find_narrows(args: &[String]) -> bool {
    if args.iter().any(|a| matches!(a.as_str(), "!" | "-not" | "-o" | "-or")) {
        return false;
    }
    args.iter().enumerate().any(|(i, a)| match a.as_str() {
        "-name" | "-iname" | "-path" | "-ipath" | "-wholename" | "-iwholename" | "-regex" | "-iregex" => {
            args.get(i + 1).is_some_and(|v| !v.is_empty() && !v.chars().all(|c| matches!(c, '*' | '.' | '?')))
        }
        "-mtime" | "-mmin" | "-atime" | "-amin" | "-ctime" | "-cmin" | "-size" | "-empty" | "-perm" | "-user"
        | "-group" | "-uid" | "-gid" | "-lname" | "-ilname" | "-samefile" | "-inum" | "-anewer" | "-cnewer" => true,
        s => s.starts_with("-newer"),
    })
}

/// Whether `mode` only adds permissions for the owner and the group (`u+rwX`, `g+w,u+x`, `+x`).
fn adds_only(mode: &str) -> bool {
    !mode.is_empty()
        && mode.split(',').all(|clause| {
            let Some(perms) = clause.trim_start_matches(['u', 'g']).strip_prefix('+') else { return false };
            !perms.is_empty() && perms.chars().all(|p| "rwxX".contains(p))
        })
}

/// Whether the owner and group of a `chown` spec (`me`, `me:me`, `:me`) are the person: `$USER`, `$(id -un)`,
/// `$(whoami)` or the name of the home folder.
fn is_user(spec: &str, home: Option<&Path>) -> bool {
    let name = home.and_then(Path::file_name).and_then(|n| n.to_str());
    let parts: Vec<&str> = spec.split(':').filter(|p| !p.is_empty()).collect();
    !parts.is_empty()
        && parts.iter().all(|p| {
            matches!(*p, "$USER" | "${USER}" | "$LOGNAME" | "${LOGNAME}" | "$(whoami)" | "`whoami`")
                || p.starts_with("$(id")
                || name == Some(*p)
        })
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
    if output_targets(cmd).iter().any(|target| outside(target, cwd, home)) {
        return Some("writes outside the session folder");
    }
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
                // Match the catastrophic floor: grouped short flags (`-lc`, `-ec`) also run a script.
                if let Some(at) =
                    args.iter().position(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('c'))
                {
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
    }
    None
}
