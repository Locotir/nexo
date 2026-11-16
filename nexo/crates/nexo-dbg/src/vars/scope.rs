//! Which variables are in scope at a stop, and where their values are.

use super::entry;
use super::render::le;
use super::*;

impl Vars {
    pub(super) fn func_at(&self, pc: u64) -> Option<&Func> {
        self.funcs
            .iter()
            .filter(|f| f.ranges.iter().any(|&(lo, hi)| pc >= lo && pc < hi))
            .min_by_key(|f| f.ranges.iter().map(|(lo, hi)| hi - lo).sum::<u64>())
    }

    /// The variables in scope at `pc` (a file address): parameters first, then locals; an inner one hides an
    /// outer one of the same name.
    pub(super) fn in_scope(&self, pc: u64) -> Vec<(&Var, gimli::Encoding, Option<&[u8]>)> {
        let Some(f) = self.func_at(pc) else { return Vec::new() };
        let mut out: Vec<(&Var, gimli::Encoding, Option<&[u8]>)> = Vec::new();
        for v in &f.vars {
            if v.scope.as_ref().is_some_and(|r| !r.iter().any(|&(lo, hi)| pc >= lo && pc < hi)) {
                continue;
            }
            out.retain(|(o, _, _)| o.name != v.name);
            out.push((v, f.enc, f.frame_base.as_deref()));
        }
        out.sort_by_key(|(v, _, _)| !v.param);
        out
    }

    /// The value of a variable at this stop: `None` when it has no place here (optimised out, or not live).
    pub(super) fn value(&self, var: &Var, enc: gimli::Encoding, fb: Option<&[u8]>, regs: &mut Regs) -> Option<At> {
        let pc = regs.rip.wrapping_sub(regs.bias);
        let expr: &[u8] = match &var.loc {
            // before the prologue ends a stack slot holds what an earlier call left there
            Loc::Expr(e) if regs.early.is_some() && entry::frame_based(e) => return None,
            Loc::Expr(e) => e,
            Loc::List(l) => &l.iter().find(|(lo, hi, _)| pc >= *lo && pc < *hi)?.2,
            Loc::None => return None,
        };
        eval(expr, enc, fb, regs)
    }

    /// The variable `name` at this stop, with why its value is not its own yet when that is known: a local in
    /// scope, else a global, else a local of a block that does not hold the stop (the block that starts last
    /// before the stop, else the first one).
    pub(super) fn find(&self, name: &str, regs: &mut Regs) -> Option<(Val, Option<String>)> {
        let pc = regs.rip.wrapping_sub(regs.bias);
        let local = self.in_scope(pc).into_iter().find(|(v, _, _)| v.name == name);
        if let Some((v, enc, fb)) = local {
            let why = self.unassigned(v, regs);
            return Some((Val { at: self.value(v, enc, fb, regs)?, ty: View(v.ty, 0) }, why));
        }
        if let Some((v, enc)) = self.globals.iter().find(|(v, _)| v.name == name) {
            return Some((Val { at: self.value(v, *enc, None, regs)?, ty: View(v.ty, 0) }, None));
        }
        let gone: Vec<_> = self.out_of_block(pc).into_iter().filter(|(v, _, _)| v.name == name).collect();
        let start = |v: &Var| v.scope.as_ref().and_then(|r| r.iter().map(|r| r.0).min()).unwrap_or(0);
        let (v, enc, fb) =
            *gone.iter().filter(|(v, _, _)| start(v) <= pc).max_by_key(|(v, _, _)| start(v)).or(gone.first())?;
        let mut why = self.unassigned(v, regs);
        if let (Some(w), n @ 2..) = (why.as_mut(), gone.len()) {
            w.push_str(&format!("; {} more named {name} in other blocks: `locals` lists them", n - 1));
        }
        Some((Val { at: self.value(v, enc, fb, regs)?, ty: View(v.ty, 0) }, why))
    }
}

pub(super) fn clone_at(a: &At) -> At {
    match a {
        At::Mem(x) => At::Mem(*x),
        At::Bytes(b) => At::Bytes(b.clone()),
    }
}

/// Runs a DWARF location expression against the stopped program.
pub(super) fn eval(expr: &[u8], enc: gimli::Encoding, fb: Option<&[u8]>, regs: &mut Regs) -> Option<At> {
    let reg = |r: &Regs, n: u16| -> Option<u64> {
        match n {
            16 => Some(r.rip),
            n => {
                let i = *crate::modules::DW2GP.get(n as usize)?;
                (r.lost >> i & 1 == 0).then_some(r.gp[i])
            }
        }
    };
    let mut e = gimli::Expression(EndianSlice::new(expr, LittleEndian)).evaluation(enc);
    let mut res = e.evaluate().ok()?;
    loop {
        res = match res {
            EvaluationResult::Complete => break,
            EvaluationResult::RequiresMemory { address, size, .. } => {
                let b = (regs.rd)(address, size as usize)?;
                e.resume_with_memory(Value::Generic(le(&b, false) as u64)).ok()?
            }
            EvaluationResult::RequiresRegister { register, .. } => {
                e.resume_with_register(Value::Generic(reg(regs, register.0)?)).ok()?
            }
            EvaluationResult::RequiresFrameBase => {
                let base = match eval(fb?, enc, None, regs)? {
                    At::Mem(a) => a,
                    At::Bytes(b) => le(&b, false) as u64,
                };
                e.resume_with_frame_base(base).ok()?
            }
            EvaluationResult::RequiresCallFrameCfa => e.resume_with_call_frame_cfa(regs.cfa?).ok()?,
            EvaluationResult::RequiresRelocatedAddress(a) => {
                e.resume_with_relocated_address(a.wrapping_add(regs.bias)).ok()?
            }
            _ => return None,
        };
    }
    let pieces = e.result();
    if pieces.len() == 1 && pieces[0].size_in_bits.is_none() {
        return match &pieces[0].location {
            Location::Address { address } => Some(At::Mem(*address)),
            Location::Register { register } => Some(At::Bytes(reg(regs, register.0)?.to_le_bytes().to_vec())),
            Location::Value { value } => Some(At::Bytes(value.to_u64(!0).ok()?.to_le_bytes().to_vec())),
            Location::Bytes { value } => Some(At::Bytes(value.slice().to_vec())),
            _ => None,
        };
    }
    // a value in pieces (a struct split over registers): its bytes one piece after another
    let mut out = Vec::new();
    for p in &pieces {
        let n = p.size_in_bits.map_or(8, |b| b.div_ceil(8)).min(1 << 16) as usize;
        let b = match &p.location {
            Location::Address { address } => (regs.rd)(*address, n)?,
            Location::Register { register } => reg(regs, register.0)?.to_le_bytes().to_vec(),
            Location::Value { value } => value.to_u64(!0).ok()?.to_le_bytes().to_vec(),
            _ => return None,
        };
        out.extend(b.into_iter().chain(std::iter::repeat(0)).take(n));
    }
    Some(At::Bytes(out))
}
