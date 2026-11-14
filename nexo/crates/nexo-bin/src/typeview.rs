//! One type view: `--as <C type>` reads bytes as a type, the same way in `nexo bin bytes` and in the debugger's
//! `mem`, `print` and `ptype`.
//!
//! A type is text: a base type (`u8`..`u64`, `i8`..`i64`, `f32`, `f64`, `char`, `bool`, `ptr`, C spellings such as
//! `unsigned int` or `uint32_t`), a pointer (`item*`, as wide as the file's pointers), an array (`u32[4]`, `char[16]`,
//! `item[3]`, `item[]` for as many as the bytes hold), a struct the person named with `struct` or one the file's
//! DWARF or PDB describes, or `str` (the strings in the bytes). Scalars print as the packed table `bytes` always
//! had; structs and arrays of them print as rows with an address, an offset and a field name each.

use serde_json::{json, Value};

use crate::decomp::types::{parse_struct, Field};
use crate::page::Row;
use crate::{Error, Image, Result};

/// How deep structs may nest inside structs before a field is shown as raw bytes.
const MAX_DEPTH: usize = 6;
/// The most field rows one answer builds; past it a line says how many instances were not shown.
const MAX_CELLS: usize = 4000;
/// Array elements shown in one field row before `...`.
const SHOWN_ELEMS: usize = 16;
/// The longest array a type may name.
const MAX_ELEMS: u64 = 1 << 24;

/// What a type is.
#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Int {
        w: usize,
        signed: bool,
    },
    Char,
    Bool,
    Float {
        w: usize,
    },
    /// A pointer `w` bytes wide; `to` is what it points at as written (empty for `ptr` and `void*`).
    Ptr {
        w: usize,
        to: String,
    },
    /// `str`: the NUL-separated strings in the bytes.
    Strs,
    /// `n` elements, or as many as the bytes hold when `n` is `None`.
    Array {
        elem: Box<Ty>,
        n: Option<u64>,
    },
    Struct(Box<Layout>),
    /// A member whose type the file names but this view cannot read (an enum, a `long double`): its bytes, as a number
    /// when it is 1, 2, 4 or 8 of them.
    Raw {
        text: String,
        size: u64,
    },
}

/// A struct laid out: from the person's `struct` or from the file's debug info.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub name: String,
    pub size: u64,
    pub fields: Vec<Member>,
    /// Named by the person (`struct`), not read from the file's debug info.
    pub named: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub off: u64,
    pub name: String,
    pub ty: Ty,
}

impl Ty {
    fn size(&self) -> Option<u64> {
        match self {
            Ty::Int { w, .. } | Ty::Float { w } | Ty::Ptr { w, .. } => Some(*w as u64),
            Ty::Char | Ty::Bool => Some(1),
            Ty::Strs => None,
            Ty::Array { elem, n } => Some(elem.size()?.checked_mul((*n)?)?),
            Ty::Struct(l) => Some(l.size),
            Ty::Raw { size, .. } => Some(*size),
        }
    }

    fn align(&self) -> u64 {
        match self {
            Ty::Array { elem, .. } => elem.align(),
            Ty::Struct(l) => l.fields.iter().map(|m| m.ty.align()).max().unwrap_or(1),
            Ty::Raw { size, .. } => (*size).clamp(1, 8).next_power_of_two().min(8),
            other => other.size().unwrap_or(1).clamp(1, 8),
        }
    }

    /// A type that is one value, shown in the packed table.
    fn is_scalar(&self) -> bool {
        matches!(self, Ty::Int { .. } | Ty::Char | Ty::Bool | Ty::Float { .. } | Ty::Ptr { .. })
    }

    pub fn name(&self) -> String {
        match self {
            Ty::Int { w, signed } => format!("{}{}", if *signed { 'i' } else { 'u' }, w * 8),
            Ty::Char => "char".into(),
            Ty::Bool => "bool".into(),
            Ty::Float { w } => format!("f{}", w * 8),
            Ty::Ptr { to, .. } if to.is_empty() => "ptr".into(),
            Ty::Ptr { to, .. } => format!("{to}*"),
            Ty::Strs => "str".into(),
            Ty::Array { .. } => {
                let mut dims = String::new();
                let mut t = self;
                while let Ty::Array { elem, n } = t {
                    dims.push_str(&n.map_or("[]".to_string(), |n| format!("[{n}]")));
                    t = elem;
                }
                format!("{}{dims}", t.name())
            }
            Ty::Struct(l) => l.name.clone(),
            Ty::Raw { text, .. } => text.clone(),
        }
    }
}

/// A type the question could not resolve: a name nothing knows, or text that is not a type.
enum Miss {
    Unknown(String),
    Syntax(String),
}

/// A resolved `--as` text.
#[derive(Debug, Clone)]
pub struct TypeView {
    ty: Ty,
}

/// The type `text` names in `img`: base types first, then the structs the person named with `struct`, then the
/// file's DWARF or PDB types. An unknown type says what was looked at and what is there.
pub fn resolve(img: &Image, text: &str) -> Result<TypeView> {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() {
        return Err(Error::Bad("--as needs a type: u32, char[16], item[4], a struct named with `struct`".into()));
    }
    match parse_ty(img, &t, 0) {
        Ok(ty) => Ok(TypeView { ty }),
        Err(Miss::Unknown(name)) => Err(Error::Bad(unknown(img, &name))),
        Err(Miss::Syntax(why)) => Err(Error::Bad(format!("{t:?} is not a type: {why}"))),
    }
}

impl TypeView {
    pub fn ty(&self) -> &Ty {
        &self.ty
    }

    /// The type as the answers write it (`unsigned int` is `u32`).
    pub fn name(&self) -> String {
        self.ty.name()
    }

    /// The bytes a struct or an array of `n` elements takes; `None` for a scalar, `str` and `T[]`, which read as many
    /// bytes as they are given.
    pub fn fixed_size(&self) -> Option<u64> {
        match &self.ty {
            Ty::Struct(_) | Ty::Array { .. } => self.ty.size(),
            _ => None,
        }
    }

    /// The bytes one value of this type takes (a scalar's width too); `None` for `str` and `T[]`.
    pub fn value_size(&self) -> Option<u64> {
        self.ty.size()
    }

    /// The type as rows with addresses: the packed table for scalars (and arrays of them), one row per field for
    /// structs. `data` starts at `addr`; `name` says what a pointer value points at.
    pub fn rows(&self, data: &[u8], addr: u64, name: &dyn Fn(u64) -> Option<String>) -> Vec<Row> {
        match &self.ty {
            Ty::Strs => strs_rows(data, addr),
            Ty::Array { elem, n } if elem.is_scalar() => {
                let w = elem.size().unwrap_or(1) as usize;
                let want = n.map(|n| (n as usize).saturating_mul(w));
                let mut rows = if **elem == Ty::Char && n.is_some() {
                    // a `char[N]` is one string
                    vec![field_row(
                        &cell_of_chars(addr, 0, "", &data[..want.unwrap_or(0).min(data.len())], &self.ty),
                        0,
                        0,
                    )]
                } else {
                    table_rows(elem, &data[..want.map_or(data.len(), |w| w.min(data.len()))], addr, name)
                };
                if let Some(want) = want {
                    if data.len() < want && **elem != Ty::Char {
                        let a = addr + (data.len() - data.len() % w.max(1)) as u64;
                        rows.push(note(
                            a,
                            format!(
                                "... the bytes hold {} of the {} elements of {}",
                                data.len() / w.max(1),
                                n.unwrap_or(0),
                                self.name()
                            ),
                        ));
                    } else if data.len() > want {
                        rows.push(note(
                            addr + want as u64,
                            format!("... {} more bytes after the {want} of {}", data.len() - want, self.name()),
                        ));
                    }
                }
                rows
            }
            t if t.is_scalar() => table_rows(t, data, addr, name),
            t => composite_rows(t, data, addr, name),
        }
    }

    /// What `ptype` shows: the type, its size and, for a struct (or a pointer to one), the fields.
    pub fn describe(&self, img: &Image) -> Vec<Row> {
        let mut out: Vec<String> = Vec::new();
        describe_ty(img, &self.ty, &mut out, 0);
        out.into_iter()
            .enumerate()
            .map(|(i, text)| Row { key: i as u64, json: Some(json!({"line": text})), text })
            .collect()
    }
}

fn describe_ty(img: &Image, ty: &Ty, out: &mut Vec<String>, depth: usize) {
    match ty {
        Ty::Struct(l) => {
            let from = if l.named { "named with `struct`" } else { "from the file's debug info" };
            out.push(format!("struct {} ({} bytes, {from})", l.name, l.size));
            let w = l.fields.iter().map(|m| m.name.len()).max().unwrap_or(0).min(24);
            for m in &l.fields {
                let size = m.ty.size().map_or(String::new(), |s| format!("  {s} bytes"));
                out.push(format!("  +{:#04x}  {:<w$}  {}{size}", m.off, m.name, m.ty.name()));
            }
        }
        Ty::Ptr { to, .. } if !to.is_empty() && depth == 0 => {
            out.push(format!("{}: a {}-byte pointer to {to}", ty.name(), ty.size().unwrap_or(8)));
            if let Ok(t) = parse_ty(img, to, 1) {
                if matches!(t, Ty::Struct(_)) {
                    describe_ty(img, &t, out, depth + 1);
                }
            }
        }
        Ty::Array { elem, n } if matches!(**elem, Ty::Struct(_)) && depth == 0 => {
            let size = ty.size().map_or(String::new(), |s| format!(", {s} bytes"));
            out.push(format!(
                "{}: {} of {}{size}",
                ty.name(),
                n.map_or("as many as fit".to_string(), |n| format!("{n} elements")),
                elem.name()
            ));
            describe_ty(img, elem, out, depth + 1);
        }
        other => {
            let kind = match other {
                Ty::Int { signed: true, .. } => "signed integer",
                Ty::Int { .. } => "unsigned integer",
                Ty::Char => "character",
                Ty::Bool => "boolean",
                Ty::Float { .. } => "floating point",
                Ty::Ptr { .. } => "pointer",
                Ty::Strs => "the NUL-separated strings in the bytes",
                Ty::Array { .. } => "array",
                Ty::Raw { .. } => "a type this view cannot read, shown as bytes",
                Ty::Struct(_) => "struct",
            };
            let size = other.size().map_or(String::new(), |s| format!(", {s} bytes"));
            out.push(format!("{}: {kind}{size}", other.name()));
        }
    }
}

// ---- resolving ----

fn parse_ty(img: &Image, text: &str, depth: usize) -> std::result::Result<Ty, Miss> {
    let (head, dims) = split_dims(text)?;
    let head = head.trim();
    let stars = head.chars().rev().take_while(|c| *c == '*' || c.is_whitespace()).filter(|c| *c == '*').count();
    let base = head.trim_end_matches(|c: char| c == '*' || c.is_whitespace());
    let base = base
        .split_whitespace()
        .filter(|w| !matches!(*w, "const" | "volatile" | "restrict"))
        .collect::<Vec<_>>()
        .join(" ");
    let (kw, name) = match base.split_once(' ') {
        Some(("struct" | "union" | "enum" | "class", rest)) => (true, rest.trim().to_string()),
        _ => (false, base.clone()),
    };
    if name.is_empty() {
        return Err(Miss::Syntax("no type name".into()));
    }
    let pw = img.ptr_size();
    if name == "str" && (stars > 0 || !dims.is_empty()) {
        return Err(Miss::Syntax(
            "`str` is every string in the bytes; it has no pointer or array form (use char* or char[N])".into(),
        ));
    }
    let mut ty = if stars > 0 {
        let void = matches!(name.as_str(), "void" | "ptr");
        // at the top, a pointer to a name nothing knows is a typo, not a pointer
        let known = void || (!kw && base_ty(&name, pw).is_some()) || layout(img, &name, depth)?.is_some();
        if depth == 0 && !known {
            return Err(Miss::Unknown(name));
        }
        let to = if void { String::new() } else { name.clone() };
        Ty::Ptr { w: pw, to: format!("{to}{}", "*".repeat(stars - 1)) }
    } else if let (false, Some(t)) = (kw, base_ty(&name, pw)) {
        t
    } else if let Some(l) = layout(img, &name, depth)? {
        Ty::Struct(Box::new(l))
    } else {
        return Err(Miss::Unknown(name));
    };
    for d in dims.iter().rev() {
        ty = Ty::Array { elem: Box::new(ty), n: *d };
    }
    Ok(ty)
}

/// `u32[4][2]` into (`u32`, [Some(4), Some(2)]); `[]` is `None`.
fn split_dims(text: &str) -> std::result::Result<(&str, Vec<Option<u64>>), Miss> {
    let mut rest = text.trim_end();
    let mut dims = Vec::new();
    while let Some(r) = rest.strip_suffix(']') {
        let Some(open) = r.rfind('[') else { return Err(Miss::Syntax("a `]` with no `[`".into())) };
        let inner = r[open + 1..].trim();
        dims.push(if inner.is_empty() {
            None
        } else {
            let n = crate::parse_addr(inner)
                .map_err(|_| Miss::Syntax(format!("the array length {inner:?} is not a number")))?;
            if n == 0 || n > MAX_ELEMS {
                return Err(Miss::Syntax(format!("an array has 1 to {MAX_ELEMS} elements; {n} was asked")));
            }
            Some(n)
        });
        rest = r[..open].trim_end();
    }
    dims.reverse();
    Ok((rest, dims))
}

fn int(w: usize, signed: bool) -> Option<Ty> {
    Some(Ty::Int { w, signed })
}

fn base_ty(name: &str, pw: usize) -> Option<Ty> {
    match name {
        "u8" | "uint8_t" | "uchar" | "unsigned char" | "byte" => int(1, false),
        "i8" | "int8_t" | "signed char" => int(1, true),
        "u16" | "uint16_t" | "ushort" | "unsigned short" | "unsigned short int" => int(2, false),
        "i16" | "int16_t" | "short" | "short int" | "signed short" => int(2, true),
        "u32" | "uint32_t" | "uint" | "unsigned" | "unsigned int" => int(4, false),
        "i32" | "int32_t" | "int" | "signed int" | "signed" => int(4, true),
        "u64" | "uint64_t" | "unsigned long long" | "unsigned long long int" => int(8, false),
        "i64" | "int64_t" | "long long" | "long long int" => int(8, true),
        "usize" | "size_t" | "uintptr_t" => int(pw, false),
        "isize" | "ssize_t" | "intptr_t" | "ptrdiff_t" => int(pw, true),
        "f32" | "float" => Some(Ty::Float { w: 4 }),
        "f64" | "double" => Some(Ty::Float { w: 8 }),
        "char" => Some(Ty::Char),
        "bool" | "_Bool" => Some(Ty::Bool),
        "ptr" => Some(Ty::Ptr { w: pw, to: String::new() }),
        "str" => Some(Ty::Strs),
        _ => None,
    }
}

/// Where a struct's fields are written: the person's `struct` first (it replaces the file's by name), then DWARF or
/// PDB.
fn struct_text<'a>(img: &'a Image, name: &str) -> Option<(&'a str, bool)> {
    if let Some(t) = img.user.structs.get(name) {
        return Some((t, true));
    }
    img.dwarf().structs.get(name).map(|t| (t.as_str(), false))
}

/// The layout of the struct `name`, `None` when nothing by that name is known.
fn layout(img: &Image, name: &str, depth: usize) -> std::result::Result<Option<Layout>, Miss> {
    let Some((text, named)) = struct_text(img, name) else { return Ok(None) };
    if depth > MAX_DEPTH {
        return Err(Miss::Unknown(name.to_string()));
    }
    let mut fields: Vec<Field> = parse_struct(text).map_err(|e| {
        Miss::Syntax(format!("struct {name} is stored as {text:?}, which is not offset:type:name: {e}"))
    })?;
    fields.sort_by_key(|f| f.off);
    let known = if named { None } else { img.dwarf().struct_sizes.get(name).copied() };
    let mut members: Vec<Member> = Vec::new();
    for (i, f) in fields.iter().enumerate() {
        let ty = match parse_ty(img, &f.ty, depth + 1) {
            Ok(t) if !matches!(t, Ty::Strs) => t,
            Ok(_) | Err(Miss::Unknown(_)) => {
                // the bytes up to the next field (or the end of the struct) are what it is
                let next = fields[i + 1..].iter().map(|n| n.off).find(|o| *o > f.off).or(known);
                let Some(end) = next else {
                    return Err(Miss::Syntax(format!(
                        "field {} of struct {name} has the type {:?}, which is not a base type or a struct, and it is the last field of a struct whose size is not known",
                        f.name, f.ty
                    )));
                };
                Ty::Raw { text: f.ty.clone(), size: end.saturating_sub(f.off) }
            }
            Err(e) => return Err(e),
        };
        members.push(Member { off: f.off, name: f.name.clone(), ty });
    }
    let end = members.iter().map(|m| m.off + m.ty.size().unwrap_or(0)).max().unwrap_or(0);
    let align = members.iter().map(|m| m.ty.align()).max().unwrap_or(1);
    let size = known.unwrap_or(end.div_ceil(align) * align);
    Ok(Some(Layout { name: name.to_string(), size, fields: members, named }))
}

/// The answer for a type nothing knows: what was looked at, and what the file does have.
fn unknown(img: &Image, name: &str) -> String {
    let named: Vec<&str> = img.user.structs.keys().map(String::as_str).collect();
    let dw = img.dwarf();
    let named_part = if named.is_empty() { String::new() } else { format!(" (named: {})", list(&named, 5)) };
    let debug_part = if dw.structs.is_empty() && dw.protos.is_empty() {
        "the file has no debug info".to_string()
    } else if dw.structs.is_empty() {
        "the file's debug info has no struct types".to_string()
    } else {
        let all: Vec<&str> = dw.structs.keys().map(String::as_str).collect();
        format!("the file's debug info has no type of that name (it has {}: {})", all.len(), list(&all, 5))
    };
    let want = name.to_ascii_lowercase();
    let close: Vec<&str> = img
        .user
        .structs
        .keys()
        .chain(dw.structs.keys())
        .map(String::as_str)
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l != want && (l.contains(&want) || want.contains(&l))
        })
        .take(3)
        .collect();
    let close = if close.is_empty() { String::new() } else { format!("; close: {}", close.join(", ")) };
    format!("no type `{name}`: not a base type, not named with `struct`{named_part}, and {debug_part}{close}")
}

fn list(names: &[&str], max: usize) -> String {
    let mut s = names.iter().take(max).copied().collect::<Vec<_>>().join(", ");
    if names.len() > max {
        s.push_str(&format!(" and {} more", names.len() - max));
    }
    s
}

// ---- rows ----

fn note(key: u64, text: String) -> Row {
    Row { key, text, json: None }
}

fn le(b: &[u8]) -> u64 {
    b.iter().rev().fold(0u64, |a, x| (a << 8) | u64::from(*x))
}

fn sext(v: u64, w: usize) -> i64 {
    let sh = 64 - 8 * w as u32;
    ((v << sh) as i64) >> sh
}

/// Elements of a scalar type, packed in rows the way `bytes --as` always showed them.
fn table_rows(ty: &Ty, data: &[u8], addr: u64, name: &dyn Fn(u64) -> Option<String>) -> Vec<Row> {
    let w = ty.size().unwrap_or(1) as usize;
    let per = match (ty, w) {
        (Ty::Ptr { .. }, _) => 1,
        (_, 1) => 16,
        (_, 2 | 4) => 8,
        _ => 4,
    };
    let mut rows: Vec<Row> = Vec::new();
    let n = data.len() / w;
    let mut i = 0;
    while i < n {
        let k = per.min(n - i);
        let a = addr + (i * w) as u64;
        let mut cells: Vec<String> = Vec::new();
        let mut vals: Vec<Value> = Vec::new();
        for j in 0..k {
            let raw = le(&data[(i + j) * w..(i + j + 1) * w]);
            let (cell, val) = match ty {
                Ty::Float { w: 4 } => {
                    let v = f32::from_bits(raw as u32);
                    (format!("{v:>12}"), json!(v))
                }
                Ty::Float { .. } => {
                    let v = f64::from_bits(raw);
                    (format!("{v:>20}"), json!(v))
                }
                Ty::Int { signed: true, w } if *w > 1 => {
                    let v = sext(raw, *w);
                    (format!("{v:>width$}", width = [0, 0, 6, 0, 11, 0, 0, 0, 20][*w]), json!(v))
                }
                Ty::Ptr { w, .. } => (format!("{raw:#0width$x}", width = 2 + 2 * w), json!(raw)),
                _ => match w {
                    1 => (format!("{raw:02x}"), json!(raw)),
                    2 => (format!("{raw:#06x}"), json!(raw)),
                    4 => (format!("{raw:#010x}"), json!(raw)),
                    _ => (format!("{raw:#018x}"), json!(raw)),
                },
            };
            cells.push(cell);
            vals.push(val);
        }
        let mut text = format!("{a:#010x}  {}", cells.join(" "));
        let mut j = json!({"addr": a, "values": vals});
        if w == 1 {
            let ascii: String =
                data[i..i + k].iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
            text.push_str(&format!("{}  |{ascii}|", " ".repeat(3 * (per - k))));
            j["ascii"] = json!(ascii);
        }
        if let Ty::Ptr { .. } = ty {
            let what = name(le(&data[i * w..(i + 1) * w]));
            if let Some(wh) = &what {
                text.push_str(&format!("  {wh}"));
            }
            j["target"] = json!(what);
        }
        rows.push(Row { key: a, text, json: Some(j) });
        i += k;
    }
    let rest = data.len() - n * w;
    if rest > 0 {
        let a = addr + (n * w) as u64;
        rows.push(Row {
            key: a,
            text: format!("{a:#010x}  ({rest} trailing bytes not shown: not a whole {})", ty.name()),
            json: None,
        });
    }
    rows
}

fn escape(b: &[u8]) -> String {
    b.iter()
        .map(|&b| match b {
            b'"' => "\\\"".to_string(),
            b'\\' => "\\\\".to_string(),
            0x20..=0x7e => (b as char).to_string(),
            _ => format!("\\x{b:02x}"),
        })
        .collect()
}

/// `--as str`: one row per string; two or more NULs in a row are padding, not empty strings.
fn strs_rows(data: &[u8], addr: u64) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let nuls = data[pos..].iter().take_while(|&&b| b == 0).count();
        if nuls > 1 {
            let a = addr + pos as u64;
            rows.push(Row {
                key: a,
                text: format!("{a:#010x}  ... {nuls} NUL bytes skipped"),
                json: Some(json!({"addr": a, "nul_bytes": nuls})),
            });
            pos += nuls;
            continue;
        }
        let end = data[pos..].iter().position(|&b| b == 0).map_or(data.len(), |n| pos + n);
        // escape here, once: `shorten` would escape the backslash of a `\x0a` a second time
        let text = escape(&data[pos..end]);
        let shown = if text.chars().count() > 200 {
            format!("{}...", text.chars().take(200).collect::<String>())
        } else {
            text.clone()
        };
        let a = addr + pos as u64;
        rows.push(Row {
            key: a,
            text: format!("{a:#010x}  \"{shown}\""),
            json: Some(json!({"addr": a, "text": text})),
        });
        pos = end + 1;
    }
    rows
}

/// One value of a type that is a field row: `addr`, offset from the start of the instance, its name and what it holds.
struct Cell {
    addr: u64,
    off: u64,
    name: String,
    ty: String,
    val: String,
    json: Value,
}

fn field_row(c: &Cell, nw: usize, tw: usize) -> Row {
    let text = format!("{:#010x}  +{:#04x}  {:<nw$}  {:<tw$}  = {}", c.addr, c.off, c.name, c.ty, c.val);
    let json = json!({"addr": c.addr, "off": c.off, "name": c.name, "type": c.ty, "value": c.json});
    Row { key: c.addr, text, json: Some(json) }
}

fn cell_of_chars(addr: u64, off: u64, name: &str, b: &[u8], ty: &Ty) -> Cell {
    let nul = b.iter().position(|&c| c == 0);
    let end = nul.unwrap_or(b.len());
    let mut val = format!("\"{}\"", escape(&b[..end]));
    if nul.is_none() {
        val.push_str(&format!(" (no NUL in the {} bytes)", b.len()));
    }
    let text = String::from_utf8_lossy(&b[..end]).into_owned();
    Cell { addr, off, name: name.to_string(), ty: ty.name(), val, json: json!(text) }
}

/// A scalar as a field value, and as JSON.
fn scalar(ty: &Ty, b: &[u8], name: &dyn Fn(u64) -> Option<String>) -> (String, Value) {
    let raw = le(b);
    match ty {
        Ty::Float { w: 4 } => {
            let v = f32::from_bits(raw as u32);
            (v.to_string(), json!(v))
        }
        Ty::Float { .. } => {
            let v = f64::from_bits(raw);
            (v.to_string(), json!(v))
        }
        Ty::Int { signed: true, w } => {
            let v = sext(raw, *w);
            let s = if (0..10).contains(&v) { v.to_string() } else { format!("{v} ({raw:#x})") };
            (s, json!(v))
        }
        Ty::Char => {
            let c = match raw as u8 {
                0 => "\\0".to_string(),
                b'\n' => "\\n".to_string(),
                b'\t' => "\\t".to_string(),
                b'\'' => "\\'".to_string(),
                b'\\' => "\\\\".to_string(),
                x @ 0x20..=0x7e => (x as char).to_string(),
                x => format!("\\x{x:02x}"),
            };
            (format!("{} '{c}'", raw as u8 as i8), json!(raw as u8 as i8))
        }
        Ty::Bool => match raw {
            0 => ("false".into(), json!(false)),
            1 => ("true".into(), json!(true)),
            n => (format!("{n} ({n:#x})"), json!(n)),
        },
        Ty::Ptr { .. } => {
            let mut s = format!("{raw:#x}");
            let what = name(raw);
            if let Some(w) = &what {
                s.push_str(&format!("  {w}"));
            }
            (s, json!({"value": raw, "target": what}))
        }
        _ => (if raw < 10 { raw.to_string() } else { format!("{raw} ({raw:#x})") }, json!(raw)),
    }
}

fn hex_bytes(b: &[u8]) -> String {
    let shown = b.iter().take(32).map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ");
    if b.len() > 32 {
        format!("{shown} ... ({} bytes)", b.len())
    } else {
        shown
    }
}

struct Walk<'a> {
    data: &'a [u8],
    addr: u64,
    /// Where the instance being walked starts, as an offset into `data`.
    base: u64,
    name: &'a dyn Fn(u64) -> Option<String>,
    cells: Vec<Cell>,
    /// Bytes of the instance the data reaches, when a field did not fit.
    cut: Option<u64>,
    limit: usize,
    full: bool,
}

impl Walk<'_> {
    fn leaf(&mut self, off: u64, size: u64) -> Option<&[u8]> {
        let (lo, hi) = (off as usize, (off + size) as usize);
        if hi > self.data.len() {
            self.cut = Some((self.data.len() as u64).saturating_sub(self.base));
            return None;
        }
        Some(&self.data[lo..hi])
    }

    fn push(&mut self, off: u64, name: &str, ty: String, val: String, json: Value) {
        self.cells.push(Cell { addr: self.addr + off, off: off - self.base, name: name.to_string(), ty, val, json });
    }

    /// The cells of `ty` found at offset `off` of the data, under the name `label`.
    fn go(&mut self, ty: &Ty, off: u64, label: &str) {
        if self.cut.is_some() || self.full {
            return;
        }
        if self.cells.len() >= self.limit {
            self.full = true;
            return;
        }
        let Some(size) = ty.size() else { return };
        match ty {
            Ty::Struct(l) => {
                for m in &l.fields {
                    let child = if label.is_empty() { m.name.clone() } else { format!("{label}.{}", m.name) };
                    self.go(&m.ty, off + m.off, &child);
                }
            }
            Ty::Array { elem, n } => match &**elem {
                Ty::Char => {
                    let Some(b) = self.leaf(off, size).map(<[u8]>::to_vec) else { return };
                    let c = cell_of_chars(self.addr + off, off - self.base, label, &b, ty);
                    self.cells.push(c);
                }
                e if e.is_scalar() => {
                    let Some(b) = self.leaf(off, size).map(<[u8]>::to_vec) else { return };
                    let w = e.size().unwrap_or(1) as usize;
                    let (val, j): (String, Value) = if matches!(e, Ty::Int { w: 1, .. } | Ty::Bool) {
                        (hex_bytes(&b), json!(b))
                    } else {
                        let all: Vec<(String, Value)> = b.chunks(w).map(|c| scalar(e, c, self.name)).collect();
                        let mut s = all.iter().take(SHOWN_ELEMS).map(|x| x.0.clone()).collect::<Vec<_>>().join(", ");
                        if all.len() > SHOWN_ELEMS {
                            s.push_str(&format!(", ... ({} elements)", all.len()));
                        }
                        (format!("[{s}]"), Value::Array(all.into_iter().map(|x| x.1).collect()))
                    };
                    self.push(off, label, ty.name(), val, j);
                }
                e => {
                    let es = e.size().unwrap_or(0);
                    for i in 0..n.unwrap_or(0) {
                        self.go(e, off + i * es, &format!("{label}[{i}]"));
                    }
                }
            },
            Ty::Raw { text, size } => {
                let Some(b) = self.leaf(off, *size).map(<[u8]>::to_vec) else { return };
                let (val, j) = if matches!(size, 1 | 2 | 4 | 8) {
                    scalar(&Ty::Int { w: *size as usize, signed: false }, &b, self.name)
                } else {
                    (hex_bytes(&b), json!(b))
                };
                self.push(off, label, text.clone(), val, j);
            }
            t => {
                let Some(b) = self.leaf(off, size).map(<[u8]>::to_vec) else { return };
                let (val, j) = scalar(t, &b, self.name);
                self.push(off, label, t.name(), val, j);
            }
        }
    }
}

/// Structs, and arrays of what is not a scalar: a heading row per instance, then one row per field.
fn composite_rows(ty: &Ty, data: &[u8], addr: u64, name: &dyn Fn(u64) -> Option<String>) -> Vec<Row> {
    // an array of structs is one instance per element; anything else is one instance
    let (elem, count, indexed): (&Ty, u64, bool) = match ty {
        Ty::Array { elem, n } if matches!(**elem, Ty::Struct(_)) => {
            let fit = (data.len() as u64).div_ceil(elem.size().unwrap_or(1).max(1)).max(1);
            (elem, n.map_or(fit, |n| n.min(fit)), true)
        }
        other => (other, 1, false),
    };
    let es = elem.size().unwrap_or(0);
    let mut rows: Vec<Row> = Vec::new();
    let mut last_key: Option<u64> = None;
    // keys never go down and never repeat, so `--from` resumes where a page was cut even when fields overlap (unions)
    let mut put = |rows: &mut Vec<Row>, mut r: Row| {
        r.key = last_key.map_or(r.key, |k| r.key.max(k + 1));
        last_key = Some(r.key);
        rows.push(r);
    };
    let mut note_at: Option<(u64, String)> = None;
    let mut cells = 0usize;
    for i in 0..count {
        let base = i * es;
        let at = addr + base;
        let mut w =
            Walk { data, addr, base, name, cells: Vec::new(), cut: None, limit: MAX_CELLS - cells, full: false };
        w.go(elem, base, "");
        cells += w.cells.len();
        let label = if indexed { format!("{}[{i}]", elem.name()) } else { ty.name() };
        let nw = w.cells.iter().map(|c| c.name.chars().count()).max().unwrap_or(0).min(24);
        let tw = w.cells.iter().map(|c| c.ty.chars().count()).max().unwrap_or(0).min(16);
        let head = format!("{at:#010x}  {label}  ({es} bytes)");
        put(&mut rows, Row { key: at, text: head, json: Some(json!({"addr": at, "type": label, "size": es})) });
        for c in &w.cells {
            put(&mut rows, field_row(c, nw, tw));
        }
        if let Some(cut) = w.cut {
            note_at = Some((
                addr + data.len() as u64,
                format!(
                    "... the bytes end {cut:#x} bytes into the {es} of {label}: the fields after that are not shown"
                ),
            ));
            break;
        }
        if w.full {
            let left = count - i - 1;
            let more = if left > 0 { format!(" and {left} more instances") } else { String::new() };
            note_at = Some((
                at + es,
                format!(
                    "... {MAX_CELLS} field rows built; the rest of {label}{more} is not shown: ask for a smaller range"
                ),
            ));
            break;
        }
    }
    if note_at.is_none() {
        let used = if indexed { count * es } else { es };
        if es > 0 && data.len() as u64 > used {
            let rest = data.len() as u64 - used;
            note_at = Some((addr + used, format!("... {rest} more bytes after the {used} of {}", ty.name())));
        }
    }
    if let Some((key, text)) = note_at {
        put(&mut rows, Row { key, text, json: None });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dims_split_off_the_end() {
        let ok = |t: &str| split_dims(t).ok().map(|(h, d)| (h.to_string(), d));
        assert_eq!(ok("u32[4]"), Some(("u32".into(), vec![Some(4)])));
        assert_eq!(ok("char *[2][0x3]"), Some(("char *".into(), vec![Some(2), Some(3)])));
        assert_eq!(ok("item[]"), Some(("item".into(), vec![None])));
        assert!(split_dims("u32[x]").is_err());
        assert!(split_dims("u32[0]").is_err());
    }

    #[test]
    fn base_types_have_their_widths() {
        assert_eq!(base_ty("unsigned int", 8), Some(Ty::Int { w: 4, signed: false }));
        assert_eq!(base_ty("size_t", 4), Some(Ty::Int { w: 4, signed: false }));
        assert_eq!(base_ty("ptr", 4), Some(Ty::Ptr { w: 4, to: String::new() }));
        assert_eq!(base_ty("u128", 8), None);
    }
}
