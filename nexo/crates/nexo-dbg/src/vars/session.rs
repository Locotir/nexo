//! The `locals`, `print` and stop-report side of a session.

use super::path::NOTES;
use super::*;

impl Session {
    /// The registers, frame and memory reader a location needs, or `None` when the program has no variables.
    fn with_vars<T>(&mut self, f: impl FnOnce(&Vars, &mut Regs) -> T) -> Result<Option<T>> {
        self.need_stopped()?;
        let r = self.t.regs(self.cur)?;
        let Session { t, mods, .. } = self;
        let cfa = {
            let mut rd8 = |a: u64| {
                let mut b = [0u8; 8];
                (t.read(a, &mut b) == 8).then(|| u64::from_le_bytes(b))
            };
            mods.backtrace(&r, &mut rd8, 1).first().and_then(|f| f.cfa)
        };
        let Some((vars, bias)) = mods.main_vars() else { return Ok(None) };
        let mut rd = |a: u64, n: usize| {
            let mut b = vec![0u8; n];
            (t.read(a, &mut b) == n).then_some(b)
        };
        let mut regs = Regs { gp: r.gp, rip: r.rip, cfa, bias, rd: &mut rd };
        Ok(Some(f(vars, &mut regs)))
    }

    /// `locals`: the parameters and locals of the function the program stopped in, with types and values.
    pub(crate) fn cmd_locals(&mut self) -> Result<Output> {
        let got = self.with_vars(|vars, regs| {
            let pc = regs.rip.wrapping_sub(regs.bias);
            let f = vars.func_at(pc).map(|f| f.name.clone());
            let mut l = Vec::new();
            for (v, enc, fb) in vars.in_scope(pc) {
                let ty = View(v.ty, 0);
                let val = match vars.value(v, enc, fb, regs) {
                    Some(at) => vars.render(&Val { at, ty }, regs, 0),
                    None => "<optimised out>".into(),
                };
                let kind = if v.param { "arg " } else { "" };
                l.push(format!("{kind}{} ({}) = {val}", v.name, vars.name(ty)));
            }
            (f, l)
        })?;
        match got {
            None => fail("the program has no variable info: build it with `-g` (cc -g, cargo's dev profile); `args` shows the argument registers"),
            Some((None, _)) => fail("the stop is outside the program's own functions (a library): `finish` or `bt` to get back to it"),
            Some((Some(f), l)) if l.is_empty() => Ok(rows("locals", vec![format!("{f} has no parameters or locals")], json!({}))),
            Some((Some(f), l)) => {
                let mut out = vec![format!("in {f}:")];
                out.extend(l);
                Ok(rows("locals", out, json!({"function": f})))
            }
        }
    }

    /// `print` of a source expression; `None` hands it to the register expressions.
    pub(crate) fn print_var(&mut self, src: &str) -> Option<Result<Output>> {
        let got = self
            .with_vars(|vars, regs| {
                let (v, amp) = match vars.path(src, regs)? {
                    Ok(x) => x,
                    Err(e) => return Some(Err(e)),
                };
                let notes = NOTES.with(|n| std::mem::take(&mut *n.borrow_mut()));
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
                    format!("{src} ({ty}{at}) = {}", vars.render(&v, regs, 0))
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

    /// One line of the locals for a stop report, short; `None` without variable info.
    pub(crate) fn locals_line(&mut self) -> Option<String> {
        self.with_vars(|vars, regs| {
            let pc = regs.rip.wrapping_sub(regs.bias);
            let vs = vars.in_scope(pc);
            if vs.is_empty() {
                return None;
            }
            let mut parts = Vec::new();
            for (v, enc, fb) in vs {
                let val = match vars.value(v, enc, fb, regs) {
                    Some(at) => vars.render(&Val { at, ty: View(v.ty, 0) }, regs, 1),
                    None => "<optimised out>".into(),
                };
                let mut val: String = val.chars().take(60).collect();
                if val.len() >= 60 {
                    val.push_str("...");
                }
                parts.push(format!("{} = {val}", v.name));
            }
            let mut s = format!("locals: {}", parts.join(", "));
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

    /// The named parameters of the function at the stop, as `args` rows; empty without variable info.
    pub(crate) fn named_args(&mut self) -> Vec<String> {
        self.with_vars(|vars, regs| {
            let pc = regs.rip.wrapping_sub(regs.bias);
            let mut l = Vec::new();
            for (v, enc, fb) in vars.in_scope(pc).into_iter().filter(|(v, _, _)| v.param) {
                let ty = View(v.ty, 0);
                let val = match vars.value(v, enc, fb, regs) {
                    Some(at) => vars.render(&Val { at, ty }, regs, 0),
                    None => "<optimised out>".into(),
                };
                l.push(format!("{} ({}) = {val}", v.name, vars.name(ty)));
            }
            l
        })
        .ok()
        .flatten()
        .unwrap_or_default()
    }
}
