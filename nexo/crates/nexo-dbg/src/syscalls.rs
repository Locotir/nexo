//! Linux x86-64 system calls: names, error names, how the common ones take their arguments, and
//! the line a trace shows for one. The name and error tables come from the kernel headers
//! (`asm/unistd_64.h`, `asm-generic/errno*.h`).

/// (number, name), from `asm/unistd_64.h`.
const NAMES: &[(u64, &str)] = &[
    (0, "read"),
    (1, "write"),
    (2, "open"),
    (3, "close"),
    (4, "stat"),
    (5, "fstat"),
    (6, "lstat"),
    (7, "poll"),
    (8, "lseek"),
    (9, "mmap"),
    (10, "mprotect"),
    (11, "munmap"),
    (12, "brk"),
    (13, "rt_sigaction"),
    (14, "rt_sigprocmask"),
    (15, "rt_sigreturn"),
    (16, "ioctl"),
    (17, "pread64"),
    (18, "pwrite64"),
    (19, "readv"),
    (20, "writev"),
    (21, "access"),
    (22, "pipe"),
    (23, "select"),
    (24, "sched_yield"),
    (25, "mremap"),
    (26, "msync"),
    (27, "mincore"),
    (28, "madvise"),
    (29, "shmget"),
    (30, "shmat"),
    (31, "shmctl"),
    (32, "dup"),
    (33, "dup2"),
    (34, "pause"),
    (35, "nanosleep"),
    (36, "getitimer"),
    (37, "alarm"),
    (38, "setitimer"),
    (39, "getpid"),
    (40, "sendfile"),
    (41, "socket"),
    (42, "connect"),
    (43, "accept"),
    (44, "sendto"),
    (45, "recvfrom"),
    (46, "sendmsg"),
    (47, "recvmsg"),
    (48, "shutdown"),
    (49, "bind"),
    (50, "listen"),
    (51, "getsockname"),
    (52, "getpeername"),
    (53, "socketpair"),
    (54, "setsockopt"),
    (55, "getsockopt"),
    (56, "clone"),
    (57, "fork"),
    (58, "vfork"),
    (59, "execve"),
    (60, "exit"),
    (61, "wait4"),
    (62, "kill"),
    (63, "uname"),
    (64, "semget"),
    (65, "semop"),
    (66, "semctl"),
    (67, "shmdt"),
    (68, "msgget"),
    (69, "msgsnd"),
    (70, "msgrcv"),
    (71, "msgctl"),
    (72, "fcntl"),
    (73, "flock"),
    (74, "fsync"),
    (75, "fdatasync"),
    (76, "truncate"),
    (77, "ftruncate"),
    (78, "getdents"),
    (79, "getcwd"),
    (80, "chdir"),
    (81, "fchdir"),
    (82, "rename"),
    (83, "mkdir"),
    (84, "rmdir"),
    (85, "creat"),
    (86, "link"),
    (87, "unlink"),
    (88, "symlink"),
    (89, "readlink"),
    (90, "chmod"),
    (91, "fchmod"),
    (92, "chown"),
    (93, "fchown"),
    (94, "lchown"),
    (95, "umask"),
    (96, "gettimeofday"),
    (97, "getrlimit"),
    (98, "getrusage"),
    (99, "sysinfo"),
    (100, "times"),
    (101, "ptrace"),
    (102, "getuid"),
    (103, "syslog"),
    (104, "getgid"),
    (105, "setuid"),
    (106, "setgid"),
    (107, "geteuid"),
    (108, "getegid"),
    (109, "setpgid"),
    (110, "getppid"),
    (111, "getpgrp"),
    (112, "setsid"),
    (113, "setreuid"),
    (114, "setregid"),
    (115, "getgroups"),
    (116, "setgroups"),
    (117, "setresuid"),
    (118, "getresuid"),
    (119, "setresgid"),
    (120, "getresgid"),
    (121, "getpgid"),
    (122, "setfsuid"),
    (123, "setfsgid"),
    (124, "getsid"),
    (125, "capget"),
    (126, "capset"),
    (127, "rt_sigpending"),
    (128, "rt_sigtimedwait"),
    (129, "rt_sigqueueinfo"),
    (130, "rt_sigsuspend"),
    (131, "sigaltstack"),
    (132, "utime"),
    (133, "mknod"),
    (134, "uselib"),
    (135, "personality"),
    (136, "ustat"),
    (137, "statfs"),
    (138, "fstatfs"),
    (139, "sysfs"),
    (140, "getpriority"),
    (141, "setpriority"),
    (142, "sched_setparam"),
    (143, "sched_getparam"),
    (144, "sched_setscheduler"),
    (145, "sched_getscheduler"),
    (146, "sched_get_priority_max"),
    (147, "sched_get_priority_min"),
    (148, "sched_rr_get_interval"),
    (149, "mlock"),
    (150, "munlock"),
    (151, "mlockall"),
    (152, "munlockall"),
    (153, "vhangup"),
    (154, "modify_ldt"),
    (155, "pivot_root"),
    (156, "_sysctl"),
    (157, "prctl"),
    (158, "arch_prctl"),
    (159, "adjtimex"),
    (160, "setrlimit"),
    (161, "chroot"),
    (162, "sync"),
    (163, "acct"),
    (164, "settimeofday"),
    (165, "mount"),
    (166, "umount2"),
    (167, "swapon"),
    (168, "swapoff"),
    (169, "reboot"),
    (170, "sethostname"),
    (171, "setdomainname"),
    (172, "iopl"),
    (173, "ioperm"),
    (174, "create_module"),
    (175, "init_module"),
    (176, "delete_module"),
    (177, "get_kernel_syms"),
    (178, "query_module"),
    (179, "quotactl"),
    (180, "nfsservctl"),
    (181, "getpmsg"),
    (182, "putpmsg"),
    (183, "afs_syscall"),
    (184, "tuxcall"),
    (185, "security"),
    (186, "gettid"),
    (187, "readahead"),
    (188, "setxattr"),
    (189, "lsetxattr"),
    (190, "fsetxattr"),
    (191, "getxattr"),
    (192, "lgetxattr"),
    (193, "fgetxattr"),
    (194, "listxattr"),
    (195, "llistxattr"),
    (196, "flistxattr"),
    (197, "removexattr"),
    (198, "lremovexattr"),
    (199, "fremovexattr"),
    (200, "tkill"),
    (201, "time"),
    (202, "futex"),
    (203, "sched_setaffinity"),
    (204, "sched_getaffinity"),
    (205, "set_thread_area"),
    (206, "io_setup"),
    (207, "io_destroy"),
    (208, "io_getevents"),
    (209, "io_submit"),
    (210, "io_cancel"),
    (211, "get_thread_area"),
    (212, "lookup_dcookie"),
    (213, "epoll_create"),
    (214, "epoll_ctl_old"),
    (215, "epoll_wait_old"),
    (216, "remap_file_pages"),
    (217, "getdents64"),
    (218, "set_tid_address"),
    (219, "restart_syscall"),
    (220, "semtimedop"),
    (221, "fadvise64"),
    (222, "timer_create"),
    (223, "timer_settime"),
    (224, "timer_gettime"),
    (225, "timer_getoverrun"),
    (226, "timer_delete"),
    (227, "clock_settime"),
    (228, "clock_gettime"),
    (229, "clock_getres"),
    (230, "clock_nanosleep"),
    (231, "exit_group"),
    (232, "epoll_wait"),
    (233, "epoll_ctl"),
    (234, "tgkill"),
    (235, "utimes"),
    (236, "vserver"),
    (237, "mbind"),
    (238, "set_mempolicy"),
    (239, "get_mempolicy"),
    (240, "mq_open"),
    (241, "mq_unlink"),
    (242, "mq_timedsend"),
    (243, "mq_timedreceive"),
    (244, "mq_notify"),
    (245, "mq_getsetattr"),
    (246, "kexec_load"),
    (247, "waitid"),
    (248, "add_key"),
    (249, "request_key"),
    (250, "keyctl"),
    (251, "ioprio_set"),
    (252, "ioprio_get"),
    (253, "inotify_init"),
    (254, "inotify_add_watch"),
    (255, "inotify_rm_watch"),
    (256, "migrate_pages"),
    (257, "openat"),
    (258, "mkdirat"),
    (259, "mknodat"),
    (260, "fchownat"),
    (261, "futimesat"),
    (262, "newfstatat"),
    (263, "unlinkat"),
    (264, "renameat"),
    (265, "linkat"),
    (266, "symlinkat"),
    (267, "readlinkat"),
    (268, "fchmodat"),
    (269, "faccessat"),
    (270, "pselect6"),
    (271, "ppoll"),
    (272, "unshare"),
    (273, "set_robust_list"),
    (274, "get_robust_list"),
    (275, "splice"),
    (276, "tee"),
    (277, "sync_file_range"),
    (278, "vmsplice"),
    (279, "move_pages"),
    (280, "utimensat"),
    (281, "epoll_pwait"),
    (282, "signalfd"),
    (283, "timerfd_create"),
    (284, "eventfd"),
    (285, "fallocate"),
    (286, "timerfd_settime"),
    (287, "timerfd_gettime"),
    (288, "accept4"),
    (289, "signalfd4"),
    (290, "eventfd2"),
    (291, "epoll_create1"),
    (292, "dup3"),
    (293, "pipe2"),
    (294, "inotify_init1"),
    (295, "preadv"),
    (296, "pwritev"),
    (297, "rt_tgsigqueueinfo"),
    (298, "perf_event_open"),
    (299, "recvmmsg"),
    (300, "fanotify_init"),
    (301, "fanotify_mark"),
    (302, "prlimit64"),
    (303, "name_to_handle_at"),
    (304, "open_by_handle_at"),
    (305, "clock_adjtime"),
    (306, "syncfs"),
    (307, "sendmmsg"),
    (308, "setns"),
    (309, "getcpu"),
    (310, "process_vm_readv"),
    (311, "process_vm_writev"),
    (312, "kcmp"),
    (313, "finit_module"),
    (314, "sched_setattr"),
    (315, "sched_getattr"),
    (316, "renameat2"),
    (317, "seccomp"),
    (318, "getrandom"),
    (319, "memfd_create"),
    (320, "kexec_file_load"),
    (321, "bpf"),
    (322, "execveat"),
    (323, "userfaultfd"),
    (324, "membarrier"),
    (325, "mlock2"),
    (326, "copy_file_range"),
    (327, "preadv2"),
    (328, "pwritev2"),
    (329, "pkey_mprotect"),
    (330, "pkey_alloc"),
    (331, "pkey_free"),
    (332, "statx"),
    (333, "io_pgetevents"),
    (334, "rseq"),
    (335, "uretprobe"),
    (336, "uprobe"),
    (424, "pidfd_send_signal"),
    (425, "io_uring_setup"),
    (426, "io_uring_enter"),
    (427, "io_uring_register"),
    (428, "open_tree"),
    (429, "move_mount"),
    (430, "fsopen"),
    (431, "fsconfig"),
    (432, "fsmount"),
    (433, "fspick"),
    (434, "pidfd_open"),
    (435, "clone3"),
    (436, "close_range"),
    (437, "openat2"),
    (438, "pidfd_getfd"),
    (439, "faccessat2"),
    (440, "process_madvise"),
    (441, "epoll_pwait2"),
    (442, "mount_setattr"),
    (443, "quotactl_fd"),
    (444, "landlock_create_ruleset"),
    (445, "landlock_add_rule"),
    (446, "landlock_restrict_self"),
    (447, "memfd_secret"),
    (448, "process_mrelease"),
    (449, "futex_waitv"),
    (450, "set_mempolicy_home_node"),
    (451, "cachestat"),
    (452, "fchmodat2"),
    (453, "map_shadow_stack"),
    (454, "futex_wake"),
    (455, "futex_wait"),
    (456, "futex_requeue"),
    (457, "statmount"),
    (458, "listmount"),
    (459, "lsm_get_self_attr"),
    (460, "lsm_set_self_attr"),
    (461, "lsm_list_modules"),
    (462, "mseal"),
    (463, "setxattrat"),
    (464, "getxattrat"),
    (465, "listxattrat"),
    (466, "removexattrat"),
    (467, "open_tree_attr"),
    (468, "file_getattr"),
    (469, "file_setattr"),
    (470, "listns"),
    (471, "rseq_slice_yield"),
];

/// (number, name), from `asm-generic/errno-base.h` and `errno.h`.
const ERRNO: &[(i64, &str)] = &[
    (1, "EPERM"),
    (2, "ENOENT"),
    (3, "ESRCH"),
    (4, "EINTR"),
    (5, "EIO"),
    (6, "ENXIO"),
    (7, "E2BIG"),
    (8, "ENOEXEC"),
    (9, "EBADF"),
    (10, "ECHILD"),
    (11, "EAGAIN"),
    (12, "ENOMEM"),
    (13, "EACCES"),
    (14, "EFAULT"),
    (15, "ENOTBLK"),
    (16, "EBUSY"),
    (17, "EEXIST"),
    (18, "EXDEV"),
    (19, "ENODEV"),
    (20, "ENOTDIR"),
    (21, "EISDIR"),
    (22, "EINVAL"),
    (23, "ENFILE"),
    (24, "EMFILE"),
    (25, "ENOTTY"),
    (26, "ETXTBSY"),
    (27, "EFBIG"),
    (28, "ENOSPC"),
    (29, "ESPIPE"),
    (30, "EROFS"),
    (31, "EMLINK"),
    (32, "EPIPE"),
    (33, "EDOM"),
    (34, "ERANGE"),
    (35, "EDEADLK"),
    (36, "ENAMETOOLONG"),
    (37, "ENOLCK"),
    (38, "ENOSYS"),
    (39, "ENOTEMPTY"),
    (40, "ELOOP"),
    (42, "ENOMSG"),
    (43, "EIDRM"),
    (44, "ECHRNG"),
    (45, "EL2NSYNC"),
    (46, "EL3HLT"),
    (47, "EL3RST"),
    (48, "ELNRNG"),
    (49, "EUNATCH"),
    (50, "ENOCSI"),
    (51, "EL2HLT"),
    (52, "EBADE"),
    (53, "EBADR"),
    (54, "EXFULL"),
    (55, "ENOANO"),
    (56, "EBADRQC"),
    (57, "EBADSLT"),
    (59, "EBFONT"),
    (60, "ENOSTR"),
    (61, "ENODATA"),
    (62, "ETIME"),
    (63, "ENOSR"),
    (64, "ENONET"),
    (65, "ENOPKG"),
    (66, "EREMOTE"),
    (67, "ENOLINK"),
    (68, "EADV"),
    (69, "ESRMNT"),
    (70, "ECOMM"),
    (71, "EPROTO"),
    (72, "EMULTIHOP"),
    (73, "EDOTDOT"),
    (74, "EBADMSG"),
    (75, "EOVERFLOW"),
    (76, "ENOTUNIQ"),
    (77, "EBADFD"),
    (78, "EREMCHG"),
    (79, "ELIBACC"),
    (80, "ELIBBAD"),
    (81, "ELIBSCN"),
    (82, "ELIBMAX"),
    (83, "ELIBEXEC"),
    (84, "EILSEQ"),
    (85, "ERESTART"),
    (86, "ESTRPIPE"),
    (87, "EUSERS"),
    (88, "ENOTSOCK"),
    (89, "EDESTADDRREQ"),
    (90, "EMSGSIZE"),
    (91, "EPROTOTYPE"),
    (92, "ENOPROTOOPT"),
    (93, "EPROTONOSUPPORT"),
    (94, "ESOCKTNOSUPPORT"),
    (95, "EOPNOTSUPP"),
    (96, "EPFNOSUPPORT"),
    (97, "EAFNOSUPPORT"),
    (98, "EADDRINUSE"),
    (99, "EADDRNOTAVAIL"),
    (100, "ENETDOWN"),
    (101, "ENETUNREACH"),
    (102, "ENETRESET"),
    (103, "ECONNABORTED"),
    (104, "ECONNRESET"),
    (105, "ENOBUFS"),
    (106, "EISCONN"),
    (107, "ENOTCONN"),
    (108, "ESHUTDOWN"),
    (109, "ETOOMANYREFS"),
    (110, "ETIMEDOUT"),
    (111, "ECONNREFUSED"),
    (112, "EHOSTDOWN"),
    (113, "EHOSTUNREACH"),
    (114, "EALREADY"),
    (115, "EINPROGRESS"),
    (116, "ESTALE"),
    (117, "EUCLEAN"),
    (118, "ENOTNAM"),
    (119, "ENAVAIL"),
    (120, "EISNAM"),
    (121, "EREMOTEIO"),
    (122, "EDQUOT"),
    (123, "ENOMEDIUM"),
    (124, "EMEDIUMTYPE"),
    (125, "ECANCELED"),
    (126, "ENOKEY"),
    (127, "EKEYEXPIRED"),
    (128, "EKEYREVOKED"),
    (129, "EKEYREJECTED"),
    (130, "EOWNERDEAD"),
    (131, "ENOTRECOVERABLE"),
    (132, "ERFKILL"),
    (133, "EHWPOISON"),
];

pub fn name(nr: u64) -> Option<&'static str> {
    NAMES.binary_search_by_key(&nr, |(n, _)| *n).ok().map(|i| NAMES[i].1)
}

pub fn number(name: &str) -> Option<u64> {
    NAMES.iter().find(|(_, s)| *s == name).map(|(n, _)| *n)
}

pub fn errno_name(e: i64) -> Option<&'static str> {
    ERRNO.iter().find(|(n, _)| *n == e).map(|(_, s)| *s)
}

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

fn flags(v: u64, names: &[(u64, &str)], zero: &str) -> String {
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

const OPEN: &[(u64, &str)] = &[
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

const MAP: &[(u64, &str)] = &[
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_and_decoding() {
        assert_eq!(name(0), Some("read"));
        assert_eq!(name(257), Some("openat"));
        assert_eq!(number("exit_group"), Some(231));
        assert_eq!(errno_name(2), Some("ENOENT"));
        assert_eq!(errno_name(133), Some("EHWPOISON"));
        assert!(NAMES.windows(2).all(|w| w[0].0 < w[1].0));
        let mut mem = |addr: u64, len: usize| -> Vec<u8> {
            let s: &[u8] = if addr == 0x1000 { b"/no/such\0" } else { b"hello world\n" };
            s[..s.len().min(len)].to_vec()
        };
        let a = [(-100i64) as u64, 0x1000, 0o2000000, 0, 0, 0];
        let args = args_at_entry("openat", &a, &mut mem).unwrap();
        assert_eq!(call_text("openat", Some(&args), &a), "openat(AT_FDCWD, \"/no/such\", O_RDONLY|O_CLOEXEC)");
        let a = [1, 0x2000, 12, 0, 0, 0];
        let args = args_at_entry("write", &a, &mut mem).unwrap();
        assert_eq!(args, vec!["1", "\"hello world\\n\"", "12"]);
        let a = [0, 0x2000, 4096, 0, 0, 0];
        let mut args = args_at_entry("read", &a, &mut mem).unwrap();
        assert_eq!(args[1], "0x2000");
        fill("read", &a, 5, false, &mut mem, &mut args);
        assert_eq!(args[1], "\"hello\"");
        assert_eq!(ret_text("openat", -2, true), "-1 ENOENT");
        assert_eq!(flags(0x22, MAP, ""), "MAP_PRIVATE|MAP_ANONYMOUS");
        assert_eq!(flags(0o1101, OPEN, "O_RDONLY"), "O_WRONLY|O_CREAT|O_TRUNC");
        let f = Filter::parse(&["file".into(), "mmap".into()]).unwrap();
        assert!(f.takes("openat") && f.takes("mmap") && !f.takes("socket"));
        assert!(Filter::parse(&["nonsense".into()]).is_err());
    }
}
