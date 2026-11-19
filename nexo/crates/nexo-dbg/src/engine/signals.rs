//! Names of signals and exceptions, per system, and the ones a session catches by default.

use super::*;

pub fn signal_name(n: u32) -> &'static str {
    match n {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        5 => "SIGTRAP",
        6 => "SIGABRT",
        7 => "SIGBUS",
        8 => "SIGFPE",
        9 => "SIGKILL",
        10 => "SIGUSR1",
        11 => "SIGSEGV",
        12 => "SIGUSR2",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        17 => "SIGCHLD",
        18 => "SIGCONT",
        19 => "SIGSTOP",
        20 => "SIGTSTP",
        28 => "SIGWINCH",
        31 => "SIGSYS",
        _ => "signal",
    }
}

/// The name of a macOS signal: past 6 the numbers are not the Linux ones (SIGBUS is 10, SIGUSR1 30).
pub fn mac_signal_name(n: u32) -> &'static str {
    match n {
        7 => "SIGEMT",
        10 => "SIGBUS",
        12 => "SIGSYS",
        16 => "SIGURG",
        17 => "SIGSTOP",
        18 => "SIGTSTP",
        19 => "SIGCONT",
        20 => "SIGCHLD",
        21 => "SIGTTIN",
        22 => "SIGTTOU",
        23 => "SIGIO",
        24 => "SIGXCPU",
        25 => "SIGXFSZ",
        26 => "SIGVTALRM",
        27 => "SIGPROF",
        28 => "SIGWINCH",
        29 => "SIGINFO",
        30 => "SIGUSR1",
        31 => "SIGUSR2",
        1..=6 | 8 | 9 | 11 | 13..=15 => signal_name(n),
        _ => "signal",
    }
}

/// The name of signal `n` on `os`.
pub fn signal_name_on(os: Os, n: u32) -> &'static str {
    match os {
        Os::MacOS => mac_signal_name(n),
        Os::Linux | Os::Windows => signal_name(n),
    }
}

/// The name of a Windows exception code (`STATUS_` left off), when it is one a program is likely to die of.
pub fn exception_name(code: u32) -> Option<&'static str> {
    Some(match code {
        0x8000_0002 => "DATATYPE_MISALIGNMENT",
        0x8000_0003 => "BREAKPOINT",
        0x8000_0004 => "SINGLE_STEP",
        0xC000_0005 => "ACCESS_VIOLATION",
        0xC000_0006 => "IN_PAGE_ERROR",
        0xC000_0008 => "INVALID_HANDLE",
        0xC000_000D => "INVALID_PARAMETER",
        0xC000_0017 => "NO_MEMORY",
        0xC000_001D => "ILLEGAL_INSTRUCTION",
        0xC000_0025 => "NONCONTINUABLE_EXCEPTION",
        0xC000_0026 => "INVALID_DISPOSITION",
        0xC000_008C => "ARRAY_BOUNDS_EXCEEDED",
        0xC000_008D => "FLOAT_DENORMAL_OPERAND",
        0xC000_008E => "FLOAT_DIVIDE_BY_ZERO",
        0xC000_0090 => "FLOAT_INVALID_OPERATION",
        0xC000_0091 => "FLOAT_OVERFLOW",
        0xC000_0094 => "INTEGER_DIVIDE_BY_ZERO",
        0xC000_0095 => "INTEGER_OVERFLOW",
        0xC000_0096 => "PRIVILEGED_INSTRUCTION",
        0xC000_00FD => "STACK_OVERFLOW",
        0xC000_0135 => "DLL_NOT_FOUND",
        0xC000_0142 => "DLL_INIT_FAILED",
        0xC000_0374 => "HEAP_CORRUPTION",
        0xC000_0409 => "STACK_BUFFER_OVERRUN",
        0xC000_0417 => "INVALID_CRUNTIME_PARAMETER",
        0xE06D_7363 => "C++ exception (throw)",
        0xE24C_4A02 => "Rust panic (unwinding)",
        _ => return None,
    })
}

/// A fault in the words of the platform: `SIGSEGV (signal 11)` or `ACCESS_VIOLATION (exception 0xc0000005)`.
pub fn fault_text(os: Os, number: u32) -> String {
    match os {
        Os::Linux => format!("{} (signal {number})", signal_name(number)),
        Os::MacOS => format!("{} (signal {number})", mac_signal_name(number)),
        Os::Windows => match exception_name(number) {
            Some(n) => format!("{n} (exception {number:#010x})"),
            None => format!("exception {number:#010x}"),
        },
    }
}

/// The Mach exception behind a macOS hardware fault and the name of its code, as crash reports print them.
fn mach_exception(number: u32, code: i32) -> Option<(&'static str, &'static str)> {
    let exc = match number {
        10 | 11 => "EXC_BAD_ACCESS",
        4 => "EXC_BAD_INSTRUCTION",
        8 => "EXC_ARITHMETIC",
        _ => return None,
    };
    let name = match (exc, code) {
        ("EXC_BAD_ACCESS", 1) => "KERN_INVALID_ADDRESS",
        ("EXC_BAD_ACCESS", 2) => "KERN_PROTECTION_FAILURE",
        ("EXC_BAD_ACCESS", 13) => "EXC_I386_GPFLT",
        ("EXC_BAD_INSTRUCTION", 1) => "EXC_I386_INVOP",
        ("EXC_ARITHMETIC", 1) => "EXC_I386_DIV",
        ("EXC_ARITHMETIC", 2) => "EXC_I386_INTO",
        ("EXC_ARITHMETIC", 5) => "EXC_I386_EXTERR",
        ("EXC_ARITHMETIC", 8) => "EXC_I386_SSEEXTERR",
        _ => "",
    };
    Some((exc, name))
}

/// The headline of a fault stop. A macOS fault the processor raised (its `code` is the Mach code) is named by its
/// Mach exception as well: `EXC_BAD_ACCESS (SIGSEGV) KERN_INVALID_ADDRESS (signal 11)`.
pub fn fault_line(os: Os, f: &crate::platform::Fault) -> String {
    match (os, mach_exception(f.number, f.code)) {
        (Os::MacOS, Some((exc, name))) if f.code != 0 => {
            let sig = mac_signal_name(f.number);
            let code = if name.is_empty() { format!("code {}", f.code) } else { name.to_string() };
            format!("{exc} ({sig}) {code} (signal {})", f.number)
        }
        _ => fault_text(os, f.number),
    }
}

/// An exception code from a name (`access_violation`, `STATUS_ACCESS_VIOLATION`) or a number (`0xc0000005`).
pub fn exception_by(word: &str) -> Option<u32> {
    if let Some(h) = word.strip_prefix("0x").or_else(|| word.strip_prefix("0X")) {
        return u32::from_str_radix(h, 16).ok();
    }
    let w = word.strip_prefix("STATUS_").or_else(|| word.strip_prefix("status_")).unwrap_or(word);
    (0x8000_0000u32..=0x8000_0004)
        .chain(0xC000_0000..=0xC000_0500)
        .find(|&c| exception_name(c).is_some_and(|n| n.eq_ignore_ascii_case(w)))
}

pub fn default_catch(os: Os) -> BTreeSet<u32> {
    match os {
        Os::Linux => [4, 5, 6, 7, 8, 11, 31].into_iter().collect(),
        // the same faults in macOS numbers: SIGBUS is 10 and SIGSYS 12 there
        Os::MacOS => [4, 5, 6, 8, 10, 11, 12].into_iter().collect(),
        // the faults a program dies of; a first-chance one the program may still handle itself, so it
        // stops here only for these. Any other exception stops when no handler took it (second chance).
        Os::Windows => [
            0xC000_0005u32,
            0xC000_0006,
            0xC000_001D,
            0xC000_008C,
            0xC000_008E,
            0xC000_0094,
            0xC000_0095,
            0xC000_0096,
            0xC000_00FD,
            0xC000_0374,
            0xC000_0409,
        ]
        .into_iter()
        .collect(),
    }
}
