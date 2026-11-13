//! How the arguments and the result of a call are read and told.

use super::tables::errno_name;

/// How each argument of the common calls is read. One letter per argument:
/// `d` signed number, `u` unsigned, `x` hex, `f` file descriptor, `D` directory descriptor (AT_FDCWD),
/// `p` path (text in memory), `b` bytes the call reads (length in the next argument), `B` bytes the call
/// writes (shown when it returns, as many as it returned), `O` open flags, `m` mode in octal (left out
/// when the flags create nothing), `P` mmap protection, `M` mmap flags, `s` signal.
fn spec(name: &str) -> Option<&'static str> {
    Some(match name {
        "read" | "recv" => "fBu",
        "write" => "fbu",
        "open" => "pOm",
        "openat" => "DpOm",
        "creat" => "pm",
        "close" | "dup" | "fsync" | "fdatasync" | "fchdir" => "f",
        "stat" | "lstat" => "px",
        "fstat" => "fx",
        "newfstatat" => "Dpxx",
        "statx" => "Dpxxx",
        "poll" => "xud",
        "lseek" => "fdd",
        "mmap" => "xuPMfx",
        "mprotect" => "xuP",
        "munmap" | "madvise" => "xud",
        "brk" => "x",
        "mremap" => "xuuxx",
        "rt_sigaction" => "sxxu",
        "rt_sigprocmask" => "dxxu",
        "ioctl" => "fxx",
        "pread64" => "fBud",
        "pwrite64" => "fbud",
        "readv" | "writev" => "fxd",
        "access" => "pd",
        "faccessat" => "Dpd",
        "faccessat2" => "Dpdx",
        "pipe" => "x",
        "pipe2" => "xx",
        "dup2" => "ff",
        "dup3" => "ffx",
        "nanosleep" => "xx",
        "socket" => "ddd",
        "connect" | "bind" => "fxu",
        "accept" => "fxx",
        "accept4" => "fxxx",
        "listen" => "fd",
        "sendto" => "fbuxxu",
        "recvfrom" => "fBuxxx",
        "shutdown" => "fd",
        "execve" => "pxx",
        "execveat" => "Dpxxx",
        "exit" | "exit_group" => "d",
        "wait4" => "dxxx",
        "kill" => "ds",
        "tkill" => "ds",
        "tgkill" => "dds",
        "uname" | "sysinfo" | "time" => "x",
        "fcntl" => "fdx",
        "ftruncate" => "fd",
        "truncate" => "pd",
        "getcwd" => "Bu",
        "chdir" | "rmdir" | "unlink" | "chroot" => "p",
        "rename" | "symlink" | "link" => "pp",
        "mkdir" | "chmod" => "pm",
        "mkdirat" | "fchmodat" => "Dpm",
        "unlinkat" => "Dpx",
        "renameat" => "DpDp",
        "renameat2" => "DpDpx",
        "readlink" => "pBu",
        "readlinkat" => "DpBu",
        "getdents64" => "fxu",
        "arch_prctl" => "xx",
        "set_tid_address" => "x",
        "set_robust_list" => "xu",
        "prlimit64" => "ddxx",
        "getrandom" => "xux",
        "rseq" => "xuxx",
        "clone" => "xxxxx",
        "clone3" => "xu",
        "futex" => "xdxxxd",
        "clock_nanosleep" => "ddxx",
        "clock_gettime" => "dx",
        "sendfile" => "ffxu",
        "eventfd2" => "ux",
        "epoll_create1" => "x",
        "epoll_ctl" => "fdfx",
        "epoll_wait" => "fxdd",
        "memfd_create" => "px",
        "prctl" => "dxxxx",
        "getpid" | "getppid" | "gettid" | "getuid" | "geteuid" | "getgid" | "getegid" | "fork" | "vfork"
        | "sched_yield" | "pause" | "setsid" | "rt_sigreturn" => "",
        _ => return None,
    })
}

/// Bytes as a quoted string, at most `cap` of them, then `...` when there were more.
pub fn quote(b: &[u8], cap: usize, more: bool) -> String {
    let mut s = String::from("\"");
    for &c in &b[..b.len().min(cap)] {
        match c {
            b'"' => s.push_str("\\\""),
            b'\\' => s.push_str("\\\\"),
            b'\n' => s.push_str("\\n"),
            b'\t' => s.push_str("\\t"),
            0x20..=0x7e => s.push(c as char),
            _ => s.push_str(&format!("\\x{c:02x}")),
        }
    }
    s.push('"');
    if more || b.len() > cap {
        s.push_str("...");
    }
    s
}

/// Bytes shown for a buffer argument.
const BUF_SHOWN: usize = 32;
/// The longest path read.
const PATH_MAX_SHOWN: usize = 256;

pub(super) fn flags(v: u64, names: &[(u64, &str)], zero: &str) -> String {
    let mut parts = Vec::new();
    let mut left = v;
    for &(bit, n) in names {
        if bit != 0 && v & bit == bit && left & bit != 0 {
            parts.push(n.to_string());
            left &= !bit;
        }
    }
    if left != 0 {
        parts.push(format!("{left:#x}"));
    }
    if parts.is_empty() {
        zero.to_string()
    } else if !zero.is_empty() && v & 3 == 0 && names.first().is_some_and(|(b, _)| *b == 1) {
        // open flags: O_RDONLY is the absence of the two access bits
        format!("{zero}|{}", parts.join("|"))
    } else {
        parts.join("|")
    }
}

pub(super) const OPEN: &[(u64, &str)] = &[
    (0o1, "O_WRONLY"),
    (0o2, "O_RDWR"),
    (0o100, "O_CREAT"),
    (0o200, "O_EXCL"),
    (0o400, "O_NOCTTY"),
    (0o1000, "O_TRUNC"),
    (0o2000, "O_APPEND"),
    (0o4000, "O_NONBLOCK"),
    (0o4010000, "O_SYNC"),
    (0o10000, "O_DSYNC"),
    (0o20000, "O_ASYNC"),
    (0o40000, "O_DIRECT"),
    (0o100000, "O_LARGEFILE"),
    (0o20200000, "O_TMPFILE"),
    (0o200000, "O_DIRECTORY"),
    (0o400000, "O_NOFOLLOW"),
    (0o1000000, "O_NOATIME"),
    (0o2000000, "O_CLOEXEC"),
    (0o10000000, "O_PATH"),
];

const PROT: &[(u64, &str)] = &[(1, "PROT_READ"), (2, "PROT_WRITE"), (4, "PROT_EXEC")];

pub(super) const MAP: &[(u64, &str)] = &[
    (3, "MAP_SHARED_VALIDATE"),
    (1, "MAP_SHARED"),
    (2, "MAP_PRIVATE"),
    (0x10, "MAP_FIXED"),
    (0x20, "MAP_ANONYMOUS"),
    (0x100, "MAP_GROWSDOWN"),
    (0x800, "MAP_DENYWRITE"),
    (0x1000, "MAP_EXECUTABLE"),
    (0x2000, "MAP_LOCKED"),
    (0x4000, "MAP_NORESERVE"),
    (0x8000, "MAP_POPULATE"),
    (0x10000, "MAP_NONBLOCK"),
    (0x20000, "MAP_STACK"),
    (0x40000, "MAP_HUGETLB"),
    (0x100000, "MAP_FIXED_NOREPLACE"),
];

fn signal(v: u64) -> String {
    let n = crate::engine::signal_name(v as u32);
    if n == "signal" {
        v.to_string()
    } else {
        n.to_string()
    }
}

/// Reads memory of the program: up to `len` bytes at an address (fewer when the rest cannot be read).
pub type Read<'a> = &'a mut dyn FnMut(u64, usize) -> Vec<u8>;

/// The arguments of a call as it starts, one text each. A `B` argument (filled by the call) is its
/// address until `fill` replaces it at the return. `None`: the call is not in the table; the caller
/// shows raw values and says so.
pub fn args_at_entry(name: &str, a: &[u64; 6], read: Read) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for (i, k) in shown(spec(name)?, a) {
        let v = a[i];
        let t = match k {
            'd' => (v as i64 as i32 as i64).to_string(),
            'u' => v.to_string(),
            'f' => (v as i32).to_string(),
            'D' if v as i32 == -100 => "AT_FDCWD".into(),
            'D' => (v as i32).to_string(),
            'p' if v == 0 => "NULL".into(),
            'p' => {
                let b = read(v, PATH_MAX_SHOWN);
                match b.iter().position(|&c| c == 0) {
                    Some(e) => quote(&b[..e], PATH_MAX_SHOWN, false),
                    None if b.is_empty() => format!("{v:#x} (unreadable)"),
                    None => quote(&b, PATH_MAX_SHOWN, true),
                }
            }
            'b' => {
                let len = a.get(i + 1).copied().unwrap_or(0) as usize;
                let b = read(v, len.min(BUF_SHOWN));
                if b.is_empty() && len > 0 {
                    format!("{v:#x} (unreadable)")
                } else {
                    quote(&b, BUF_SHOWN, len > BUF_SHOWN)
                }
            }
            'O' => flags(v & 0xffff_ffff, OPEN, "O_RDONLY"),
            'm' => format!("{:#o}", v & 0o7777),
            'P' if v == 0 => "PROT_NONE".into(),
            'P' => flags(v, PROT, ""),
            'M' => flags(v, MAP, ""),
            's' => signal(v),
            'x' if v == 0 => "NULL".into(),
            _ => format!("{v:#x}"),
        };
        out.push(t);
    }
    Some(out)
}

/// At the return: the bytes a `B` argument got, as many as the call returned.
pub fn fill(name: &str, a: &[u64; 6], ret: i64, error: bool, read: Read, args: &mut [String]) {
    let Some(sp) = spec(name) else { return };
    if error || ret <= 0 {
        return;
    }
    for (slot, (i, k)) in shown(sp, a).into_iter().enumerate() {
        if k != 'B' {
            continue;
        }
        if let Some(t) = args.get_mut(slot) {
            let n = ret as usize;
            let b = read(a[i], n.min(BUF_SHOWN));
            if !b.is_empty() {
                *t = quote(&b, BUF_SHOWN, n > BUF_SHOWN);
            }
        }
    }
}

/// The arguments shown, with their position: a mode is left out when the open flags create nothing.
fn shown(sp: &str, a: &[u64; 6]) -> Vec<(usize, char)> {
    let fl = sp.find('O').map(|j| a[j]);
    sp.chars()
        .enumerate()
        .filter(|&(_, k)| !(k == 'm' && fl.is_some_and(|f| f & 0o100 == 0 && f & 0o20000000 != 0o20000000)))
        .collect()
}

/// `= 3`, `= -1 ENOENT`, `= 0x7f0000001000`.
pub fn ret_text(name: &str, ret: i64, error: bool) -> String {
    if error {
        let e = -ret;
        return match errno_name(e) {
            Some(n) => format!("-1 {n}"),
            None => format!("-1 errno {e}"),
        };
    }
    match name {
        "mmap" | "mremap" | "brk" | "shmat" => format!("{:#x}", ret as u64),
        _ => ret.to_string(),
    }
}

/// `name(args)`, with the raw first three values for a call the table does not know.
pub fn call_text(name: &str, args: Option<&[String]>, raw: &[u64; 6]) -> String {
    match args {
        Some(a) => format!("{name}({})", a.join(", ")),
        None => format!("{name}(raw {:#x}, {:#x}, {:#x}, ...)", raw[0], raw[1], raw[2]),
    }
}
