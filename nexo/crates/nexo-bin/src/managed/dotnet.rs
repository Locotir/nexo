//! .NET assemblies: the CLI header inside a PE file, the metadata tables (ECMA-335 partition II),
//! signatures, and CIL method bodies with resolved tokens. Pure Rust, bounds-checked everywhere.

use super::il::{self, Arg};
use super::{Code, FieldDef, Insn, MethodDef, TypeDef};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy)]
enum Col {
    U1,
    U2,
    U4,
    Str,
    Guid,
    Blob,
    Tab(usize),
    /// Index into the coded-index set (tag bits, tables; 255 = unused).
    Coded(&'static [u8], u32),
}
use Col::*;

const TDR: Col = Coded(&[2, 1, 27], 2);
const HC: Col = Coded(&[4, 8, 23], 2);
const HCA: Col = Coded(&[6, 4, 1, 2, 8, 9, 10, 0, 14, 23, 20, 17, 26, 27, 32, 35, 38, 39, 40, 42, 44, 43], 5);
const HFM: Col = Coded(&[4, 8], 1);
const HDS: Col = Coded(&[2, 6, 32], 2);
const MRP: Col = Coded(&[2, 1, 26, 6, 27], 3);
const HS: Col = Coded(&[20, 23], 1);
const MDR: Col = Coded(&[6, 10], 1);
const MF: Col = Coded(&[4, 6], 1);
const IMPL: Col = Coded(&[38, 35, 39], 2);
const CAT: Col = Coded(&[255, 255, 6, 10, 255], 3);
const RS: Col = Coded(&[0, 26, 35, 1], 2);
const TMD: Col = Coded(&[2, 6], 1);

fn schema(t: usize) -> &'static [Col] {
    match t {
        0 => &[U2, Str, Guid, Guid, Guid],
        1 => &[RS, Str, Str],
        2 => &[U4, Str, Str, TDR, Tab(4), Tab(6)],
        3 => &[Tab(4)],
        4 => &[U2, Str, Blob],
        5 => &[Tab(6)],
        6 => &[U4, U2, U2, Str, Blob, Tab(8)],
        7 => &[Tab(8)],
        8 => &[U2, U2, Str],
        9 => &[Tab(2), TDR],
        10 => &[MRP, Str, Blob],
        11 => &[U1, U1, HC, Blob],
        12 => &[HCA, CAT, Blob],
        13 => &[HFM, Blob],
        14 => &[U2, HDS, Blob],
        15 => &[U2, U4, Tab(2)],
        16 => &[U4, Tab(4)],
        17 => &[Blob],
        18 => &[Tab(2), Tab(20)],
        19 => &[Tab(20)],
        20 => &[U2, Str, TDR],
        21 => &[Tab(2), Tab(23)],
        22 => &[Tab(23)],
        23 => &[U2, Str, Blob],
        24 => &[U2, Tab(6), HS],
        25 => &[Tab(2), MDR, MDR],
        26 => &[Str],
        27 => &[Blob],
        28 => &[U2, MF, Str, Tab(26)],
        29 => &[U4, Tab(4)],
        30 => &[U4, U4],
        31 => &[U4],
        32 => &[U4, U2, U2, U2, U2, U4, Blob, Str, Str],
        33 => &[U4],
        34 => &[U4, U4, U4],
        35 => &[U2, U2, U2, U2, U4, Blob, Str, Str, Blob],
        36 => &[U4, Tab(35)],
        37 => &[U4, U4, U4, Tab(35)],
        38 => &[U4, Str, Blob],
        39 => &[U4, U4, Str, Str, IMPL],
        40 => &[U4, U4, Str, IMPL],
        41 => &[Tab(2), Tab(2)],
        42 => &[U2, U2, TMD, Str],
        43 => &[MDR, Blob],
        44 => &[Tab(42), TDR],
        _ => &[],
    }
}

struct Pe<'a> {
    d: &'a [u8],
    /// `(virtual address, virtual size, raw offset, raw size)`.
    sections: Vec<(u32, u32, u32, u32)>,
    clr: (u32, u32),
}

fn rd16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
fn rd32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

/// True when the PE headers in `head` carry a CLI header directory (a .NET assembly, pure or mixed).
pub fn has_cli_header(head: &[u8]) -> bool {
    pe_open(head).is_ok_and(|p| p.clr.0 != 0)
}

/// `Some(true)` for a CLI header with COMIMAGE_FLAGS_ILONLY, `Some(false)` without it (mixed-mode C++/CLI
/// or native code inside), `None` when there is no CLI header or it is not inside `head`.
fn il_only(head: &[u8]) -> Option<bool> {
    let pe = pe_open(head).ok()?;
    if pe.clr.0 == 0 {
        return None;
    }
    let cli = pe.off(pe.clr.0)?;
    Some(rd32(head, cli.checked_add(16)?)? & 1 != 0)
}

/// True when the PE headers in `head` say this is an IL-only .NET assembly: what `managed` reads whole.
pub fn is_dotnet(head: &[u8]) -> bool {
    il_only(head) == Some(true)
}

/// True for a .NET assembly that also holds native code (C++/CLI): the native ops read it, and `managed`
/// reads its metadata and IL.
pub fn is_mixed_mode(head: &[u8]) -> bool {
    il_only(head) == Some(false)
}

fn pe_open(d: &[u8]) -> Result<Pe<'_>, String> {
    if d.get(..2) != Some(b"MZ") {
        return Err("no MZ header".into());
    }
    let pe = rd32(d, 0x3c).ok_or("cut short")? as usize;
    if d.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err("no PE signature".into());
    }
    let nsec = rd16(d, pe + 6).ok_or("cut short")? as usize;
    let optsz = rd16(d, pe + 20).ok_or("cut short")? as usize;
    let opt = pe + 24;
    let base = match rd16(d, opt) {
        Some(0x10b) => 96,
        Some(0x20b) => 112,
        _ => return Err("unknown optional header".into()),
    };
    let ndirs = rd32(d, opt + base - 4).unwrap_or(0) as usize;
    let clr = if ndirs > 14 {
        (rd32(d, opt + base + 14 * 8).ok_or("cut short")?, rd32(d, opt + base + 14 * 8 + 4).ok_or("cut short")?)
    } else {
        (0, 0)
    };
    let mut sections = Vec::new();
    for i in 0..nsec {
        let s = opt + optsz + i * 40;
        // Header-only prefixes (detection reads 4 KB) may not hold every section: take what is there.
        let (Some(vs), Some(va), Some(rs), Some(ro)) =
            (rd32(d, s + 8), rd32(d, s + 12), rd32(d, s + 16), rd32(d, s + 20))
        else {
            break;
        };
        sections.push((va, vs, ro, rs));
    }
    Ok(Pe { d, sections, clr })
}

impl Pe<'_> {
    fn off(&self, rva: u32) -> Option<usize> {
        // only bytes the file really holds: past the raw size an RVA is zero fill, not the next section
        let s = self.sections.iter().find(|(va, _, _, rs)| rva.checked_sub(*va).is_some_and(|x| x < *rs))?;
        Some((rva - s.0).checked_add(s.2)? as usize)
    }
}

struct Layout {
    rows: [usize; 64],
    off: [usize; 64],
    col_off: Vec<Vec<usize>>,
    row_size: [usize; 64],
}

struct Meta<'a> {
    d: &'a [u8],
    strings: &'a [u8],
    us: &'a [u8],
    blob: &'a [u8],
    tab: &'a [u8],
    lay: Layout,
}

fn sizes(rows: &[usize; 64], heap: u8) -> Layout {
    let idx = |n: usize| if n < 1 << 16 { 2 } else { 4 };
    let heap_idx = |bit: u8| if heap & bit != 0 { 4 } else { 2 };
    let coded = |tabs: &[u8], bits: u32| {
        let max = tabs.iter().filter(|&&t| t != 255).map(|&t| rows[t as usize]).max().unwrap_or(0);
        if max < 1usize << (16 - bits) {
            2
        } else {
            4
        }
    };
    let mut lay = Layout { rows: *rows, off: [0; 64], col_off: vec![vec![]; 64], row_size: [0; 64] };
    let mut pos = 0;
    for t in 0..64 {
        let mut o = 0;
        for c in schema(t) {
            lay.col_off[t].push(o);
            o += match c {
                U1 => 1,
                U2 => 2,
                U4 => 4,
                Str => heap_idx(1),
                Guid => heap_idx(2),
                Blob => heap_idx(4),
                Tab(x) => idx(rows[*x]),
                Coded(tabs, bits) => coded(tabs, *bits),
            };
        }
        lay.row_size[t] = o;
        lay.off[t] = pos;
        pos += o * rows[t];
    }
    lay
}

impl<'a> Meta<'a> {
    fn cell(&self, t: usize, row: usize, col: usize) -> u32 {
        if row == 0 || row > self.lay.rows[t] {
            return 0;
        }
        let start = self.lay.off[t] + (row - 1) * self.lay.row_size[t] + self.lay.col_off[t][col];
        let width = match schema(t)[col] {
            U1 => 1,
            U2 => 2,
            U4 => 4,
            _ => {
                let next = if col + 1 < schema(t).len() { self.lay.col_off[t][col + 1] } else { self.lay.row_size[t] };
                next - self.lay.col_off[t][col]
            }
        };
        match self.tab.get(start..start + width) {
            Some([a]) => *a as u32,
            Some([a, b]) => u16::from_le_bytes([*a, *b]) as u32,
            Some([a, b, c, d]) => u32::from_le_bytes([*a, *b, *c, *d]),
            _ => 0,
        }
    }
    fn rows(&self, t: usize) -> usize {
        self.lay.rows[t]
    }
    fn string(&self, i: u32) -> String {
        let s = self.strings.get(i as usize..).unwrap_or(&[]);
        let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
        String::from_utf8_lossy(&s[..end]).into_owned()
    }
    fn blob(&self, i: u32) -> &'a [u8] {
        let b = self.blob.get(i as usize..).unwrap_or(&[]);
        let mut s = Sig { b, p: 0 };
        match s.uint() {
            Some(n) => b.get(s.p..s.p + n as usize).unwrap_or(&[]),
            None => &[],
        }
    }
    /// A coded index to `(table, row)`.
    fn decode(&self, v: u32, tabs: &[u8], bits: u32) -> Option<(usize, usize)> {
        let tag = (v & ((1 << bits) - 1)) as usize;
        let t = *tabs.get(tag)?;
        (t != 255).then_some((t as usize, (v >> bits) as usize))
    }
}

struct Sig<'a> {
    b: &'a [u8],
    p: usize,
}

impl Sig<'_> {
    fn byte(&mut self) -> Option<u8> {
        let v = *self.b.get(self.p)?;
        self.p += 1;
        Some(v)
    }
    fn uint(&mut self) -> Option<u32> {
        let a = self.byte()? as u32;
        if a & 0x80 == 0 {
            Some(a)
        } else if a & 0xC0 == 0x80 {
            Some(((a & 0x3F) << 8) | self.byte()? as u32)
        } else if a & 0xE0 == 0xC0 {
            let (b, c, d) = (self.byte()? as u32, self.byte()? as u32, self.byte()? as u32);
            Some(((a & 0x1F) << 24) | (b << 16) | (c << 8) | d)
        } else {
            None
        }
    }
}

const TYPE_DEF: usize = 2;
const TYPE_REF: usize = 1;
const FIELD: usize = 4;
const METHOD: usize = 6;
const MEMBER_REF: usize = 10;

struct Names {
    types: Vec<String>,
    /// `type_of_method[m]` = the TypeDef row that owns MethodDef row `m` (1-based, 0 unused).
    method_owner: Vec<usize>,
    field_owner: Vec<usize>,
}

struct Ctx<'a> {
    m: &'a Meta<'a>,
    names: Names,
    /// Nodes a signature may still read: bounds the work a crafted one can ask for.
    work: std::cell::Cell<u32>,
}

const WORK: u32 = 256;

impl Ctx<'_> {
    /// A new signature starts: it gets the whole work budget again.
    fn fresh(&self) {
        self.work.set(WORK);
    }

    /// `type_of` as a signature of its own (an extends or implements column).
    fn type_name(&self, coded: u32) -> String {
        self.fresh();
        self.type_of(coded, 0)
    }

    fn typeref_name(&self, row: usize, depth: u32) -> String {
        let (scope, name, ns) =
            (self.m.cell(TYPE_REF, row, 0), self.m.cell(TYPE_REF, row, 1), self.m.cell(TYPE_REF, row, 2));
        let (name, ns) = (self.m.string(name), self.m.string(ns));
        if let Some((1, outer)) = self.m.decode(scope, &[0, 26, 35, 1], 2) {
            if depth < 8 && outer != row {
                return format!("{}/{name}", self.typeref_name(outer, depth + 1));
            }
        }
        if ns.is_empty() {
            name
        } else {
            format!("{ns}.{name}")
        }
    }

    fn type_of(&self, coded: u32, depth: u32) -> String {
        match self.m.decode(coded, &[2, 1, 27], 2) {
            Some((2, r)) => self.names.types.get(r).cloned().unwrap_or_else(|| format!("TypeDef#{r}?")),
            Some((1, r)) => self.typeref_name(r, 0),
            Some((27, r)) => {
                let blob = self.m.blob(self.m.cell(27, r, 0));
                let mut s = Sig { b: blob, p: 0 };
                self.read_type(&mut s, depth + 1)
            }
            _ => format!("?{coded:#x}"),
        }
    }

    fn read_type(&self, s: &mut Sig<'_>, depth: u32) -> String {
        if depth > 12 {
            return "<not read: nested too deep>".into();
        }
        // TypeSpecs may point at each other and fan out 32 or 255 ways: a node count caps the whole signature
        match self.work.get() {
            0 => return "<not read: signature too large>".into(),
            w => self.work.set(w - 1),
        }
        let Some(e) = s.byte() else { return "?".into() };
        let prim = |n: &str| n.to_string();
        match e {
            0x01 => prim("void"),
            0x02 => prim("bool"),
            0x03 => prim("char"),
            0x04 => prim("sbyte"),
            0x05 => prim("byte"),
            0x06 => prim("short"),
            0x07 => prim("ushort"),
            0x08 => prim("int"),
            0x09 => prim("uint"),
            0x0a => prim("long"),
            0x0b => prim("ulong"),
            0x0c => prim("float"),
            0x0d => prim("double"),
            0x0e => prim("string"),
            0x0f => format!("{}*", self.read_type(s, depth + 1)),
            0x10 => format!("{}&", self.read_type(s, depth + 1)),
            0x11 | 0x12 => {
                let v = s.uint().unwrap_or(0);
                self.type_of(v, depth)
            }
            0x13 => format!("!{}", s.uint().unwrap_or(0)),
            0x1e => format!("!!{}", s.uint().unwrap_or(0)),
            0x14 => {
                let t = self.read_type(s, depth + 1);
                // a real array has rank 32 at most; the counts after it cannot exceed the rank either
                let bad = |what: &str| format!("{t}[<not read: {what}>]");
                let Some(rank) = s.uint().filter(|r| (1..=32).contains(r)) else { return bad("array rank") };
                for _ in 0..2 {
                    match s.uint() {
                        Some(n) if n <= rank => (0..n).for_each(|_| _ = s.uint()),
                        _ => return bad("array bounds"),
                    }
                }
                format!("{t}[{}]", ",".repeat(rank as usize - 1))
            }
            0x15 => {
                s.byte();
                let v = s.uint().unwrap_or(0);
                let base = self.type_of(v, depth);
                let n = s.uint().unwrap_or(0);
                let args: Vec<String> = (0..n.min(32)).map(|_| self.read_type(s, depth + 1)).collect();
                format!("{base}<{}>", args.join(","))
            }
            0x16 => prim("typedref"),
            0x18 => prim("nint"),
            0x19 => prim("nuint"),
            0x1b => {
                let (params, ret, _) = self.method_sig(s, depth + 1);
                format!("fnptr({}) -> {ret}", params.join(", "))
            }
            0x1c => prim("object"),
            0x1d => format!("{}[]", self.read_type(s, depth + 1)),
            0x1f | 0x20 => {
                let v = s.uint().unwrap_or(0);
                let m = self.type_of(v, depth);
                let rest = self.read_type(s, depth + 1);
                format!("{rest} modopt({m})")
            }
            0x45 => format!("pinned {}", self.read_type(s, depth + 1)),
            0x41 => self.read_type(s, depth + 1),
            other => format!("?elem{other:#x}"),
        }
    }

    /// `(parameter types, return type, has this)`.
    fn method_sig(&self, s: &mut Sig<'_>, depth: u32) -> (Vec<String>, String, bool) {
        let conv = s.byte().unwrap_or(0);
        if conv & 0x10 != 0 {
            s.uint();
        }
        let n = s.uint().unwrap_or(0);
        let ret = self.read_type(s, depth);
        let params = (0..n.min(255)).map(|_| self.read_type(s, depth)).collect();
        (params, ret, conv & 0x20 != 0)
    }

    fn field_type(&self, blob: &[u8]) -> String {
        self.fresh();
        let mut s = Sig { b: blob, p: 0 };
        s.byte();
        self.read_type(&mut s, 0)
    }

    fn method_parts(&self, blob: &[u8]) -> (String, String) {
        self.fresh();
        let mut s = Sig { b: blob, p: 0 };
        let (p, r, _) = self.method_sig(&mut s, 0);
        (format!("({})", p.join(",")), format!("({}) -> {r}", p.join(", ")))
    }

    fn field_name(&self, row: usize) -> String {
        let owner = self.names.field_owner.get(row).copied().unwrap_or(0);
        let o = self.names.types.get(owner).cloned().unwrap_or_default();
        format!("{o}.{}", self.m.string(self.m.cell(FIELD, row, 1)))
    }

    /// `(display text, callee key if it is a method)`.
    fn token(&self, tok: u32) -> (String, Option<String>) {
        self.fresh();
        let (table, row) = ((tok >> 24) as usize, (tok & 0xFF_FFFF) as usize);
        match table {
            TYPE_DEF | TYPE_REF | 0x1b => (
                self.type_of(
                    ((row as u32) << 2)
                        | [0, 1, 0, 0, 0, 0, 0, 0][0].max(match table {
                            TYPE_DEF => 0,
                            TYPE_REF => 1,
                            _ => 2,
                        }),
                    0,
                ),
                None,
            ),
            FIELD => {
                let t = self.field_type(self.m.blob(self.m.cell(FIELD, row, 2)));
                (format!("{}: {t}", self.field_name(row)), None)
            }
            METHOD => {
                let owner = self.names.method_owner.get(row).copied().unwrap_or(0);
                let o = self.names.types.get(owner).cloned().unwrap_or_default();
                let name = self.m.string(self.m.cell(METHOD, row, 3));
                let (raw, pretty) = self.method_parts(self.m.blob(self.m.cell(METHOD, row, 4)));
                (format!("{o}.{name}{pretty}"), Some(format!("{o}.{name}{raw}")))
            }
            MEMBER_REF => {
                let (parent, name, sigb) =
                    (self.m.cell(10, row, 0), self.m.cell(10, row, 1), self.m.blob(self.m.cell(10, row, 2)));
                let p = match self.m.decode(parent, &[2, 1, 26, 6, 27], 3) {
                    Some((2, r)) => self.names.types.get(r).cloned().unwrap_or_default(),
                    Some((1, r)) => self.typeref_name(r, 0),
                    Some((26, r)) => format!("[{}]", self.m.string(self.m.cell(26, r, 0))),
                    Some((6, r)) => {
                        let o = self.names.method_owner.get(r).copied().unwrap_or(0);
                        self.names.types.get(o).cloned().unwrap_or_default()
                    }
                    Some((27, r)) => {
                        let mut s = Sig { b: self.m.blob(self.m.cell(27, r, 0)), p: 0 };
                        self.read_type(&mut s, 1)
                    }
                    _ => "?".into(),
                };
                let name = self.m.string(name);
                if sigb.first() == Some(&0x06) {
                    (format!("{p}.{name}: {}", self.field_type(sigb)), None)
                } else {
                    let (raw, pretty) = self.method_parts(sigb);
                    (format!("{p}.{name}{pretty}"), Some(format!("{p}.{name}{raw}")))
                }
            }
            0x2b => {
                let (m, blob) = (self.m.cell(43, row, 0), self.m.blob(self.m.cell(43, row, 1)));
                let (base, key) = match self.m.decode(m, &[6, 10], 1) {
                    Some((t, r)) => self.token(((t as u32) << 24) | r as u32),
                    None => ("?".into(), None),
                };
                let mut s = Sig { b: blob, p: 0 };
                s.byte();
                let n = s.uint().unwrap_or(0);
                let args: Vec<String> = (0..n.min(16)).map(|_| self.read_type(&mut s, 1)).collect();
                (format!("{base} <{}>", args.join(",")), key)
            }
            0x11 => {
                let mut s = Sig { b: self.m.blob(self.m.cell(17, row, 0)), p: 0 };
                let (p, r, _) = self.method_sig(&mut s, 0);
                (format!("signature({}) -> {r}", p.join(", ")), None)
            }
            _ => (format!("token {tok:#010x}?"), None),
        }
    }
}

pub struct Parsed {
    pub types: Vec<TypeDef>,
    pub strings: Vec<(String, String)>,
    pub facts: Vec<String>,
}

/// TypeDef visibility (ECMA-335 II.23.1.15): the low three flag bits tell top-level from nested.
fn type_vis(tf: u32) -> &'static str {
    match tf & 7 {
        0 => "internal",
        1 => "public",
        2 => "nested public",
        3 => "nested private",
        4 => "nested protected",
        5 => "nested internal",
        6 => "nested private protected",
        _ => "nested protected internal",
    }
}

fn flags(v: u32, t: &[(u32, &str)]) -> String {
    t.iter().filter(|(b, _)| v & b == *b).map(|(_, n)| *n).collect::<Vec<_>>().join(" ")
}

fn quote(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars().take(160) {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\x{:02x}", c as u32)),
            c => o.push(c),
        }
    }
    if s.chars().count() > 160 {
        o.push_str("...");
    }
    o.push('"');
    o
}

pub fn parse(d: &[u8]) -> Result<Parsed, String> {
    let pe = pe_open(d)?;
    if pe.clr.0 == 0 {
        return Err("the PE file has no CLI header (not a .NET assembly)".into());
    }
    let cli = pe.off(pe.clr.0).ok_or("the CLI header is outside every section")?;
    let md_rva = rd32(d, cli + 8).ok_or("CLI header cut short")?;
    let cli_flags = rd32(d, cli + 16).unwrap_or(0);
    let entry = rd32(d, cli + 20).unwrap_or(0);
    let root = pe.off(md_rva).ok_or("the metadata is outside every section")?;
    if rd32(d, root) != Some(0x424A_5342) {
        return Err("metadata root has no BSJB signature".into());
    }
    let vlen = rd32(d, root + 12).ok_or("cut short")? as usize;
    let runtime = String::from_utf8_lossy(d.get(root + 16..root + 16 + vlen).ok_or("cut short")?)
        .trim_end_matches('\0')
        .to_string();
    let nstreams = rd16(d, root + 16 + vlen + 2).ok_or("cut short")? as usize;
    let mut p = root + 16 + vlen + 4;
    let mut heaps: HashMap<String, &[u8]> = HashMap::new();
    for _ in 0..nstreams {
        let (off, size) =
            (rd32(d, p).ok_or("stream header cut short")? as usize, rd32(d, p + 4).ok_or("cut short")? as usize);
        let nlen = d.get(p + 8..).and_then(|s| s.iter().position(|&b| b == 0)).ok_or("stream name cut short")?;
        let name = String::from_utf8_lossy(&d[p + 8..p + 8 + nlen]).into_owned();
        p += 8 + ((nlen + 1 + 3) & !3);
        let data = d.get(root + off..root + off + size).ok_or_else(|| format!("stream {name} runs past the file"))?;
        heaps.insert(name, data);
    }
    let tab_stream = *heaps.get("#~").or_else(|| heaps.get("#-")).ok_or("no #~ metadata tables stream")?;
    if heaps.contains_key("#-") && !heaps.contains_key("#~") {
        return Err("uncompressed (#-) metadata with pointer tables is not read".into());
    }
    let heap_sizes = *tab_stream.get(6).ok_or("table stream cut short")?;
    let valid = u64::from_le_bytes(tab_stream.get(8..16).ok_or("table stream cut short")?.try_into().unwrap());
    let mut rows = [0usize; 64];
    let mut q = 24;
    for (t, r) in rows.iter_mut().enumerate() {
        if valid >> t & 1 == 1 {
            *r = rd32(tab_stream, q).ok_or("row counts cut short")? as usize;
            q += 4;
        }
    }
    if heap_sizes & 0x40 != 0 {
        q += 4;
    }
    for t in [3usize, 5, 7, 19, 22] {
        if rows[t] > 0 {
            return Err(format!("metadata uses pointer table {t} (an edit-and-continue assembly): not read"));
        }
    }
    if rows.iter().enumerate().any(|(t, &r)| r > 0 && schema(t).is_empty()) {
        return Err("metadata has a table this reader does not know".into());
    }
    let lay = sizes(&rows, heap_sizes);
    let total: usize = (0..64).map(|t| lay.row_size[t] * rows[t]).sum();
    let tab = tab_stream.get(q..q + total).ok_or("the metadata tables run past their stream")?;
    let m = Meta {
        d,
        strings: heaps.get("#Strings").copied().unwrap_or(&[]),
        us: heaps.get("#US").copied().unwrap_or(&[]),
        blob: heaps.get("#Blob").copied().unwrap_or(&[]),
        tab,
        lay,
    };
    let _ = m.d;
    // owners and names
    let ntd = m.rows(2);
    let mut owner_m = vec![0usize; m.rows(6) + 2];
    let mut owner_f = vec![0usize; m.rows(4) + 2];
    for t in 1..=ntd {
        let (mf, ff) = (m.cell(2, t, 5) as usize, m.cell(2, t, 4) as usize);
        let (mn, fn_) = if t < ntd {
            (m.cell(2, t + 1, 5) as usize, m.cell(2, t + 1, 4) as usize)
        } else {
            (m.rows(6) + 1, m.rows(4) + 1)
        };
        for x in mf..mn.min(owner_m.len()) {
            owner_m[x] = t;
        }
        for x in ff..fn_.min(owner_f.len()) {
            owner_f[x] = t;
        }
    }
    let mut nested: HashMap<usize, usize> = HashMap::new();
    for r in 1..=m.rows(41) {
        nested.insert(m.cell(41, r, 0) as usize, m.cell(41, r, 1) as usize);
    }
    let mut tnames = vec![String::new(); ntd + 1];
    #[allow(clippy::needless_range_loop)]
    for t in 1..=ntd {
        let mut chain = vec![t];
        while let Some(&o) = nested.get(chain.last().unwrap()) {
            if chain.len() > 8 || chain.contains(&o) {
                break;
            }
            chain.push(o);
        }
        let mut s = String::new();
        for (i, &r) in chain.iter().rev().enumerate() {
            let (n, ns) = (m.string(m.cell(2, r, 1)), m.string(m.cell(2, r, 2)));
            if i == 0 {
                s = if ns.is_empty() { n } else { format!("{ns}.{n}") };
            } else {
                s = format!("{s}/{n}");
            }
        }
        tnames[t] = s;
    }
    let cx = Ctx {
        m: &m,
        names: Names { types: tnames, method_owner: owner_m, field_owner: owner_f },
        work: std::cell::Cell::new(WORK),
    };
    // P/Invoke
    let mut pinvoke: HashMap<usize, String> = HashMap::new();
    for r in 1..=m.rows(28) {
        let (mf, imp, module) =
            (m.cell(28, r, 1), m.string(m.cell(28, r, 2)), m.string(m.cell(26, m.cell(28, r, 3) as usize, 0)));
        if let Some((6, row)) = m.decode(mf, &[4, 6], 1) {
            pinvoke.insert(row, format!("{module}!{imp}"));
        }
    }
    let mut strings: Vec<(String, String)> = Vec::new();
    let mut used_us: HashSet<u32> = HashSet::new();
    // methods whose decoding stopped: they may load user strings no decoded code loads
    let mut stops: Vec<String> = Vec::new();
    let mut types: Vec<TypeDef> = Vec::new();
    for t in 1..=ntd {
        let (tf, ext) = (m.cell(2, t, 0), m.cell(2, t, 3));
        let name = cx.names.types[t].clone();
        let extends = (ext != 0).then(|| cx.type_name(ext));
        let kind = if tf & 0x20 != 0 {
            "interface"
        } else {
            match extends.as_deref() {
                Some("System.Enum") => "enum",
                Some("System.ValueType") => "struct",
                Some("System.MulticastDelegate") => "delegate",
                _ => "class",
            }
        };
        let mut fl = vec![type_vis(tf).to_string()];
        let rest = flags(tf, &[(0x80, "abstract"), (0x100, "sealed")]);
        if !rest.is_empty() {
            fl.push(rest);
        }
        let (mstart, mend) =
            (m.cell(2, t, 5) as usize, if t < ntd { m.cell(2, t + 1, 5) as usize } else { m.rows(6) + 1 });
        let (fstart, fend) =
            (m.cell(2, t, 4) as usize, if t < ntd { m.cell(2, t + 1, 4) as usize } else { m.rows(4) + 1 });
        let mut fields = Vec::new();
        for f in fstart..fend.min(m.rows(4) + 1) {
            let sig = m.blob(m.cell(4, f, 2));
            let ff = m.cell(4, f, 0);
            let vis = match ff & 7 {
                1 => "private",
                2 => "famandassem",
                3 => "internal",
                4 => "protected",
                5 => "protected internal",
                6 => "public",
                _ => "",
            };
            let extra = flags(ff, &[(0x10, "static"), (0x20, "readonly"), (0x40, "const")]);
            fields.push(FieldDef {
                name: m.string(m.cell(4, f, 1)),
                ty: cx.field_type(sig),
                raw: cx.field_type(sig),
                flags: [vis, extra.as_str()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" "),
                constant: None,
            });
        }
        let mut methods = Vec::new();
        for mi in mstart..mend.min(m.rows(6) + 1) {
            let mname = m.string(m.cell(6, mi, 3));
            let (raw, pretty) = cx.method_parts(m.blob(m.cell(6, mi, 4)));
            let mf = m.cell(6, mi, 2);
            let vis = match mf & 7 {
                1 => "private",
                2 => "famandassem",
                3 => "internal",
                4 => "protected",
                5 => "protected internal",
                6 => "public",
                _ => "",
            };
            let mut fl = flags(mf, &[(0x10, "static"), (0x20, "final"), (0x40, "virtual"), (0x400, "abstract")]);
            if let Some(pi) = pinvoke.get(&mi) {
                fl = format!("{fl} pinvoke {pi}");
            }
            let fl = format!("{vis} {fl}").trim().to_string();
            let rva = m.cell(6, mi, 0);
            let owner = name.clone();
            let code = (rva != 0).then(|| {
                decode_body(&pe, &cx, rva, &format!("{owner}.{mname}"), &mut used_us, &mut strings, &mut stops)
            });
            methods.push(MethodDef {
                name: mname.clone(),
                sig: pretty,
                raw: raw.clone(),
                flags: fl,
                key: format!("{owner}.{mname}{raw}"),
                code,
            });
        }
        types.push(TypeDef {
            name,
            kind: kind.into(),
            flags: fl.join(" "),
            extends,
            implements: vec![],
            fields,
            methods,
            origin: String::new(),
        });
    }
    for r in 1..=m.rows(9) {
        let (c, i) = (m.cell(9, r, 0) as usize, m.cell(9, r, 1));
        if let Some(ty) = types.get_mut(c.wrapping_sub(1)) {
            ty.implements.push(cx.type_name(i));
        }
    }
    // user strings no decoded code loads
    let origin = if stops.is_empty() {
        "(user strings heap: no code loads it)".to_string()
    } else {
        let shown = stops.iter().take(3).cloned().collect::<Vec<_>>().join("; ");
        let more = if stops.len() > 3 { format!("; {} more", stops.len() - 3) } else { String::new() };
        format!(
            "(user strings heap: {shown}{more}; {} may load this string)",
            if stops.len() == 1 { "it" } else { "one of them" }
        )
    };
    let mut i = 1usize;
    while i < m.us.len() {
        let mut s = Sig { b: m.us, p: i };
        let Some(len) = s.uint() else { break };
        let start = s.p;
        let len = len as usize;
        if len == 0 {
            i = start;
            continue;
        }
        if start + len > m.us.len() {
            break;
        }
        if !used_us.contains(&(i as u32)) && len >= 3 {
            let u: Vec<u16> =
                m.us[start..start + (len - 1) / 2 * 2].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            let text = String::from_utf16_lossy(&u);
            if !text.is_empty() {
                strings.push((origin.clone(), text));
            }
        }
        i = start + len;
    }
    // facts
    let mut facts = vec![format!("runtime {runtime}")];
    if m.rows(32) > 0 {
        facts.push(format!(
            "assembly {} {}.{}.{}.{}",
            m.string(m.cell(32, 1, 7)),
            m.cell(32, 1, 1),
            m.cell(32, 1, 2),
            m.cell(32, 1, 3),
            m.cell(32, 1, 4)
        ));
    }
    if entry != 0 {
        let (text, _) = cx.token(entry);
        facts.push(format!("entry point {}", text.split('(').next().unwrap_or(&text)));
    }
    if cli_flags & 2 != 0 {
        facts.push("32-bit required".into());
    }
    if m.rows(35) > 0 {
        let names: Vec<String> = (1..=m.rows(35)).map(|r| m.string(m.cell(35, r, 6))).collect();
        let shown = names.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
        facts.push(format!(
            "references {shown}{}",
            if names.len() > 6 { format!(", {} more", names.len() - 6) } else { String::new() }
        ));
    }
    if m.rows(28) > 0 {
        facts.push(format!("{} P/Invoke imports (shown as `pinvoke dll!function` on the method)", m.rows(28)));
    }
    if !stops.is_empty() {
        facts.push(format!(
            "{} methods not fully decoded (WARNING in their listing): their strings and calls may be missing",
            stops.len()
        ));
    }
    types.retain(|t| !(t.name == "<Module>" && t.methods.is_empty() && t.fields.is_empty()));
    Ok(Parsed { types, strings, facts })
}

fn decode_body(
    pe: &Pe<'_>,
    cx: &Ctx<'_>,
    rva: u32,
    label: &str,
    used_us: &mut HashSet<u32>,
    strings: &mut Vec<(String, String)>,
    stops: &mut Vec<String>,
) -> Code {
    let mut c = Code::default();
    let Some(off) = pe.off(rva) else {
        c.note = Some(format!("method body at RVA {rva:#x} is outside every section"));
        stops.push(format!("{label} was not decoded (body outside every section)"));
        return c;
    };
    let d = pe.d;
    let Some(&h) = d.get(off) else {
        c.note = Some("method body is past the end of the file".into());
        stops.push(format!("{label} was not decoded (body past the end of the file)"));
        return c;
    };
    let (code_off, code_len, stack, hdr_flags) = if h & 3 == 2 {
        (off + 1, (h >> 2) as usize, 8u16, 0u16)
    } else if h & 3 == 3 {
        let (Some(f), Some(ms), Some(cs)) = (rd16(d, off), rd16(d, off + 2), rd32(d, off + 4)) else {
            c.note = Some("fat method header cut short".into());
            stops.push(format!("{label} was not decoded (header cut short)"));
            return c;
        };
        (off + ((f >> 12) as usize) * 4, cs as usize, ms, f & 0xFFF)
    } else {
        c.note = Some(format!("unknown method header byte {h:#04x}"));
        stops.push(format!("{label} was not decoded (unknown header byte)"));
        return c;
    };
    let Some(code) = d.get(code_off..code_off + code_len) else {
        c.note = Some("method body runs past the end of the file".into());
        stops.push(format!("{label} was not decoded (body runs past the end of the file)"));
        return c;
    };
    c.size = code_len;
    c.info = format!("maxstack {stack}");
    if hdr_flags & 8 != 0 {
        decode_sections(d, (code_off + code_len + 3) & !3, cx, &mut c);
    }
    let mut p = 0usize;
    while p < code.len() {
        let mut ins = Insn { off: p as u32, ..Insn::default() };
        let (res, len) = decode_il(cx, code, p, &mut ins, used_us, strings, label);
        if let Err(e) = res {
            ins.text = format!("?? {e}");
            c.insns.push(ins);
            c.note = Some(format!("decoding stopped at {p:#06x}: {e}"));
            stops.push(format!("decoding of {label} stopped at offset {p:#06x}"));
            return c;
        }
        p += len;
        c.insns.push(ins);
    }
    c
}

fn decode_sections(d: &[u8], mut p: usize, cx: &Ctx<'_>, c: &mut Code) {
    loop {
        let Some(&kind) = d.get(p) else { return };
        let fat = kind & 0x40 != 0;
        let size = if fat {
            match d.get(p + 1..p + 4) {
                Some(b) => b[0] as usize | (b[1] as usize) << 8 | (b[2] as usize) << 16,
                None => return,
            }
        } else {
            d.get(p + 1).copied().unwrap_or(0) as usize
        };
        if kind & 1 != 0 {
            let body = p + 4;
            let (esz, n) = if fat { (24, size.saturating_sub(4) / 24) } else { (12, size.saturating_sub(4) / 12) };
            for i in 0..n {
                let e = body + i * esz;
                let g = |o: usize, w: usize| -> Option<u32> {
                    match w {
                        1 => d.get(e + o).map(|&b| b as u32),
                        2 => rd16(d, e + o).map(u32::from),
                        _ => rd32(d, e + o),
                    }
                };
                let (fl, to, tl, ho, hl, tok) = if fat {
                    (g(0, 4), g(4, 4), g(8, 4), g(12, 4), g(16, 4), g(20, 4))
                } else {
                    (g(0, 2), g(2, 2), g(4, 1), g(5, 2), g(7, 1), g(8, 4))
                };
                let (Some(fl), Some(to), Some(tl), Some(ho), Some(hl), Some(tok)) = (fl, to, tl, ho, hl, tok) else {
                    c.note = Some("exception clause table cut short".into());
                    return;
                };
                let what = match fl & 7 {
                    0 => format!("catch {}", cx.token(tok).0),
                    1 => format!("filter at {tok:#06x}"),
                    2 => "finally".to_string(),
                    4 => "fault".to_string(),
                    o => format!("clause kind {o}"),
                };
                // the offsets come from the file: a sum past 4 GB is damage, said as such and never wrapped
                match (to.checked_add(tl), ho.checked_add(hl)) {
                    (Some(te), Some(he)) => {
                        c.handlers.push(format!("try {to:#06x}..{te:#06x} {what} {ho:#06x}..{he:#06x}"))
                    }
                    _ => {
                        c.handlers.push(format!(
                            "try {to:#06x}+{tl:#x} {what} {ho:#06x}+{hl:#x} (offsets overflow 32 bits: damaged clause)"
                        ));
                        c.note = Some(
                            "an exception clause has offsets that overflow 32 bits: the handler table is damaged"
                                .into(),
                        );
                    }
                }
            }
        }
        if kind & 0x80 == 0 {
            return;
        }
        p = (p + size.max(4) + 3) & !3;
    }
}

fn decode_il(
    cx: &Ctx<'_>,
    code: &[u8],
    p: usize,
    ins: &mut Insn,
    used_us: &mut HashSet<u32>,
    strings: &mut Vec<(String, String)>,
    label: &str,
) -> (Result<(), String>, usize) {
    let op = code[p];
    let (entry, mut len) = if op == 0xFE {
        match code.get(p + 1).and_then(|&b| il::two(b)) {
            Some(e) => (e, 2),
            None => return (Err(format!("unknown opcode 0xfe {:#04x}", code.get(p + 1).copied().unwrap_or(0))), 1),
        }
    } else {
        match il::one(op) {
            Some(e) => (e, 1),
            None => return (Err(format!("unknown opcode {op:#04x}")), 1),
        }
    };
    let (name, arg) = entry;
    let a = p + len;
    let need =
        |n: usize| -> Result<&[u8], String> { code.get(a..a + n).ok_or_else(|| format!("{name}: operand cut short")) };
    let mut text = name.to_string();
    let r = (|| -> Result<(), String> {
        match arg {
            Arg::Nothing => {}
            Arg::I8 => {
                text += &format!(" {}", need(1)?[0] as i8);
                len += 1;
            }
            Arg::U8 => {
                text += &format!(" {}", need(1)?[0]);
                len += 1;
            }
            Arg::I32 => {
                text += &format!(" {}", i32::from_le_bytes(need(4)?.try_into().unwrap()));
                len += 4;
            }
            Arg::I64 => {
                text += &format!(" {}", i64::from_le_bytes(need(8)?.try_into().unwrap()));
                len += 8;
            }
            Arg::F32 => {
                text += &format!(" {}", f32::from_le_bytes(need(4)?.try_into().unwrap()));
                len += 4;
            }
            Arg::F64 => {
                text += &format!(" {}", f64::from_le_bytes(need(8)?.try_into().unwrap()));
                len += 8;
            }
            Arg::Var1 => {
                text += &format!(" {}", need(1)?[0]);
                len += 1;
            }
            Arg::Var2 => {
                text += &format!(" {}", u16::from_le_bytes(need(2)?.try_into().unwrap()));
                len += 2;
            }
            Arg::Br1 | Arg::Br4 => {
                let (rel, w) = if arg == Arg::Br1 {
                    (need(1)?[0] as i8 as i64, 1)
                } else {
                    (i32::from_le_bytes(need(4)?.try_into().unwrap()) as i64, 4)
                };
                len += w;
                text += &format!(" -> {}", branch(code.len(), p + len, rel));
            }
            Arg::Switch => {
                let n = u32::from_le_bytes(need(4)?.try_into().unwrap()) as usize;
                if n > 100_000 {
                    return Err(format!("switch with {n} cases is not sane"));
                }
                let end = a + 4 + n * 4;
                let tbl = code.get(a + 4..end).ok_or("switch table cut short")?;
                let cases: Vec<String> = tbl
                    .chunks(4)
                    .enumerate()
                    .map(|(i, b)| {
                        format!(
                            "case {i} -> {}",
                            branch(code.len(), end, i32::from_le_bytes(b.try_into().unwrap()) as i64)
                        )
                    })
                    .collect();
                len += 4 + n * 4;
                if cases.len() <= 4 {
                    text += &format!(" {{{}}}", cases.join(", "));
                } else {
                    text += &format!(" {} cases", cases.len());
                    ins.cont = cases;
                }
            }
            Arg::Str => {
                let tok = u32::from_le_bytes(need(4)?.try_into().unwrap());
                len += 4;
                let idx = tok & 0xFF_FFFF;
                let mut s = Sig { b: cx.m.us, p: idx as usize };
                let l = s.uint().ok_or("bad user string")? as usize;
                let raw = cx.m.us.get(s.p..s.p + l.saturating_sub(1) / 2 * 2).ok_or("user string past its heap")?;
                let u: Vec<u16> = raw.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                let t = String::from_utf16_lossy(&u);
                used_us.insert(idx);
                text += &format!(" {}", quote(&t));
                strings.push((format!("{label} +{p:#06x}"), t));
            }
            Arg::Token => {
                let tok = u32::from_le_bytes(need(4)?.try_into().unwrap());
                len += 4;
                let (t, callee) = cx.token(tok);
                text += &format!(" {t}");
                // calls, and ldftn/ldvirtftn (0xFE06/0xFE07): the delegate or event handler target is a reference too
                let second = code.get(p + 1).copied();
                let call = matches!(op, 0x28 | 0x6f | 0x73 | 0x27);
                let addr = op == 0xFE && matches!(second, Some(0x06 | 0x07));
                if (call || addr) && callee.is_some() {
                    ins.callee = callee;
                    ins.addr = addr;
                }
            }
        }
        Ok(())
    })();
    ins.text = text;
    (r, len)
}

fn branch(size: usize, base: usize, rel: i64) -> String {
    let t = base as i64 + rel;
    if t < 0 || t as usize > size {
        format!("{t:#06x} (outside the method)")
    } else {
        format!("{t:#06x}")
    }
}

/// Test hooks: crafted metadata and signature blobs without building a whole assembly.
#[cfg(test)]
pub(super) mod probe {
    use super::*;

    fn with_ctx<R>(typespecs: &[Vec<u8>], f: impl FnOnce(&Ctx<'_>) -> R) -> R {
        let mut blob = vec![0u8];
        let mut tab = Vec::new();
        for t in typespecs {
            tab.extend_from_slice(&(blob.len() as u16).to_le_bytes());
            if t.len() < 0x80 {
                blob.push(t.len() as u8);
            } else {
                blob.extend_from_slice(&(0x8000u16 | t.len() as u16).to_be_bytes());
            }
            blob.extend_from_slice(t);
        }
        let mut rows = [0usize; 64];
        rows[27] = typespecs.len();
        let m = Meta { d: &[], strings: &[], us: &[], blob: &blob, tab: &tab, lay: sizes(&rows, 0) };
        let cx = Ctx {
            m: &m,
            names: Names { types: vec![String::new()], method_owner: vec![], field_owner: vec![] },
            work: std::cell::Cell::new(WORK),
        };
        f(&cx)
    }

    /// Read one type from `sig`; `typespecs` are rows 1.. of the TypeSpec table.
    pub fn read_type(typespecs: &[Vec<u8>], sig: &[u8]) -> String {
        with_ctx(typespecs, |cx| {
            cx.fresh();
            cx.read_type(&mut Sig { b: sig, p: 0 }, 0)
        })
    }

    /// Decode the exception sections that start at `p` of `d`.
    pub fn sections(d: &[u8], p: usize) -> Code {
        with_ctx(&[], |cx| {
            let mut c = Code::default();
            decode_sections(d, p, cx, &mut c);
            c
        })
    }

    /// `(is_dotnet, is_mixed_mode)` for a PE image.
    pub fn claims(d: &[u8]) -> (bool, bool) {
        (is_dotnet(d), is_mixed_mode(d))
    }

    pub fn type_vis(tf: u32) -> &'static str {
        super::type_vis(tf)
    }

    pub fn parse_err(d: &[u8]) -> String {
        parse(d).err().unwrap_or_default()
    }
}
