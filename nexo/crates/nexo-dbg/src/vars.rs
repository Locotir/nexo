//! Variables from DWARF (`.debug_info`): the parameters and locals of the function at a stop, globals, and their
//! types, so `locals`, `print p->tag` and `print rows[2][1]` answer in the program's own words. Only the program
//! itself is read (not its libraries), and only plain (uncompressed) ELF sections, like `lines.rs`.

use std::collections::HashMap;

use gimli::{AttributeValue, EndianSlice, EvaluationResult, LittleEndian, Location, Value};
use nexo_bin::Image;
use serde_json::json;

use crate::engine::{rows, Session};
use nexo_bin::Output;

use crate::{fail, Result};

/// Biggest `.debug_info` read: past it no variables are built.
const MAX_BYTES: usize = 64 << 20;

/// Longest value text: past it the rest is cut with `...`.
const MAX_TEXT: usize = 400;
/// How deep a type may nest before it is taken to loop back on itself.
const MAX_TYPE_DEPTH: usize = 32;

/// Array elements shown before `...`.
const SHOWN: usize = 12;

type Slice<'a> = EndianSlice<'a, LittleEndian>;

/// Where a variable lives: one expression, a list valid by address range (optimised code), or nowhere.
enum Loc {
    Expr(Vec<u8>),
    List(Vec<(u64, u64, Vec<u8>)>),
    None,
}

struct Var {
    name: String,
    param: bool,
    loc: Loc,
    ty: Option<u64>,
    /// The ranges of the lexical block it was declared in; `None` for the whole function.
    scope: Option<Vec<(u64, u64)>>,
}

struct Func {
    name: String,
    ranges: Vec<(u64, u64)>,
    frame_base: Option<Vec<u8>>,
    enc: gimli::Encoding,
    vars: Vec<Var>,
}

enum Ty {
    Base {
        name: String,
        size: u64,
        enc: u8,
    },
    Ptr {
        to: Option<u64>,
    },
    Array {
        elem: Option<u64>,
        dims: Vec<Option<u64>>,
    },
    Record {
        name: String,
        size: u64,
        members: Vec<(String, u64, Option<u64>)>,
        union: bool,
    },
    /// A typedef (with its name) or a `const`/`volatile` (name empty).
    Alias {
        name: String,
        to: Option<u64>,
    },
    Enum {
        name: String,
        size: u64,
        vals: Vec<(String, i64)>,
    },
    Func,
}

/// The variables and types of a program. Types are keyed by their `.debug_info` offset.
pub struct Vars {
    funcs: Vec<Func>,
    globals: Vec<(Var, gimli::Encoding)>,
    types: HashMap<u64, Ty>,
}

/// A type as used in a value: the type, and how many array dimensions were already indexed.
#[derive(Clone, Copy)]
struct View(Option<u64>, usize);

/// A value: where its bytes are, and its type.
struct Val {
    at: At,
    ty: View,
}

enum At {
    Mem(u64),
    Bytes(Vec<u8>),
}

/// What a location needs from the stopped program.
pub struct Regs<'a> {
    pub gp: [u64; 16],
    pub rip: u64,
    pub cfa: Option<u64>,
    pub bias: u64,
    pub rd: &'a mut dyn FnMut(u64, usize) -> Option<Vec<u8>>,
}

enum Ctx {
    Func(usize),
    Block(Vec<(u64, u64)>),
    Type(u64),
    Skip,
    Other,
}

impl Vars {
    /// The variables of an image, or `None` when it has no (readable) DWARF.
    pub fn of(img: &Image) -> Option<Vars> {
        // the DWARF reader trusts address arithmetic that a damaged file can overflow (a panic in debug builds);
        // debug info it cannot read is "no debug info", not a crash
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Self::read(img))).ok().flatten()
    }

    fn read(img: &Image) -> Option<Vars> {
        let get = |name: &str| -> &[u8] {
            img.sections.iter().find(|s| s.name == name).map_or(&[][..], |s| img.section_bytes(s))
        };
        if get(".debug_info").is_empty() || get(".debug_info").len() > MAX_BYTES {
            return None;
        }
        let load = |id: gimli::SectionId| -> std::result::Result<Slice<'_>, gimli::Error> {
            Ok(EndianSlice::new(get(id.name()), LittleEndian))
        };
        let dwarf = gimli::Dwarf::load(load).ok()?;
        let mut out = Vars { funcs: Vec::new(), globals: Vec::new(), types: HashMap::new() };
        let mut units = dwarf.units();
        while let Ok(Some(header)) = units.next() {
            let Ok(unit) = dwarf.unit(header) else { continue };
            out.read_unit(&dwarf, &unit);
        }
        Some(out)
    }

    fn read_unit<'a>(&mut self, dwarf: &gimli::Dwarf<Slice<'a>>, unit: &gimli::Unit<Slice<'a>>) {
        let enc = unit.encoding();
        let name = |e: &gimli::DebuggingInformationEntry<Slice<'a>>| -> String {
            e.attr_value(gimli::DW_AT_name)
                .and_then(|v| dwarf.attr_string(unit, v).ok())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let type_of = |e: &gimli::DebuggingInformationEntry<Slice<'a>>| -> Option<u64> {
            match e.attr_value(gimli::DW_AT_type)? {
                AttributeValue::UnitRef(o) => o.to_debug_info_offset(&unit.header).map(|o| o.0 as u64),
                AttributeValue::DebugInfoRef(o) => Some(o.0 as u64),
                _ => None,
            }
        };
        let ranges = |e: &gimli::DebuggingInformationEntry<Slice<'a>>| -> Vec<(u64, u64)> {
            let mut v = Vec::new();
            if let Ok(mut r) = dwarf.die_ranges(unit, e) {
                while let Ok(Some(r)) = r.next() {
                    if r.end > r.begin {
                        v.push((r.begin, r.end));
                    }
                }
            }
            v
        };
        let mut stack: Vec<(isize, Ctx)> = Vec::new();
        let mut entries = unit.entries();
        while let Ok(Some(e)) = entries.next_dfs() {
            let d = e.depth();
            while stack.last().is_some_and(|(sd, _)| *sd >= d) {
                stack.pop();
            }
            let off = e.offset().to_debug_info_offset(&unit.header).map_or(0, |o| o.0 as u64);
            let size = e.attr_value(gimli::DW_AT_byte_size).and_then(|v| v.udata_value()).unwrap_or(0);
            let ctx = match e.tag() {
                gimli::DW_TAG_subprogram => {
                    let r = ranges(e);
                    if r.is_empty() || stack.iter().any(|(_, c)| matches!(c, Ctx::Skip)) {
                        Ctx::Skip
                    } else {
                        let frame_base = e.attr_value(gimli::DW_AT_frame_base).and_then(|v| v.exprloc_value());
                        self.funcs.push(Func {
                            name: name(e),
                            ranges: r,
                            frame_base: frame_base.map(|x| x.0.slice().to_vec()),
                            enc,
                            vars: Vec::new(),
                        });
                        Ctx::Func(self.funcs.len() - 1)
                    }
                }
                gimli::DW_TAG_lexical_block => Ctx::Block(ranges(e)),
                gimli::DW_TAG_inlined_subroutine => Ctx::Skip,
                t @ (gimli::DW_TAG_formal_parameter | gimli::DW_TAG_variable) => {
                    let loc = match e.attr_value(gimli::DW_AT_location) {
                        Some(AttributeValue::Exprloc(x)) => Loc::Expr(x.0.slice().to_vec()),
                        Some(v) => match dwarf.attr_locations(unit, v) {
                            Ok(Some(mut it)) => {
                                let mut l = Vec::new();
                                while let Ok(Some(x)) = it.next() {
                                    l.push((x.range.begin, x.range.end, x.data.0.slice().to_vec()));
                                }
                                Loc::List(l)
                            }
                            _ => Loc::None,
                        },
                        None => Loc::None,
                    };
                    let mut var = Var {
                        name: name(e),
                        param: t == gimli::DW_TAG_formal_parameter,
                        loc,
                        ty: type_of(e),
                        scope: None,
                    };
                    let mut owner = None;
                    for (_, c) in stack.iter().rev() {
                        match c {
                            Ctx::Skip | Ctx::Type(_) => break,
                            Ctx::Block(r) if var.scope.is_none() => var.scope = Some(r.clone()),
                            Ctx::Func(i) => {
                                owner = Some(Some(*i));
                                break;
                            }
                            _ => {}
                        }
                    }
                    if stack.is_empty() || stack.iter().all(|(_, c)| matches!(c, Ctx::Other)) {
                        owner = Some(None);
                    }
                    match owner {
                        Some(Some(i)) if !var.name.is_empty() => self.funcs[i].vars.push(var),
                        Some(None) if !var.name.is_empty() && !matches!(var.loc, Loc::None) => {
                            self.globals.push((var, enc))
                        }
                        _ => {}
                    }
                    Ctx::Other
                }
                gimli::DW_TAG_base_type => {
                    let enc = match e.attr_value(gimli::DW_AT_encoding) {
                        Some(AttributeValue::Encoding(x)) => x.0,
                        _ => 0,
                    };
                    self.types.insert(off, Ty::Base { name: name(e), size, enc });
                    Ctx::Other
                }
                gimli::DW_TAG_pointer_type | gimli::DW_TAG_reference_type | gimli::DW_TAG_rvalue_reference_type => {
                    self.types.insert(off, Ty::Ptr { to: type_of(e) });
                    Ctx::Other
                }
                gimli::DW_TAG_array_type => {
                    self.types.insert(off, Ty::Array { elem: type_of(e), dims: Vec::new() });
                    Ctx::Type(off)
                }
                gimli::DW_TAG_subrange_type => {
                    let n = match e.attr_value(gimli::DW_AT_count).and_then(|v| v.udata_value()) {
                        Some(n) => Some(n),
                        None => e.attr_value(gimli::DW_AT_upper_bound).and_then(|v| v.udata_value()).map(|u| u + 1),
                    };
                    if let Some((_, Ctx::Type(a))) = stack.last() {
                        if let Some(Ty::Array { dims, .. }) = self.types.get_mut(a) {
                            dims.push(n);
                        }
                    }
                    Ctx::Other
                }
                t @ (gimli::DW_TAG_structure_type | gimli::DW_TAG_class_type | gimli::DW_TAG_union_type) => {
                    let union = t == gimli::DW_TAG_union_type;
                    self.types.insert(off, Ty::Record { name: name(e), size, members: Vec::new(), union });
                    Ctx::Type(off)
                }
                gimli::DW_TAG_member => {
                    let at = match e.attr_value(gimli::DW_AT_data_member_location) {
                        Some(v) => v.udata_value().unwrap_or(0),
                        None => 0,
                    };
                    let m = (name(e), at, type_of(e));
                    if let Some((_, Ctx::Type(r))) = stack.last() {
                        if let Some(Ty::Record { members, .. }) = self.types.get_mut(r) {
                            members.push(m);
                        }
                    }
                    Ctx::Other
                }
                gimli::DW_TAG_typedef => {
                    self.types.insert(off, Ty::Alias { name: name(e), to: type_of(e) });
                    Ctx::Other
                }
                gimli::DW_TAG_const_type
                | gimli::DW_TAG_volatile_type
                | gimli::DW_TAG_restrict_type
                | gimli::DW_TAG_atomic_type => {
                    self.types.insert(off, Ty::Alias { name: String::new(), to: type_of(e) });
                    Ctx::Other
                }
                gimli::DW_TAG_enumeration_type => {
                    self.types.insert(off, Ty::Enum { name: name(e), size, vals: Vec::new() });
                    Ctx::Type(off)
                }
                gimli::DW_TAG_enumerator => {
                    let v = e.attr_value(gimli::DW_AT_const_value);
                    let v = v.as_ref().and_then(|v| v.sdata_value().or_else(|| v.udata_value().map(|u| u as i64)));
                    if let (Some((_, Ctx::Type(t))), Some(v)) = (stack.last(), v) {
                        if let Some(Ty::Enum { vals, .. }) = self.types.get_mut(t) {
                            vals.push((name(e), v));
                        }
                    }
                    Ctx::Other
                }
                gimli::DW_TAG_subroutine_type => {
                    self.types.insert(off, Ty::Func);
                    Ctx::Skip
                }
                gimli::DW_TAG_compile_unit | gimli::DW_TAG_namespace => Ctx::Other,
                _ => Ctx::Skip,
            };
            stack.push((d, ctx));
        }
    }

    fn func_at(&self, pc: u64) -> Option<&Func> {
        self.funcs
            .iter()
            .filter(|f| f.ranges.iter().any(|&(lo, hi)| pc >= lo && pc < hi))
            .min_by_key(|f| f.ranges.iter().map(|(lo, hi)| hi - lo).sum::<u64>())
    }

    /// The variables in scope at `pc` (a file address): parameters first, then locals; an inner one hides an
    /// outer one of the same name.
    fn in_scope(&self, pc: u64) -> Vec<(&Var, gimli::Encoding, Option<&[u8]>)> {
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

    fn strip(&self, mut t: Option<u64>) -> Option<&Ty> {
        for _ in 0..16 {
            match self.types.get(&t?)? {
                Ty::Alias { to, .. } => t = *to,
                other => return Some(other),
            }
        }
        None
    }

    fn size(&self, v: View) -> u64 {
        self.size_in(v, 0)
    }

    /// `size` with a depth guard: damaged debug info can make a type contain itself.
    fn size_in(&self, v: View, depth: usize) -> u64 {
        if depth > MAX_TYPE_DEPTH {
            return 0;
        }
        match self.strip(v.0) {
            Some(Ty::Base { size, .. } | Ty::Record { size, .. } | Ty::Enum { size, .. }) => *size,
            Some(Ty::Ptr { .. }) => 8,
            Some(Ty::Array { elem, dims }) => {
                let n: u64 = dims.iter().skip(v.1).fold(1u64, |a, d| a.saturating_mul(d.unwrap_or(0)));
                n.saturating_mul(self.size_in(View(*elem, 0), depth + 1))
            }
            _ => 0,
        }
    }

    fn name(&self, v: View) -> String {
        self.name_in(v, 0)
    }

    /// `name` with a depth guard: a pointer chain that loops back on itself ends in `...`.
    fn name_in(&self, v: View, depth: usize) -> String {
        if depth > MAX_TYPE_DEPTH {
            return "...".into();
        }
        let Some(t) = v.0 else { return "void".into() };
        match self.types.get(&t) {
            Some(Ty::Base { name, .. }) => name.clone(),
            Some(Ty::Ptr { to }) => match self.strip(*to) {
                Some(Ty::Func) => "function pointer".into(),
                _ => format!("{} *", self.name_in(View(*to, 0), depth + 1)),
            },
            Some(Ty::Array { elem, dims }) => {
                let d: String = dims.iter().skip(v.1).map(|d| d.map_or("[]".into(), |n| format!("[{n}]"))).collect();
                format!("{} {d}", self.name_in(View(*elem, 0), depth + 1))
            }
            Some(Ty::Record { name, union, .. }) => {
                let n = if name.is_empty() { "<anonymous>" } else { name };
                if n.contains("::") {
                    n.to_string()
                } else {
                    format!("{} {n}", if *union { "union" } else { "struct" })
                }
            }
            Some(Ty::Alias { name, to }) if name.is_empty() => {
                format!("const {}", self.name_in(View(*to, 0), depth + 1))
            }
            Some(Ty::Alias { name, .. }) => name.clone(),
            Some(Ty::Enum { name, .. }) => format!("enum {name}"),
            Some(Ty::Func) => "function".into(),
            None => "?".into(),
        }
    }

    fn char_like(&self, t: Option<u64>) -> bool {
        matches!(self.strip(t), Some(Ty::Base { size: 1, enc, name }) if [6, 8].contains(enc) || name.contains("char"))
    }

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

    fn int(&self, v: &Val, regs: &mut Regs) -> Option<i64> {
        let b = self.bytes(v, regs)?;
        let signed = matches!(self.strip(v.ty.0), Some(Ty::Base { enc: 5 | 6, .. }) | Some(Ty::Enum { .. }));
        Some(le(&b, signed))
    }

    /// The value as text, at most `MAX_TEXT` long; `depth` 0 is the top.
    fn render(&self, v: &Val, regs: &mut Regs, depth: usize) -> String {
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

    /// The value of a variable at this stop: `None` when it has no place here (optimised out, or not live).
    fn value(&self, var: &Var, enc: gimli::Encoding, fb: Option<&[u8]>, regs: &mut Regs) -> Option<At> {
        let pc = regs.rip.wrapping_sub(regs.bias);
        let expr: &[u8] = match &var.loc {
            Loc::Expr(e) => e,
            Loc::List(l) => &l.iter().find(|(lo, hi, _)| pc >= *lo && pc < *hi)?.2,
            Loc::None => return None,
        };
        eval(expr, enc, fb, regs)
    }

    fn find(&self, name: &str, regs: &mut Regs) -> Option<Val> {
        let pc = regs.rip.wrapping_sub(regs.bias);
        let local = self.in_scope(pc).into_iter().find(|(v, _, _)| v.name == name);
        if let Some((v, enc, fb)) = local {
            return Some(Val { at: self.value(v, enc, fb, regs)?, ty: View(v.ty, 0) });
        }
        let (v, enc) = self.globals.iter().find(|(v, _)| v.name == name)?;
        Some(Val { at: self.value(v, *enc, None, regs)?, ty: View(v.ty, 0) })
    }

    /// `name`, `*p`, `&x`, `p->tag`, `s.x`, `rows[y][2]`: `None` when the first name is no variable here.
    fn path(&self, src: &str, regs: &mut Regs) -> Option<std::result::Result<(Val, bool), String>> {
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
    static NOTES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn clone_at(a: &At) -> At {
    match a {
        At::Mem(x) => At::Mem(*x),
        At::Bytes(b) => At::Bytes(b.clone()),
    }
}

/// Little-endian bytes as a number, sign-extended when `signed`.
fn le(b: &[u8], signed: bool) -> i64 {
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
fn quote(b: &[u8], nul: bool) -> String {
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

/// Runs a DWARF location expression against the stopped program.
fn eval(expr: &[u8], enc: gimli::Encoding, fb: Option<&[u8]>, regs: &mut Regs) -> Option<At> {
    let reg = |r: &Regs, n: u16| -> Option<u64> {
        match n {
            16 => Some(r.rip),
            n => r.gp.get(*crate::modules::DW2GP.get(n as usize)?).copied(),
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

impl Session {
    /// The registers, frame and memory reader a location needs, or `None` when the program has no variables.
    fn with_vars<T>(&mut self, f: impl FnOnce(&Vars, &mut Regs) -> T) -> Result<Option<T>> {
        self.need_stopped()?;
        let r = self.t.regs(self.cur)?;
        let Session { t, mods, .. } = self;
        let cfa = {
            let mut rd8 = |a: u64| {
                let mut b = [0u8; 8];
                (t.read(a, &mut b) == 8).then(|| u64::from_le_bytes(b))
            };
            mods.backtrace(&r, &mut rd8, 1).first().and_then(|f| f.cfa)
        };
        let Some((vars, bias)) = mods.main_vars() else { return Ok(None) };
        let mut rd = |a: u64, n: usize| {
            let mut b = vec![0u8; n];
            (t.read(a, &mut b) == n).then_some(b)
        };
        let mut regs = Regs { gp: r.gp, rip: r.rip, cfa, bias, rd: &mut rd };
        Ok(Some(f(vars, &mut regs)))
    }

    /// `locals`: the parameters and locals of the function the program stopped in, with types and values.
    pub(crate) fn cmd_locals(&mut self) -> Result<Output> {
        let got = self.with_vars(|vars, regs| {
            let pc = regs.rip.wrapping_sub(regs.bias);
            let f = vars.func_at(pc).map(|f| f.name.clone());
            let mut l = Vec::new();
            for (v, enc, fb) in vars.in_scope(pc) {
                let ty = View(v.ty, 0);
                let val = match vars.value(v, enc, fb, regs) {
                    Some(at) => vars.render(&Val { at, ty }, regs, 0),
                    None => "<optimised out>".into(),
                };
                let kind = if v.param { "arg " } else { "" };
                l.push(format!("{kind}{} ({}) = {val}", v.name, vars.name(ty)));
            }
            (f, l)
        })?;
        match got {
            None => fail("the program has no variable info: build it with `-g` (cc -g, cargo's dev profile); `args` shows the argument registers"),
            Some((None, _)) => fail("the stop is outside the program's own functions (a library): `finish` or `bt` to get back to it"),
            Some((Some(f), l)) if l.is_empty() => Ok(rows("locals", vec![format!("{f} has no parameters or locals")], json!({}))),
            Some((Some(f), l)) => {
                let mut out = vec![format!("in {f}:")];
                out.extend(l);
                Ok(rows("locals", out, json!({"function": f})))
            }
        }
    }

    /// `print` of a source expression; `None` hands it to the register expressions.
    pub(crate) fn print_var(&mut self, src: &str) -> Option<Result<Output>> {
        let got = self
            .with_vars(|vars, regs| {
                let (v, amp) = match vars.path(src, regs)? {
                    Ok(x) => x,
                    Err(e) => return Some(Err(e)),
                };
                let notes = NOTES.with(|n| std::mem::take(&mut *n.borrow_mut()));
                let ty = vars.name(v.ty);
                let mut l = vec![if amp {
                    match v.at {
                        At::Mem(a) => format!("{src} ({ty} *) = {a:#x}"),
                        At::Bytes(_) => return Some(Err(format!("{} is in a register: it has no address", &src[1..]))),
                    }
                } else {
                    let at = match &v.at {
                        At::Mem(a) => format!(" at {a:#x}"),
                        At::Bytes(_) => String::new(),
                    };
                    format!("{src} ({ty}{at}) = {}", vars.render(&v, regs, 0))
                }];
                l.extend(notes);
                Some(Ok(l))
            })
            .ok()???;
        Some(match got {
            Ok(l) => Ok(rows("print", l, json!({}))),
            Err(e) => fail(e),
        })
    }

    /// One line of the locals for a stop report, short; `None` without variable info.
    pub(crate) fn locals_line(&mut self) -> Option<String> {
        self.with_vars(|vars, regs| {
            let pc = regs.rip.wrapping_sub(regs.bias);
            let vs = vars.in_scope(pc);
            if vs.is_empty() {
                return None;
            }
            let mut parts = Vec::new();
            for (v, enc, fb) in vs {
                let val = match vars.value(v, enc, fb, regs) {
                    Some(at) => vars.render(&Val { at, ty: View(v.ty, 0) }, regs, 1),
                    None => "<optimised out>".into(),
                };
                let mut val: String = val.chars().take(60).collect();
                if val.len() >= 60 {
                    val.push_str("...");
                }
                parts.push(format!("{} = {val}", v.name));
            }
            let mut s = format!("locals: {}", parts.join(", "));
            if s.len() > 300 {
                let cut = (0..=300).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);
                s.truncate(cut);
                s.push_str(" ... (`locals` for all)");
            }
            Some(s)
        })
        .ok()
        .flatten()
        .flatten()
    }

    /// The named parameters of the function at the stop, as `args` rows; empty without variable info.
    pub(crate) fn named_args(&mut self) -> Vec<String> {
        self.with_vars(|vars, regs| {
            let pc = regs.rip.wrapping_sub(regs.bias);
            let mut l = Vec::new();
            for (v, enc, fb) in vars.in_scope(pc).into_iter().filter(|(v, _, _)| v.param) {
                let ty = View(v.ty, 0);
                let val = match vars.value(v, enc, fb, regs) {
                    Some(at) => vars.render(&Val { at, ty }, regs, 0),
                    None => "<optimised out>".into(),
                };
                l.push(format!("{} ({}) = {val}", v.name, vars.name(ty)));
            }
            l
        })
        .ok()
        .flatten()
        .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn little_endian_numbers_and_c_strings_read_as_a_person_writes_them() {
        assert_eq!(le(&[0xff, 0xff, 0xff, 0xff], true), -1);
        assert_eq!(le(&[0xff, 0xff, 0xff, 0xff], false), 0xffff_ffff);
        assert_eq!(quote(b"ab\0cd", true), "\"ab\"");
        assert_eq!(quote(b"a\nb", true), "\"a\\nb\" (no NUL in its 3 bytes)");
    }

    fn lone(types: Vec<(u64, Ty)>) -> Vars {
        Vars { funcs: Vec::new(), globals: Vec::new(), types: types.into_iter().collect() }
    }

    #[test]
    fn types_that_contain_themselves_end_in_a_name_and_a_size_not_a_stack_overflow() {
        // a pointer to itself, an array of itself, an alias loop, and a huge array of huge arrays
        let v = lone(vec![
            (1, Ty::Ptr { to: Some(1) }),
            (2, Ty::Array { elem: Some(2), dims: vec![Some(4)] }),
            (3, Ty::Alias { name: String::new(), to: Some(3) }),
            (4, Ty::Array { elem: Some(5), dims: vec![Some(u64::MAX), Some(u64::MAX)] }),
            (5, Ty::Array { elem: Some(5), dims: vec![Some(u64::MAX)] }),
            (6, Ty::Ptr { to: Some(6) }),
        ]);
        for t in 1..=6 {
            let _ = v.name(View(Some(t), 0));
            let _ = v.size(View(Some(t), 0));
        }
        assert!(v.name(View(Some(1), 0)).contains("..."));
    }

    #[test]
    fn random_location_expressions_never_panic_or_allocate_without_bound() {
        let enc = gimli::Encoding { address_size: 8, format: gimli::Format::Dwarf32, version: 4 };
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..4000 {
            let n = (next() % 24) as usize;
            let mut e: Vec<u8> = (0..n).map(|_| next() as u8).collect();
            // lean on the opcodes that matter: piece (0x93), bit_piece (0x9d), deref (0x06), fbreg (0x91), addr (0x03)
            for byte in e.iter_mut() {
                if next() % 3 == 0 {
                    *byte = [0x93, 0x9d, 0x06, 0x91, 0x03, 0x23, 0x9f, 0x70, 0x9c][(next() % 9) as usize];
                }
            }
            let mut rd = |_: u64, n: usize| Some(vec![1u8; n.min(64)]);
            let mut regs = Regs { gp: [0x1000; 16], rip: 0x40_0000, cfa: Some(0x7fff_0000), bias: 0, rd: &mut rd };
            let t0 = std::time::Instant::now();
            let _ = eval(&e, enc, Some(&[0x9c]), &mut regs);
            assert!(t0.elapsed().as_secs() < 2, "{e:02x?}");
        }
    }
}
