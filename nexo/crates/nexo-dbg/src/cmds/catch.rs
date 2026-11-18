//! `catch`: signals and, on Windows, exceptions; `catch throw|panic|abort` break on the function that raises them.

use super::*;

/// The functions `catch throw`, `catch panic` and `catch abort` break on (the first is always set, a pending
/// breakpoint when no loaded file has it; the others only when a loaded file has them).
pub(crate) fn catch_symbols(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "throw" => &["__cxa_throw"],
        "panic" => &["rust_begin_unwind", "rust_panic", "runtime.gopanic"],
        "abort" => &["abort"],
        _ => return None,
    })
}

impl Session {
    /// `catch throw|panic|abort`: a breakpoint (pending until a library has the name) on what raises it.
    pub(super) fn cmd_catch_event(&mut self, a: &Args) -> Result<Output> {
        let kind = a.pos[0].clone();
        let Some(names) = catch_symbols(&kind) else { return usage("catch throw|panic|abort") };
        if a.pos.len() > 1 {
            return usage("catch throw|panic|abort");
        }
        let mut found: Vec<&str> = names.iter().copied().filter(|n| self.resolve_place(n).is_ok()).collect();
        if found.is_empty() {
            found.push(names[0]);
        }
        let mut lines = Vec::new();
        let mut ids = Vec::new();
        for n in found {
            let words = vec![n.to_string()];
            let args = Args::new(&words)?;
            let before = self.bps.len();
            self.cmd_break(false, &words, &args)?;
            for b in self.bps.iter().skip(before) {
                ids.push(b.id);
                let how = if b.addr == 0 {
                    "pending, no loaded file has it yet".to_string()
                } else {
                    format!("at {:#x}", b.addr)
                };
                lines.push(format!("catch {kind}: breakpoint #{} on {n} ({how})", b.id));
            }
        }
        Ok(rows("catch", lines, json!({"catch": kind, "breakpoints": ids})))
    }

    pub(super) fn cmd_catch(&mut self, a: &Args) -> Result<Output> {
        if self.t.os() == Os::Windows {
            return self.cmd_catch_windows(a);
        }
        let os = self.t.os();
        let names: Vec<(u32, &str)> =
            (1..32).map(|n| (n, crate::engine::signal_name_on(os, n))).filter(|(_, s)| *s != "signal").collect();
        let by = |w: &str| -> Result<u32> {
            names
                .iter()
                .find(|(n, s)| s.eq_ignore_ascii_case(w) || s[3..].eq_ignore_ascii_case(w) || n.to_string() == w)
                .map(|(n, _)| *n)
                .ok_or_else(|| crate::Error::Usage(format!("{w:?} is not a signal: SIGSEGV, SIGABRT, 11 ...")))
        };
        match a.pos.first().map(String::as_str) {
            Some("add") | Some("on") => {
                for w in &a.pos[1..] {
                    self.catch.insert(by(w)?);
                }
            }
            Some("del") | Some("off") => {
                for w in &a.pos[1..] {
                    self.catch.remove(&by(w)?);
                }
            }
            Some("all") => {
                self.catch = names
                    .iter()
                    .map(|(n, _)| *n)
                    .filter(|n| !matches!(crate::engine::signal_name_on(os, *n), "SIGCHLD" | "SIGWINCH"))
                    .collect()
            }
            Some(_) => return usage("catch [add|del <signal>...] | catch all"),
            None => {}
        }
        let l: Vec<String> = self.catch.iter().map(|n| crate::engine::signal_name_on(os, *n).to_string()).collect();
        Ok(rows("catch", vec![format!("stops on: {}", l.join(" "))], json!({})))
    }

    /// `catch` on Windows: exceptions by name or code. Besides these, any exception no handler took stops the program.
    fn cmd_catch_windows(&mut self, a: &Args) -> Result<Output> {
        let by = |w: &str| -> Result<u32> {
            crate::engine::exception_by(w).ok_or_else(|| {
                crate::Error::Usage(format!(
                    "{w:?} is not an exception: access_violation, integer_divide_by_zero, 0xc0000005 ..."
                ))
            })
        };
        match a.pos.first().map(String::as_str) {
            Some("add") | Some("on") => {
                for w in &a.pos[1..] {
                    self.catch.insert(by(w)?);
                }
            }
            Some("del") | Some("off") => {
                for w in &a.pos[1..] {
                    self.catch.remove(&by(w)?);
                }
            }
            Some("all") => {
                self.catch =
                    (0xC000_0000u32..=0xC000_0500).filter(|c| crate::engine::exception_name(*c).is_some()).collect();
                self.catch.extend([0xE06D_7363u32, 0x8000_0002]);
            }
            Some(_) => return usage(
                "catch [add|del <exception>...] | catch all   (a name like access_violation or a code like 0xc0000005)",
            ),
            None => {}
        }
        let l: Vec<String> = self.catch.iter().map(|n| crate::engine::fault_text(Os::Windows, *n)).collect();
        Ok(rows(
            "catch",
            vec![format!("stops on: {}; and on any exception the program does not handle", l.join(", "))],
            json!({}),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::catch_symbols;

    #[test]
    fn catch_throw_panic_and_abort_name_the_functions_that_raise_them() {
        assert_eq!(catch_symbols("throw"), Some(&["__cxa_throw"][..]));
        assert_eq!(catch_symbols("panic"), Some(&["rust_begin_unwind", "rust_panic", "runtime.gopanic"][..]));
        assert_eq!(catch_symbols("abort"), Some(&["abort"][..]));
        assert_eq!(catch_symbols("add"), None);
        assert_eq!(catch_symbols("load"), None);
    }
}
