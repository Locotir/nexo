//! The value a function returned, read from the ABI's return registers once `finish` is back in the caller: System V
//! x86-64 (rax, rdx, xmm0, xmm1, st0), Windows x64 (rax, xmm0) and a 32-bit program's eax, edx and st0.

use super::*;

/// The registers a return value can be in.
pub(crate) struct RetRegs {
    pub rax: u64,
    pub rdx: u64,
    /// xmm0 and xmm1, when the target gives vector registers.
    pub xmm: [Option<[u8; 16]>; 2],
    /// The top of the x87 stack, when it holds a value.
    pub st0: Option<f64>,
}

/// What the function's debug info and the registers say it returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Returned {
    /// No debug info for the function, or a type this reader does not place: only the raw registers are known.
    Unknown,
    /// `void`.
    Void,
    Value {
        text: String,
        ty: String,
    },
    /// A struct too big for registers: the caller gave the address to write it to, and rax holds it again.
    Memory {
        ty: String,
        size: u64,
        at: u64,
        text: Option<String>,
    },
}

/// The raw registers, for a function without a known return type.
pub(crate) fn raw_line(rr: &RetRegs) -> String {
    let mut s = format!("returned (raw, no type info): rax {:#x} ({})", rr.rax, rr.rax as i64);
    if let Some(x) = rr.xmm[0] {
        let d = f64::from_bits(u64::from_le_bytes([x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7]]));
        s.push_str(&format!(", xmm0 as double {}", crate::vector::float_text(d)));
    }
    s
}

/// The line `finish` adds to its stop; `None` for a `void` function.
pub(crate) fn returned_line(r: &Returned, rr: &RetRegs) -> Option<String> {
    match r {
        Returned::Void => None,
        Returned::Unknown => Some(raw_line(rr)),
        Returned::Value { text, ty } => Some(format!("returned: {text} ({ty})")),
        Returned::Memory { ty, size, at, text: Some(t) } => {
            Some(format!("returned: {t} ({ty}, {size} bytes, returned through memory at {at:#x}, the address in rax)"))
        }
        Returned::Memory { ty, size, at, text: None } => Some(format!(
            "returned: a {ty} of {size} bytes, returned through memory at {at:#x} (the address in rax); `mem {at:#x} {size}` reads it"
        )),
    }
}

/// An integer-class value of `s` bytes out of the integer return registers.
fn int_bytes(rr: &RetRegs, x32: bool, win: bool, s: usize) -> Option<Vec<u8>> {
    let (a, d) = (rr.rax.to_le_bytes(), rr.rdx.to_le_bytes());
    match s {
        1..=4 => Some(a[..s].to_vec()),
        8 if x32 => Some([&a[..4], &d[..4]].concat()),
        5..=8 if !x32 => Some(a[..s].to_vec()),
        16 if !x32 && !win => Some([a, d].concat()),
        _ => None,
    }
}

impl Vars {
    fn show(&self, bytes: Vec<u8>, ty: View, regs: &mut Regs) -> String {
        self.render(&Val { at: At::Bytes(bytes), ty }, regs, 0)
    }

    /// The scalar pieces of a type as (offset, size, is floating point); `false` when a piece is not one the
    /// classification below can place (a long double, a complex number, damaged debug info).
    fn leaves(&self, t: Option<u64>, off: u64, out: &mut Vec<(u64, u64, bool)>, depth: usize) -> bool {
        if depth > MAX_TYPE_DEPTH || out.len() > 64 {
            return false;
        }
        match self.strip(t) {
            Some(Ty::Base { enc: 4, size, .. }) if *size <= 8 => out.push((off, *size, true)),
            Some(Ty::Base { enc: 3 | 4, .. }) => return false,
            Some(Ty::Base { size, .. } | Ty::Enum { size, .. }) => out.push((off, *size, false)),
            Some(Ty::Ptr { .. }) => out.push((off, self.ptr, false)),
            Some(Ty::Record { members, .. }) => {
                for (_, moff, mt) in members {
                    if !self.leaves(*mt, off + moff, out, depth + 1) {
                        return false;
                    }
                }
            }
            Some(Ty::Array { elem, dims }) => {
                let mut n = 1u64;
                for d in dims {
                    let Some(d) = d else { return false };
                    n = n.saturating_mul(*d);
                }
                let es = self.size(View(*elem, 0));
                for i in 0..n.min(16) {
                    if !self.leaves(*elem, off + i * es, out, depth + 1) {
                        return false;
                    }
                }
            }
            _ => return false,
        }
        true
    }

    /// A System V struct of 16 bytes or less: each eightbyte is in an SSE register when everything in it is floating
    /// point and in the next integer register otherwise.
    fn small_struct_bytes(&self, t: Option<u64>, size: u64, rr: &RetRegs) -> Option<Vec<u8>> {
        let mut ls = Vec::new();
        if !self.leaves(t, 0, &mut ls, 0) {
            return None;
        }
        let (mut next_int, mut next_sse) = (0usize, 0usize);
        let mut out = Vec::new();
        for k in 0..size.div_ceil(8) {
            let (lo, hi) = (k * 8, k * 8 + 8);
            let here: Vec<_> = ls.iter().filter(|(o, s, _)| *o < hi && o + s > lo).collect();
            let sse = !here.is_empty() && here.iter().all(|l| l.2);
            if sse {
                let x = rr.xmm.get(next_sse).copied().flatten()?;
                next_sse += 1;
                out.extend_from_slice(&x[..8]);
            } else {
                let r = [rr.rax, rr.rdx].get(next_int).copied()?;
                next_int += 1;
                out.extend_from_slice(&r.to_le_bytes());
            }
        }
        out.truncate(size as usize);
        Some(out)
    }

    /// What the function holding `pc` (a file address) returned, from its DWARF return type and `rr`.
    pub(super) fn returned(&self, pc: u64, win: bool, rr: &RetRegs, regs: &mut Regs) -> Returned {
        let Some(f) = self.func_at(pc) else { return Returned::Unknown };
        let Some(rt) = f.ret else { return Returned::Void };
        let (ty, x32) = (View(Some(rt), 0), f.enc.address_size == 4);
        let (ty_name, size) = (self.name(ty), self.size(ty));
        let value = |text: String| Returned::Value { text, ty: ty_name.clone() };
        match self.strip(Some(rt)) {
            Some(Ty::Base { enc: 4, size: s, .. }) if x32 || *s > 8 => match rr.st0 {
                Some(x) => value(crate::vector::float_text(x)),
                None => Returned::Unknown,
            },
            Some(Ty::Base { enc: 4, size: s, .. }) => match rr.xmm[0] {
                Some(x) => value(self.show(x[..(*s as usize).min(8)].to_vec(), ty, regs)),
                None => Returned::Unknown,
            },
            Some(Ty::Base { enc: 3, .. }) => Returned::Unknown,
            Some(Ty::Base { .. } | Ty::Enum { .. } | Ty::Ptr { .. }) => match int_bytes(rr, x32, win, size as usize) {
                Some(b) => value(self.show(b, ty, regs)),
                None => Returned::Unknown,
            },
            Some(Ty::Record { .. }) if x32 => Returned::Unknown,
            Some(Ty::Record { .. }) => {
                let in_memory = if win { ![1, 2, 4, 8].contains(&size) } else { size > 16 };
                if in_memory {
                    let at = rr.rax;
                    let text = (size <= 4096).then(|| self.render(&Val { at: At::Mem(at), ty }, regs, 0));
                    let text = text.filter(|t| t != "<unreadable>");
                    return Returned::Memory { ty: ty_name.clone(), size, at, text };
                }
                let bytes = if win {
                    Some(rr.rax.to_le_bytes()[..size as usize].to_vec())
                } else {
                    self.small_struct_bytes(Some(rt), size, rr)
                };
                match bytes {
                    Some(b) => value(self.show(b, ty, regs)),
                    None => Returned::Unknown,
                }
            }
            _ => Returned::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regs(rax: u64, xmm0: Option<f64>) -> RetRegs {
        let x = xmm0.map(|d| {
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&d.to_bits().to_le_bytes());
            b
        });
        RetRegs { rax, rdx: 0, xmm: [x, None], st0: None }
    }

    #[test]
    fn an_int_a_double_and_a_struct_in_memory_read_as_the_value_with_its_type() {
        let rr = regs(42, Some(2.5));
        let int = Returned::Value { text: "42".into(), ty: "int".into() };
        assert_eq!(returned_line(&int, &rr).as_deref(), Some("returned: 42 (int)"));
        let dbl = Returned::Value { text: "2.5".into(), ty: "double".into() };
        assert_eq!(returned_line(&dbl, &rr).as_deref(), Some("returned: 2.5 (double)"));
        let mem = Returned::Memory { ty: "struct big".into(), size: 40, at: 0x7ffd_1000, text: None };
        let l = returned_line(&mem, &rr).unwrap();
        assert!(
            l.contains("struct big of 40 bytes") && l.contains("through memory at 0x7ffd1000") && l.contains("rax"),
            "{l}"
        );
        let shown =
            Returned::Memory { ty: "struct big".into(), size: 40, at: 0x7ffd_1000, text: Some("{a = 1}".into()) };
        let l = returned_line(&shown, &rr).unwrap();
        assert!(l.starts_with("returned: {a = 1} (struct big, 40 bytes, returned through memory at 0x7ffd1000"), "{l}");
    }

    #[test]
    fn void_says_nothing_and_an_unknown_type_shows_the_raw_registers() {
        let rr = regs(0xffff_ffff_ffff_ffff, Some(1.5));
        assert_eq!(returned_line(&Returned::Void, &rr), None);
        assert_eq!(
            returned_line(&Returned::Unknown, &rr).as_deref(),
            Some("returned (raw, no type info): rax 0xffffffffffffffff (-1), xmm0 as double 1.5")
        );
        let no_vector = regs(7, None);
        assert_eq!(raw_line(&no_vector), "returned (raw, no type info): rax 0x7 (7)");
    }

    #[test]
    fn integer_values_come_from_rax_and_rdx_by_size() {
        let rr = RetRegs { rax: 0x1122_3344_5566_7788, rdx: 0x99aa_bbcc_ddee_ff00, xmm: [None, None], st0: None };
        assert_eq!(int_bytes(&rr, false, false, 4), Some(vec![0x88, 0x77, 0x66, 0x55]));
        assert_eq!(int_bytes(&rr, false, false, 8).map(|b| b.len()), Some(8));
        assert_eq!(int_bytes(&rr, false, false, 16).map(|b| b[8]), Some(0x00));
        assert_eq!(int_bytes(&rr, false, true, 16), None);
        assert_eq!(int_bytes(&rr, true, false, 8), Some(vec![0x88, 0x77, 0x66, 0x55, 0x00, 0xff, 0xee, 0xdd]));
    }
}
