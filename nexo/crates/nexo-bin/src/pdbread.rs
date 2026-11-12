//! Names, source lines and types from a `.pdb` file next to a PE file, read with the `pdb` crate.
//!
//! What comes out has the same shape as what DWARF gives (see `dwarf.rs`): function names and sizes join
//! the symbols, prototypes and struct layouts join the `Dwarf` maps the decompiler reads, and the line
//! table answers `Image::src_line`. A pdb that cannot be read is said so (`Pdb::note`), never skipped silently.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pdb::{FallibleIterator, PrimitiveKind, SymbolData, TypeData, TypeIndex};

use crate::dwarf::Dwarf;

/// The function being read: address, parameters so far (type, name), name, return type.
type Open = Option<(u64, Vec<(String, String)>, String, String)>;

const MAX_FUNCS: usize = 300_000;
const MAX_LINES: usize = 3_000_000;
const MAX_TYPES: usize = 400_000;
const MAX_DEPTH: usize = 6;
/// Largest pdb file read (bytes).
const MAX_PDB: u64 = 1 << 30;

/// One line record: the code from `addr` belongs to `line` of file number `file`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LineRec {
    pub addr: u64,
    pub file: u32,
    pub line: u32,
}

#[derive(Debug, Default)]
pub(crate) struct Pdb {
    /// Where it was read from.
    pub path: PathBuf,
    /// (address, size, name) of every function the pdb names, private records first.
    pub funcs: Vec<(u64, u64, String)>,
    /// Sorted by address.
    pub lines: Vec<LineRec>,
    pub files: Vec<String>,
    pub types: Dwarf,
    /// Why part of it is missing, when it is.
    pub note: Option<String>,
}

/// The pdb that goes with a PE file: `name.pdb` next to it, else the file name the CodeView record gives,
/// looked up in the same folder.
pub(crate) fn find(exe: &Path, data: &[u8]) -> Option<PathBuf> {
    let same = exe.with_extension("pdb");
    if same.is_file() {
        return Some(same);
    }
    let dir = exe.parent()?;
    let from_header = object::File::parse(data).ok().and_then(|o| {
        use object::Object;
        o.pdb_info().ok().flatten().map(|i| String::from_utf8_lossy(i.path()).into_owned())
    })?;
    let base = from_header.rsplit(['\\', '/']).next()?;
    if base.is_empty() || base.contains("..") {
        return None;
    }
    let p = dir.join(base);
    p.is_file().then_some(p)
}

pub(crate) fn read(path: &Path, image_base: u64) -> Pdb {
    let mut out = Pdb { path: path.to_path_buf(), ..Pdb::default() };
    match std::fs::metadata(path) {
        Ok(m) if m.len() <= MAX_PDB => {}
        Ok(_) => {
            out.note = Some(format!("{} is over 1 GiB, not read", path.display()));
            return out;
        }
        Err(e) => {
            out.note = Some(format!("cannot read {}: {e}", path.display()));
            return out;
        }
    }
    let Ok(file) = std::fs::File::open(path) else {
        out.note = Some(format!("cannot open {}", path.display()));
        return out;
    };
    if let Err(e) = read_inner(file, image_base, &mut out) {
        out.note = Some(format!("{} was read only in part: {e}", path.display()));
    }
    out.lines.sort_by_key(|l| l.addr);
    out
}

fn read_inner(file: std::fs::File, base: u64, out: &mut Pdb) -> pdb::Result<()> {
    let mut pdb = pdb::PDB::open(file)?;
    let map = pdb.address_map()?;
    let strings = pdb.string_table().ok();
    let tpi = pdb.type_information()?;
    let mut finder = tpi.finder();
    let mut it = tpi.iter();
    let mut n = 0usize;
    while let Some(t) = it.next()? {
        finder.update(&it);
        n += 1;
        if n > MAX_TYPES {
            break;
        }
        let _ = t;
    }
    let tcx = Types { finder: &finder };
    let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let dbi = pdb.debug_information()?;
    let mut modules = dbi.modules()?;
    let mut files: BTreeMap<String, u32> = BTreeMap::new();
    while let Some(m) = modules.next()? {
        let Some(info) = pdb.module_info(&m)? else { continue };
        let lp = info.line_program().ok();
        let mut syms = info.symbols()?;
        // the open function: (start, parameters so far)
        let mut open: Open = None;
        while let Some(s) = syms.next()? {
            let Ok(data) = s.parse() else { continue };
            match data {
                SymbolData::Procedure(p) => {
                    flush(&mut open, out);
                    let Some(rva) = p.offset.to_rva(&map) else { continue };
                    let addr = base + u64::from(rva.0);
                    if out.funcs.len() >= MAX_FUNCS || !seen.insert(addr) {
                        continue;
                    }
                    let name = p.name.to_string().into_owned();
                    out.funcs.push((addr, u64::from(p.len), name.clone()));
                    let (ret, _) = tcx.proc_parts(p.type_index);
                    open = Some((addr, Vec::new(), name, ret));
                    if let (Some(lp), Some(st)) = (&lp, &strings) {
                        let mut ls = lp.lines_for_symbol(p.offset);
                        while let Some(l) = ls.next()? {
                            if out.lines.len() >= MAX_LINES {
                                break;
                            }
                            let Some(rva) = l.offset.to_rva(&map) else { continue };
                            let fname = lp
                                .get_file_info(l.file_index)
                                .ok()
                                .and_then(|f| f.name.to_string_lossy(st).ok().map(|c| c.into_owned()))
                                .unwrap_or_default();
                            let next = files.len() as u32;
                            let fid = *files.entry(fname).or_insert(next);
                            out.lines.push(LineRec { addr: base + u64::from(rva.0), file: fid, line: l.line_start });
                        }
                    }
                }
                SymbolData::Local(l) if l.flags.isparam => {
                    if let Some(o) = open.as_mut() {
                        o.1.push((tcx.ctype(l.type_index, 0), l.name.to_string().into_owned()));
                    }
                }
                _ => {}
            }
        }
        flush(&mut open, out);
    }
    let mut names: Vec<(&String, &u32)> = files.iter().collect();
    names.sort_by_key(|(_, i)| **i);
    out.files = names.into_iter().map(|(n, _)| n.clone()).collect();
    // functions that only the public table names (no private record)
    let globals = pdb.global_symbols()?;
    let mut gs = globals.iter();
    while let Some(s) = gs.next()? {
        if let Ok(SymbolData::Public(p)) = s.parse() {
            if !p.function {
                continue;
            }
            let Some(rva) = p.offset.to_rva(&map) else { continue };
            let addr = base + u64::from(rva.0);
            if out.funcs.len() < MAX_FUNCS && seen.insert(addr) {
                out.funcs.push((addr, 0, p.name.to_string().into_owned()));
            }
        }
    }
    // struct layouts
    let mut it = tpi.iter();
    let mut n = 0;
    while let Some(t) = it.next()? {
        n += 1;
        if n > MAX_TYPES {
            break;
        }
        if let Ok(TypeData::Class(c)) = t.parse() {
            if c.properties.forward_reference() {
                continue;
            }
            if let (Some(fl), name) = (c.fields, c.name.to_string().into_owned()) {
                if let Some(text) = tcx.fields(fl) {
                    out.types.structs.entry(name).or_insert(text);
                }
            }
        }
    }
    Ok(())
}

fn flush(open: &mut Open, out: &mut Pdb) {
    let Some((addr, params, name, ret)) = open.take() else { return };
    let mut args: Vec<String> = Vec::new();
    for (i, (t, n)) in params.iter().enumerate() {
        let n = if n.is_empty() { format!("a{}", i + 1) } else { n.clone() };
        args.push(if t.ends_with('*') { format!("{t}{n}") } else { format!("{t} {n}") });
    }
    let list = if args.is_empty() { "void".to_string() } else { args.join(", ") };
    // only a plain identifier can be a prototype name (C++ names stay in the symbol table)
    if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        let head = if ret.ends_with('*') { format!("{ret}{name}") } else { format!("{ret} {name}") };
        out.types.protos.insert(addr, format!("{head}({list})"));
    }
}

struct Types<'a, 't> {
    finder: &'a pdb::TypeFinder<'t>,
}

impl Types<'_, '_> {
    fn data(&self, t: TypeIndex) -> Option<TypeData<'_>> {
        self.finder.find(t).ok()?.parse().ok()
    }

    /// (return type, number of parameters) of a procedure type.
    fn proc_parts(&self, t: TypeIndex) -> (String, usize) {
        match self.data(t) {
            Some(TypeData::Procedure(p)) => {
                let ret = p.return_type.map_or("void".to_string(), |r| self.ctype(r, 0));
                (ret, usize::from(p.parameter_count))
            }
            _ => ("i32".into(), 0),
        }
    }

    /// A C type as short text in the style DWARF gives: `i32`, `u8`, `char *`, `struct item *`.
    fn ctype(&self, t: TypeIndex, depth: usize) -> String {
        if depth > MAX_DEPTH {
            return "void".into();
        }
        match self.data(t) {
            Some(TypeData::Primitive(p)) => {
                let base = prim(p.kind);
                if p.indirection.is_some() {
                    format!("{base} *")
                } else {
                    base.to_string()
                }
            }
            Some(TypeData::Pointer(p)) => {
                let inner = self.ctype(p.underlying_type, depth + 1);
                if inner.ends_with('*') {
                    format!("{inner}*")
                } else {
                    format!("{inner} *")
                }
            }
            Some(TypeData::Modifier(m)) => {
                let inner = self.ctype(m.underlying_type, depth + 1);
                if m.constant && !inner.ends_with('*') {
                    format!("const {inner}")
                } else {
                    inner
                }
            }
            Some(TypeData::Class(c)) => format!("struct {}", c.name),
            Some(TypeData::Union(u)) => format!("union {}", u.name),
            Some(TypeData::Enumeration(e)) => format!("enum {}", e.name),
            Some(TypeData::Array(a)) => {
                let inner = self.ctype(a.element_type, depth + 1);
                format!("{inner} *")
            }
            Some(TypeData::Procedure(_) | TypeData::MemberFunction(_)) => "void *".into(),
            _ => "void".into(),
        }
    }

    /// `0x0:i32:id 0x8:i64:score` for a field list.
    fn fields(&self, fl: TypeIndex) -> Option<String> {
        let mut out: Vec<String> = Vec::new();
        let mut cur = Some(fl);
        let mut hops = 0;
        while let Some(i) = cur.take() {
            hops += 1;
            if hops > 16 {
                break;
            }
            let Some(TypeData::FieldList(l)) = self.data(i) else { break };
            for f in &l.fields {
                if let TypeData::Member(m) = f {
                    let ty = self
                        .ctype(m.field_type, 0)
                        .replace("const ", "")
                        .replace("struct ", "")
                        .replace("union ", "")
                        .replace("enum ", "")
                        .replace(' ', "");
                    if !ty.is_empty() && !ty.contains(':') {
                        out.push(format!("{:#x}:{ty}:{}", m.offset, m.name));
                    }
                }
            }
            cur = l.continuation;
        }
        (!out.is_empty()).then(|| out.join(" "))
    }
}

fn prim(k: PrimitiveKind) -> &'static str {
    use PrimitiveKind::*;
    match k {
        Void | NoType => "void",
        Char | RChar => "char",
        UChar => "u8",
        WChar | RChar16 | U16 | UShort => "u16",
        RChar32 | U32 | ULong => "u32",
        I8 => "i8",
        U8 => "u8",
        Short | I16 => "i16",
        Long | I32 => "i32",
        Quad | I64 => "i64",
        UQuad | U64 => "u64",
        Bool8 | Bool16 | Bool32 | Bool64 => "bool",
        F32 | F32PP => "float",
        F64 => "double",
        _ => "i32",
    }
}
