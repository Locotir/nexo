//! A stop before a function's prologue ends (a breakpoint at its first instruction): its stack slots still hold
//! what an earlier call left there, so the parameters are read where the call put them, the argument registers.

use super::*;

/// The integer argument registers in order, with their index in `Regs::gp`.
const SYSV: [(&str, usize); 6] = [("rdi", 5), ("rsi", 4), ("rdx", 3), ("rcx", 2), ("r8", 8), ("r9", 9)];
const WIN64: [(&str, usize); 4] = [("rcx", 2), ("rdx", 3), ("r8", 8), ("r9", 9)];

/// A parameter as the call left it: its register and value, or why it is not shown.
/// Where a pushed parameter is, in place of a register name.
pub(super) const ON_STACK: &str = "the stack";

pub(super) type InReg<'v> = (&'v Var, std::result::Result<(&'static str, String), &'static str>);

/// A location in the frame (`DW_OP_fbreg`, `DW_OP_breg6`/`breg7`, `DW_OP_call_frame_cfa`): a stack slot the
/// function's own code fills.
pub(super) fn frame_based(e: &[u8]) -> bool {
    matches!(e.first(), Some(0x91 | 0x76 | 0x77 | 0x9c))
}

impl Vars {
    /// Whether a variable lives in a stack slot for its whole function.
    pub(super) fn in_slot(v: &Var) -> bool {
        matches!(&v.loc, Loc::Expr(e) if frame_based(e))
    }

    /// The function holding `pc` (a file address): its name, its entry and the end of the range it starts.
    pub(crate) fn func_span(&self, pc: u64) -> Option<(&str, u64, u64)> {
        let f = self.func_at(pc)?;
        let &(lo, hi) = f.ranges.first()?;
        Some((&f.name, lo, hi))
    }

    /// The parameters of the function at `pc` in the integer argument registers (Windows x64 or System V),
    /// in order, rendered at `depth`. A float, a struct by value or a parameter past the registers is not
    /// shown, and says why.
    pub(super) fn params_in_regs(&self, pc: u64, win: bool, regs: &mut Regs, depth: usize) -> Vec<InReg<'_>> {
        let Some(f) = self.func_at(pc) else { return Vec::new() };
        if f.enc.address_size == 4 {
            return self.params_pushed(f, regs, depth);
        }
        let order: &[(&str, usize)] = if win { &WIN64 } else { &SYSV };
        let params: Vec<&Var> = f.vars.iter().filter(|v| v.param).collect();
        // a struct returned through memory: the address to write it to comes first
        let mut next = match self.strip(f.ret) {
            Some(Ty::Record { size, .. }) if if win { ![1, 2, 4, 8].contains(size) } else { *size > 16 } => 1,
            _ => 0,
        };
        let mut lost = (params.len() != f.nparams).then_some("a parameter without a name comes first");
        let mut out = Vec::new();
        for v in params {
            if let Some(why) = lost {
                out.push((v, Err(why)));
                continue;
            }
            // `Ok` takes the next integer register; `Err((why, true))` takes none on System V (Windows gives
            // each parameter the register of its place), `Err((why, false))` takes System V registers by a rule
            // of its own, so the ones after it are not certain
            let pass = match self.strip(v.ty) {
                Some(Ty::Ptr { .. }) => Ok(()),
                Some(Ty::Base { enc: 4, size, .. }) if *size > 8 => {
                    Err(("a long double: on the stack or by reference", true))
                }
                Some(Ty::Base { enc: 3 | 4, .. }) => Err(("floating point: in an xmm register", true)),
                Some(Ty::Base { size, .. } | Ty::Enum { size, .. }) if *size <= 8 => Ok(()),
                Some(Ty::Base { .. } | Ty::Enum { .. }) => Err(("wider than a register", false)),
                Some(Ty::Record { .. }) => Err(("a struct by value: the ABI splits or copies it", false)),
                _ => Err(("its type does not say how it is passed", false)),
            };
            let got = match pass {
                Ok(()) => match order.get(next) {
                    Some(&(name, i)) => {
                        let at = At::Bytes(regs.gp[i].to_le_bytes().to_vec());
                        Ok((name, self.render(&Val { at, ty: View(v.ty, 0) }, regs, depth)))
                    }
                    None => Err("on the stack, past the argument registers"),
                },
                Err((why, known)) => {
                    if !win && !known {
                        lost = Some("its register depends on how the parameter before it was passed");
                    }
                    Err(why)
                }
            };
            if got.is_ok() || win {
                next += 1;
            }
            out.push((v, got));
        }
        out
    }

    /// Whether the function at `pc` takes its parameters on the stack (a 32-bit program's), not in registers.
    pub(super) fn pushed_params(&self, pc: u64) -> bool {
        self.func_at(pc).is_some_and(|f| f.enc.address_size == 4)
    }

    /// A 32-bit program's parameters before the prologue ends: the caller pushed them on the stack, so a slot named
    /// from the CFA (the frame base `DW_OP_call_frame_cfa`, where gcc and clang put it) holds its value already. A
    /// slot named from a register the prologue sets is not read.
    fn params_pushed<'v>(&'v self, f: &'v Func, regs: &mut Regs, depth: usize) -> Vec<InReg<'v>> {
        let fb = f.frame_base.as_deref();
        let from_cfa = fb == Some(&[0x9c][..]);
        f.vars
            .iter()
            .filter(|v| v.param)
            .map(|v| {
                let got = match &v.loc {
                    // DW_OP_fbreg
                    Loc::Expr(e) if from_cfa && e.first() == Some(&0x91) => {
                        match super::scope::eval(e, f.enc, fb, regs) {
                            Some(at) => Ok((ON_STACK, self.render(&Val { at, ty: View(v.ty, 0) }, regs, depth))),
                            None => Err("its stack slot cannot be read"),
                        }
                    }
                    _ => Err("its slot is named from a register the prologue has not set yet"),
                };
                (v, got)
            })
            .collect()
    }

    /// A variable's value as text; before the prologue ends a stack slot is not read: a parameter shows its
    /// argument register (from `inreg`), a local says it is not stored yet.
    pub(super) fn shown(&self, v: &Var, at: Option<At>, regs: &mut Regs, depth: usize, inreg: &[InReg]) -> String {
        if regs.early.is_some() && Self::in_slot(v) {
            return match inreg.iter().find(|(p, _)| std::ptr::eq(*p, v)) {
                Some((_, Ok((r, val)))) if *r == ON_STACK => {
                    format!("{val} (on the stack, where the caller pushed it)")
                }
                Some((_, Ok((r, val)))) => format!("{val} (in {r})"),
                Some((_, Err(why))) => format!("not shown ({why})"),
                None => "not stored yet".into(),
            };
        }
        match at {
            Some(at) => self.render(&Val { at, ty: View(v.ty, 0) }, regs, depth),
            None => "<optimised out>".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(name: &str, ty: u64) -> Var {
        Var { name: name.into(), param: true, loc: Loc::Expr(vec![0x91, 0x68]), ty: Some(ty), scope: None, decl: None }
    }

    /// f(double d, char *p, struct pt s, int n) returning `ret`, with rdi..r9 and rcx holding 0x10 * their index.
    fn shown_regs(win: bool, ret: Option<u64>) -> Vec<String> {
        let types = [
            (1, Ty::Base { name: "double".into(), size: 8, enc: 4 }),
            (2, Ty::Base { name: "char".into(), size: 1, enc: 6 }),
            (3, Ty::Ptr { to: Some(2) }),
            (4, Ty::Record { name: "pt".into(), size: 8, members: Vec::new(), union: false }),
            (5, Ty::Base { name: "int".into(), size: 4, enc: 5 }),
            (6, Ty::Record { name: "big".into(), size: 24, members: Vec::new(), union: false }),
        ];
        let enc = gimli::Encoding { address_size: 8, format: gimli::Format::Dwarf32, version: 4 };
        let f = Func {
            name: "f".into(),
            ranges: vec![(0x100, 0x140)],
            frame_base: None,
            enc,
            vars: vec![var("d", 1), var("p", 3), var("s", 4), var("n", 5)],
            nparams: 4,
            variadic: false,
            prototyped: true,
            ret,
        };
        let v =
            Vars { funcs: vec![f], globals: Vec::new(), types: types.into_iter().collect(), files: Vec::new(), ptr: 8 };
        let mut rd = |_: u64, _: usize| None;
        let gp: [u64; 16] = std::array::from_fn(|i| 0x10 * i as u64);
        let mut regs =
            Regs { gp, rip: 0x100, cfa: None, bias: 0, rd: &mut rd, early: Some(String::new()), lines: None, lost: 0 };
        v.params_in_regs(0x100, win, &mut regs, 1)
            .into_iter()
            .map(|(v, got)| match got {
                Ok((r, val)) => format!("{} {r} {val}", v.name),
                Err(why) => format!("{} - {why}", v.name),
            })
            .collect()
    }

    #[test]
    fn integers_and_pointers_take_the_argument_registers_in_order_and_the_rest_says_why_not() {
        // System V: the double goes to xmm0 and takes no integer register; after the struct the order is lost
        assert_eq!(
            shown_regs(false, None),
            [
                "d - floating point: in an xmm register",
                "p rdi 0x50 <unreadable>",
                "s - a struct by value: the ABI splits or copies it",
                "n - its register depends on how the parameter before it was passed",
            ]
        );
        // Windows x64: each parameter has the register of its place
        assert_eq!(
            shown_regs(true, None),
            [
                "d - floating point: in an xmm register",
                "p rdx 0x30 <unreadable>",
                "s - a struct by value: the ABI splits or copies it",
                "n r9 144",
            ]
        );
        // a big struct returned through memory: its address takes the first register
        assert_eq!(shown_regs(false, Some(6))[1], "p rsi 0x40 <unreadable>");
    }
}
