//! Values shown in the program's own words.

use super::*;

impl Vars {
    fn bytes(&self, v: &Val, regs: &mut Regs) -> Option<Vec<u8>> {
        let n = self.size(v.ty).min(1 << 16) as usize;
        match &v.at {
            At::Mem(a) => (regs.rd)(*a, n),
            At::Bytes(b) => {
                let mut b = b.clone();
                b.resize(n.max(b.len().min(n)), 0);
                Some(b)
            }
        }
    }

    pub(super) fn int(&self, v: &Val, regs: &mut Regs) -> Option<i64> {
        let b = self.bytes(v, regs)?;
        let signed = matches!(self.strip(v.ty.0), Some(Ty::Base { enc: 5 | 6, .. }) | Some(Ty::Enum { .. }));
        Some(le(&b, signed))
    }

    /// The value as text, at most `MAX_TEXT` long; `depth` 0 is the top.
    pub(super) fn render(&self, v: &Val, regs: &mut Regs, depth: usize) -> String {
        let mut s = self.render_inner(v, regs, depth);
        if s.len() > MAX_TEXT {
            let cut = (0..=MAX_TEXT).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);
            s.truncate(cut);
            s.push_str("...");
        }
        s
    }

    fn render_inner(&self, v: &Val, regs: &mut Regs, depth: usize) -> String {
        let Some(ty) = self.strip(v.ty.0) else { return "?".into() };
        if let Ty::Array { elem, dims } = ty {
            let dims = &dims[v.ty.1.min(dims.len())..];
            let count = dims.first().copied().flatten();
            let ev = if dims.len() > 1 { View(v.ty.0, v.ty.1 + 1) } else { View(*elem, 0) };
            let esz = self.size(ev);
            if dims.len() == 1 && self.char_like(*elem) {
                let Some(b) = self.bytes(v, regs) else { return "<unreadable>".into() };
                return quote(&b, true);
            }
            if depth > 2 {
                return "{...}".into();
            }
            let n = count.unwrap_or(0) as usize;
            let mut parts = Vec::new();
            for i in 0..n.min(SHOWN) {
                let at = match &v.at {
                    At::Mem(a) => At::Mem(a.wrapping_add((i as u64).wrapping_mul(esz))),
                    At::Bytes(b) => At::Bytes(b.get((i as u64).saturating_mul(esz) as usize..).unwrap_or(&[]).to_vec()),
                };
                parts.push(self.render_inner(&Val { at, ty: ev }, regs, depth + 1));
            }
            if n > SHOWN {
                parts.push(format!("... ({n} in all)"));
            }
            return format!("{{{}}}", parts.join(", "));
        }
        let Some(b) = self.bytes(v, regs) else { return "<unreadable>".into() };
        match ty {
            Ty::Base { enc, size, .. } => match (enc, size) {
                (2, _) => (le(&b, false) != 0).to_string(),
                (4, 4) => f32::from_bits(le(&b, false) as u32).to_string(),
                (4, 8) => f64::from_bits(le(&b, false) as u64).to_string(),
                (6 | 8, 1) => {
                    let c = le(&b, *enc == 6);
                    format!("{c} {}", quote(&[c as u8], false).replace('"', "'"))
                }
                (5, _) => le(&b, true).to_string(),
                (7, _) => (le(&b, false) as u64).to_string(),
                _ => format!("{:#x}", le(&b, false) as u64),
            },
            Ty::Ptr { to } => {
                let p = le(&b, false) as u64;
                if p == 0 {
                    return "NULL".into();
                }
                if self.char_like(*to) {
                    return match read_c(regs, p) {
                        Some(t) => format!("{p:#x} {t}"),
                        None => format!("{p:#x} <unreadable>"),
                    };
                }
                if depth == 0 && matches!(self.strip(*to), Some(Ty::Record { .. })) {
                    let inner = self.render_inner(&Val { at: At::Mem(p), ty: View(*to, 0) }, regs, depth + 1);
                    return format!("{p:#x} -> {inner}");
                }
                format!("{p:#x}")
            }
            Ty::Record { members, .. } => {
                if depth > 2 {
                    return "{...}".into();
                }
                let parts: Vec<String> = members
                    .iter()
                    .map(|(n, off, t)| {
                        let at = match &v.at {
                            At::Mem(a) => At::Mem(a.wrapping_add(*off)),
                            At::Bytes(b) => At::Bytes(b.get(*off as usize..).unwrap_or(&[]).to_vec()),
                        };
                        format!("{n} = {}", self.render_inner(&Val { at, ty: View(*t, 0) }, regs, depth + 1))
                    })
                    .collect();
                format!("{{{}}}", parts.join(", "))
            }
            Ty::Enum { vals, .. } => {
                let x = le(&b, true);
                vals.iter().find(|(_, v)| *v == x).map_or(x.to_string(), |(n, _)| n.clone())
            }
            _ => format!("{:#x}", le(&b, false) as u64),
        }
    }
}

/// Little-endian bytes as a number, sign-extended when `signed`.
pub(super) fn le(b: &[u8], signed: bool) -> i64 {
    let n = b.len().min(8);
    let mut x = [0u8; 8];
    x[..n].copy_from_slice(&b[..n]);
    let v = u64::from_le_bytes(x);
    if signed && n > 0 && n < 8 {
        let shift = 64 - 8 * n as u32;
        ((v << shift) as i64) >> shift
    } else {
        v as i64
    }
}

/// Bytes as a C string literal; with `nul` it ends at the first NUL and says when there is none.
pub(super) fn quote(b: &[u8], nul: bool) -> String {
    let end = if nul { b.iter().position(|&c| c == 0) } else { None };
    let body = &b[..end.unwrap_or(b.len())];
    let mut s = String::from("\"");
    for &c in body {
        match c {
            b'"' => s.push_str("\\\""),
            b'\\' => s.push_str("\\\\"),
            b'\n' => s.push_str("\\n"),
            b'\t' => s.push_str("\\t"),
            0x20..=0x7e => s.push(c as char),
            _ => s.push_str(&format!("\\x{c:02x}")),
        }
    }
    s.push('"');
    if nul && end.is_none() {
        s.push_str(&format!(" (no NUL in its {} bytes)", b.len()));
    }
    s
}

fn read_c(regs: &mut Regs, p: u64) -> Option<String> {
    let b = (regs.rd)(p, 80).or_else(|| (regs.rd)(p, 1))?;
    let mut t = quote(&b, true);
    if b.len() == 80 && !b.contains(&0) {
        t = format!("{}...\"", &t[..t.find(" (no NUL").unwrap_or(t.len()) - 1]);
    }
    Some(t)
}
