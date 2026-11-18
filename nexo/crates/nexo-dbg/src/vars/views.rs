//! Views of the common containers, chosen from the DWARF type name when a value is printed: Rust `String`, `&str`,
//! `Vec<T>`, `&[T]`, enums (`Option<T>` reads `Some(5)` or `None`), `Box`, `Rc` and `Arc`; libstdc++ `std::string`,
//! `std::vector`, `std::unique_ptr` and `std::shared_ptr`; Go `string` and `[]T`. A map shows its length only.
//! The pieces are found by member name (the layouts changed between versions), never by offset. A value that does
//! not look like the container (garbage, other layout) is left to the raw struct, which `print x --as raw` shows.

use std::cell::Cell;

use super::render::le;
use super::*;

/// Bytes of a string read from the program; past them the text ends in `…(N bytes)`.
const STR_BYTES: usize = 200;
/// A length (in bytes) past which an object is taken for garbage, not a container.
const MAX_LEN: u64 = 1 << 40;

/// What the views read besides members: enum variants, template parameters and base classes.
#[derive(Default)]
pub(super) struct Shapes {
    pub variants: HashMap<u64, VariantPart>,
    pub tparams: HashMap<u64, Vec<(String, Option<u64>)>>,
    pub bases: HashMap<u64, Vec<(u64, Option<u64>)>>,
}

/// The `DW_TAG_variant_part` of a record: where the discriminant is (offset, type), and the variants.
#[derive(Default)]
pub(super) struct VariantPart {
    pub discr: Option<(u64, Option<u64>)>,
    pub variants: Vec<Variant>,
}

/// One variant: the discriminant value that picks it (`None` for the default one, as a niche gives), its name, and
/// the offset and type of its payload struct.
#[derive(Default)]
pub(super) struct Variant {
    pub discr: Option<u64>,
    pub name: String,
    pub off: u64,
    pub ty: Option<u64>,
}

thread_local! {
    static RAW: Cell<bool> = const { Cell::new(false) };
}

struct RawGuard(bool);

impl Drop for RawGuard {
    fn drop(&mut self) {
        RAW.with(|r| r.set(self.0));
    }
}

/// Run `f` with the views off (`raw`) or as they were.
pub(super) fn raw_scope<T>(raw: bool, f: impl FnOnce() -> T) -> T {
    let _back = RawGuard(RAW.with(|r| r.replace(raw || r.get())));
    f()
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    RustStr,
    RustSlice,
    RustString,
    RustVec,
    RustBox,
    RustRc,
    CppString,
    CppVec,
    CppUnique,
    CppShared,
    GoString,
    GoSlice,
    GoMap,
    /// A map that shows its length, found under this member name.
    Map(&'static str),
}

/// The view a type name asks for.
fn classify(name: &str) -> Option<Kind> {
    let n = name.trim();
    if matches!(n, "&str" | "&mut str") {
        return Some(Kind::RustStr);
    }
    if (n.starts_with("&[") || n.starts_with("&mut [")) && n.ends_with(']') {
        return Some(Kind::RustSlice);
    }
    if n.starts_with("[]") {
        return Some(Kind::GoSlice);
    }
    if n.starts_with("map[") {
        return Some(Kind::GoMap);
    }
    if n == "string" {
        return Some(Kind::GoString);
    }
    let head = n.split('<').next().unwrap_or(n);
    let head = head.rsplit("::").next().unwrap_or(head);
    Some(match head {
        "String" => Kind::RustString,
        "Vec" => Kind::RustVec,
        "Box" => Kind::RustBox,
        "Rc" | "Arc" => Kind::RustRc,
        "basic_string" => Kind::CppString,
        "vector" => Kind::CppVec,
        "unique_ptr" => Kind::CppUnique,
        "shared_ptr" => Kind::CppShared,
        "HashMap" | "HashSet" => Kind::Map("items"),
        "BTreeMap" | "BTreeSet" => Kind::Map("length"),
        "map" | "set" | "multimap" | "multiset" => Kind::Map("_M_node_count"),
        "unordered_map" | "unordered_set" | "unordered_multimap" | "unordered_multiset" => {
            Kind::Map("_M_element_count")
        }
        _ => return None,
    })
}

fn unreadable(p: u64) -> String {
    format!("<unreadable at {p:#x}>")
}

/// The value `off` bytes further in.
fn sub(a: &At, off: u64) -> At {
    match a {
        At::Mem(m) => At::Mem(m.wrapping_add(off)),
        At::Bytes(b) => At::Bytes(b.get(off as usize..).unwrap_or(&[]).to_vec()),
    }
}

impl Vars {
    /// The view of a value, from the name of its type (a typedef's, then the struct's) and its members; `None` when
    /// the type is no container this knows, the bytes do not look like one, or the views are off.
    pub(super) fn view(&self, v: &Val, regs: &mut Regs, depth: usize) -> Option<String> {
        if RAW.with(Cell::get) {
            return None;
        }
        let (kinds, core) = self.chain(v.ty.0);
        let core = core?;
        if self.shapes.variants.contains_key(&core) {
            return self.enum_view(v, core, regs, depth);
        }
        kinds.into_iter().find_map(|k| self.kind_view(k, v, core, regs, depth))
    }

    /// The views the names along a typedef chain ask for, and the id of the type the chain ends in.
    fn chain(&self, mut t: Option<u64>) -> (Vec<Kind>, Option<u64>) {
        let mut kinds = Vec::new();
        for _ in 0..16 {
            let Some(id) = t else { break };
            match self.types.get(&id) {
                Some(Ty::Alias { name, to }) => {
                    kinds.extend(classify(name));
                    t = *to;
                }
                Some(Ty::Record { name, .. }) => {
                    kinds.extend(classify(name));
                    return (kinds, Some(id));
                }
                Some(_) => return (kinds, Some(id)),
                None => break,
            }
        }
        (kinds, None)
    }

    fn core(&self, t: Option<u64>) -> Option<u64> {
        self.chain(t).1
    }

    /// A member of a record by name as (offset, type), through its base classes; with `deep`, through members that
    /// are records too (never through a pointer).
    fn field(&self, t: Option<u64>, name: &str, deep: bool, depth: usize) -> Option<(u64, Option<u64>)> {
        if depth > 8 {
            return None;
        }
        let id = self.core(t)?;
        let members = match self.types.get(&id) {
            Some(Ty::Record { members, .. }) => members.as_slice(),
            _ => &[],
        };
        if let Some((_, o, mt)) = members.iter().find(|(n, _, _)| n.as_str() == name) {
            return Some((*o, *mt));
        }
        let none: &[(u64, Option<u64>)] = &[];
        let bases = self.shapes.bases.get(&id).map_or(none, |b| b.as_slice());
        for (bo, bt) in bases {
            if let Some((o, mt)) = self.field(*bt, name, deep, depth + 1) {
                return Some((bo.wrapping_add(o), mt));
            }
        }
        if deep {
            for (_, mo, mt) in members {
                if let Some((o, t2)) = self.field(*mt, name, true, depth + 1) {
                    return Some((mo.wrapping_add(o), t2));
                }
            }
        }
        None
    }

    /// Members by a chain of names: the offset and type of the last.
    fn walk(&self, t: Option<u64>, names: &[&str]) -> Option<(u64, Option<u64>)> {
        let (mut off, mut ty) = (0u64, t);
        for n in names {
            let (o, mt) = self.field(ty, n, false, 0)?;
            off = off.wrapping_add(o);
            ty = mt;
        }
        Some((off, ty))
    }

    /// The template parameter of a record under one of these names.
    fn tparam(&self, t: Option<u64>, names: &[&str]) -> Option<u64> {
        let id = self.core(t)?;
        let (_, ty) = self.shapes.tparams.get(&id)?.iter().find(|(n, _)| names.contains(&n.as_str()))?;
        *ty
    }

    /// The pointer inside a Rust smart pointer or buffer (`inner`, `ptr`, `pointer`, `0`, whichever the layout of
    /// that version has) as (offset, pointee type).
    fn ptr_leaf(&self, t: Option<u64>, depth: usize) -> Option<(u64, Option<u64>)> {
        if depth > 8 {
            return None;
        }
        match self.strip(t)? {
            Ty::Ptr { to } => Some((0, *to)),
            Ty::Record { members, .. } => ["inner", "ptr", "pointer", "0"].iter().find_map(|n| {
                let (_, o, mt) = members.iter().find(|(m, _, _)| m.as_str() == *n)?;
                let (o2, to) = self.ptr_leaf(*mt, depth + 1)?;
                Some((o.wrapping_add(o2), to))
            }),
            _ => None,
        }
    }

    /// The integer at the bottom of single-member wrappers (`Cell<usize>`, `AtomicUsize`) as (offset, type).
    fn int_leaf(&self, t: Option<u64>, depth: usize) -> Option<(u64, Option<u64>)> {
        if depth > 8 {
            return None;
        }
        match self.strip(t)? {
            Ty::Base { .. } | Ty::Enum { .. } => Some((0, t)),
            Ty::Record { members, .. } => {
                let (_, o, mt) = members.first()?;
                let (o2, ty) = self.int_leaf(*mt, depth + 1)?;
                Some((o.wrapping_add(o2), ty))
            }
            _ => None,
        }
    }

    fn read(&self, a: &At, n: usize, regs: &mut Regs) -> Option<Vec<u8>> {
        match a {
            At::Mem(m) => (regs.rd)(*m, n).filter(|b| b.len() >= n),
            At::Bytes(b) => b.get(..n).map(<[u8]>::to_vec),
        }
    }

    /// A pointer-sized unsigned number.
    fn word(&self, a: &At, regs: &mut Regs) -> Option<u64> {
        let b = self.read(a, (self.ptr as usize).clamp(1, 8), regs)?;
        Some(le(&b, false) as u64)
    }

    /// An integer of type `ty` (up to 8 bytes) at `a`.
    fn int_at(&self, a: &At, ty: Option<u64>, regs: &mut Regs) -> Option<u64> {
        let sz = (self.size(View(ty, 0)) as usize).clamp(1, 8);
        let b = self.read(a, sz, regs)?;
        Some(le(&b, false) as u64)
    }

    /// Text of `len` bytes at `ptr`: at most `STR_BYTES` of it, then `…(N bytes)`.
    fn text(&self, ptr: u64, len: u64, regs: &mut Regs) -> Option<String> {
        if len > MAX_LEN {
            return None;
        }
        if len == 0 {
            return Some("\"\"".into());
        }
        let n = len.min(STR_BYTES as u64) as usize;
        let Some(b) = (regs.rd)(ptr, n).filter(|b| b.len() >= n) else { return Some(unreadable(ptr)) };
        let mut s = format!("{:?}", String::from_utf8_lossy(&b[..n]));
        if len > n as u64 {
            s.push_str(&format!("…({len} bytes)"));
        }
        Some(s)
    }

    /// `len` elements of type `elem` from `ptr`, as `[a, b, ...]` (at most `SHOWN` of them).
    fn seq(&self, ptr: u64, len: u64, elem: Option<u64>, regs: &mut Regs, depth: usize) -> Option<String> {
        let esz = self.size(View(elem, 0));
        if len.checked_mul(esz.max(1)).is_none_or(|b| b > MAX_LEN) {
            return None;
        }
        if len == 0 {
            return Some("[]".into());
        }
        if esz == 0 {
            return Some(format!("[{len} elements of unknown size]"));
        }
        if depth > 2 {
            return Some(format!("[...] ({len} in all)"));
        }
        if (regs.rd)(ptr, 1).is_none() {
            return Some(unreadable(ptr));
        }
        let mut parts = Vec::new();
        for i in 0..len.min(SHOWN as u64) {
            let at = At::Mem(ptr.wrapping_add(i.wrapping_mul(esz)));
            parts.push(self.render_inner(&Val { at, ty: View(elem, 0) }, regs, depth + 1));
        }
        if len > SHOWN as u64 {
            parts.push(format!("... ({len} in all)"));
        }
        Some(format!("[{}]", parts.join(", ")))
    }

    /// What a smart pointer points at: `0x.. -> value`, with `extra` (a count) after it.
    fn pointee(&self, p: u64, to: Option<u64>, extra: &str, regs: &mut Regs, depth: usize) -> String {
        if p == 0 {
            return "NULL".into();
        }
        if to.is_none() || depth > 2 {
            return format!("{p:#x}{extra}");
        }
        if (regs.rd)(p, 1).is_none() {
            return unreadable(p);
        }
        let inner = self.render_inner(&Val { at: At::Mem(p), ty: View(to, 0) }, regs, depth + 1);
        format!("{p:#x} -> {inner}{extra}")
    }

    /// The pointer, length and element type of a Rust `Vec` whose struct is at `base` in `v`.
    fn rust_vec(&self, v: &Val, t: Option<u64>, base: u64, regs: &mut Regs) -> Option<(u64, u64, Option<u64>)> {
        let (bo, bt) = self.field(t, "buf", false, 0)?;
        let (po, to) = self.ptr_leaf(bt, 0)?;
        let (lo, _) = self.field(t, "len", false, 0)?;
        let ptr = self.word(&sub(&v.at, base.wrapping_add(bo).wrapping_add(po)), regs)?;
        let len = self.word(&sub(&v.at, base.wrapping_add(lo)), regs)?;
        // newer layouts keep the buffer untyped (`u8`): the element type is the parameter T of the Vec
        Some((ptr, len, self.tparam(t, &["T"]).or(to)))
    }

    /// A pointer member `name` of the record at `v`, and its pointee type.
    fn ptr_field(&self, v: &Val, t: Option<u64>, name: &str, regs: &mut Regs) -> Option<(u64, Option<u64>)> {
        let (o, mt) = self.field(t, name, false, 0)?;
        let to = match self.strip(mt)? {
            Ty::Ptr { to } => *to,
            _ => return None,
        };
        Some((self.word(&sub(&v.at, o), regs)?, to))
    }

    fn kind_view(&self, k: Kind, v: &Val, core: u64, regs: &mut Regs, depth: usize) -> Option<String> {
        let t = Some(core);
        match k {
            Kind::RustStr | Kind::GoString => {
                let (pn, ln) = if k == Kind::RustStr { ("data_ptr", "length") } else { ("str", "len") };
                let (ptr, _) = self.ptr_field(v, t, pn, regs)?;
                let (lo, _) = self.field(t, ln, false, 0)?;
                let len = self.word(&sub(&v.at, lo), regs)?;
                self.text(ptr, len, regs)
            }
            Kind::RustSlice | Kind::GoSlice => {
                let (pn, ln) = if k == Kind::RustSlice { ("data_ptr", "length") } else { ("array", "len") };
                let (ptr, elem) = self.ptr_field(v, t, pn, regs)?;
                let (lo, _) = self.field(t, ln, false, 0)?;
                let len = self.word(&sub(&v.at, lo), regs)?;
                self.seq(ptr, len, elem, regs, depth)
            }
            Kind::RustString => {
                let (vo, vt) = self.field(t, "vec", false, 0)?;
                let (ptr, len, _) = self.rust_vec(v, vt, vo, regs)?;
                self.text(ptr, len, regs)
            }
            Kind::RustVec => {
                let (ptr, len, elem) = self.rust_vec(v, t, 0, regs)?;
                self.seq(ptr, len, elem, regs, depth)
            }
            Kind::RustBox => {
                let (po, to) = self.ptr_leaf(t, 0)?;
                let p = self.word(&sub(&v.at, po), regs)?;
                Some(self.pointee(p, to, "", regs, depth))
            }
            Kind::RustRc => {
                let (po, to) = self.ptr_leaf(self.field(t, "ptr", false, 0)?.1, 0)?;
                let off = self.field(t, "ptr", false, 0)?.0.wrapping_add(po);
                let p = self.word(&sub(&v.at, off), regs)?;
                // the allocation holds the counts and the value
                let (vo, vt) = self.field(to, "value", false, 0)?;
                if p == 0 {
                    return Some("NULL".into());
                }
                let strong = self.field(to, "strong", false, 0).and_then(|(so, st)| {
                    let (lo, lt) = self.int_leaf(st, 0)?;
                    self.int_at(&At::Mem(p.wrapping_add(so).wrapping_add(lo)), lt, regs)
                });
                let extra = strong.map_or(String::new(), |n| format!(" (strong {n})"));
                Some(self.pointee(p.wrapping_add(vo), vt, &extra, regs, depth))
            }
            Kind::CppString => {
                let (po, _) = self.walk(t, &["_M_dataplus", "_M_p"])?;
                let (lo, _) = self.field(t, "_M_string_length", false, 0)?;
                let ptr = self.word(&sub(&v.at, po), regs)?;
                let len = self.word(&sub(&v.at, lo), regs)?;
                self.text(ptr, len, regs)
            }
            Kind::CppVec => {
                let (so, st) = self.walk(t, &["_M_impl", "_M_start"])?;
                let (eo, _) = self.walk(t, &["_M_impl", "_M_finish"])?;
                let elem = match self.strip(st)? {
                    Ty::Ptr { to } => *to,
                    _ => return None,
                };
                let esz = self.size(View(elem, 0));
                let (start, end) = (self.word(&sub(&v.at, so), regs)?, self.word(&sub(&v.at, eo), regs)?);
                if esz == 0 || end < start {
                    return None;
                }
                self.seq(start, (end - start) / esz, elem, regs, depth)
            }
            Kind::CppUnique => {
                let (po, pt) = self.field(t, "_M_head_impl", true, 0)?;
                let Ty::Ptr { to } = self.strip(pt)? else { return None };
                let p = self.word(&sub(&v.at, po), regs)?;
                Some(self.pointee(p, *to, "", regs, depth))
            }
            Kind::CppShared => {
                let (p, to) = self.ptr_field(v, t, "_M_ptr", regs)?;
                let count = self.walk(t, &["_M_refcount", "_M_pi"]).and_then(|(o, pt)| {
                    let Ty::Ptr { to: ctl } = self.strip(pt)? else { return None };
                    let pi = self.word(&sub(&v.at, o), regs)?;
                    if pi == 0 {
                        return None;
                    }
                    let (uo, ut) = self.field(*ctl, "_M_use_count", false, 0)?;
                    self.int_at(&At::Mem(pi.wrapping_add(uo)), ut, regs)
                });
                let extra = count.map_or(String::new(), |n| format!(" (use_count {n})"));
                Some(self.pointee(p, to, &extra, regs, depth))
            }
            Kind::Map(name) => {
                let (o, _) = self.field(t, name, true, 0)?;
                let n = self.word(&sub(&v.at, o), regs)?;
                Some(format!("(map: {n} entries, view not supported yet)"))
            }
            Kind::GoMap => {
                let Some(Ty::Ptr { to }) = self.types.get(&core) else { return None };
                let p = self.word(&v.at, regs)?;
                if p == 0 {
                    return Some("(map: 0 entries, view not supported yet)".into());
                }
                let (o, _) = self.field(*to, "count", false, 0)?;
                let n = self.word(&At::Mem(p.wrapping_add(o)), regs)?;
                Some(format!("(map: {n} entries, view not supported yet)"))
            }
        }
    }

    /// An enum (a record with a variant part): `Some(5)`, `None`, `Circle { r = 1 }`.
    fn enum_view(&self, v: &Val, core: u64, regs: &mut Regs, depth: usize) -> Option<String> {
        let part = self.shapes.variants.get(&core)?;
        if part.variants.is_empty() {
            return None;
        }
        if depth > 2 {
            return Some("{...}".into());
        }
        let chosen = match part.discr {
            Some((doff, dty)) => {
                let sz = (self.size(View(dty, 0)) as usize).clamp(1, 8);
                let x = le(&self.read(&sub(&v.at, doff), sz, regs)?, false) as u64;
                let mask = |d: u64| if sz >= 8 { d } else { d & ((1u64 << (8 * sz)) - 1) };
                part.variants
                    .iter()
                    .find(|vr| vr.discr.is_some_and(|d| mask(d) == x))
                    .or_else(|| part.variants.iter().find(|vr| vr.discr.is_none()))?
            }
            None => part.variants.first()?,
        };
        let name = if chosen.name.is_empty() { "?" } else { chosen.name.as_str() };
        let Some(Ty::Record { members, .. }) = self.strip(chosen.ty) else { return Some(name.to_string()) };
        if members.is_empty() {
            return Some(name.to_string());
        }
        let tuple = members.iter().all(|(n, _, _)| n.starts_with("__"));
        let parts: Vec<String> = members
            .iter()
            .map(|(n, off, t)| {
                let at = sub(&v.at, chosen.off.wrapping_add(*off));
                let s = self.render_inner(&Val { at, ty: View(*t, 0) }, regs, depth + 1);
                if tuple {
                    s
                } else {
                    format!("{n} = {s}")
                }
            })
            .collect();
        Some(if tuple { format!("{name}({})", parts.join(", ")) } else { format!("{name} {{{}}}", parts.join(", ")) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(name: &str, size: u64, members: &[(&str, u64, u64)]) -> Ty {
        let members = members.iter().map(|(n, o, t)| (n.to_string(), *o, Some(*t))).collect();
        Ty::Record { name: name.into(), size, members, union: false }
    }

    fn base(name: &str, size: u64, enc: u8) -> Ty {
        Ty::Base { name: name.into(), size, enc }
    }

    fn words(w: &[u64]) -> Vec<u8> {
        w.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    fn i32s(w: &[i32]) -> Vec<u8> {
        w.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    /// Types: 1 u8, 2 i32, 3 usize, 4 u32. A Vec<i32> in the layout with RawVecInner (10), one in the older layout
    /// with the fields the other way round (20), a Vec<u8> (16), a String (30), Option<i32> (40), a Go string (50)
    /// and a Go []int32 (52).
    fn vars() -> Vars {
        let mut t: Vec<(u64, Ty)> = vec![
            (1, base("u8", 1, 7)),
            (2, base("i32", 4, 5)),
            (3, base("usize", 8, 7)),
            (4, base("u32", 4, 7)),
            (15, Ty::Ptr { to: Some(1) }),
            (14, rec("NonNull<u8>", 8, &[("pointer", 0, 15)])),
            (13, rec("Unique<u8>", 8, &[("pointer", 0, 14)])),
            (12, rec("RawVecInner<alloc::alloc::Global>", 16, &[("ptr", 0, 13), ("cap", 8, 3)])),
            (11, rec("RawVec<i32, alloc::alloc::Global>", 16, &[("inner", 0, 12)])),
            (10, rec("Vec<i32, alloc::alloc::Global>", 24, &[("buf", 0, 11), ("len", 16, 3)])),
            (16, rec("Vec<u8, alloc::alloc::Global>", 24, &[("buf", 0, 11), ("len", 16, 3)])),
            (24, Ty::Ptr { to: Some(2) }),
            (23, rec("NonNull<i32>", 8, &[("pointer", 0, 24)])),
            (22, rec("Unique<i32>", 8, &[("pointer", 0, 23)])),
            (21, rec("RawVec<i32>", 16, &[("ptr", 0, 22), ("cap", 8, 3)])),
            (20, rec("Vec<i32>", 24, &[("len", 0, 3), ("buf", 8, 21)])),
            (30, rec("String", 24, &[("vec", 0, 16)])),
            (41, base("isize", 8, 5)),
            (42, rec("None", 0, &[])),
            (43, rec("Some<i32>", 8, &[("__0", 4, 2)])),
            (40, rec("Option<i32>", 8, &[])),
            (51, Ty::Ptr { to: Some(1) }),
            (50, rec("string", 16, &[("str", 0, 51), ("len", 8, 3)])),
            (53, Ty::Ptr { to: Some(2) }),
            (52, rec("[]int32", 24, &[("array", 0, 53), ("len", 8, 3), ("cap", 16, 3)])),
        ];
        // the u32 discriminant of Option<i32>
        t.push((44, base("u32", 4, 7)));
        let mut v = Vars {
            funcs: Vec::new(),
            globals: Vec::new(),
            types: t.into_iter().collect(),
            files: Vec::new(),
            ptr: 8,
            shapes: Default::default(),
        };
        v.shapes.tparams.insert(10, vec![("T".into(), Some(2)), ("A".into(), None)]);
        v.shapes.tparams.insert(16, vec![("T".into(), Some(1))]);
        v.shapes.variants.insert(
            40,
            VariantPart {
                discr: Some((0, Some(4))),
                variants: vec![
                    Variant { discr: Some(0), name: "None".into(), off: 0, ty: Some(42) },
                    Variant { discr: Some(1), name: "Some".into(), off: 0, ty: Some(43) },
                ],
            },
        );
        v
    }

    fn show(v: &Vars, ty: u64, at: u64, mem: Vec<(u64, Vec<u8>)>) -> String {
        let mut rd = move |a: u64, n: usize| -> Option<Vec<u8>> {
            let (s, b) = mem.iter().find(|(s, b)| a >= *s && a + n as u64 <= *s + b.len() as u64)?;
            let o = (a - s) as usize;
            Some(b[o..o + n].to_vec())
        };
        let mut regs = Regs { gp: [0; 16], rip: 0, cfa: None, bias: 0, rd: &mut rd, early: None, lines: None, lost: 0 };
        v.render(&Val { at: At::Mem(at), ty: View(Some(ty), 0) }, &mut regs, 0)
    }

    #[test]
    fn a_rust_vec_is_read_through_its_nested_members_in_both_layouts() {
        let v = vars();
        // buf.inner.ptr.pointer.pointer and len, the element type from the parameter T (the buffer says u8)
        let new = vec![(0x1000, words(&[0x2000, 4, 3])), (0x2000, i32s(&[1, 2, 3, 4]))];
        assert_eq!(show(&v, 10, 0x1000, new), "[1, 2, 3]");
        // buf.ptr.pointer.pointer, with len before buf
        let old = vec![(0x1100, words(&[2, 0x2100, 8])), (0x2100, i32s(&[7, 8]))];
        assert_eq!(show(&v, 20, 0x1100, old), "[7, 8]");
        // more than SHOWN elements
        let long: Vec<i32> = (0..20).collect();
        let m = vec![(0x1000, words(&[0x2000, 20, 20])), (0x2000, i32s(&long))];
        let s = show(&v, 10, 0x1000, m);
        assert!(s.starts_with("[0, 1, 2, ") && s.contains("11, ... (20 in all)]"), "{s}");
    }

    #[test]
    fn a_rust_string_reads_as_text_and_a_long_one_is_cut_with_its_size() {
        let v = vars();
        let m = vec![(0x1200, words(&[0x3000, 5, 5])), (0x3000, b"hello".to_vec())];
        assert_eq!(show(&v, 30, 0x1200, m), "\"hello\"");
        let m = vec![(0x1200, words(&[0x4000, 300, 300])), (0x4000, vec![b'a'; 300])];
        assert_eq!(show(&v, 30, 0x1200, m), format!("\"{}\"…(300 bytes)", "a".repeat(STR_BYTES)));
    }

    #[test]
    fn a_pointer_that_cannot_be_read_says_where_and_garbage_stays_raw() {
        let v = vars();
        let m = vec![(0x1300, words(&[0x9000, 5, 5]))];
        assert_eq!(show(&v, 30, 0x1300, m), "<unreadable at 0x9000>");
        let m = vec![(0x1300, words(&[0x9000, 5, 5]))];
        assert_eq!(show(&v, 10, 0x1300, m), "<unreadable at 0x9000>");
        // a length no program has: not a container, so the raw struct
        let m = vec![(0x1300, words(&[0x9000, u64::MAX, u64::MAX]))];
        assert!(show(&v, 30, 0x1300, m).starts_with("{vec = "));
    }

    #[test]
    fn an_option_reads_as_some_or_none() {
        let v = vars();
        let mut some = 1u32.to_le_bytes().to_vec();
        some.extend(5i32.to_le_bytes());
        assert_eq!(show(&v, 40, 0x1400, vec![(0x1400, some)]), "Some(5)");
        assert_eq!(show(&v, 40, 0x1400, vec![(0x1400, vec![0; 8])]), "None");
    }

    #[test]
    fn a_go_string_and_slice_read_by_their_members() {
        let v = vars();
        let m = vec![(0x1500, words(&[0x5000, 2])), (0x5000, b"hi".to_vec())];
        assert_eq!(show(&v, 50, 0x1500, m), "\"hi\"");
        let m = vec![(0x1600, words(&[0x6000, 2, 4])), (0x6000, i32s(&[10, 20]))];
        assert_eq!(show(&v, 52, 0x1600, m), "[10, 20]");
    }

    #[test]
    fn raw_turns_the_views_off() {
        let v = vars();
        let m = vec![(0x1200, words(&[0x3000, 5, 5])), (0x3000, b"hello".to_vec())];
        let s = raw_scope(true, || show(&v, 30, 0x1200, m));
        assert!(s.starts_with("{vec = "), "{s}");
        assert!(!RAW.with(Cell::get));
    }

    #[test]
    fn type_names_pick_the_view() {
        assert_eq!(classify("alloc::vec::Vec<i32, alloc::alloc::Global>"), Some(Kind::RustVec));
        assert_eq!(classify("&mut [u8]"), Some(Kind::RustSlice));
        assert_eq!(
            classify("std::__cxx11::basic_string<char, std::char_traits<char>, std::allocator<char> >"),
            Some(Kind::CppString)
        );
        assert_eq!(classify("HashMap<i32, i32, RandomState>"), Some(Kind::Map("items")));
        assert_eq!(classify("Option<i32>"), None);
    }
}
