//! Linux system calls (x86-64 and AArch64) for the emulator, with no effect on the host: `read` takes bytes from a buffer
//! the person gave (`--stdin`), `write` goes into the run's output, memory calls work on emulator pages, the
//! clock and the process id are fixed, and the random bytes come from a seed. A system call this file does not
//! know stops the run with its number and name; nothing is faked.

use super::cpu::{RCX, RDI, RDX, RSI};
use super::*;

/// Where `mmap` puts memory (bump allocation, never reused), and the most it hands out in all.
const ARENA_BASE: u64 = 0x7f00_0000_0000;
const ARENA_MAX: u64 = 64 << 20;
/// Where `brk` starts, and how far it may grow.
const BRK_BASE: u64 = 0x7e80_0000_0000;
const BRK_MAX: u64 = 64 << 20;
/// The clock (seconds since 1970) when `--clock` is not given: 2023-11-14 22:13:20 UTC.
pub(super) const DEFAULT_CLOCK: u64 = 1_700_000_000;
/// What `getpid` and `gettid` answer.
const FAKE_PID: u64 = 4242;
/// The most bytes `--stdin` may hold.
const MAX_STDIN: u64 = 16 << 20;

const EBADF: u64 = 9;
const ENOMEM: u64 = 12;
const EFAULT: u64 = 14;
const ENODEV: u64 = 19;
const EINVAL: u64 = 22;

/// The state the system calls keep between calls.
pub(super) struct Sys {
    pub stdin: Vec<u8>,
    stdin_pos: usize,
    pub clock: u64,
    /// The state of the random byte stream (starts at `--seed`).
    pub rng: u64,
    brk: u64,
    /// The end of the pages `brk` has mapped (page aligned).
    brk_end: u64,
    mmap_next: u64,
}

impl Sys {
    pub(super) fn new() -> Sys {
        Sys {
            stdin: Vec::new(),
            stdin_pos: 0,
            clock: DEFAULT_CLOCK,
            rng: 0,
            brk: BRK_BASE,
            brk_end: BRK_BASE,
            mmap_next: ARENA_BASE,
        }
    }

    /// The next 8 bytes of the stream (splitmix64: same seed, same bytes).
    fn next_u64(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// What `--stdin` gives: a file's bytes when the file exists, `hex:DE AD` as bytes, else the text itself.
pub(super) fn load_stdin(s: &str) -> Result<Vec<u8>> {
    let p = std::path::Path::new(s);
    if p.is_file() {
        let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        if len > MAX_STDIN {
            return Err(Error::Bad(format!("--stdin {s:?}: {len} bytes; emu takes at most {} MiB", MAX_STDIN >> 20)));
        }
        return std::fs::read(p).map_err(|e| Error::Bad(format!("--stdin {s:?}: {e}")));
    }
    if s.starts_with("hex:") {
        if let Value_::Bytes(b, _) = parse_value(s)? {
            return Ok(b);
        }
    }
    Ok(s.as_bytes().to_vec())
}

fn neg(e: u64) -> u64 {
    0u64.wrapping_sub(e)
}

fn page_up(v: u64) -> u64 {
    (v + mem::PAGE - 1) & !(mem::PAGE - 1)
}

/// The name of a system call number (Linux x86-64), `?` when this table does not have it.
pub(super) fn name_of(n: u64) -> &'static str {
    match n {
        0 => "read",
        1 => "write",
        2 => "open",
        3 => "close",
        4 => "stat",
        5 => "fstat",
        6 => "lstat",
        7 => "poll",
        8 => "lseek",
        9 => "mmap",
        10 => "mprotect",
        11 => "munmap",
        12 => "brk",
        13 => "rt_sigaction",
        14 => "rt_sigprocmask",
        16 => "ioctl",
        17 => "pread64",
        19 => "readv",
        20 => "writev",
        21 => "access",
        22 => "pipe",
        24 => "sched_yield",
        25 => "mremap",
        28 => "madvise",
        35 => "nanosleep",
        39 => "getpid",
        41 => "socket",
        42 => "connect",
        56 => "clone",
        57 => "fork",
        59 => "execve",
        60 => "exit",
        61 => "wait4",
        62 => "kill",
        63 => "uname",
        72 => "fcntl",
        79 => "getcwd",
        89 => "readlink",
        96 => "gettimeofday",
        97 => "getrlimit",
        102 => "getuid",
        104 => "getgid",
        110 => "getppid",
        158 => "arch_prctl",
        186 => "gettid",
        201 => "time",
        202 => "futex",
        218 => "set_tid_address",
        228 => "clock_gettime",
        230 => "clock_nanosleep",
        231 => "exit_group",
        257 => "openat",
        262 => "newfstatat",
        273 => "set_robust_list",
        302 => "prlimit64",
        318 => "getrandom",
        334 => "rseq",
        435 => "clone3",
        _ => "?",
    }
}

/// What a system call does, whichever architecture numbers it. Only the number table and the register
/// mapping are per architecture; the effects below are shared.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Op {
    Read,
    Write,
    Mmap,
    Mprotect,
    Munmap,
    Brk,
    Getpid,
    Exit,
    Gettimeofday,
    ArchPrctl,
    Time,
    ClockGettime,
    Getrandom,
}

/// The emulated calls of Linux x86-64, by number.
fn op_x86(n: u64) -> Option<Op> {
    Some(match n {
        0 => Op::Read,
        1 => Op::Write,
        9 => Op::Mmap,
        10 => Op::Mprotect,
        11 => Op::Munmap,
        12 => Op::Brk,
        39 | 186 => Op::Getpid,
        60 | 231 => Op::Exit,
        96 => Op::Gettimeofday,
        158 => Op::ArchPrctl,
        201 => Op::Time,
        228 => Op::ClockGettime,
        318 => Op::Getrandom,
        _ => return None,
    })
}

/// The emulated calls of Linux AArch64 (the generic table of `asm-generic/unistd.h`), by number.
fn op_a64(n: u64) -> Option<Op> {
    Some(match n {
        63 => Op::Read,
        64 => Op::Write,
        222 => Op::Mmap,
        226 => Op::Mprotect,
        215 => Op::Munmap,
        214 => Op::Brk,
        172 | 178 => Op::Getpid,
        93 | 94 => Op::Exit,
        169 => Op::Gettimeofday,
        113 => Op::ClockGettime,
        278 => Op::Getrandom,
        _ => return None,
    })
}

/// The name of a system call number (Linux AArch64), `?` when this table does not have it.
pub(super) fn name_of_a64(n: u64) -> &'static str {
    match n {
        29 => "ioctl",
        56 => "openat",
        57 => "close",
        63 => "read",
        64 => "write",
        66 => "writev",
        78 => "readlinkat",
        79 => "newfstatat",
        80 => "fstat",
        93 => "exit",
        94 => "exit_group",
        96 => "set_tid_address",
        113 => "clock_gettime",
        134 => "rt_sigaction",
        135 => "rt_sigprocmask",
        160 => "uname",
        169 => "gettimeofday",
        172 => "getpid",
        178 => "gettid",
        214 => "brk",
        215 => "munmap",
        222 => "mmap",
        226 => "mprotect",
        261 => "prlimit64",
        278 => "getrandom",
        _ => "?",
    }
}

/// How many arguments a system call this file emulates takes.
fn arg_count(op: Op) -> usize {
    match op {
        Op::Getpid => 0,
        Op::Brk | Op::Exit | Op::Time => 1,
        Op::Munmap | Op::Gettimeofday | Op::ArchPrctl | Op::ClockGettime => 2,
        Op::Read | Op::Write | Op::Mprotect | Op::Getrandom => 3,
        Op::Mmap => 6,
    }
}

fn errno_name(e: u64) -> &'static str {
    match e {
        EBADF => "EBADF",
        ENOMEM => "ENOMEM",
        EFAULT => "EFAULT",
        ENODEV => "ENODEV",
        EINVAL => "EINVAL",
        _ => "error",
    }
}

/// A value in a call log: small numbers and negative ones in decimal, the rest in hex.
fn short(v: u64) -> String {
    let s = v as i64;
    if v < 10 || (-4096..0).contains(&s) {
        format!("{s}")
    } else {
        format!("{v:#x}")
    }
}

fn result_text(ret: u64) -> String {
    let s = ret as i64;
    if (-4095..0).contains(&s) {
        format!("{s} ({})", errno_name(s.unsigned_abs()))
    } else {
        short(ret)
    }
}

impl Machine<'_> {
    /// Run x86-64 system call `n` (the arguments are in the registers, as `syscall` takes them). `Ok` when the
    /// run goes on: rax has the answer, rcx the next instruction and r11 the flags, as the real instruction
    /// leaves them. `site` is the address of the `syscall` instruction and `next` the one after it.
    pub(super) fn syscall(&mut self, n: u64, site: u64, next: u64) -> std::result::Result<(), Stop> {
        let g = self.cpu.gpr;
        let a = [g[RDI], g[RSI], g[RDX], g[10], g[8], g[9]];
        let ret = self.run_sys(n, name_of(n), op_x86(n), a, site)?;
        self.cpu.gpr[RAX] = ret;
        self.cpu.gpr[RCX] = next;
        self.cpu.gpr[11] = self.cpu.flags;
        Ok(())
    }

    /// Run AArch64 system call `n` (x8) with the arguments x0..x5. `Ok` has the answer for x0.
    pub(super) fn syscall_a64(&mut self, n: u64, a: [u64; 6], site: u64) -> std::result::Result<u64, Stop> {
        self.run_sys(n, name_of_a64(n), op_a64(n), a, site)
    }

    /// The part both architectures share: log the call, do its effect, answer. A call `op` does not name stops
    /// the run with its number and name.
    fn run_sys(
        &mut self,
        n: u64,
        name: &str,
        op: Option<Op>,
        a: [u64; 6],
        site: u64,
    ) -> std::result::Result<u64, Stop> {
        let Some(op) = op else {
            self.log_kind(site, format!("syscall {n} ({name}) stopped the run"), true);
            return Err(Stop::Syscall(n));
        };
        let args: Vec<String> = (0..arg_count(op))
            .map(|i| if op == Op::Write && i == 1 { self.show_ptr(a[1]) } else { short(a[i]) })
            .collect();
        let args = args.join(", ");
        let ret = match op {
            Op::Read => self.sys_read(a[0], a[1], a[2]),
            Op::Write => self.sys_write(a[0], a[1], a[2]),
            Op::Mmap => self.sys_mmap(a),
            Op::Mprotect => self.sys_mprotect(a[0], a[1], a[2]),
            Op::Munmap => self.sys_munmap(a[0], a[1]),
            Op::Brk => self.sys_brk(a[0]),
            Op::Getpid => FAKE_PID,
            Op::Exit => {
                self.log_kind(site, format!("syscall {name}({args})"), true);
                return Err(Stop::Exit(a[0] as i32 as i64));
            }
            Op::Gettimeofday => self.sys_gettimeofday(a[0], a[1]),
            Op::ArchPrctl => self.sys_arch_prctl(a[0], a[1]),
            Op::Time => self.sys_time(a[0]),
            Op::ClockGettime => self.sys_clock_gettime(a[1]),
            Op::Getrandom => self.sys_getrandom(a[0], a[1]),
        };
        self.log_kind(site, format!("syscall {name}({args}) = {}", result_text(ret)), true);
        Ok(ret)
    }

    fn put(&mut self, at: u64, bytes: &[u8]) -> bool {
        self.mem.write(at, bytes).is_ok()
    }

    fn sys_read(&mut self, fd: u64, buf: u64, count: u64) -> u64 {
        if fd != 0 {
            return neg(EBADF);
        }
        let left = (self.sys.stdin.len() - self.sys.stdin_pos) as u64;
        let n = count.min(left) as usize;
        if n == 0 {
            return 0;
        }
        let from = self.sys.stdin_pos;
        let chunk = self.sys.stdin[from..from + n].to_vec();
        if !self.put(buf, &chunk) {
            return neg(EFAULT);
        }
        self.sys.stdin_pos += n;
        n as u64
    }

    fn sys_write(&mut self, fd: u64, buf: u64, count: u64) -> u64 {
        if fd != 1 && fd != 2 {
            return neg(EBADF);
        }
        if count > MAX_LIB_BYTES {
            return neg(EINVAL);
        }
        let mut data = vec![0u8; count as usize];
        if self.mem.read(buf, &mut data).is_err() {
            return neg(EFAULT);
        }
        self.emit(fd == 2, &data);
        count
    }

    /// Anonymous memory only; a file-backed mapping is `-ENODEV`. `MAP_FIXED` works inside memory `mmap` already
    /// gave out. Addresses are not reused after `munmap`.
    fn sys_mmap(&mut self, a: [u64; 6]) -> u64 {
        let (addr, len, prot, flags) = (a[0], a[1], a[2], a[3]);
        if len == 0 {
            return neg(EINVAL);
        }
        if flags & 0x20 == 0 {
            return neg(ENODEV);
        }
        if len > ARENA_MAX {
            return neg(ENOMEM);
        }
        let size = page_up(len);
        let at = if flags & 0x10 != 0 {
            if addr % mem::PAGE != 0 {
                return neg(EINVAL);
            }
            if addr < ARENA_BASE || addr.checked_add(size).is_none_or(|end| end > self.sys.mmap_next) {
                return neg(ENOMEM);
            }
            self.mem.unmap(addr, size);
            addr
        } else {
            if self.sys.mmap_next + size > ARENA_BASE + ARENA_MAX {
                return neg(ENOMEM);
            }
            let at = self.sys.mmap_next;
            self.sys.mmap_next += size;
            at
        };
        self.mem.map(at, size, prot & 2 != 0, prot & 4 != 0);
        at
    }

    fn sys_munmap(&mut self, addr: u64, len: u64) -> u64 {
        if addr % mem::PAGE != 0 || len == 0 || len > ARENA_MAX {
            return neg(EINVAL);
        }
        let size = page_up(len);
        if addr < ARENA_BASE || addr.checked_add(size).is_none_or(|end| end > self.sys.mmap_next) {
            return neg(EINVAL);
        }
        self.mem.unmap(addr, size);
        0
    }

    /// Succeeds on mapped pages. It only adds: a page that gains write or exec keeps it, and none is taken away.
    fn sys_mprotect(&mut self, addr: u64, len: u64, prot: u64) -> u64 {
        if addr % mem::PAGE != 0 || len > ARENA_MAX * 4 {
            return neg(EINVAL);
        }
        let size = page_up(len);
        if !self.mem.is_range_mapped(addr, size) {
            return neg(ENOMEM);
        }
        self.mem.map(addr, size, prot & 2 != 0, prot & 4 != 0);
        0
    }

    fn sys_brk(&mut self, want: u64) -> u64 {
        if !(BRK_BASE..=BRK_BASE + BRK_MAX).contains(&want) {
            return self.sys.brk;
        }
        let end = page_up(want);
        let old = self.sys.brk_end;
        if end > old {
            self.mem.map(old, end - old, true, false);
        } else if end < old {
            self.mem.unmap(end, old - end);
        }
        self.sys.brk_end = end;
        self.sys.brk = want;
        want
    }

    fn sys_time(&mut self, tloc: u64) -> u64 {
        let t = self.sys.clock;
        if tloc != 0 && !self.put(tloc, &t.to_le_bytes()) {
            return neg(EFAULT);
        }
        t
    }

    fn sys_clock_gettime(&mut self, ts: u64) -> u64 {
        let mut b = [0u8; 16];
        b[..8].copy_from_slice(&self.sys.clock.to_le_bytes());
        if !self.put(ts, &b) {
            return neg(EFAULT);
        }
        0
    }

    fn sys_gettimeofday(&mut self, tv: u64, tz: u64) -> u64 {
        let mut b = [0u8; 16];
        b[..8].copy_from_slice(&self.sys.clock.to_le_bytes());
        if tv != 0 && !self.put(tv, &b) {
            return neg(EFAULT);
        }
        if tz != 0 && !self.put(tz, &[0u8; 8]) {
            return neg(EFAULT);
        }
        0
    }

    /// At most 1 MiB per call; the answer is the number of bytes written, as the real call allows.
    fn sys_getrandom(&mut self, buf: u64, len: u64) -> u64 {
        let n = len.min(MAX_ARG_BUF as u64) as usize;
        let mut data = Vec::with_capacity(n);
        while data.len() < n {
            let w = self.sys.next_u64().to_le_bytes();
            let take = (n - data.len()).min(8);
            data.extend_from_slice(&w[..take]);
        }
        if !self.put(buf, &data) {
            return neg(EFAULT);
        }
        n as u64
    }

    /// `ARCH_SET_FS` and `ARCH_GET_FS`; any other code is `-EINVAL`.
    fn sys_arch_prctl(&mut self, code: u64, addr: u64) -> u64 {
        match code {
            0x1002 => {
                self.cpu.fs_base = addr;
                0
            }
            0x1003 => {
                let fs = self.cpu.fs_base;
                if self.put(addr, &fs.to_le_bytes()) {
                    0
                } else {
                    neg(EFAULT)
                }
            }
            _ => neg(EINVAL),
        }
    }
}
