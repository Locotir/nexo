//! The groups a trace filter can name, and the filter itself.

use super::tables::number;

/// The groups a filter can name, and the calls in each.
pub const CLASSES: &[(&str, &[&str])] = &[
    (
        "file",
        &[
            "read",
            "write",
            "open",
            "openat",
            "openat2",
            "creat",
            "close",
            "close_range",
            "pread64",
            "pwrite64",
            "readv",
            "writev",
            "preadv",
            "pwritev",
            "preadv2",
            "pwritev2",
            "lseek",
            "stat",
            "fstat",
            "lstat",
            "newfstatat",
            "statx",
            "access",
            "faccessat",
            "faccessat2",
            "unlink",
            "unlinkat",
            "rename",
            "renameat",
            "renameat2",
            "mkdir",
            "mkdirat",
            "rmdir",
            "getdents64",
            "readlink",
            "readlinkat",
            "chdir",
            "fchdir",
            "getcwd",
            "fcntl",
            "ioctl",
            "dup",
            "dup2",
            "dup3",
            "pipe",
            "pipe2",
            "chmod",
            "fchmod",
            "fchmodat",
            "chown",
            "fchown",
            "lchown",
            "fchownat",
            "truncate",
            "ftruncate",
            "fsync",
            "fdatasync",
            "sendfile",
            "copy_file_range",
            "splice",
            "tee",
            "symlink",
            "symlinkat",
            "link",
            "linkat",
            "flock",
            "utimensat",
            "statfs",
            "fstatfs",
            "poll",
            "ppoll",
            "select",
            "pselect6",
            "epoll_create1",
            "epoll_ctl",
            "epoll_wait",
            "epoll_pwait",
            "epoll_pwait2",
            "inotify_init1",
            "inotify_add_watch",
            "eventfd2",
            "memfd_create",
            "fallocate",
            "name_to_handle_at",
            "open_by_handle_at",
            "mknod",
            "mknodat",
            "umask",
        ],
    ),
    (
        "net",
        &[
            "socket",
            "socketpair",
            "connect",
            "accept",
            "accept4",
            "bind",
            "listen",
            "sendto",
            "recvfrom",
            "sendmsg",
            "recvmsg",
            "sendmmsg",
            "recvmmsg",
            "shutdown",
            "getsockname",
            "getpeername",
            "setsockopt",
            "getsockopt",
        ],
    ),
    (
        "process",
        &[
            "clone",
            "clone3",
            "fork",
            "vfork",
            "execve",
            "execveat",
            "exit",
            "exit_group",
            "wait4",
            "waitid",
            "kill",
            "tkill",
            "tgkill",
            "getpid",
            "getppid",
            "gettid",
            "setsid",
            "setpgid",
            "getpgid",
            "prctl",
            "arch_prctl",
            "set_tid_address",
            "rt_sigaction",
            "rt_sigprocmask",
            "rt_sigreturn",
            "rt_sigsuspend",
            "rt_sigtimedwait",
            "sigaltstack",
            "ptrace",
            "pidfd_open",
            "pidfd_send_signal",
            "setuid",
            "setgid",
            "setresuid",
            "setresgid",
            "getuid",
            "geteuid",
            "getgid",
            "getegid",
            "prlimit64",
            "setrlimit",
            "getrlimit",
            "sched_yield",
            "nanosleep",
            "clock_nanosleep",
            "pause",
            "alarm",
            "futex",
            "set_robust_list",
            "rseq",
        ],
    ),
    (
        "memory",
        &[
            "brk",
            "mmap",
            "munmap",
            "mprotect",
            "mremap",
            "madvise",
            "msync",
            "mlock",
            "mlock2",
            "munlock",
            "mlockall",
            "munlockall",
            "mincore",
            "pkey_mprotect",
            "process_vm_readv",
            "process_vm_writev",
        ],
    ),
];

pub fn class_of(name: &str) -> Option<&'static str> {
    CLASSES.iter().find(|(_, l)| l.contains(&name)).map(|(c, _)| *c)
}

/// Calls by name or by group; empty takes every call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    pub names: Vec<String>,
    pub classes: Vec<String>,
}

impl Filter {
    /// Words like `openat write` or `file,net`.
    pub fn parse(words: &[String]) -> Result<Filter, String> {
        let mut f = Filter::default();
        for w in words.iter().flat_map(|w| w.split(',')).map(str::trim).filter(|w| !w.is_empty()) {
            if CLASSES.iter().any(|(c, _)| *c == w) {
                f.classes.push(w.to_string());
            } else if number(w).is_some() {
                f.names.push(w.to_string());
            } else {
                let classes: Vec<&str> = CLASSES.iter().map(|(c, _)| *c).collect();
                return Err(format!("{w:?} is not a system call of Linux x86-64 nor a group ({})", classes.join(", ")));
            }
        }
        Ok(f)
    }

    pub fn all(&self) -> bool {
        self.names.is_empty() && self.classes.is_empty()
    }

    pub fn takes(&self, name: &str) -> bool {
        self.all()
            || self.names.iter().any(|n| n == name)
            || class_of(name).is_some_and(|c| self.classes.iter().any(|x| x == c))
    }

    pub fn text(&self) -> String {
        if self.all() {
            return "every call".into();
        }
        let mut v = self.classes.clone();
        v.extend(self.names.iter().cloned());
        v.join(",")
    }
}
