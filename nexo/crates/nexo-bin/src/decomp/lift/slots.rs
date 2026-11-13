//! Promote untouched stack slots to variables.

use super::*;

/// Turn stack memory that nothing takes the address of into variables.
pub(super) fn promote_slots(f: &mut Lifted, raw_escapes: &BTreeSet<i64>) {
    // accesses: slot -> widths; escapes: offsets whose address is used for anything else
    let mut widths: BTreeMap<i64, BTreeSet<Bits>> = BTreeMap::new();
    let mut escapes: BTreeSet<i64> = raw_escapes.clone();
    fn scan(e: &Expr, widths: &mut BTreeMap<i64, BTreeSet<Bits>>, escapes: &mut BTreeSet<i64>) {
        match e {
            Expr::Load(a, w) => {
                if let Expr::StackAddr(k) = **a {
                    widths.entry(k).or_default().insert(*w);
                    return;
                }
                scan(a, widths, escapes);
            }
            Expr::StackAddr(k) => {
                escapes.insert(*k);
            }
            Expr::Const(..) | Expr::Var(..) | Expr::Unknown(_) | Expr::SegBase(_) => {}
            Expr::Un(_, a) | Expr::Trunc(a, _) | Expr::Zext(a, _) | Expr::Sext(a, _) => scan(a, widths, escapes),
            Expr::Bin(_, a, c)
            | Expr::Insert(a, c)
            | Expr::Cmp(_, a, c)
            | Expr::Flags(_, a, c)
            | Expr::Cond(_, a, c) => {
                scan(a, widths, escapes);
                scan(c, widths, escapes);
            }
            Expr::Select(a, c, d) => {
                scan(a, widths, escapes);
                scan(c, widths, escapes);
                scan(d, widths, escapes);
            }
            Expr::Call(c) => {
                if let Target::Indirect(t) = &c.target {
                    scan(t, widths, escapes);
                }
                c.args.iter().for_each(|a| scan(a, widths, escapes));
            }
            Expr::Intr(_, args, _) => args.iter().for_each(|a| scan(a, widths, escapes)),
        }
    }
    for bl in &f.blocks {
        for s in &bl.stmts {
            match &s.st {
                Stmt::Store(a, w, v) => {
                    if let Expr::StackAddr(k) = a {
                        widths.entry(*k).or_default().insert(*w);
                    } else {
                        scan(a, &mut widths, &mut escapes);
                    }
                    scan(v, &mut widths, &mut escapes);
                }
                other => other.exprs().into_iter().for_each(|e| scan(e, &mut widths, &mut escapes)),
            }
        }
        bl.term.exprs().into_iter().for_each(|e| scan(e, &mut widths, &mut escapes));
    }
    let min_escape = escapes.iter().next().copied();
    let slots: Vec<(i64, Bits)> =
        widths.iter().filter(|(_, w)| w.len() == 1).map(|(k, w)| (*k, *w.iter().next().unwrap())).collect();
    let mut ok: HashMap<i64, Bits> = HashMap::new();
    for (i, &(k, w)) in slots.iter().enumerate() {
        let size = (w as i64 / 8).max(1);
        let saved = f.saves.contains(&k);
        if !saved {
            if min_escape.is_some_and(|m| k >= m) && k < 0 {
                continue;
            }
            if escapes.iter().any(|&e| e > k && e < k + size) {
                continue;
            }
        }
        // overlap with the neighbour slot
        if let Some(&(k2, _)) = slots.get(i + 1) {
            if k2 < k + size {
                continue;
            }
        }
        if i > 0 {
            let (k0, w0) = slots[i - 1];
            if k0 + (w0 as i64 / 8).max(1) > k {
                continue;
            }
        }
        // a multi-width slot is not promoted, and neither is a slot another width overlaps
        if widths.range(k + 1..k + size).next().is_some() {
            continue;
        }
        ok.insert(k, w);
    }
    // slots with several widths stay memory
    let promoted = ok;
    let fix = |e: &mut Expr| {
        if let Expr::Load(a, w) = e {
            if let Expr::StackAddr(k) = **a {
                if promoted.get(&k) == Some(w) {
                    *e = Expr::Var(Var::Stack(k as i32), *w);
                }
            }
        }
    };
    for bl in &mut f.blocks {
        for s in &mut bl.stmts {
            for e in s.st.exprs_mut() {
                e.rewrite(&mut |x| fix(x));
            }
            if let Stmt::Store(Expr::StackAddr(k), w, v) = &s.st {
                if promoted.get(k) == Some(w) {
                    let (k, v) = (*k, v.clone());
                    s.st = Stmt::Assign(Var::Stack(k as i32), v);
                }
            }
        }
        for e in bl.term.exprs_mut() {
            e.rewrite(&mut |x| fix(x));
        }
    }
}
