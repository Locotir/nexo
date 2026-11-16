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

mod assembly;
mod context;
mod il_bodies;
pub use self::assembly::*;
use self::il_bodies::*;

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

fn pe_open(d: &[u8]) -> Result<Pe<'_>, String> {
    let h = nexo_binmagic::pe_cli(d)?;
    Ok(Pe { d, sections: h.sections, clr: h.clr })
}

impl Pe<'_> {
    fn off(&self, rva: u32) -> Option<usize> {
        nexo_binmagic::section_offset(&self.sections, rva)
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

pub struct Parsed {
    pub types: Vec<TypeDef>,
    pub strings: Vec<(String, String)>,
    pub facts: Vec<String>,
}

/// Test hooks: crafted metadata and signature blobs without building a whole assembly.
#[cfg(test)]
pub(super) mod probe {
    use super::*;
    use nexo_binmagic::{is_dotnet, is_mixed_mode};

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
