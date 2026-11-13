//! Call sites: argument recovery, signatures and printf-style conversions.

use super::*;

/// Where a call reads its arguments from.
fn arg_vars(abi: Abi, n: usize) -> Vec<Expr> {
    abi.arg_regs().iter().take(n).map(|&r| Expr::Var(Var::Reg(r), 64)).collect()
}

fn each_call(f: &mut Lifted, mut g: impl FnMut(usize, usize, &mut Call)) {
    for bi in 0..f.blocks.len() {
        for si in 0..f.blocks[bi].stmts.len() {
            if let Stmt::Assign(_, Expr::Call(c)) | Stmt::Eval(Expr::Call(c)) = &mut f.blocks[bi].stmts[si].st {
                g(bi, si, c);
            }
        }
        let n = f.blocks[bi].stmts.len();
        if let Term::TailCall(Expr::Call(c)) = &mut f.blocks[bi].term {
            g(bi, n, c);
        }
    }
}

/// Fix the arguments of every call whose signature is known or guessed. `variadic` also adds the
/// arguments a constant format string asks for.
pub fn resolve_calls(f: &mut Lifted, sigs: &mut dyn Sigs, variadic: bool) {
    let an = analyze(f);
    let abi = f.abi;
    // for each call site: the number of leading argument registers that hold a value set in the function
    let mut prefix: HashMap<(usize, usize), usize> = HashMap::new();
    for u in &an.uses {
        if let Key::V(Var::Reg(r)) = u.key {
            if let Some(pos) = abi.arg_regs().iter().position(|&x| x == r) {
                let fresh = u.reach.iter().any(|&d| {
                    let s = an.defs[d];
                    if s.blk == ENTRY {
                        return false;
                    }
                    !matches!(f.blocks[s.blk].stmts[s.idx].st, Stmt::Clobber(_))
                });
                let entry = u.reach.iter().any(|&d| an.defs[d].blk == ENTRY);
                if fresh {
                    let e = prefix.entry((u.blk, u.idx)).or_insert(0);
                    *e = (*e).max(pos + 1);
                } else if entry {
                    prefix.entry((u.blk, u.idx)).or_insert(0);
                }
            }
        }
    }
    // the call's own uses are not recorded for the argument registers (it has no arguments yet), so look
    // at the definitions that reach the call instead
    let mut at_call: HashMap<(usize, usize), usize> = HashMap::new();
    {
        // reaching defs of each argument register right before the call
        for bi in 0..f.blocks.len() {
            let n = f.blocks[bi].stmts.len();
            for si in 0..=n {
                let is_call = if si < n {
                    matches!(&f.blocks[bi].stmts[si].st, Stmt::Assign(_, Expr::Call(_)) | Stmt::Eval(Expr::Call(_)))
                } else {
                    matches!(f.blocks[bi].term, Term::TailCall(_))
                };
                if !is_call {
                    continue;
                }
                // the register that holds the address called is not also an argument
                let target = if si < n {
                    match &f.blocks[bi].stmts[si].st {
                        Stmt::Assign(_, Expr::Call(c)) | Stmt::Eval(Expr::Call(c)) => Some(&c.target),
                        _ => None,
                    }
                } else {
                    match &f.blocks[bi].term {
                        Term::TailCall(Expr::Call(c)) => Some(&c.target),
                        _ => None,
                    }
                };
                let held = match target {
                    Some(Target::Indirect(Expr::Var(Var::Reg(r), _))) => Some(*r),
                    _ => None,
                };
                let mut best = 0;
                for (pos, &r) in abi.arg_regs().iter().enumerate() {
                    if Some(r) == held {
                        continue;
                    }
                    if fresh_reach(f, &an, bi, si, Var::Reg(r)) {
                        best = pos + 1;
                    }
                }
                at_call.insert((bi, si), best);
            }
        }
    }
    each_call(f, |bi, si, c| {
        if c.known && !variadic {
            return;
        }
        let sig = sigs.sig_of(&c.target);
        if c.known && sig.as_ref().is_none_or(|s| s.fmt.is_none()) {
            return;
        }
        let fixed = at_call.get(&(bi, si)).copied().unwrap_or(0);
        match sig {
            Some(s) => {
                let mut n = s.args;
                if let (Some(fi), true) = (s.fmt, variadic) {
                    // the format argument is a constant string: count its conversions
                    let fmt = c.args.get(fi).and_then(|a| match a {
                        Expr::Const(v, _) => sigs.cstr(*v),
                        _ => None,
                    });
                    match fmt {
                        Some(text) => n = fi + 1 + count_conversions(&text),
                        None => n = n.max(c.args.len()).max(fixed.min(abi.arg_count_max())),
                    }
                    let old = std::mem::take(&mut c.args);
                    let fresh = arg_vars(abi, n.min(abi.arg_count_max()));
                    c.args = fresh.into_iter().enumerate().map(|(i, v)| old.get(i).cloned().unwrap_or(v)).collect();
                    c.known = true;
                    return;
                }
                if s.fmt.is_some() {
                    // wait for the format string; keep every register set before the call alive until then
                    c.args = arg_vars(abi, n.max(fixed).min(abi.arg_count_max()));
                    c.known = false;
                    return;
                }
                c.args = arg_vars(abi, n.min(abi.arg_count_max()));
                c.known = true;
            }
            None => {
                c.args = arg_vars(abi, fixed);
                c.known = true;
            }
        }
    });
}

pub fn count_conversions(fmt: &str) -> usize {
    let b = fmt.as_bytes();
    let mut n = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            i += 1;
            if i < b.len() && b[i] == b'%' {
                i += 1;
                continue;
            }
            while i < b.len() && !b[i].is_ascii_alphabetic() {
                if b[i] == b'*' {
                    n += 1;
                }
                i += 1;
            }
            // length modifiers
            while i < b.len() && matches!(b[i], b'h' | b'l' | b'L' | b'j' | b'z' | b't' | b'q') {
                i += 1;
            }
            if i < b.len() {
                n += 1;
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    n
}

/// True when a value written in the function (not at entry, not by a call's clobber) reaches the
/// point just before statement `si` of block `bi`.
fn fresh_reach(f: &Lifted, an: &Analysis, bi: usize, si: usize, v: Var) -> bool {
    let Some(&ki) = an.key_ix.get(&Key::V(v)) else { return false };
    // last local definition before si
    for j in (0..si.min(f.blocks[bi].stmts.len())).rev() {
        for &d in &an.stmt_defs[bi][j] {
            if an.defs[d].key == Key::V(v) {
                return !matches!(f.blocks[bi].stmts[j].st, Stmt::Clobber(_));
            }
        }
    }
    // walk back through predecessors: any path that brings a real definition counts
    let mut seen = HashSet::new();
    let mut stack: Vec<usize> = an.preds[bi].clone();
    let _ = ki;
    while let Some(p) = stack.pop() {
        if !seen.insert(p) {
            continue;
        }
        let mut found_local = false;
        for j in (0..f.blocks[p].stmts.len()).rev() {
            for &d in &an.stmt_defs[p][j] {
                if an.defs[d].key == Key::V(v) {
                    if !matches!(f.blocks[p].stmts[j].st, Stmt::Clobber(_)) {
                        return true;
                    }
                    found_local = true;
                }
            }
            if found_local {
                break;
            }
        }
        if !found_local {
            stack.extend(an.preds[p].iter().copied());
        }
    }
    false
}
