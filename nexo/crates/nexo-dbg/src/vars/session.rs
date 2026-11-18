//! The `locals`, `print` and stop-report side of a session.

use super::path::{FACT, NOTES};
use super::*;

impl Session {
    /// The registers, frame and memory reader a location needs, or `None` when the program has no variables.
    /// With `prologue`, a stop before the function's prologue ends is marked so its stack slots are not read.
    /// In a caller's frame (`up`) they are that frame's, and its pc is the call (one byte before the return
    /// address), which is where its scope and line are.
    fn with_vars<T>(&mut self, prologue: bool, f: impl FnOnce(&Vars, &mut Regs) -> T) -> Result<Option<T>> {
        self.need_stopped()?;
        let (r, cfa) = if self.frame > 0 { self.sel_frame()? } else { (self.t.regs(self.cur)?, None) };
        let caller = self.frame > 0;
        let early = if prologue && !caller { self.early(r.rip) } else { None };
        let Session { t, mods, .. } = self;
        let cfa = if caller {
            cfa
        } else {
            let mut rd8 = |a: u64| {
                let mut b = [0u8; 8];
                (t.read(a, &mut b) == 8).then(|| u64::from_le_bytes(b))
            };
            mods.backtrace(&r, &mut rd8, 1).first().and_then(|f| f.cfa)
        };
        let Some((vars, lines, bias)) = mods.main_vars_lines() else { return Ok(None) };
        let mut rd = |a: u64, n: usize| {
            let mut b = vec![0u8; n];
            (t.read(a, &mut b) == n).then_some(b)
        };
        let rip = if caller { r.rip.wrapping_sub(1) } else { r.rip };
        let mut regs = Regs { gp: r.gp, rip, cfa, bias, rd: &mut rd, early, lines, lost: r.lost };
        Ok(Some(f(vars, &mut regs)))
    }

    /// `stopped at store+0x0, the prologue ends at store+0x10` when `rip` is in a function of the program before
    /// its prologue ends (from the line table); without lines for it, only its first instruction is known to be.
    fn early(&mut self, rip: u64) -> Option<String> {
        let (vars, bias) = self.mods.main_vars()?;
        let pc = rip.wrapping_sub(bias);
        let (name, lo, hi) = vars.func_span(pc)?;
        let name = name.to_string();
        let end = self.mods.main_lines().and_then(|(l, _)| l.after_prologue(lo, hi));
        let at = format!("stopped at {name}+{:#x}", pc.wrapping_sub(lo));
        match end {
            Some(e) if pc >= lo && pc < e => Some(format!("{at}, the prologue ends at {name}+{:#x}", e - lo)),
            None if pc == lo => Some(format!("{at}, its first instruction; no line info says where the prologue ends")),
            _ => None,
        }
    }

    fn win(&self) -> bool {
        matches!(self.t.os(), crate::platform::Os::Windows)
    }

    /// `locals`: the parameters and locals of the function the program stopped in, with types and values.
    pub(crate) fn cmd_locals(&mut self) -> Result<Output> {
        let win = self.win();
        let got = self.with_vars(true, |vars, regs| {
            let pc = regs.rip.wrapping_sub(regs.bias);
            let f = vars.func_at(pc).map(|f| f.name.clone());
            let vs = vars.in_scope(pc);
            let early = regs.early.clone().filter(|_| vs.iter().any(|(v, _, _)| Vars::in_slot(v)));
            let inreg = if early.is_some() { vars.params_in_regs(pc, win, regs, 0) } else { Vec::new() };
            let mut l = Vec::new();
            // the locals of blocks that do not hold the stop come last, saying so
            for (v, enc, fb) in vs.into_iter().chain(vars.out_of_block(pc)) {
                let ty = View(v.ty, 0);
                let at = vars.value(v, enc, fb, regs);
                let val = vars.shown_noted(v, at, regs, 0, &inreg);
                let kind = if v.param { "arg " } else { "" };
                l.push(format!("{kind}{} ({}) = {val}", v.name, vars.name(ty)));
            }
            (f, l, early)
        })?;
        match got {
            None => fail("the program has no variable info: build it with `-g` (cc -g, cargo's dev profile); `args` shows the argument registers"),
            Some((None, ..)) if self.frame > 0 => fail(format!("frame #{} is outside the program's own functions (a library): `up` selects its caller, `bt` lists the chain", self.frame)),
            Some((None, ..)) => fail("the stop is outside the program's own functions (a library): `up` selects the caller's frame, `finish` runs back to it, `bt` lists the chain"),
            Some((Some(f), l, _)) if l.is_empty() => Ok(rows("locals", vec![format!("{f} has no parameters or locals")], json!({}))),
            Some((Some(f), l, early)) => {
                let head = match early {
                    Some(at) => format!("in {f} ({at}: its stack slots are not written yet):"),
                    None => format!("in {f}:"),
                };
                let mut out = vec![head];
                out.extend(l);
                Ok(rows("locals", out, json!({"function": f})))
            }
        }
    }

    /// `print` of a source expression; `None` hands it to the register expressions.
    /// `raw` shows the container types as their raw layout (`print x --as raw`).
    pub(crate) fn print_var(&mut self, src: &str, raw: bool) -> Option<Result<Output>> {
        let got = self
            .with_vars(true, |vars, regs| {
                let (v, amp) = match vars.path(src, regs)? {
                    Ok(x) => x,
                    Err(e) => return Some(Err(e)),
                };
                let notes = NOTES.with(|n| std::mem::take(&mut *n.borrow_mut()));
                // why the variable the expression starts from does not hold its own value yet, as `locals` says
                let fact = match FACT.with(|f| f.borrow_mut().take()) {
                    Some((head, why)) if head == src.trim() => format!(" ({why})"),
                    Some((head, why)) => format!(" ({head}: {why})"),
                    None => String::new(),
                };
                let ty = vars.name(v.ty);
                let mut l = vec![if amp {
                    match v.at {
                        At::Mem(a) => format!("{src} ({ty} *) = {a:#x}"),
                        At::Bytes(_) => return Some(Err(format!("{} is in a register: it has no address", &src[1..]))),
                    }
                } else {
                    let at = match &v.at {
                        At::Mem(a) => format!(" at {a:#x}"),
                        At::Bytes(_) => String::new(),
                    };
                    format!("{src} ({ty}{at}) = {}{fact}", views::raw_scope(raw, || vars.render(&v, regs, 0)))
                }];
                l.extend(notes);
                Some(Ok(l))
            })
            .ok()???;
        Some(match got {
            Ok(l) => Ok(rows("print", l, json!({}))),
            Err(e) => fail(e),
        })
    }

    /// The type of a source expression (`ptype node->next`) as the program's variable info names it; `None` when the
    /// program has no variable info or the expression is not one of its variables.
    pub(crate) fn var_type_name(&mut self, src: &str) -> Option<String> {
        // a type does not depend on the values, so a stop before the prologue ends does not hide it
        self.with_vars(false, |vars, regs| match vars.path(src, regs)? {
            Ok((v, amp)) => Some(format!("{}{}", vars.name(v.ty), if amp { " *" } else { "" })),
            Err(_) => None,
        })
        .ok()
        .flatten()
        .flatten()
    }

    /// One line of the locals for a stop report, short; `None` without variable info. Before the prologue ends
    /// the stack slots are not read: the parameters are shown from the argument registers.
    pub(crate) fn locals_line(&mut self) -> Option<String> {
        let win = self.win();
        self.with_vars(true, |vars, regs| {
            let pc = regs.rip.wrapping_sub(regs.bias);
            let vs = vars.in_scope(pc);
            if vs.is_empty() && vars.out_of_block(pc).is_empty() {
                return None;
            }
            let short = |val: String| {
                let mut val: String = val.chars().take(60).collect();
                if val.len() >= 60 {
                    val.push_str("...");
                }
                val
            };
            let mut s = match regs.early.clone().filter(|_| vs.iter().any(|(v, _, _)| Vars::in_slot(v))) {
                Some(at) => {
                    let mut s = format!("locals: not stored yet ({at})");
                    let ps: Vec<String> = vars
                        .params_in_regs(pc, win, regs, 1)
                        .into_iter()
                        .map(|(v, got)| match got {
                            Ok((r, val)) if r == super::entry::ON_STACK => format!("{} = {}", v.name, short(val)),
                            Ok((r, val)) => format!("{} ({r}) = {}", v.name, short(val)),
                            Err(why) => format!("{} = not shown ({why})", v.name),
                        })
                        .collect();
                    if !ps.is_empty() {
                        let place = if vars.pushed_params(pc) { "on the stack" } else { "in registers" };
                        s.push_str(&format!("; parameters {place}: {}", ps.join(", ")));
                    }
                    // what is not in a stack slot (a static, a register) is real already
                    let rest: Vec<String> = vs
                        .into_iter()
                        .filter(|(v, _, _)| !v.param && !Vars::in_slot(v))
                        .map(|(v, enc, fb)| {
                            let at = vars.value(v, enc, fb, regs);
                            format!("{} = {}", v.name, short(vars.shown(v, at, regs, 1, &[])))
                        })
                        .collect();
                    if !rest.is_empty() {
                        s.push_str(&format!("; {}", rest.join(", ")));
                    }
                    s
                }
                None => {
                    let mut parts = Vec::new();
                    for (v, enc, fb) in vs.into_iter().chain(vars.out_of_block(pc)) {
                        let at = vars.value(v, enc, fb, regs);
                        let val = short(vars.shown(v, at, regs, 1, &[]));
                        // the fact is not cut with the value
                        match vars.unassigned(v, regs) {
                            Some(why) => parts.push(format!("{} = {val} ({why})", v.name)),
                            None => parts.push(format!("{} = {val}", v.name)),
                        }
                    }
                    format!("locals: {}", parts.join(", "))
                }
            };
            if s.len() > 300 {
                let cut = (0..=300).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);
                s.truncate(cut);
                s.push_str(" ... (`locals` for all)");
            }
            Some(s)
        })
        .ok()
        .flatten()
        .flatten()
    }

    /// The line a `finish` stop adds: what the function whose pc was `from` returned, read from the return registers
    /// of thread `tid` (`r`) as its DWARF return type says. A function without debug info gets the raw registers; a
    /// `void` one gets `None`.
    pub(crate) fn returned_line(&mut self, from: u64, r: &crate::Regs, tid: crate::platform::Tid) -> Option<String> {
        let win = self.win();
        let vec = self.vector(tid);
        let rr = super::returned::RetRegs {
            rax: r.gp[0],
            rdx: r.gp[3],
            xmm: [vec.as_ref().map(|v| v.xmm(0)), vec.as_ref().map(|v| v.xmm(1))],
            st0: vec.as_ref().and_then(|v| v.st(0)),
        };
        let got = self
            .with_vars(false, |vars, regs| vars.returned(from.wrapping_sub(regs.bias), win, &rr, regs))
            .ok()
            .flatten()
            .unwrap_or(super::returned::Returned::Unknown);
        super::returned::returned_line(&got, &rr)
    }

    /// Which register `return VALUE` fills for the function the stopped thread is in.
    pub(crate) fn return_slot(&mut self) -> Result<super::returned::RetSlot> {
        let got = self.with_vars(false, |vars, regs| vars.ret_slot(regs.rip.wrapping_sub(regs.bias)))?;
        Ok(got.unwrap_or(super::returned::RetSlot::Int))
    }

    /// The parameters of the function at the stop as `args` rows, with how many DWARF lists, whether the
    /// function is variadic, and which of them (by position) had no readable value; `None` without variable
    /// info for this function. Before the prologue ends they come from the argument registers.
    pub(crate) fn named_args(&mut self) -> Option<NamedArgs> {
        let win = self.win();
        self.with_vars(true, |vars, regs| {
            let pc = regs.rip.wrapping_sub(regs.bias);
            let f = vars.func_at(pc).filter(|f| f.nparams > 0 || f.variadic || f.prototyped)?;
            let mut a = NamedArgs { lines: Vec::new(), count: f.nparams, variadic: f.variadic, unread: Vec::new() };
            let ps: Vec<_> = vars.in_scope(pc).into_iter().filter(|(v, _, _)| v.param).collect();
            let early = regs.early.clone().filter(|_| ps.iter().any(|(v, _, _)| Vars::in_slot(v)));
            let inreg = if early.is_some() { vars.params_in_regs(pc, win, regs, 0) } else { Vec::new() };
            if let Some(at) = early {
                a.lines.push(format!("{at}: the parameters as the call passed them"));
            }
            for (i, (v, enc, fb)) in ps.into_iter().enumerate() {
                let ty = View(v.ty, 0);
                let at = vars.value(v, enc, fb, regs);
                if at.is_none() && !(regs.early.is_some() && Vars::in_slot(v)) {
                    a.unread.push(i);
                }
                let val = vars.shown(v, at, regs, 0, &inreg);
                a.lines.push(format!("{} ({}) = {val}", v.name, vars.name(ty)));
            }
            Some(a)
        })
        .ok()
        .flatten()
        .flatten()
    }
}
