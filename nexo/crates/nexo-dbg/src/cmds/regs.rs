//! Registers: the `status` line, `regs` and `setreg`.

use super::*;
use crate::vector::{hex, parse_float, Lanes, VReg, Vector, LANE_NAMES};
use crate::Error;

impl Session {
    pub(super) fn status(&mut self) -> Result<Output> {
        let mut l = vec![format!(
            "pid {} {} | {} | {} breakpoints, {} watches | {} stops so far",
            self.t.pid(),
            if self.t.launched() { "started by the debugger" } else { "attached" },
            match self.state {
                State::Stopped => "stopped".to_string(),
                State::Running => "running".to_string(),
                State::Exited => format!("ended ({})", self.exit.clone().unwrap_or_default()),
            },
            self.bps.len(),
            self.wps.len(),
            self.nstop
        )];
        if self.state == State::Stopped {
            let r = self.t.regs(self.cur)?;
            let name = self.mods.label(r.rip, false).unwrap_or_default();
            l.push(format!("thread {} at {:#x} {name}", self.cur, r.rip));
        }
        if let Some(o) = self.new_output_line() {
            l.push(o);
        }
        Ok(rows(
            "status",
            l,
            json!({"state": match self.state { State::Stopped => "stopped", State::Running => "running", State::Exited => "exited" }, "pid": self.t.pid()}),
        ))
    }

    pub(super) fn cmd_regs(&mut self, a: &Args) -> Result<Output> {
        self.need_stopped()?;
        if let Some(vr) = a.pos.first().and_then(|n| VReg::parse(n)) {
            return self.vector_reg(vr, a.flag("as"));
        }
        let r = self.sel_regs()?;
        if let Some(n) = a.pos.first() {
            if r.is_lost(n) {
                return Ok(rows(
                    "regs",
                    vec![format!(
                        "{n} is not saved in frame #{}: the call out of it may have changed it and no unwind rule gives it back",
                        self.frame
                    )],
                    json!({"frame": self.frame}),
                ));
            }
            let Some(v) = r.get(n) else { return usage(format!("no register named {n:?}")) };
            return Ok(rows("regs", vec![format!("{n} = {v:#x} ({})", v as i64)], json!({})));
        }
        let mut l = Vec::new();
        let named = r.named();
        // a 32-bit thread's registers in 10 characters, as its code sees them
        let (slot, width) = if r.x32 { (4u64, 10) } else { (8, 18) };
        for chunk in named.chunks(4) {
            let cell = |(n, v): &(&str, u64)| {
                if r.is_lost(n) {
                    format!("{n:<7}{:<width$}", "<not saved>")
                } else {
                    format!("{n:<7}{v:#0width$x}")
                }
            };
            l.push(chunk.iter().map(cell).collect::<Vec<_>>().join("  ").trim_end().to_string());
        }
        if r.is_lost("eflags") {
            l.push(format!(
                "<not saved>: {} (the call out of frame #{} may have changed them; no unwind rule gives them back)",
                r.lost_names().join(" "),
                self.frame
            ));
        } else {
            l.push(format!("flags {}", r.flags_text()));
        }
        let sp = r.rsp();
        let mut stack = Vec::new();
        for i in 0..4u64 {
            let mut b = [0u8; 8];
            if self.t.read(sp + i * slot, &mut b[..slot as usize]) == slot as usize {
                stack.push(format!("{:#x}", u64::from_le_bytes(b)));
            }
        }
        if !stack.is_empty() {
            l.push(format!("[{}] {}", if r.x32 { "esp" } else { "rsp" }, stack.join(" ")));
        }
        if a.has("all") {
            l.extend(self.vector_rows(r.x32));
        }
        Ok(rows("regs", l, json!({"thread": self.cur})))
    }

    pub(super) fn cmd_setreg(&mut self, a: &Args) -> Result<Output> {
        let (Some(n), Some(v)) = (a.pos.first(), a.pos.get(1..).filter(|v| !v.is_empty())) else {
            return usage(format!(
                "setreg <register> <value or expression> | setreg xmm0 <lane0> [lane1 ...] [--as {LANE_NAMES}]"
            ));
        };
        if self.frame > 0 {
            return fail(format!(
                "frame #{} is selected: setreg changes the stopped thread's registers, which are frame #0's; `frame 0` selects it",
                self.frame
            ));
        }
        if let Some(vr) = VReg::parse(n) {
            return self.set_vector_reg(vr, v, a.flag("as"));
        }
        let val = self.addr_of(&v.join(" "))?;
        let mut r = self.tid_regs()?;
        if !r.set(n, val) {
            let what = if r.x32 { "eax .. edi, esp, ebp, eip or eflags" } else { "a 64-bit register, rip or eflags" };
            return usage(format!("{n:?} cannot be set: use {what}"));
        }
        self.t.set_regs(self.cur, &r)?;
        Ok(rows("setreg", vec![format!("{n} = {val:#x}")], json!({})))
    }

    /// The stopped thread's vector registers; `what` says what wanted them, for the refusal in an outer frame.
    fn frame0_vector(&mut self, what: &str) -> Result<Vector> {
        if self.frame > 0 {
            return fail(format!(
                "frame #{} is selected: {what} the stopped thread's vector registers, which are frame #0's (no unwind rule gives a caller's back); `frame 0` selects it",
                self.frame
            ));
        }
        let cur = self.cur;
        match self.vector(cur) {
            Some(v) => Ok(v),
            None => fail(format!("thread {cur}'s vector registers cannot be read: this target (a core file, a minidump or a remote stub) does not give them")),
        }
    }

    /// `regs xmm0|ymm0|st0 [--as LANES]`: the register in hex and by lanes (f64 and f32 unless `--as` names one).
    fn vector_reg(&mut self, vr: VReg, lanes: Option<&str>) -> Result<Output> {
        self.need_stopped()?;
        let lanes = match lanes {
            None => vec![Lanes::F64, Lanes::F32],
            Some(s) => vec![Lanes::parse(s).ok_or_else(|| Error::Usage(format!("--as {s:?}: one of {LANE_NAMES}")))?],
        };
        let v = self.frame0_vector("`regs` shows")?;
        let (name, bytes) = match vr {
            VReg::Xmm(i) => (format!("xmm{i}"), v.xmm(i).to_vec()),
            VReg::Ymm(i) => match v.ymm(i) {
                Some(b) => (format!("ymm{i}"), b.to_vec()),
                None => {
                    let why = if self.t.os() == Os::Windows {
                        "this debugger reads no upper ymm halves on Windows"
                    } else {
                        "the system gave no upper ymm halves (no AVX state)"
                    };
                    return Ok(rows("regs", vec![format!("ymm{i}: {why}; its lower half: `regs xmm{i}`")], json!({})));
                }
            },
            VReg::St(i) => {
                let x = v.st(i);
                let t = x.map_or("empty (no value on the x87 stack there)".into(), crate::vector::float_text);
                return Ok(rows("regs", vec![format!("st{i} = {t}")], json!({"value": x})));
            }
        };
        let mut l = vec![format!("{name} = {}", hex(&bytes))];
        for x in lanes {
            l.push(format!("  {}x{} {}", x.name(), bytes.len() / x.size(), x.show(&bytes)));
        }
        Ok(rows("regs", l, json!({"hex": hex(&bytes)})))
    }

    /// `regs --all`'s vector rows: xmm0-15 (xmm0-7 in a 32-bit thread) in hex and as doubles, the x87 values.
    fn vector_rows(&mut self, x32: bool) -> Vec<String> {
        let v = match self.frame0_vector("`regs --all` shows") {
            Ok(v) => v,
            Err(e) => return vec![e.to_string()],
        };
        let mut l = Vec::new();
        for i in 0..if x32 { 8 } else { 16 } {
            let b = v.xmm(i);
            l.push(format!("{:<6} {} f64x2 {}", format!("xmm{i}"), hex(&b), Lanes::F64.show(&b)));
        }
        let st: Vec<String> =
            (0..8).filter_map(|i| v.st(i).map(|x| format!("st{i}={}", crate::vector::float_text(x)))).collect();
        l.push(if st.is_empty() { "x87 stack empty".into() } else { format!("x87 {}", st.join(" ")) });
        if v.has_ymm() {
            l.push("ymm0-15: `regs ymm0`; lanes: `regs xmm0 --as u32x4`".into());
        }
        l
    }

    /// `setreg xmm0 <lane0> [lane1 ...] [--as LANES]` (doubles unless `--as` names the lanes; the lanes not given keep
    /// their value), `setreg ymm0 ...` the same over 32 bytes, `setreg st0 <float>`.
    fn set_vector_reg(&mut self, vr: VReg, words: &[String], lanes: Option<&str>) -> Result<Output> {
        self.need_stopped()?;
        let ln = match lanes {
            None => Lanes::F64,
            Some(s) => Lanes::parse(s).ok_or_else(|| Error::Usage(format!("--as {s:?}: one of {LANE_NAMES}")))?,
        };
        let mut v = self.frame0_vector("setreg changes")?;
        let shown = match vr {
            VReg::St(0) => {
                let w = words.join(" ");
                let Some(x) = parse_float(&w) else {
                    return usage(format!("st0 takes a float such as 2.5, not {w:?}"));
                };
                if !v.set_st0(x) {
                    return fail("st0 holds no value (the x87 stack is empty there), so there is none to replace");
                }
                format!("st0 = {}", crate::vector::float_text(x))
            }
            VReg::St(i) => return usage(format!("st{i} cannot be set: only st0, the top of the x87 stack, can")),
            VReg::Xmm(i) | VReg::Ymm(i) => {
                let ymm = matches!(vr, VReg::Ymm(_));
                let name = format!("{}{i}", if ymm { "ymm" } else { "xmm" });
                let mut b = if ymm {
                    match v.ymm(i) {
                        Some(b) => b.to_vec(),
                        None => return fail(format!("{name} cannot be set: the upper ymm halves are not read here; `setreg xmm{i}` sets its lower half")),
                    }
                } else {
                    v.xmm(i).to_vec()
                };
                let n = b.len() / ln.size();
                if words.len() > n {
                    return usage(format!("{name} holds {n} {} lanes; {} values were given", ln.name(), words.len()));
                }
                for (k, w) in words.iter().enumerate() {
                    let Some(x) = ln.value(w) else {
                        return usage(format!("{w:?} is not a {} value", ln.name()));
                    };
                    b[k * ln.size()..(k + 1) * ln.size()].copy_from_slice(&x);
                }
                if ymm {
                    v.set_ymm(i, b.clone().try_into().unwrap_or_default());
                } else {
                    v.set_xmm(i, b.clone().try_into().unwrap_or_default());
                }
                format!("{name} = {} {}x{n} {}", hex(&b), ln.name(), ln.show(&b))
            }
        };
        self.t.set_vector_state(self.cur, v.bytes())?;
        Ok(rows("setreg", vec![shown], json!({})))
    }
}
