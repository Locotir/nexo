//! What each thread of a live Linux process is doing: its name, its state letter and the system call it is in,
//! from `/proc/<pid>/task/<tid>/{comm,stat,syscall}`. The parsing is pure; only [`read_task`] touches the files.

/// What `/proc/<pid>/task/<tid>/syscall` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sys {
    /// `running`: the thread is not blocked, so the file has nothing to say.
    Running,
    /// `-1 <sp> <pc>`: blocked, but not in a system call.
    NoSyscall,
    /// `<nr> <a0>..<a5> <sp> <pc>`: in system call `nr` with these arguments.
    In { nr: u64, args: [u64; 6] },
}

/// What one thread is doing, each part `None` when its file could not be read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Task {
    pub comm: Option<String>,
    pub state: Option<char>,
    pub sys: Option<Sys>,
}

/// The state letter of a `/proc/<pid>/task/<tid>/stat` line. The name is in parentheses and may hold spaces and
/// parentheses of its own, so the fields start after the last `)`.
pub fn parse_stat_state(line: &str) -> Option<char> {
    let after = &line[line.rfind(')')? + 1..];
    after.trim_start().chars().next().filter(|c| c.is_ascii_alphabetic())
}

/// The name in a stat line (between the first `(` and the last `)`), for a thread whose `comm` file is gone.
pub fn parse_stat_name(line: &str) -> Option<&str> {
    let (open, close) = (line.find('(')?, line.rfind(')')?);
    (open < close).then(|| &line[open + 1..close])
}

/// A state letter in words.
pub fn state_meaning(c: char) -> &'static str {
    match c {
        'R' => "running",
        'S' => "sleeping",
        'D' => "disk wait",
        'T' => "stopped",
        't' => "stopped by the debugger",
        'Z' => "zombie",
        'X' | 'x' => "dead",
        'I' => "idle",
        'K' => "wakekill",
        'W' => "waking",
        'P' => "parked",
        _ => "unknown state",
    }
}

fn hex(s: &str) -> Option<u64> {
    u64::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).ok()
}

/// The text of `/proc/<pid>/task/<tid>/syscall`: `running`, `-1 sp pc`, or a number then six hex arguments, sp and pc.
pub fn parse_syscall(text: &str) -> Option<Sys> {
    let mut w = text.split_whitespace();
    let first = w.next()?;
    if first == "running" {
        return Some(Sys::Running);
    }
    if first == "-1" {
        return Some(Sys::NoSyscall);
    }
    let nr: u64 = first.parse().ok()?;
    let mut args = [0u64; 6];
    for a in args.iter_mut() {
        *a = hex(w.next()?)?;
    }
    Some(Sys::In { nr, args })
}

/// A futex operation's name (the low bits of its second argument, past the private and clock flags).
fn futex_op(op: u64) -> String {
    match op & 0x7f {
        0 => "WAIT".into(),
        1 => "WAKE".into(),
        3 => "REQUEUE".into(),
        4 => "CMP_REQUEUE".into(),
        5 => "WAKE_OP".into(),
        6 => "LOCK_PI".into(),
        7 => "UNLOCK_PI".into(),
        8 => "TRYLOCK_PI".into(),
        9 => "WAIT_BITSET".into(),
        10 => "WAKE_BITSET".into(),
        11 => "WAIT_REQUEUE_PI".into(),
        12 => "CMP_REQUEUE_PI".into(),
        n => format!("op {n}"),
    }
}

/// `in read(fd 3)`-style words for a system call; a futex says what it waits on.
pub fn syscall_text(s: &Sys, name_of: &dyn Fn(u64) -> Option<&'static str>) -> Option<String> {
    match s {
        Sys::Running | Sys::NoSyscall => None,
        Sys::In { nr, args } => {
            let name = name_of(*nr).map_or_else(|| format!("syscall {nr}"), str::to_string);
            Some(if name.starts_with("futex") {
                format!("in {name} ({}) on {:#x}", futex_op(args[1]), args[0])
            } else {
                format!("in {name}")
            })
        }
    }
}

/// The part of a `threads` row after the pc: `"worker" t (stopped by the debugger), in futex (WAIT) on 0x...`.
/// Empty when nothing is known.
pub fn row_tail(t: &Task, name_of: &dyn Fn(u64) -> Option<&'static str>) -> String {
    let mut parts = Vec::new();
    if let Some(c) = &t.comm {
        parts.push(format!("\"{c}\""));
    }
    if let Some(s) = t.state {
        parts.push(format!("{s} ({})", state_meaning(s)));
    }
    if let Some(x) = t.sys.as_ref().and_then(|s| syscall_text(s, name_of)) {
        parts.push(x);
    }
    parts.join(", ")
}

/// Read what thread `tid` of process `pid` is doing; a file that cannot be read leaves its part `None`.
pub fn read_task(pid: u32, tid: u32) -> Task {
    let dir = format!("/proc/{pid}/task/{tid}");
    let read = |f: &str| std::fs::read_to_string(format!("{dir}/{f}")).ok();
    let stat = read("stat");
    let comm = read("comm")
        .map(|c| c.trim_end_matches('\n').to_string())
        .filter(|c| !c.is_empty())
        .or_else(|| stat.as_deref().and_then(parse_stat_name).map(str::to_string));
    Task {
        comm,
        state: stat.as_deref().and_then(parse_stat_state),
        sys: read("syscall").and_then(|t| parse_syscall(&t)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_is_found_after_the_last_parenthesis_of_a_name_with_spaces_and_parentheses() {
        let plain = "1234 (worker) S 1 1234 1234 0 -1 4194560 100 0";
        assert_eq!(parse_stat_state(plain), Some('S'));
        let odd = "77 (my (odd) name) t 1 77 77 0 -1 0";
        assert_eq!(parse_stat_state(odd), Some('t'));
        assert_eq!(parse_stat_name(odd), Some("my (odd) name"));
        // a name that looks like a state and a field list
        let trick = "9 (a) R (b) D 1 9";
        assert_eq!(parse_stat_state(trick), Some('D'));
        assert_eq!(parse_stat_state("no parentheses here"), None);
        assert_eq!(parse_stat_state("5 (x)"), None);
        assert_eq!(state_meaning('D'), "disk wait");
        assert_eq!(state_meaning('t'), "stopped by the debugger");
    }

    #[test]
    fn the_syscall_file_is_running_not_in_a_call_or_a_number_with_arguments() {
        assert_eq!(parse_syscall("running\n"), Some(Sys::Running));
        assert_eq!(parse_syscall("-1 0x7ffd1234 0x401000\n"), Some(Sys::NoSyscall));
        let t = "202 0x7f0000001000 0x80 0x2 0x0 0x0 0x0 0x7ffd5678 0x7f0000002000\n";
        assert_eq!(parse_syscall(t), Some(Sys::In { nr: 202, args: [0x7f00_0000_1000, 0x80, 0x2, 0, 0, 0] }));
        assert_eq!(parse_syscall(""), None);
        assert_eq!(parse_syscall("202 0x1 0x2"), None);
        assert_eq!(parse_syscall("zzz"), None);
    }

    #[test]
    fn a_futex_wait_names_the_address_and_other_calls_just_their_name() {
        let names = |n: u64| match n {
            202 => Some("futex"),
            0 => Some("read"),
            _ => None,
        };
        let futex = Sys::In { nr: 202, args: [0x7f00_0000_1000, 0x89, 0, 0, 0, 0] };
        // 0x89 is FUTEX_WAIT_BITSET | FUTEX_PRIVATE_FLAG
        assert_eq!(syscall_text(&futex, &names).as_deref(), Some("in futex (WAIT_BITSET) on 0x7f0000001000"));
        let read = Sys::In { nr: 0, args: [3, 0, 0, 0, 0, 0] };
        assert_eq!(syscall_text(&read, &names).as_deref(), Some("in read"));
        let odd = Sys::In { nr: 9999, args: [0; 6] };
        assert_eq!(syscall_text(&odd, &names).as_deref(), Some("in syscall 9999"));
        assert_eq!(syscall_text(&Sys::Running, &names), None);
        let task = Task { comm: Some("worker".into()), state: Some('t'), sys: Some(futex) };
        assert_eq!(
            row_tail(&task, &names),
            "\"worker\", t (stopped by the debugger), in futex (WAIT_BITSET) on 0x7f0000001000"
        );
        assert_eq!(row_tail(&Task::default(), &names), "");
    }
}
