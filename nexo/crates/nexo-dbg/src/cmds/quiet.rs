//! Cheaper long sessions: `stops compact|full` (a stop in one line) and `signal <SIG|0>` (continue delivering
//! a signal, or dropping the pending one). The words are parsed here without a session, so tests need none.

use super::*;

/// What `stops` was asked.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StopsAsk {
    Show,
    Compact,
    Full,
}

/// `stops` words: none shows the mode, `compact` or `full` sets it.
pub(crate) fn parse_stops(words: &[String]) -> std::result::Result<StopsAsk, String> {
    match words.first().map(String::as_str) {
        None => Ok(StopsAsk::Show),
        Some("compact") => Ok(StopsAsk::Compact),
        Some("full") => Ok(StopsAsk::Full),
        Some(w) => Err(format!("stops {w:?}: `stops compact` (one line per stop) or `stops full` (the default)")),
    }
}

/// A stop is told in one line when the mode asks for it, except a signal: a crash is triaged, and that needs
/// the detail more than it needs the tokens.
pub(crate) fn compact_applies(on: bool, why: &Why) -> bool {
    on && !matches!(why, Why::Signal(_))
}

/// Why a stop happened, in a few words.
pub(crate) fn compact_why(why: &Why) -> String {
    let ids = |v: &[u32]| v.iter().map(|i| format!("#{i}")).collect::<Vec<_>>().join(" ");
    match why {
        Why::Entry => "entry".into(),
        Why::Step => "step".into(),
        Why::Next => "next".into(),
        Why::Finish => "finish".into(),
        Why::Until => "until".into(),
        Why::Break(v) => format!("breakpoint {}", ids(v)),
        Why::Watch(v) => format!("watch {}", ids(v)),
        Why::Signal(_) => "signal".into(),
        Why::Int3 => "int3".into(),
        Why::Interrupt => "interrupt".into(),
        Why::Exec => "exec (addresses are new)".into(),
        Why::Syscall(t) => format!("syscall {t}"),
        Why::Load(t) | Why::Fork(t) | Why::LetGo(t) => t.clone(),
        Why::Hook(_, t) => format!("hook {t}"),
        Why::Returned => "call returned".into(),
        Why::Exited(_) | Why::Killed(_) => "exit".into(),
    }
}

/// Where a stop is: the source line when there is one (after the symbol), else the symbol and offset, else the pc.
pub(crate) fn compact_place(pc: u64, name: &str, src: Option<&str>) -> String {
    match (src, name) {
        (Some(s), "") => s.to_string(),
        (Some(s), n) => format!("{n} {s}"),
        (None, "") => format!("pc {pc:#x}"),
        (None, n) => n.to_string(),
    }
}

/// The one line of a compact stop: why, where, then what else the stop carries (a returned value, each display
/// as `expr=value`), parts split by ` | `.
pub(crate) fn compact_line(why: &str, place: &str, rest: &[String]) -> String {
    let mut parts: Vec<&str> = vec![why, place];
    parts.extend(rest.iter().map(String::as_str));
    parts.retain(|p| !p.is_empty());
    parts.join(" | ")
}

/// A signal for `signal`: 0 (drop the pending one), a number, or a name with or without `SIG`.
pub(crate) fn parse_signal(os: Os, word: &str) -> std::result::Result<u32, String> {
    let w = word.trim();
    if let Ok(n) = w.parse::<u32>() {
        return if n <= 64 { Ok(n) } else { Err(format!("{n} is not a signal number (0 to 64)")) };
    }
    let up = w.to_ascii_uppercase();
    let full = if up.starts_with("SIG") { up } else { format!("SIG{up}") };
    (1..32u32)
        .find(|n| crate::engine::signal_name_on(os, *n) == full.as_str())
        .ok_or_else(|| format!("{word:?} is not a signal: SIGUSR1, SIGINT, 10 ... or 0 to drop the pending one"))
}

impl Session {
    /// `stops [compact|full]`.
    pub(super) fn cmd_stops(&mut self, a: &Args) -> Result<Output> {
        let ask = parse_stops(&a.pos).map_err(crate::Error::Usage)?;
        match ask {
            StopsAsk::Show => {}
            StopsAsk::Compact => {
                self.compact_stops = true;
                self.compact_told = false;
            }
            StopsAsk::Full => self.compact_stops = false,
        }
        let mode = if self.compact_stops { "compact" } else { "full" };
        Ok(rows("stops", vec![format!("stops: {mode}")], json!({"stops": mode})))
    }

    /// `signal <SIG|0> [--wait N]`: go on from the stop delivering that signal; 0 drops the pending one.
    pub(super) fn cmd_signal(&mut self, a: &Args) -> Result<Output> {
        let os = self.t.os();
        if os != Os::Linux {
            return fail("`signal` runs on Linux only: this target cannot hold or deliver a signal at a resume");
        }
        let Some(w) = a.pos.first() else {
            return usage("signal <SIG|N|0> [--wait N]   (go on delivering that signal; 0 drops the pending one)");
        };
        let n = parse_signal(os, w).map_err(crate::Error::Usage)?;
        self.need_stopped()?;
        let wait = a.wait()?;
        let tid = self.cur;
        self.t.give_signal(tid, u64::from(n));
        self.go(Run::default(), wait)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn a_breakpoint_stop_is_one_line_with_its_displays() {
        let why = compact_why(&Why::Break(vec![1, 3]));
        let place = compact_place(0x401136, "main+0x4", Some("a.c:12"));
        let line = compact_line(&why, &place, &["x=5".to_string(), "y=7".to_string()]);
        assert_eq!(line, "breakpoint #1 #3 | main+0x4 a.c:12 | x=5 | y=7");
        assert!(!line.contains('\n'));
    }

    #[test]
    fn a_watch_a_step_and_a_finish_with_its_value_are_one_line_each() {
        assert_eq!(compact_why(&Why::Watch(vec![2])), "watch #2");
        assert_eq!(compact_line("step", &compact_place(0x10, "f+0x2", None), &[]), "step | f+0x2");
        assert_eq!(compact_line("step", &compact_place(0x10, "", None), &[]), "step | pc 0x10");
        let fin = compact_line(&compact_why(&Why::Finish), "main a.c:9", &["returned: 5 (int)".to_string()]);
        assert_eq!(fin, "finish | main a.c:9 | returned: 5 (int)");
        assert_eq!(compact_place(1, "", Some("a.c:3")), "a.c:3");
    }

    #[test]
    fn a_crash_keeps_the_full_report_and_the_mode_off_changes_nothing() {
        let crash =
            Why::Signal(crate::platform::Fault { number: 11, code: 1, addr: Some(0), touch: None, first_chance: true });
        assert!(!compact_applies(true, &crash));
        assert!(compact_applies(true, &Why::Break(vec![1])));
        assert!(!compact_applies(false, &Why::Break(vec![1])));
    }

    #[test]
    fn stops_and_signal_words_are_parsed() {
        assert_eq!(parse_stops(&w("")), Ok(StopsAsk::Show));
        assert_eq!(parse_stops(&w("compact")), Ok(StopsAsk::Compact));
        assert_eq!(parse_stops(&w("full")), Ok(StopsAsk::Full));
        assert!(parse_stops(&w("tiny")).is_err());
        assert_eq!(parse_signal(Os::Linux, "0"), Ok(0));
        assert_eq!(parse_signal(Os::Linux, "11"), Ok(11));
        assert_eq!(parse_signal(Os::Linux, "SIGUSR1"), Ok(10));
        assert_eq!(parse_signal(Os::Linux, "usr1"), Ok(10));
        assert!(parse_signal(Os::Linux, "SIGNOPE").is_err());
        assert!(parse_signal(Os::Linux, "99").is_err());
    }
}
