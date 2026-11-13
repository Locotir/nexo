//! `catch`: signals and, on Windows, exceptions.

use super::*;

impl Session {
    pub(super) fn cmd_catch(&mut self, a: &Args) -> Result<Output> {
        if self.t.os() == Os::Windows {
            return self.cmd_catch_windows(a);
        }
        let names: Vec<(u32, &str)> = (1..32).map(|n| (n, signal_name(n))).filter(|(_, s)| *s != "signal").collect();
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
            Some("all") => self.catch = names.iter().map(|(n, _)| *n).filter(|n| ![17, 28].contains(n)).collect(),
            Some(_) => return usage("catch [add|del <signal>...] | catch all"),
            None => {}
        }
        let l: Vec<String> = self.catch.iter().map(|n| signal_name(*n).to_string()).collect();
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
