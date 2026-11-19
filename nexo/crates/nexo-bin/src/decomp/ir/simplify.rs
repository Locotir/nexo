//! Constant folding and expression simplification.

use super::*;

pub(in crate::decomp) fn fold_bin(op: Bin, x: u64, y: u64, w: Bits) -> Option<u64> {
    let (sx, sy) = (signed(x, w), signed(y, w));
    let sh = (y & if w > 32 { 63 } else { 31 }) as u32;
    Some(
        match op {
            Bin::Add => x.wrapping_add(y),
            Bin::Sub => x.wrapping_sub(y),
            Bin::Mul => x.wrapping_mul(y),
            Bin::And => x & y,
            Bin::Or => x | y,
            Bin::Xor => x ^ y,
            Bin::Shl => x << sh,
            Bin::Shr => x >> sh,
            Bin::Sar => (sx >> sh) as u64,
            Bin::UDiv if y != 0 => x / y,
            Bin::UMod if y != 0 => x % y,
            Bin::SDiv if y != 0 => sx.wrapping_div(sy) as u64,
            Bin::SMod if y != 0 => sx.wrapping_rem(sy) as u64,
            _ => return None,
        } & mask(w),
    )
}

pub(in crate::decomp) fn fold_cmp(op: Cmp, x: u64, y: u64, w: Bits) -> Option<bool> {
    let (sx, sy) = (signed(x, w), signed(y, w));
    Some(match op {
        Cmp::Eq => x == y,
        Cmp::Ne => x != y,
        Cmp::Lt => sx < sy,
        Cmp::Le => sx <= sy,
        Cmp::Gt => sx > sy,
        Cmp::Ge => sx >= sy,
        Cmp::Ult => x < y,
        Cmp::Ule => x <= y,
        Cmp::Ugt => x > y,
        Cmp::Uge => x >= y,
        _ => return None,
    })
}

fn cmp(op: Cmp, a: Expr, c: Expr) -> Expr {
    simplify_node(Expr::Cmp(op, b(a), b(c)))
}

fn zero(w: Bits) -> Expr {
    Expr::Const(0, w)
}

/// Turn a condition code over flags into a compare, when the flags are known.
fn fold_cond(cc: Cc, ops: &Expr, res: &Expr) -> Option<Expr> {
    if let Expr::Flags(kind, x, y) = ops {
        let (x, y) = ((**x).clone(), (**y).clone());
        let w = x.bits();
        return match kind {
            FlagKind::Sub | FlagKind::SubNoCf => {
                let cf = *kind == FlagKind::Sub;
                let op = match cc {
                    Cc::E => Cmp::Eq,
                    Cc::NE => Cmp::Ne,
                    Cc::L => Cmp::Lt,
                    Cc::GE => Cmp::Ge,
                    Cc::LE => Cmp::Le,
                    Cc::G => Cmp::Gt,
                    Cc::B if cf => Cmp::Ult,
                    Cc::AE if cf => Cmp::Uge,
                    Cc::BE if cf => Cmp::Ule,
                    Cc::A if cf => Cmp::Ugt,
                    Cc::S => return Some(cmp(Cmp::Lt, simplify_node(Expr::bin(Bin::Sub, x, y)), zero(w))),
                    Cc::NS => return Some(cmp(Cmp::Ge, simplify_node(Expr::bin(Bin::Sub, x, y)), zero(w))),
                    _ => return None,
                };
                Some(cmp(op, x, y))
            }
            FlagKind::Logic => fold_result(cc, x, true),
            FlagKind::Add => {
                let sum = simplify_node(Expr::bin(Bin::Add, x.clone(), y));
                match cc {
                    Cc::B => Some(cmp(Cmp::Ult, sum, x)),
                    Cc::AE => Some(cmp(Cmp::Uge, sum, x)),
                    _ => fold_result(cc, sum, false),
                }
            }
            FlagKind::FCmp => Some(match cc {
                // After `ucomisd x, y` an unordered pair sets ZF, PF and CF and clears OF, SF and AF, so SF == OF.
                Cc::A => cmp(Cmp::FOgt, x, y),
                Cc::AE => cmp(Cmp::FOge, x, y),
                Cc::B => cmp(Cmp::FUlt, x, y),
                Cc::BE => cmp(Cmp::FUle, x, y),
                Cc::E | Cc::LE => cmp(Cmp::FUeq, x, y),
                Cc::NE | Cc::G => cmp(Cmp::FOne, x, y),
                Cc::P => cmp(Cmp::FUno, x, y),
                Cc::NP => cmp(Cmp::FOrd, x, y),
                Cc::S | Cc::O | Cc::L => Expr::Const(0, 1),
                Cc::NS | Cc::NO | Cc::GE => Expr::Const(1, 1),
                _ => return None,
            }),
            FlagKind::Bt => {
                let bit =
                    simplify_node(Expr::bin(Bin::And, simplify_node(Expr::bin(Bin::Shr, x, y)), Expr::Const(1, w)));
                match cc {
                    Cc::B => Some(cmp(Cmp::Ne, bit, zero(w))),
                    Cc::AE => Some(cmp(Cmp::Eq, bit, zero(w))),
                    _ => None,
                }
            }
            FlagKind::Res => fold_result(cc, x, false),
            FlagKind::ResLogic => fold_result(cc, x, true),
        };
    }
    if matches!(ops, Expr::Var(Var::FlagOps, _)) {
        if let Expr::Flags(kind @ (FlagKind::Res | FlagKind::ResLogic), r, _) = res {
            return fold_result(cc, (**r).clone(), *kind == FlagKind::ResLogic);
        }
    }
    None
}

/// Conditions that only need the result `r`; `logic` says carry and overflow are clear.
fn fold_result(cc: Cc, r: Expr, logic: bool) -> Option<Expr> {
    let w = r.bits();
    let op = match cc {
        Cc::E => Cmp::Eq,
        Cc::NE => Cmp::Ne,
        Cc::S => Cmp::Lt,
        Cc::NS => Cmp::Ge,
        Cc::L if logic => Cmp::Lt,
        Cc::GE if logic => Cmp::Ge,
        Cc::LE if logic => Cmp::Le,
        Cc::G if logic => Cmp::Gt,
        Cc::BE if logic => Cmp::Eq,
        Cc::A if logic => Cmp::Ne,
        Cc::B | Cc::O if logic => return Some(Expr::Const(0, 1)),
        Cc::AE | Cc::NO if logic => return Some(Expr::Const(1, 1)),
        _ => return None,
    };
    Some(cmp(op, r, zero(w)))
}

/// Make the whole expression smaller: children first.
pub fn simplify(mut e: Expr) -> Expr {
    e.rewrite(&mut |n| {
        let old = std::mem::replace(n, Expr::Unknown(0));
        *n = simplify_node(old);
    });
    e
}

/// Push a truncation into the expression when the low bits of the result only need the low bits
/// of the operands.
fn trunc_of(e: Expr, w: Bits) -> Expr {
    use Expr::*;
    let from = e.bits();
    if from == w {
        return e;
    }
    if from < w {
        return Zext(b(e), w);
    }
    match e {
        Const(v, _) => konst(v, w),
        // The low bits of the same vector register (a copy made by `movaps`).
        Var(self::Var::Xmm(n), _) => Var(self::Var::Xmm(n), w),
        Trunc(x, _) => trunc_of(*x, w),
        Zext(x, _) | Sext(x, _) if x.bits() >= w => trunc_of(*x, w),
        Zext(x, _) => Zext(x, w),
        Sext(x, _) => Sext(x, w),
        Insert(_, new) if new.bits() >= w => trunc_of(*new, w),
        Bin(
            op @ (self::Bin::Add | self::Bin::Sub | self::Bin::Mul | self::Bin::And | self::Bin::Or | self::Bin::Xor),
            x,
            y,
        ) if w >= 8 => simplify_node(Bin(op, b(trunc_of(*x, w)), b(trunc_of(*y, w)))),
        Bin(self::Bin::Shl, x, y) if w >= 8 && y.as_const().is_some_and(|c| c < w as u64) => {
            simplify_node(Bin(self::Bin::Shl, b(trunc_of(*x, w)), y))
        }
        Un(op @ (self::Un::Neg | self::Un::Not), x) if w >= 8 => simplify_node(Un(op, b(trunc_of(*x, w)))),
        Select(c, x, y) => Select(c, b(trunc_of(*x, w)), b(trunc_of(*y, w))),
        Load(a, _) if w >= 8 => Load(a, w),
        Unknown(_) => Unknown(w),
        other => Trunc(b(other), w),
    }
}

/// True when the value is known to fit `bits` bits (the rest are zero).
fn fits(e: &Expr, bits: Bits) -> bool {
    match e {
        Expr::Const(v, _) => *v <= mask(bits),
        Expr::Zext(x, _) => x.bits() <= bits,
        Expr::Cmp(..) | Expr::Cond(..) => true,
        _ => e.bits() <= bits,
    }
}

/// One step: the children are already as small as they get.
pub fn simplify_node(e: Expr) -> Expr {
    use Expr::*;
    match e {
        Const(v, w) => konst(v, w),
        Trunc(x, w) => trunc_of(*x, w),
        Zext(x, w) => match *x {
            _ if x.bits() >= w => trunc_of(*x, w),
            Const(v, _) => konst(v, w),
            Zext(y, _) => Zext(y, w),
            other => Zext(b(other), w),
        },
        Sext(x, w) => match *x {
            _ if x.bits() >= w => trunc_of(*x, w),
            Const(v, from) => konst(signed(v, from) as u64, w),
            Sext(y, _) => Sext(y, w),
            Zext(y, _) => Zext(y, w),
            other => Sext(b(other), w),
        },
        Insert(old, new) => {
            let w = old.bits();
            if new.bits() >= w {
                return trunc_of(*new, w);
            }
            match *old {
                ref o if fits(o, new.bits()) => simplify_node(Zext(new, w)),
                Insert(o, n) if n.bits() <= new.bits() => Insert(o, new),
                other => Insert(b(other), new),
            }
        }
        Un(op, x) => match (op, *x) {
            (self::Un::Neg, Const(v, w)) => konst(v.wrapping_neg(), w),
            (self::Un::Not, Const(v, w)) => konst(!v, w),
            (self::Un::LNot, Const(v, _)) => Const(u64::from(v == 0), 1),
            (self::Un::Neg, Un(self::Un::Neg, y))
            | (self::Un::Not, Un(self::Un::Not, y))
            | (self::Un::LNot, Un(self::Un::LNot, y)) => *y,
            (self::Un::LNot, Cmp(c, l, r)) => Cmp(c.negate(), l, r),
            (self::Un::LNot, Cond(cc, o, r)) => Cond(cc.negate(), o, r),
            (self::Un::LNot, Bin(self::Bin::LAnd, l, r)) => {
                Bin(self::Bin::LOr, b(simplify_node(Un(self::Un::LNot, l))), b(simplify_node(Un(self::Un::LNot, r))))
            }
            (self::Un::LNot, Bin(self::Bin::LOr, l, r)) => {
                Bin(self::Bin::LAnd, b(simplify_node(Un(self::Un::LNot, l))), b(simplify_node(Un(self::Un::LNot, r))))
            }
            (op, other) => Un(op, b(other)),
        },
        Bin(op, x, y) => simplify_bin(op, *x, *y),
        Cmp(op, x, y) => simplify_cmp(op, *x, *y),
        Cond(cc, o, r) => match fold_cond(cc, &o, &r) {
            Some(folded) => folded,
            None => Cond(cc, o, r),
        },
        Select(c, x, y) => match *c {
            Const(v, _) => {
                if v != 0 {
                    *x
                } else {
                    *y
                }
            }
            _ if x == y => *x,
            other => match float_select_as_logic(&other, &x, &y) {
                Some(folded) => folded,
                None => Select(b(other), x, y),
            },
        },
        Load(a, w) => Load(a, w),
        Intr("as_float", args, w) => fold_as_float(args, w),
        other => other,
    }
}

/// `as_float` of a value whose bits are visible: a constant, a mask that clears the sign, a mask that flips it.
fn fold_as_float(mut args: Vec<Expr>, w: Bits) -> Expr {
    use Expr::{Bin as B, Const, Intr};
    let (abs, sign) = if w == 32 { (0x7fff_ffffu64, 0x8000_0000u64) } else { (0x7fff_ffff_ffff_ffff, 1u64 << 63) };
    if args.len() != 1 || !(w == 32 || w == 64) {
        return Intr("as_float", args, w);
    }
    let a = args.remove(0);
    match a {
        Const(v, _) => Const(v & mask(w), w),
        B(op @ (Bin::And | Bin::Xor), x, y) => {
            let hit = match (&*x, &*y) {
                (Intr("as_int", xs, _), Const(c, _)) if xs.len() == 1 && xs[0].bits() == w => {
                    Some((xs[0].clone(), *c))
                }
                _ => None,
            };
            match hit {
                Some((v, c)) if op == Bin::And && c == abs => Intr(if w == 32 { "fabsf" } else { "fabs" }, vec![v], w),
                Some((v, c)) if op == Bin::Xor && c == sign => Expr::Un(Un::FNeg, b(v)),
                _ => Intr("as_float", vec![B(op, x, y)], w),
            }
        }
        other => Intr("as_float", vec![other], w),
    }
}

/// `c ? 0 : y` and its mirrors on a float compare with a 1-bit `y`: the logic the compiler spelled with `setcc` and
/// `cmovcc` (gcc writes `a == b` as `setnp` and `cmovne`).
fn float_select_as_logic(c: &Expr, x: &Expr, y: &Expr) -> Option<Expr> {
    let Expr::Cmp(op, ..) = c else { return None };
    if !op.is_float() || x.bits() != y.bits() {
        return None;
    }
    let w = x.bits();
    let bit = |e: &Expr| -> Option<Expr> {
        match e {
            Expr::Zext(i, _) if i.bits() == 1 => Some((**i).clone()),
            _ if e.bits() == 1 => Some(e.clone()),
            _ => None,
        }
    };
    let (k, other, const_first) = match (x, y) {
        (Expr::Const(k @ (0 | 1), _), o) => (*k, bit(o)?, true),
        (o, Expr::Const(k @ (0 | 1), _)) => (*k, bit(o)?, false),
        _ => return None,
    };
    // `c ? k : o` with the constant first; `c ? o : k` is `!c ? k : o`.
    let cond = if const_first { c.clone() } else { simplify_node(Expr::Un(Un::LNot, b(c.clone()))) };
    let logic = if k == 0 {
        simplify_node(Expr::bin(Bin::LAnd, simplify_node(Expr::Un(Un::LNot, b(cond))), other))
    } else {
        simplify_node(Expr::bin(Bin::LOr, cond, other))
    };
    Some(logic.zext(w))
}

fn simplify_bin(op: Bin, x: Expr, y: Expr) -> Expr {
    use self::Bin::*;
    use Expr::{Bin as B, Const, StackAddr};
    let w = x.bits();
    if let (Const(p, _), Const(q, _)) = (&x, &y) {
        if matches!(op, LAnd | LOr) {
            return Const(u64::from(if op == LAnd { *p != 0 && *q != 0 } else { *p != 0 || *q != 0 }), 1);
        }
        if let Some(v) = fold_bin(op, *p, *q, w) {
            return Const(v, w);
        }
    }
    // A constant goes to the right of an operator that allows it.
    if matches!(op, Add | Mul | And | Or | Xor) && matches!(x, Const(..)) && !matches!(y, Const(..)) {
        return simplify_bin(op, y, x);
    }
    match (op, x, y) {
        (Sub, x, Const(c, cw)) => simplify_bin(Add, x, konst(c.wrapping_neg(), cw)),
        (Sub, x, y) if x == y && !x.has_call() => Const(0, w),
        (Sub, StackAddr(p), StackAddr(q)) => Const((p - q) as u64, 64),
        (Add | Or | Xor | Shl | Shr | Sar, x, Const(0, _)) => x,
        (Add, StackAddr(p), Const(c, _)) => StackAddr(p.wrapping_add(c as i64)),
        (Add, B(Add, p, q), Const(c, _)) if matches!(*q, Const(..)) => {
            let Const(d, _) = *q else { unreachable!() };
            simplify_bin(Add, *p, konst(c.wrapping_add(d), w))
        }
        // (x + c) + y: keep the constant outside.
        (Add, B(Add, p, q), y) if matches!(*q, Const(..)) => simplify_bin(Add, simplify_bin(Add, *p, y), *q),
        (Add, x, B(Add, p, q)) if matches!(*q, Const(..)) => simplify_bin(Add, simplify_bin(Add, x, *p), *q),
        (Sub, B(Add, p, q), y) if matches!(*q, Const(..)) => simplify_bin(Add, simplify_bin(Sub, *p, y), *q),
        (Mul, x, Const(1, _)) => x,
        (Mul | And, _, Const(0, _)) => Const(0, w),
        (Mul, B(Mul, p, q), Const(c, _)) if matches!(*q, Const(..)) => {
            let Const(d, _) = *q else { unreachable!() };
            simplify_bin(Mul, *p, konst(c.wrapping_mul(d), w))
        }
        (And, x, Const(c, _)) if c == mask(w) => x,
        (And, x, Const(c, _)) if matches!(x, Expr::Zext(ref i, _) if c & mask(i.bits()) == mask(i.bits())) => x,
        (And | Or, x, y) if x == y && !x.has_call() => x,
        (Xor, x, y) if x == y && !x.has_call() => Const(0, w),
        (LAnd, Const(c, _), y) => {
            if c != 0 {
                y
            } else {
                Const(0, 1)
            }
        }
        (LOr, Const(c, _), y) => {
            if c != 0 {
                Const(1, 1)
            } else {
                y
            }
        }
        (LAnd, x, Const(c, _)) if c != 0 => x,
        (LOr, x, Const(0, _)) => x,
        // `jp` then `je` (or `setnp` then `cmovne`): an ordered test joined with the unordered one is the ordered one.
        (op @ (LAnd | LOr), Expr::Cmp(c1, a1, q1), Expr::Cmp(c2, a2, q2))
            if c1.is_float() && c2.is_float() && ((a1 == a2 && q1 == q2) || (a1 == q2 && q1 == a2)) =>
        {
            // the swapped operand order is the same test with the predicate mirrored
            let c2 = if a1 == a2 && q1 == q2 { c2 } else { swap_float(c2) };
            match fuse_float(op, c1, c2) {
                Some(Ok(c)) => Expr::Cmp(c, a1, q1),
                Some(Err(v)) => Const(u64::from(v), 1),
                None => B(op, b(Expr::Cmp(c1, a1, q1)), b(Expr::Cmp(c2, a2, q2))),
            }
        }
        (op, x, y) => B(op, b(x), b(y)),
    }
}

/// The predicate that tests `(q, a)` when `c` tests `(a, q)`.
fn swap_float(c: Cmp) -> Cmp {
    use Cmp::*;
    match c {
        FOlt => FOgt,
        FOgt => FOlt,
        FOle => FOge,
        FOge => FOle,
        FUlt => FUgt,
        FUgt => FUlt,
        FUle => FUge,
        FUge => FUle,
        other => other,
    }
}

/// `FOrd(a, b) && P(a, b)` is the ordered form of `P`; `FUno(a, b) || P(a, b)` is the unordered form of `P`.
/// `Err` is a constant outcome.
fn fuse_float(op: Bin, c1: Cmp, c2: Cmp) -> Option<Result<Cmp, bool>> {
    use Cmp::*;
    // The other predicate, when one of the two is `t`.
    let pick = |t: Cmp| if c1 == t { Some(c2) } else if c2 == t { Some(c1) } else { None };
    match op {
        Bin::LAnd => Some(match pick(FOrd)? {
            FUeq => Ok(FOeq),
            FUlt => Ok(FOlt),
            FUle => Ok(FOle),
            FUgt => Ok(FOgt),
            FUge => Ok(FOge),
            FUne => Ok(FOne),
            FUno => Err(false),
            other => Ok(other),
        }),
        Bin::LOr => Some(match pick(FUno)? {
            FOne => Ok(FUne),
            FOeq => Ok(FUeq),
            FOlt => Ok(FUlt),
            FOle => Ok(FUle),
            FOgt => Ok(FUgt),
            FOge => Ok(FUge),
            FOrd => Err(true),
            other => Ok(other),
        }),
        _ => None,
    }
}

fn simplify_cmp(op: Cmp, x: Expr, y: Expr) -> Expr {
    use Expr::{Bin as B, Const};
    let w = x.bits();
    if let (Const(p, _), Const(q, _)) = (&x, &y) {
        if let Some(v) = fold_cmp(op, *p, *q, w) {
            return Const(u64::from(v), 1);
        }
    }
    let eq = matches!(op, Cmp::Eq | Cmp::Ne);
    match (op, x, y) {
        // A condition tested against zero is the condition itself.
        (Cmp::Ne, x, Const(0, _)) if x.bits() == 1 => x,
        (Cmp::Eq, x, Const(0, _)) if x.bits() == 1 => simplify_node(Expr::Un(Un::LNot, b(x))),
        (Cmp::Ne, Expr::Zext(i, _), Const(0, _)) if i.bits() == 1 => *i,
        (Cmp::Eq, Expr::Zext(i, _), Const(0, _)) if i.bits() == 1 => simplify_node(Expr::Un(Un::LNot, i)),
        (_, B(Bin::Sub, p, q), Const(0, _)) if eq => simplify_cmp(op, *p, *q),
        (_, B(Bin::Xor, p, q), Const(0, _)) if eq => simplify_cmp(op, *p, *q),
        (_, B(Bin::Add, p, q), Const(c, cw)) if eq && matches!(*q, Const(..)) => {
            let Const(d, _) = *q else { unreachable!() };
            simplify_cmp(op, *p, konst(c.wrapping_sub(d), cw))
        }
        (_, Expr::Zext(i, _), Const(c, _)) if (eq || op.is_unsigned()) && c <= mask(i.bits()) && i.bits() >= 8 => {
            let iw = i.bits();
            simplify_cmp(op, *i, Const(c, iw))
        }
        (_, Expr::Sext(i, _), Const(c, cw))
            if (eq || op.is_signed()) && i.bits() >= 8 && {
                let s = signed(c, cw);
                s == signed(c & mask(i.bits()), i.bits())
            } =>
        {
            let iw = i.bits();
            simplify_cmp(op, *i, konst(c, iw))
        }
        (Cmp::Ult, x, Const(1, cw)) | (Cmp::Ule, x, Const(0, cw)) => Expr::Cmp(Cmp::Eq, b(x), b(Const(0, cw))),
        (Cmp::Uge, x, Const(1, cw)) | (Cmp::Ugt, x, Const(0, cw)) => Expr::Cmp(Cmp::Ne, b(x), b(Const(0, cw))),
        (op, x, y) => Expr::Cmp(op, b(x), b(y)),
    }
}
