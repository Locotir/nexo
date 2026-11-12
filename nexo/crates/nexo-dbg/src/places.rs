//! Places a person names the way a source debugger takes them: `file.c:42`, `*0x401136`, and a function of the
//! program, which stops after its prologue so its arguments and locals already hold their values.

use crate::engine::Session;
use crate::{fail, Result};

impl Session {
    /// The code address for a place the line table answers, or `None` to hand it to `resolve`. Function names
    /// move past the prologue only with `prologue` (a breakpoint), never for `mem main`.
    pub(crate) fn source_place(&mut self, spec: &str, prologue: bool) -> Option<Result<u64>> {
        if let Some((file, line)) = file_line(spec) {
            let Some((lines, bias)) = self.mods.main_lines() else {
                return Some(fail(format!(
                    "{spec}: the program has no line info; build it with `-g` (cc -g, cargo's dev profile) or break at a function name"
                )));
            };
            return Some(lines.line_addr(file, line).map(|(a, _)| a.wrapping_add(bias)).or_else(fail));
        }
        if !prologue || !is_name(spec) {
            return None;
        }
        let start = match self.mods.resolve(spec) {
            Ok(nexo_bin::Place::Code(x)) => x,
            _ => return None,
        };
        let (lines, bias) = self.mods.main_lines()?;
        lines.after_prologue(start.wrapping_sub(bias)).map(|a| Ok(a.wrapping_add(bias)))
    }
}

/// `paint.c:42` split into the file and the line; a name with a `:` in it (`std::mem`) is not one.
fn file_line(spec: &str) -> Option<(&str, u32)> {
    let (file, line) = spec.rsplit_once(':')?;
    let line: u32 = line.parse().ok().filter(|&l| l > 0)?;
    (!file.is_empty() && !file.ends_with(':') && !file.contains('!')).then_some((file, line))
}

/// A plain function name: `plot`, `_start`, not `plot+4`, `libc!puts` or a number.
fn is_name(spec: &str) -> bool {
    let mut c = spec.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_and_line_is_told_apart_from_a_path_like_name() {
        assert_eq!(file_line("paint.c:42"), Some(("paint.c", 42)));
        assert_eq!(file_line("src/a.rs:7"), Some(("src/a.rs", 7)));
        assert_eq!(file_line("std::mem"), None);
        assert_eq!(file_line("main"), None);
        assert!(
            is_name("plot") && is_name("_start") && !is_name("plot+4") && !is_name("0x40") && !is_name("libc!puts")
        );
    }
}
