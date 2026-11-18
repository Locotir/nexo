//! `list`: the source around the stop or a given place, with the current line marked.

use super::*;
use crate::lines::short_path;

/// How many lines `list` shows when no count is given.
const LIST_LINES: u32 = 10;
/// The most one `list` shows.
const LIST_MAX: u32 = 500;

/// The first and last line (1-based, both shown) of a window of `n` lines around `center` in a file of `total`
/// lines. The window slides back at the start of the file and up at its end, so it still holds `n` lines when
/// the file has that many. `None` when `center` is not a line of the file.
fn window(center: u32, n: u32, total: u32) -> Option<(u32, u32)> {
    if center == 0 || center > total {
        return None;
    }
    let n = n.clamp(1, total);
    let first = center.saturating_sub(n / 2).max(1);
    let last = first.saturating_add(n - 1).min(total);
    Some(((last + 1).saturating_sub(n).max(1), last))
}

/// The lines of `text` around `center`, numbered, with `=>` in front of the `current` one. Returns the first
/// and last line shown and the rows; `None` when `center` is past the end of the file.
fn render(text: &str, center: u32, n: u32, current: Option<u32>) -> Option<(u32, u32, Vec<String>)> {
    let all: Vec<&str> = text.lines().collect();
    let (first, last) = window(center, n, all.len() as u32)?;
    let width = last.to_string().len();
    let rows = (first..=last)
        .map(|i| {
            let mark = if current == Some(i) { "=>" } else { "  " };
            format!("{mark} {i:>width$}  {}", all[i as usize - 1].trim_end())
        })
        .collect();
    Some((first, last, rows))
}

impl Session {
    /// The source line (file and number) of the code at `addr`, the innermost one when it was inlined.
    fn source_line_at(&mut self, addr: u64) -> Option<(String, u32)> {
        let (lines, bias) = self.mods.main_lines()?;
        let w = lines.at(addr.wrapping_sub(bias))?;
        Some(w.inside.unwrap_or((w.file, w.line)))
    }

    /// The line the selected frame is at: its pc, or the call (one byte before the return address) in a caller.
    fn stop_line(&mut self) -> Option<(String, u32)> {
        if self.state != State::Stopped {
            return None;
        }
        let (r, _) = self.sel_frame().ok()?;
        let pc = if self.frame > 0 { r.rip.wrapping_sub(1) } else { r.rip };
        self.source_line_at(pc)
    }

    pub(super) fn cmd_list(&mut self, a: &Args) -> Result<Output> {
        const FORMS: &str = "list [file:line|function|address] [N]";
        let mut pos: Vec<&str> = a.pos.iter().map(String::as_str).collect();
        let count = if pos.len() > 1 {
            let Some(n) = pos.pop().and_then(|w| w.parse::<u32>().ok()).filter(|&n| n > 0) else {
                return usage(format!("the last word is how many lines to show, a number above 0: {FORMS}"));
            };
            n.min(LIST_MAX)
        } else {
            LIST_LINES
        };
        if pos.len() > 1 {
            return usage(FORMS);
        }
        let here = self.stop_line();
        let (file, line) = match pos.first() {
            None => match here.clone() {
                Some(h) => h,
                None => {
                    return fail(
                        "no source line to list around (the program is not stopped in code with line info): `list file:line` or `list function`",
                    )
                }
            },
            // a bare number is a line of the file the stop is in
            Some(spec) if spec.parse::<u32>().is_ok() => match here.clone() {
                Some((f, _)) => (f, spec.parse::<u32>().unwrap_or(1)),
                None => return fail(format!("{spec} is a line of the file the stop is in, and there is no stop in a source file: `list file:{spec}`")),
            },
            Some(spec) => self.list_place(spec)?,
        };
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) => return fail(format!("{file}: the source is not on this machine ({e})")),
        };
        let current = here.filter(|(f, _)| *f == file).map(|(_, l)| l);
        let Some((first, last, mut l)) = render(&text, line, count, current) else {
            return fail(format!(
                "{} has {} lines: line {line} is past its end",
                short_path(&file),
                text.lines().count()
            ));
        };
        l.insert(0, format!("{} lines {first}-{last}", short_path(&file)));
        Ok(rows("list", l, json!({"file": file, "first": first, "last": last, "current": current})))
    }

    /// The file and line `list` means by `spec`: `file:line`, or the line of the code at a function or address.
    fn list_place(&mut self, spec: &str) -> Result<(String, u32)> {
        if let Some((f, l)) = crate::places::file_line(spec) {
            let Some((lines, _)) = self.mods.main_lines() else {
                return fail(format!("{spec}: the program has no line info; build it with `-g`"));
            };
            let (addr, _) = lines.line_addr(f, l).or_else(fail)?;
            let w = lines.at(addr).ok_or_else(|| crate::Error::Fail(format!("{spec}: no code there")))?;
            let file = w.inside.map_or(w.file, |(f, _)| f);
            return Ok((file, l));
        }
        let addr = self.addr_of(spec)?;
        match self.source_line_at(addr) {
            Some(p) => Ok(p),
            None => fail(format!("{spec} ({addr:#x}) has no source line: the program has no line info there")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_slides_at_the_start_and_the_end_of_the_file() {
        assert_eq!(window(50, 10, 100), Some((45, 54)));
        assert_eq!(window(1, 10, 100), Some((1, 10)));
        assert_eq!(window(3, 10, 100), Some((1, 10)));
        assert_eq!(window(100, 10, 100), Some((91, 100)));
        assert_eq!(window(98, 10, 100), Some((91, 100)));
        // a file shorter than the window is shown whole
        assert_eq!(window(2, 10, 4), Some((1, 4)));
        assert_eq!(window(4, 1, 4), Some((4, 4)));
        // a line that is not in the file
        assert_eq!(window(0, 10, 4), None);
        assert_eq!(window(5, 10, 4), None);
        assert_eq!(window(1, 10, 0), None);
    }

    #[test]
    fn the_current_line_is_marked_and_numbers_line_up() {
        let text: String = (1..=12).map(|i| format!("line {i}\n")).collect();
        let (first, last, rows) = render(&text, 12, 4, Some(12)).unwrap();
        assert_eq!((first, last), (9, 12));
        assert_eq!(rows[0], "    9  line 9");
        assert_eq!(rows[3], "=> 12  line 12");
        assert!(render(&text, 13, 4, None).is_none());
    }
}
