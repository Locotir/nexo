//! Late passes: narrowed returns, flag snapshots, dead inserts, division recovery, values kept across calls.

use super::*;

/// `eax` after a `setcc al` that only the low byte of is read: return the byte.
pub(super) fn narrow_returns(f: &mut Lifted) {
    fn strip(e: Expr) -> Expr {
        match e {
            Expr::Insert(_, n) if n.bits() <= 16 => *n,
            Expr::Trunc(x, _) | Expr::Zext(x, _) if matches!(*x, Expr::Insert(_, ref n) if n.bits() <= 16) => strip(*x),
            other => other,
        }
    }
    for bl in &mut f.blocks {
        if let Term::Return(Some(e)) = &mut bl.term {
            let old = std::mem::replace(e, Expr::Unknown(0));
            *e = strip(old);
        }
    }
}

/// A flag definition reads registers; when one of them is written before the flags are used, keep a copy
/// so that the condition can still be written in terms of the operands.
pub(super) fn snapshot_flags(f: &mut Lifted) {
    let mut next: u8 = 16;
    for bi in 0..f.blocks.len() {
        let mut i = 0;
        while i < f.blocks[bi].stmts.len() {
            let is_def = matches!(&f.blocks[bi].stmts[i].st, Stmt::Assign(Var::FlagOps, _));
            if !is_def {
                i += 1;
                continue;
            }
            // the registers both flag definitions read
            let mut regs: Vec<u8> = Vec::new();
            let mut j = i;
            let mut end = f.blocks[bi].stmts.len();
            let mut defs = Vec::new();
            while j < end {
                match &f.blocks[bi].stmts[j].st {
                    Stmt::Assign(Var::FlagOps | Var::FlagRes, e) if j <= i + 1 => {
                        defs.push(j);
                        e.visit(&mut |x| {
                            if let Expr::Var(Var::Reg(r), _) = x {
                                if !regs.contains(r) {
                                    regs.push(*r);
                                }
                            }
                        });
                    }
                    Stmt::Assign(Var::FlagOps, _) if j > i + 1 => {
                        end = j;
                    }
                    _ => {}
                }
                j += 1;
            }
            let mut need: Vec<u8> = Vec::new();
            // the last place in the block that reads the flags; the statement that reads them reads before it writes
            let mut last = i + 1;
            for k in i + 1..end {
                let mut reads = false;
                f.blocks[bi].stmts[k].st.uses(&mut |v| reads |= matches!(v, Var::FlagOps | Var::FlagRes));
                if reads {
                    last = k;
                }
            }
            if end == f.blocks[bi].stmts.len()
                && f.blocks[bi].term.exprs().iter().any(|e| e.uses(Var::FlagOps) > 0 || e.uses(Var::FlagRes) > 0)
            {
                last = end;
            }
            for st in &f.blocks[bi].stmts[i + 1..last] {
                match &st.st {
                    Stmt::Assign(Var::Reg(r), _) if regs.contains(r) && !need.contains(r) => need.push(*r),
                    Stmt::Clobber(vs) => {
                        for v in vs {
                            if let Var::Reg(r) = v {
                                if regs.contains(r) && !need.contains(r) {
                                    need.push(*r);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            let mut ins = 0;
            // a value read from memory that memory writes may change before the flags are used
            if next < u8::MAX && f.blocks[bi].stmts[i + 1..last].iter().any(|s| s.st.writes_mem()) {
                let mut load: Option<Expr> = None;
                if let Stmt::Assign(_, e) = &f.blocks[bi].stmts[i].st {
                    e.visit(&mut |x| {
                        if load.is_none() && matches!(x, Expr::Load(..)) {
                            load = Some(x.clone());
                        }
                    });
                }
                if let Some(l) = load {
                    let t = Var::Tmp(next);
                    next += 1;
                    let lb = l.bits();
                    let addr = f.blocks[bi].stmts[i].addr;
                    for &d in &defs {
                        if let Stmt::Assign(_, e) = &mut f.blocks[bi].stmts[d].st {
                            e.rewrite(&mut |x| {
                                if *x == l {
                                    *x = Expr::Var(t, lb);
                                }
                            });
                        }
                    }
                    f.blocks[bi].stmts.insert(i, S { addr, st: Stmt::Assign(t, l.zext(64)) });
                    ins += 1;
                }
            }
            for r in need {
                if next == u8::MAX {
                    break;
                }
                let t = Var::Tmp(next);
                next += 1;
                let addr = f.blocks[bi].stmts[i].addr;
                for &d in &defs {
                    if let Stmt::Assign(_, e) = &mut f.blocks[bi].stmts[d + ins].st {
                        e.subst(Var::Reg(r), &Expr::Var(t, 64));
                    }
                }
                f.blocks[bi].stmts.insert(i, S { addr, st: Stmt::Assign(t, Expr::Var(Var::Reg(r), 64)) });
                ins += 1;
            }
            i += ins + 2;
        }
    }
}

/// `al = cond; eax = zx(al)`: the upper bits written by the first are overwritten by the second, so the first
/// can write the whole register.
pub(super) fn drop_dead_inserts(f: &mut Lifted) {
    for bl in &mut f.blocks {
        for i in 0..bl.stmts.len().saturating_sub(1) {
            let Stmt::Assign(Var::Reg(r), Expr::Insert(old, new)) = &bl.stmts[i].st else { continue };
            if !matches!(&**old, Expr::Var(Var::Reg(q), 64) if q == r) {
                continue;
            }
            let nb = new.bits();
            let Stmt::Assign(Var::Reg(r2), rhs) = &bl.stmts[i + 1].st else { continue };
            if r2 != r {
                continue;
            }
            let mut narrow = true;
            rhs.visit(&mut |x| {
                if let Expr::Var(Var::Reg(q), w) = x {
                    if q == r && *w > nb {
                        narrow = false;
                    }
                }
            });
            // the second statement must write the whole register
            let whole = matches!(rhs, Expr::Zext(_, 64) | Expr::Sext(_, 64));
            if narrow && whole {
                let v = Var::Reg(*r);
                let n = (**new).clone();
                bl.stmts[i].st = Stmt::Assign(v, n.zext(64));
            }
        }
    }
}

/// Turn multiply-and-shift sequences into divisions and remainders by their constant.
pub(super) fn recover_divs(f: &mut Lifted) {
    super::super::divrec::recover_fn(f);
}

/// A callee that never writes a register leaves it as it was: the caller must not treat it as lost.
/// (Compilers do this on purpose for functions of the same file, "ipa-ra".)
pub(super) fn keep_across_calls(f: &mut Lifted, sigs: &mut dyn Sigs) {
    for bl in &mut f.blocks {
        for i in 0..bl.stmts.len().saturating_sub(1) {
            let target = match &bl.stmts[i].st {
                Stmt::Assign(_, Expr::Call(c)) | Stmt::Eval(Expr::Call(c)) => c.target.clone(),
                _ => continue,
            };
            if matches!(target, Target::Indirect(_)) {
                continue;
            }
            let keep = sigs.keeps(&target);
            if keep.is_empty() {
                continue;
            }
            if let Stmt::Clobber(vs) = &mut bl.stmts[i + 1].st {
                vs.retain(|v| !matches!(v, Var::Reg(r) if keep.contains(r)));
            }
        }
    }
}

/// The caller-saved registers that code from `start` to `end` never writes (empty when it ends in
/// a tail call, which hands the question to another function).
pub fn unwritten_regs(
    env: &dyn super::super::lift::Env,
    start: u64,
    end: u64,
    abi: super::super::lift::Abi,
) -> Vec<u8> {
    let f = super::super::lift::lift(env, start, end, abi);
    let mut written: HashSet<u8> = HashSet::new();
    for bl in &f.blocks {
        if matches!(bl.term, Term::TailCall(_) | Term::Indirect(_)) {
            return Vec::new();
        }
        for s in &bl.stmts {
            for k in stmt_def_keys(&s.st) {
                if let Key::V(Var::Reg(r)) = k {
                    written.insert(r);
                }
            }
        }
    }
    abi.caller_saved().iter().copied().filter(|r| !written.contains(r)).collect()
}
