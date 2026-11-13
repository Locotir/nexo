//! Paths like `p->tag` and `rows[2][1]`, walked from a variable to the value they name.

use super::scope::clone_at;
use super::*;

impl Vars {
    /// `name`, `*p`, `&x`, `p->tag`, `s.x`, `rows[y][2]`: `None` when the first name is no variable here.
    pub(super) fn path(&self, src: &str, regs: &mut Regs) -> Option<std::result::Result<(Val, bool), String>> {
        let s = src.trim();
        let stars = s.len() - s.trim_start_matches('*').len();
        let s = s[stars..].trim_start();
        let (amp, s) = match s.strip_prefix('&') {
            Some(r) => (true, r.trim_start()),
            None => (false, s),
        };
        let end = s.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(s.len());
        let (head, mut rest) = s.split_at(end);
        if head.is_empty() || head.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        let mut v = match self.find(head, regs) {
            Some(v) => v,
            None => {
                let pc = regs.rip.wrapping_sub(regs.bias);
                let known = self.in_scope(pc).iter().any(|(v, _, _)| v.name == head);
                return known.then(|| Err(format!("{head} has no place here (optimised out, or not live yet)")));
            }
        };
        let mut notes = Vec::new();
        loop {
            rest = rest.trim_start();
            if rest.is_empty() {
                break;
            }
            if let Some(r) = rest.strip_prefix("->") {
                v = match self.deref(&v, regs) {
                    Ok(x) => x,
                    Err(e) => return Some(Err(e)),
                };
                rest = r;
                rest = rest.trim_start();
            } else if let Some(r) = rest.strip_prefix('.') {
                rest = r.trim_start();
            } else if let Some(r) = rest.strip_prefix('[') {
                let Some(close) = r.find(']') else { return Some(Err("a `[` without its `]`".into())) };
                let inner = &r[..close];
                rest = &r[close + 1..];
                let i = match nexo_bin::parse_addr(inner.trim()) {
                    Ok(n) => n as i64,
                    Err(_) => match self.path(inner, regs) {
                        Some(Ok((x, _))) => match self.int(&x, regs) {
                            Some(n) => n,
                            None => return Some(Err(format!("{inner} is not a number"))),
                        },
                        Some(Err(e)) => return Some(Err(e)),
                        None => return Some(Err(format!("{inner}: no such variable here"))),
                    },
                };
                v = match self.index(v, i, &mut notes, regs) {
                    Ok(x) => x,
                    Err(e) => return Some(Err(e)),
                };
                continue;
            } else {
                return Some(Err(format!("cannot read {rest:?}: use name, *p, &x, p->f, s.f, a[i]")));
            }
            let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
            let (field, r) = rest.split_at(end);
            rest = r;
            v = match self.member(v, field) {
                Ok(x) => x,
                Err(e) => return Some(Err(e)),
            };
        }
        for _ in 0..stars {
            v = match self.deref(&v, regs) {
                Ok(x) => x,
                Err(e) => return Some(Err(e)),
            };
        }
        NOTES.with(|n| *n.borrow_mut() = notes);
        Some(Ok((v, amp)))
    }

    fn deref(&self, v: &Val, regs: &mut Regs) -> std::result::Result<Val, String> {
        match self.strip(v.ty.0) {
            Some(Ty::Ptr { to }) => {
                let p = self.int(v, regs).ok_or("the pointer cannot be read")? as u64;
                if p == 0 {
                    return Err("the pointer is NULL".into());
                }
                Ok(Val { at: At::Mem(p), ty: View(*to, 0) })
            }
            Some(Ty::Array { .. }) => self.index(Val { at: clone_at(&v.at), ty: v.ty }, 0, &mut Vec::new(), regs),
            _ => Err(format!("{} is not a pointer", self.name(v.ty))),
        }
    }

    fn member(&self, v: Val, field: &str) -> std::result::Result<Val, String> {
        let Some(Ty::Record { members, .. }) = self.strip(v.ty.0) else {
            return Err(format!("{} has no fields (`->` follows a pointer)", self.name(v.ty)));
        };
        let Some((_, off, t)) = members.iter().find(|(n, _, _)| n == field) else {
            let names: Vec<&str> = members.iter().map(|(n, _, _)| n.as_str()).collect();
            return Err(format!("{} has no field {field}; its fields: {}", self.name(v.ty), names.join(", ")));
        };
        let at = match v.at {
            At::Mem(a) => At::Mem(a.wrapping_add(*off)),
            At::Bytes(b) => At::Bytes(b.get(*off as usize..).unwrap_or(&[]).to_vec()),
        };
        Ok(Val { at, ty: View(*t, 0) })
    }

    fn index(&self, v: Val, i: i64, notes: &mut Vec<String>, regs: &mut Regs) -> std::result::Result<Val, String> {
        match self.strip(v.ty.0) {
            Some(Ty::Array { elem, dims }) => {
                let left = &dims[v.ty.1.min(dims.len())..];
                let ev = if left.len() > 1 { View(v.ty.0, v.ty.1 + 1) } else { View(*elem, 0) };
                if let Some(Some(n)) = left.first() {
                    if i < 0 || i as u64 >= *n {
                        notes.push(format!("index {i} is outside the array ({n} elements): this reads past its end"));
                    }
                }
                let esz = self.size(ev) as i64;
                let at = match v.at {
                    At::Mem(a) => At::Mem(a.wrapping_add(i.wrapping_mul(esz) as u64)),
                    At::Bytes(_) => return Err("this array is not in memory".into()),
                };
                Ok(Val { at, ty: ev })
            }
            Some(Ty::Ptr { to }) => {
                let p = self.int(&v, regs).ok_or("the pointer cannot be read")? as u64;
                let esz = self.size(View(*to, 0)) as i64;
                Ok(Val { at: At::Mem(p.wrapping_add(i.wrapping_mul(esz) as u64)), ty: View(*to, 0) })
            }
            _ => Err(format!("{} cannot be indexed", self.name(v.ty))),
        }
    }
}

thread_local! {
    /// Notes from the last `path` (an index past the end), read right after it.
    pub(super) static NOTES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}
