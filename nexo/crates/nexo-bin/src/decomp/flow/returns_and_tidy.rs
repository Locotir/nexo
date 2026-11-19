//! Return-value recovery and the cleanup passes: prune, tidy, switch merging, canary removal.

use super::*;

/// Decide whether the function returns a value, and which.
pub fn fix_returns(f: &mut Lifted) -> Option<Var> {
    let an = analyze(f);
    let mut rets: Vec<(usize, Vec<usize>)> = Vec::new();
    for (bi, bl) in f.blocks.iter().enumerate() {
        if matches!(bl.term, Term::Return(_)) {
            let reach: Vec<usize> = an.use_at[bi][bl.stmts.len()]
                .iter()
                .filter(|&&u| an.uses[u].key == Key::V(RAX))
                .flat_map(|&u| an.uses[u].reach.clone())
                .collect();
            rets.push((bi, reach));
        }
    }
    let real = |d: &usize| {
        let s = an.defs[*d];
        s.blk != ENTRY && !matches!(f.blocks[s.blk].stmts[s.idx].st, Stmt::Clobber(_))
    };
    let all_real = !rets.is_empty() && rets.iter().all(|(_, r)| !r.is_empty() && r.iter().all(real));
    if all_real {
        return Some(RAX);
    }
    // a float result
    let mut xmm_ok = !rets.is_empty();
    for (bi, _) in &rets {
        let bl = &f.blocks[*bi];
        let x0 = Var::Xmm(0);
        // the nearest definition of xmm0 must be a real one
        let mut ok = false;
        'o: for j in (0..bl.stmts.len()).rev() {
            for &d in &an.stmt_defs[*bi][j] {
                if an.defs[d].key == Key::V(x0) {
                    ok = !matches!(bl.stmts[j].st, Stmt::Clobber(_));
                    break 'o;
                }
            }
        }
        xmm_ok &= ok;
    }
    let rax_defined_somewhere = rets.iter().any(|(_, r)| r.iter().any(real));
    if xmm_ok && !rax_defined_somewhere {
        return Some(Var::Xmm(0));
    }
    for bl in &mut f.blocks {
        if let Term::Return(e) = &mut bl.term {
            *e = None;
        }
    }
    None
}

/// For the return statements of a function that returns a value, make the expression the variable.
pub fn set_return_var(f: &mut Lifted, v: Option<Var>) {
    for bl in &mut f.blocks {
        if let Term::Return(e) = &mut bl.term {
            *e = v.map(|x| Expr::Var(x, 64));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Graph clean-up

/// Drop blocks nothing reaches and renumber the rest.
pub fn prune(f: &mut Lifted) {
    let n = f.blocks.len();
    let mut seen = vec![false; n];
    let mut stack = vec![0usize];
    while let Some(b) = stack.pop() {
        if b >= n || seen[b] {
            continue;
        }
        seen[b] = true;
        stack.extend(f.blocks[b].term.succs());
    }
    if seen.iter().all(|&s| s) {
        return;
    }
    let mut map = vec![usize::MAX; n];
    let mut next = 0;
    for i in 0..n {
        if seen[i] {
            map[i] = next;
            next += 1;
        }
    }
    let old = std::mem::take(&mut f.blocks);
    for (i, mut bl) in old.into_iter().enumerate() {
        if seen[i] {
            bl.term.retarget(|t| map[t]);
            f.blocks.push(bl);
        }
    }
}

/// Thread jumps through empty blocks, fold branches with equal arms, merge straight lines.
pub fn tidy(f: &mut Lifted) {
    loop {
        let mut changed = false;
        // empty jump-only blocks
        let n = f.blocks.len();
        let mut fwd: Vec<usize> = (0..n).collect();
        for i in 1..n {
            if f.blocks[i].stmts.is_empty() {
                if let Term::Jump(t) = f.blocks[i].term {
                    if t != i {
                        fwd[i] = t;
                    }
                }
            }
        }
        let resolve = |mut x: usize| {
            let mut guard = 0;
            while fwd[x] != x && guard < 50 {
                x = fwd[x];
                guard += 1;
            }
            x
        };
        for i in 0..n {
            let mut t = std::mem::replace(&mut f.blocks[i].term, Term::Stop);
            let before = t.clone();
            t.retarget(resolve);
            if let Term::Branch(_, a, c) = &t {
                if a == c {
                    t = Term::Jump(*a);
                }
            }
            if t != before {
                changed = true;
            }
            f.blocks[i].term = t;
        }
        // merge A -> B when B has no other predecessor
        let preds = preds_of(f);
        for a in 0..f.blocks.len() {
            if let Term::Jump(b) = f.blocks[a].term {
                if b != a && b != 0 && preds[b].len() == 1 && preds[b][0] == a {
                    let bb = std::mem::replace(
                        &mut f.blocks[b],
                        Block { addr: 0, stmts: Vec::new(), term: Term::Stop, term_addr: 0 },
                    );
                    f.blocks[a].stmts.extend(bb.stmts);
                    f.blocks[a].term = bb.term;
                    f.blocks[a].term_addr = bb.term_addr;
                    changed = true;
                    break;
                }
            }
        }
        prune(f);
        if !changed {
            break;
        }
    }
}

/// `if (x > N) goto default; switch table` becomes one switch.
pub fn merge_switches(f: &mut Lifted) {
    let preds = preds_of(f);
    for p in 0..f.blocks.len() {
        let Term::Branch(c, t, e) = f.blocks[p].term.clone() else { continue };
        let Expr::Cmp(op, x, n) = &c else { continue };
        let Some(bound) = n.as_const() else { continue };
        for (sw, other, taken_default) in [(e, t, true), (t, e, false)] {
            if sw == p || preds[sw].len() != 1 || !f.blocks[sw].stmts.is_empty() {
                continue;
            }
            let Term::Switch(_, cases, None) = f.blocks[sw].term.clone() else { continue };
            // the compare must send out-of-range values to `other`
            let outside = if taken_default { *op } else { op.negate() };
            let max_ok = match outside {
                Cmp::Ugt => Some(bound),
                Cmp::Uge => bound.checked_sub(1),
                _ => None,
            };
            let Some(max) = max_ok else { continue };
            let cases: Vec<(u64, BlockId)> = cases.into_iter().filter(|(v, _)| *v <= max).collect();
            // `x - c` as the index shifts the case values
            let (scr, off) = match &**x {
                Expr::Bin(Bin::Add, y, k) if k.as_const().is_some() => ((**y).clone(), k.as_const().unwrap_or(0)),
                other => (other.clone(), 0),
            };
            let w = scr.bits();
            let cases: Vec<(u64, BlockId)> =
                cases.into_iter().map(|(v, b)| (v.wrapping_sub(off) & mask(w), b)).collect();
            f.blocks[p].term = Term::Switch(scr, cases, Some(other));
            f.blocks[sw].term = Term::Stop;
            break;
        }
    }
    prune(f);
}

/// Hide the stack protector: the canary check can never fail in the code we show.
pub fn drop_canary(f: &mut Lifted) {
    let has = |e: &Expr| {
        e.has(&|x| {
            matches!(x, Expr::Intr("canary", ..))
                || matches!(x, Expr::Load(a, _) if matches!(**a, Expr::SegBase(_)))
                || matches!(x, Expr::Bin(_, a, _) if matches!(**a, Expr::SegBase(_)))
        })
    };
    for i in 0..f.blocks.len() {
        if let Term::Branch(c, t, e) = f.blocks[i].term.clone() {
            if has(&c) {
                // the side that ends in a call that does not return is the failure
                let fail = |b: usize| matches!(f.blocks[b].term, Term::Stop) && f.blocks[b].stmts.len() <= 3;
                let keep = if fail(t) {
                    e
                } else if fail(e) {
                    t
                } else {
                    continue;
                };
                f.blocks[i].term = Term::Jump(keep);
            }
        }
    }
    prune(f);
}
