//! `return` and `jump`: force a code path in the stopped program. `return [VALUE]` pops the stopped function's frame
//! with the unwinder and puts VALUE where its caller reads a result; `jump PLACE` moves the pc and goes on.

use super::*;
use crate::vars::RetSlot;
use crate::vector::Lanes;

/// What `return VALUE` puts in the result register.
enum Given {
    Int(u64),
    /// The bytes of a `float` (4) or `double` (8), for the low lane of xmm0.
    Float(Vec<u8>),
}

/// Whether a jump to `to` stays inside the function the program is in, given as `span` (memory addresses, `None`
/// when the debug info does not place the pc in a function). The text says why not.
fn within(span: Option<(u64, u64)>, to: u64) -> std::result::Result<(), String> {
    match span {
        Some((lo, hi)) if to >= lo && to < hi => Ok(()),
        Some((lo, hi)) => Err(format!(
            "{to:#x} is outside the function the program is in ({lo:#x}..{hi:#x}): its stack frame would not fit there; `--force` jumps anyway"
        )),
        None => Err(format!(
            "no debug info places the pc in a function, so {to:#x} cannot be checked against it; `--force` jumps anyway"
        )),
    }
}

impl Session {
    pub(super) fn cmd_return(&mut self, a: &Args) -> Result<Output> {
        self.need_stopped()?;
        if self.frame > 0 {
            return fail(format!(
                "frame #{} is selected: return pops the stopped function, which is frame #0; `frame 0` selects it",
                self.frame
            ));
        }
        let frames = self.frames(2)?;
        let (Some(&callee), Some(&caller)) = (frames.first(), frames.get(1)) else {
            return fail("cannot find the caller of this frame (no unwind data): `jump <place>` or `setreg rip <addr>` move the pc");
        };
        let mut r = self.tid_regs()?;
        // everything that can fail is checked before the first write
        let given = if a.pos.is_empty() { None } else { Some(self.return_value(&a.pos.join(" "), r.x32)?) };
        let mut state = None;
        if let Some(Given::Float(b)) = &given {
            let Some(mut v) = self.vector(self.cur) else {
                return fail("this target gives no vector registers, so a float result cannot be set");
            };
            let mut x = v.xmm(0);
            x[..b.len()].copy_from_slice(b);
            v.set_xmm(0, x);
            state = Some(v.bytes().to_vec());
        }
        // the caller's registers as the unwinder gives them; the ones it has no value for keep the stopped values
        for (i, &g) in caller.gp.iter().enumerate() {
            if (caller.lost >> i) & 1 == 0 {
                r.gp[i] = g;
            }
        }
        r.gp[crate::regs::RSP] = caller.sp;
        r.rip = caller.pc;
        // a thread stopped inside a system call must not have that call restarted at the new pc
        r.orig_rax = u64::MAX;
        let shown = match &given {
            Some(Given::Int(n)) => {
                r.gp[0] = *n;
                Some(format!("{} = {n:#x}", if r.x32 { "eax" } else { "rax" }))
            }
            Some(Given::Float(b)) => {
                let x = if b.len() == 4 {
                    f32::from_le_bytes(b[..4].try_into().unwrap_or_default()) as f64
                } else {
                    f64::from_le_bytes(b[..8].try_into().unwrap_or_default())
                };
                Some(format!("xmm0 = {}", crate::vector::float_text(x)))
            }
            None => None,
        };
        self.t.set_regs(self.cur, &r)?;
        if let Some(bytes) = state {
            self.t.set_vector_state(self.cur, &bytes)?;
        }
        let name = self.mods.label(callee.pc, false).unwrap_or_default();
        let mut l = vec![format!("returned from {name}")];
        if let Some(s) = shown {
            l.push(s);
        }
        let now = self.frames(1)?;
        if let Some(fr) = now.first() {
            l.push(self.frame_line(0, fr));
        }
        l.push("stopped in the caller: `cont` runs on from there".to_string());
        Ok(rows("return", l, json!({"pc": caller.pc, "sp": caller.sp})))
    }

    /// `VALUE` for the register the function's return type uses: a number or expression for an integer, pointer or
    /// enum (rax), a float for `float` and `double` (xmm0).
    fn return_value(&mut self, text: &str, x32: bool) -> Result<Given> {
        match self.return_slot()? {
            RetSlot::No(why) => fail(format!("return {text}: {why}")),
            RetSlot::Float(size) => {
                let lane = if size == 4 { Lanes::F32 } else { Lanes::F64 };
                match lane.value(text.trim()) {
                    Some(b) => Ok(Given::Float(b)),
                    None => usage(format!("{text:?} is not a {} value: the function returns a float", lane.name())),
                }
            }
            RetSlot::Int => {
                let t = text.trim();
                let n = if let Ok(i) = t.parse::<i64>() {
                    i as u64
                } else if let Ok(u) = parse_addr(t) {
                    u
                } else {
                    self.eval_frame(t).map_err(|(e, _)| e)?
                };
                Ok(Given::Int(if x32 { n & 0xffff_ffff } else { n }))
            }
        }
    }

    pub(super) fn cmd_jump(&mut self, a: &Args) -> Result<Output> {
        const FORMS: &str = "jump <file:line|address|function> [--force] [--stay] [--wait N]";
        let [spec] = a.pos.as_slice() else { return usage(FORMS) };
        self.need_stopped()?;
        if self.frame > 0 {
            return fail(format!(
                "frame #{} is selected: jump moves the stopped thread's pc, which is frame #0's; `frame 0` selects it",
                self.frame
            ));
        }
        let to = self.jump_target(spec)?;
        let mut r = self.tid_regs()?;
        if !a.has("force") {
            let span = self.current_span(r.rip);
            if let Err(why) = within(span, to) {
                return fail(why);
            }
        }
        r.rip = to;
        // a thread stopped inside a system call must not have that call restarted at the new pc
        r.orig_rax = u64::MAX;
        self.t.set_regs(self.cur, &r)?;
        let now = self.frames(1)?;
        let at = match now.first() {
            Some(fr) => self.frame_line(0, fr),
            None => format!("{to:#x}"),
        };
        if a.has("stay") {
            return Ok(rows(
                "jump",
                vec![format!("pc moved to {at}"), "stopped there: `cont` runs from it".to_string()],
                json!({"pc": to}),
            ));
        }
        let w = a.wait()?;
        let mut out = self.go(Run::default(), w)?;
        out.rows.insert(0, nexo_bin::Row { key: 0, text: format!("jumped to {at}"), json: None });
        Ok(out)
    }

    /// The code address `jump` means: a place as `break` reads it (`file:line`, a function past its prologue,
    /// `*addr` exactly, an address).
    fn jump_target(&mut self, spec: &str) -> Result<u64> {
        let exact = spec.starts_with('*');
        let spec = spec.trim_start_matches('*');
        if let Some(r) = self.source_place(spec, !exact) {
            return r;
        }
        match self.resolve_place(spec)? {
            Place::Code(x) => Ok(x),
            Place::Data(x) if self.t.maps().is_ok_and(|m| m.iter().any(|m| m.exec && m.start <= x && x < m.end)) => {
                Ok(x)
            }
            _ => fail(format!("{spec} is not code: jump takes a line, a function or an address in executable memory")),
        }
    }

    /// The function the pc is in, from the debug info, as memory addresses.
    fn current_span(&mut self, pc: u64) -> Option<(u64, u64)> {
        let (vars, bias) = self.mods.main_vars()?;
        let (_, lo, hi) = vars.func_span(pc.wrapping_sub(bias))?;
        Some((lo.wrapping_add(bias), hi.wrapping_add(bias)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_jump_out_of_the_function_is_refused_unless_forced() {
        let span = Some((0x1000, 0x1040));
        assert!(within(span, 0x1000).is_ok());
        assert!(within(span, 0x103f).is_ok());
        let e = within(span, 0x1040).unwrap_err();
        assert!(e.contains("outside the function") && e.contains("--force"), "{e}");
        assert!(within(span, 0xfff).is_err());
        let e = within(None, 0x1000).unwrap_err();
        assert!(e.contains("no debug info") && e.contains("--force"), "{e}");
    }
}
