//! Frame selection: `frame [N]`, `up [N]`, `down [N]` and `info frame`. The selected frame is the one `regs`, `locals`,
//! `args`, `print`, `mem` and `dis` read. A caller's registers come from the unwind tables (`Mods::backtrace`):
//! those the call may have changed and no rule gives back are said to be not saved in that frame.

use super::*;
use crate::modules::Frame;

/// Frames searched for `frame function <name>`.
const FIND_MAX: usize = 200;

/// The commands whose answer depends on the selected frame, so it is named at their top.
pub(super) fn reads_frame(words: &[String]) -> bool {
    let Some(first) = words.first() else { return false };
    match crate::gdbdialect::base_name(first) {
        "regs" | "args" | "locals" | "print" | "p" | "output" | "printf" | "x" | "mem" | "dis" | "disasm" | "ptype"
        | "whatis" => true,
        "info" => words.get(1).is_some_and(|w| {
            matches!(w.as_str(), "registers" | "reg" | "regs" | "r" | "all-registers" | "locals" | "args")
        }),
        _ => false,
    }
}

fn level(text: &str) -> Result<usize> {
    text.trim_start_matches('#').parse::<usize>().map_err(|_| {
        crate::Error::Usage(format!("{text:?} is not a frame number: `bt` numbers the frames #0, #1, ..."))
    })
}

impl Session {
    /// The registers of the selected frame: the stopped thread's own in frame #0; in a caller's frame the unwound
    /// ones, with `lost` naming those it has no value for.
    pub(crate) fn sel_regs(&mut self) -> Result<crate::Regs> {
        if self.frame == 0 {
            return self.t.regs(self.cur);
        }
        Ok(self.sel_frame()?.0)
    }

    /// The selected frame's registers (as [`Session::sel_regs`]) and its CFA, where the caller's stack pointer is
    /// after the return.
    pub(crate) fn sel_frame(&mut self) -> Result<(crate::Regs, Option<u64>)> {
        let mut r = self.tid_regs()?;
        let f = self.frames(self.frame + 1)?;
        let Some(fr) = f.get(self.frame) else {
            // A write to the stack can shorten the chain under a selected caller: the stop's own frame is what is left.
            self.frame = 0;
            return Ok((r, f.first().and_then(|x| x.cfa)));
        };
        if self.frame > 0 {
            (r.gp, r.rip, r.lost) = (fr.gp, fr.pc, fr.lost);
        }
        Ok((r, fr.cfa))
    }

    /// `#1  0x401196 middle+0x2e  (frames.c:12)`: a frame as `bt` shows it. A caller is named by its call, one byte
    /// before the return address, which can be the last byte of a function or a line of its own.
    pub(super) fn frame_line(&mut self, i: usize, fr: &Frame) -> String {
        let look = if i == 0 { fr.pc } else { fr.pc.wrapping_sub(1) };
        let name = self.mods.label(look, false).unwrap_or_default();
        let at = self.mods.source(look).map(|s| format!("  ({s})")).unwrap_or_default();
        format!("#{i:<2} {:#x} {name}{at}", fr.pc)
    }

    /// The line that names the selected frame at the top of an answer read in it; `None` in frame #0.
    pub(super) fn frame_head(&mut self) -> Option<String> {
        if self.frame == 0 || self.state != State::Stopped {
            return None;
        }
        let n = self.frame;
        let f = self.frames(n + 1).ok()?;
        let fr = f.get(n)?;
        Some(format!("in frame {}", self.frame_line(n, fr)))
    }

    /// `the registers rax rcx ... eflags are not saved in this frame (...)`, or `None` when it has them all.
    fn lost_line(r: &crate::Regs) -> Option<String> {
        let names = r.lost_names();
        (!names.is_empty()).then(|| {
            format!(
                "not saved in this frame: {} (the call out of it may have changed them; no unwind rule gives them back)",
                names.join(" ")
            )
        })
    }

    /// `frame [N]`, `frame level N`, `frame function <name>`: select a frame; with no number, describe the selected one.
    pub(super) fn cmd_frame(&mut self, a: &Args) -> Result<Output> {
        self.need_stopped()?;
        let want = match a.pos.as_slice() {
            [] => return self.cmd_frame_info(),
            [l, n] if l == "level" => level(n)?,
            [f, name] if f == "function" => self.frame_of(name)?,
            [n] => level(n)?,
            _ => return usage("frame [N] | frame level N | frame function <name>"),
        };
        self.select(want)
    }

    /// `up [N]` selects the caller (N frames out), `down [N]` the frame it called.
    pub(super) fn cmd_up(&mut self, a: &Args, up: bool) -> Result<Output> {
        self.need_stopped()?;
        let n = match a.pos.first() {
            Some(x) => level(x)?,
            None => 1,
        };
        let want = if up {
            self.frame.saturating_add(n)
        } else if n > self.frame {
            return fail(format!(
                "frame #{} is selected and frame #0 is where the thread stopped: `down {}` is the furthest down",
                self.frame, self.frame
            ));
        } else {
            self.frame - n
        };
        self.select(want)
    }

    /// The first frame (from #0 out) whose function is `name`.
    fn frame_of(&mut self, name: &str) -> Result<usize> {
        let f = self.frames(FIND_MAX)?;
        for (i, fr) in f.iter().enumerate() {
            let look = if i == 0 { fr.pc } else { fr.pc.wrapping_sub(1) };
            let l = self.mods.label(look, false).unwrap_or_default();
            let func = l.split('+').next().unwrap_or("");
            if func == name || func.rsplit('!').next() == Some(name) {
                return Ok(i);
            }
        }
        fail(format!("none of the {} frames in the chain is in {name}: `bt` lists them", f.len()))
    }

    fn select(&mut self, want: usize) -> Result<Output> {
        let f = self.frames(want + 1)?;
        let Some(fr) = f.get(want).copied() else {
            return fail(format!(
                "there is no frame #{want}: the chain has {} (#0 to #{}), and frame #{} stays selected; `bt` lists them",
                f.len(),
                f.len().saturating_sub(1),
                self.frame
            ));
        };
        self.frame = want;
        let mut l = vec![self.frame_line(want, &fr)];
        let look = if want == 0 { fr.pc } else { fr.pc.wrapping_sub(1) };
        if let Some(t) = self.mods.source_text(look) {
            l.push(format!("   {t}"));
        }
        l.extend(self.locals_line());
        let r = self.sel_regs()?;
        l.extend(Self::lost_line(&r));
        Ok(rows("frame", l, json!({"frame": want, "pc": fr.pc, "sp": fr.sp})))
    }

    /// `info frame` (and `frame` alone): where the selected frame is, how it was found, its caller and callee, and
    /// the registers it has no value for.
    pub(super) fn cmd_frame_info(&mut self) -> Result<Output> {
        self.need_stopped()?;
        let n = self.frame;
        let f = self.frames(n + 2)?;
        let Some(fr) = f.get(n).copied() else { return fail("no frame is known here: `bt` shows the chain") };
        let mut l = vec![format!("frame {}", self.frame_line(n, &fr))];
        let cfa = fr.cfa.map_or("not known".to_string(), |c| format!("{c:#x}"));
        l.push(format!("rip {:#x}, rsp {:#x}, cfa {cfa} (the caller's rsp after the return)", fr.pc, fr.sp));
        l.push(match n {
            0 => format!("the registers are thread {}'s, where it stopped", self.cur),
            _ => format!("found from frame #{} by {}", n - 1, fr.how),
        });
        if n > 0 {
            let callee = f[n - 1];
            let look = if n == 1 { callee.pc } else { callee.pc.wrapping_sub(1) };
            let name = self.mods.label(look, false).unwrap_or_else(|| format!("{:#x}", callee.pc));
            l.push(format!("called frame #{}: {name}", n - 1));
        }
        match f.get(n + 1) {
            Some(c) => {
                let name = self.mods.label(c.pc.wrapping_sub(1), false).unwrap_or_else(|| format!("{:#x}", c.pc));
                l.push(format!("called by frame #{}: {name}, which goes on at {:#x}", n + 1, c.pc));
            }
            None => l.push("the outermost frame found: no caller is known".into()),
        }
        let r = self.sel_regs()?;
        l.extend(Self::lost_line(&r));
        Ok(rows("frame", l, json!({"frame": n, "pc": fr.pc, "sp": fr.sp, "cfa": fr.cfa})))
    }
}
