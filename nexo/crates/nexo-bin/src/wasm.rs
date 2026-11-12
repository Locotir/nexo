//! WebAssembly modules: the sections, the function index space (imports first), names from the name
//! section, data segments and, for every function body, the calls and constants it holds.
//!
//! Positions in a module are file offsets: a function is named by its index (decimal), a position by
//! its offset (hex), the way `wasm-objdump` prints them. A module is only read, never run.

use std::collections::BTreeMap;
use std::ops::Range;

use sha2::{Digest, Sha256};
use wasmparser::{
    CompositeInnerType, DataKind, ElementItems, ElementKind, ExternalKind, FunctionBody, KnownCustom, Name, Operator,
    Parser, Payload, TypeRef,
};

use crate::image::demangle;
use crate::{Error, Result};

/// The four bytes every module starts with.
pub const MAGIC: &[u8; 4] = b"\0asm";
/// Shortest run of printable bytes `strings` reports.
pub const MIN_STRING: usize = 4;

#[derive(Debug, Clone)]
pub(crate) struct Import {
    pub module: String,
    pub name: String,
    /// `func`, `table`, `memory`, `global` or `tag`.
    pub kind: &'static str,
}

#[derive(Debug, Clone)]
pub(crate) struct Export {
    pub name: String,
    pub kind: &'static str,
    pub index: u32,
}

/// A function in the index space: an import (no body) or a defined one.
#[derive(Debug, Clone)]
pub(crate) struct Func {
    pub index: u32,
    pub type_idx: u32,
    /// `(module, name)` for an imported function.
    pub import: Option<(String, String)>,
    /// The name from the name section, demangled.
    pub name: Option<String>,
    /// The body as file offsets (after the size field).
    pub body: Option<Range<usize>>,
}

#[derive(Debug, Clone)]
pub(crate) struct Segment {
    pub index: usize,
    /// Linear-memory address of an active segment with a constant offset.
    pub mem_addr: Option<u64>,
    /// File offset of the bytes.
    pub file_off: usize,
    pub len: usize,
}

/// What one defined function does, found by reading its instructions once.
#[derive(Debug, Default, Clone)]
pub(crate) struct Scan {
    pub ops: usize,
    /// `(callee, offset of the call)`, including `return_call`.
    pub calls: Vec<(u32, usize)>,
    /// `(type index, offset)` of each `call_indirect`.
    pub indirect: Vec<(u32, usize)>,
    /// `(value, offset)` of each `i32.const` and `i64.const`.
    pub consts: Vec<(i64, usize)>,
    /// `(function, offset)` of each `ref.func`.
    pub ref_funcs: Vec<(u32, usize)>,
    /// Where decoding stopped and why, when the body is not valid.
    pub bad: Option<(usize, String)>,
}

pub(crate) struct Module<'a> {
    pub data: &'a [u8],
    pub sha256: String,
    pub version: u32,
    pub types: Vec<(Vec<String>, Vec<String>)>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub funcs: Vec<Func>,
    /// One line each: `memory[0] 1..2 pages`.
    pub memories: Vec<String>,
    pub tables: Vec<String>,
    pub globals: Vec<String>,
    pub start: Option<u32>,
    pub segments: Vec<Segment>,
    /// Functions placed in a table by an element segment.
    pub in_tables: Vec<u32>,
    /// `(name, offset, size)` of the custom sections.
    pub customs: Vec<(String, usize, usize)>,
    /// `(field, name, version)` from the producers section.
    pub producers: Vec<(String, String, String)>,
    /// Names of locals from the name section: `(function, local) -> name`.
    pub local_names: BTreeMap<(u32, u32), String>,
    pub global_names: BTreeMap<u32, String>,
    pub scans: Vec<Scan>,
    /// Defined functions: the index of the first one.
    pub first_defined: u32,
    bodies: Vec<FunctionBody<'a>>,
}

fn bad(e: wasmparser::BinaryReaderError) -> Error {
    Error::NotRead(format!("not a readable WebAssembly module: {} (at file offset {:#x})", e.message(), e.offset()))
}

fn kind_name(k: ExternalKind) -> &'static str {
    match k {
        ExternalKind::Func => "func",
        ExternalKind::Table => "table",
        ExternalKind::Memory => "memory",
        ExternalKind::Global => "global",
        ExternalKind::Tag => "tag",
    }
}

fn limits(min: u64, max: Option<u64>, unit: &str) -> String {
    match max {
        Some(m) => format!("{min}..{m} {unit}"),
        None => format!("{min}.. {unit}"),
    }
}

impl<'a> Module<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Module<'a>> {
        if !data.starts_with(MAGIC) {
            return Err(Error::NotRead("not a WebAssembly module (no \\0asm signature)".into()));
        }
        let mut m = Module {
            data,
            sha256: Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect(),
            version: 0,
            types: Vec::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            funcs: Vec::new(),
            memories: Vec::new(),
            tables: Vec::new(),
            globals: Vec::new(),
            start: None,
            segments: Vec::new(),
            in_tables: Vec::new(),
            customs: Vec::new(),
            producers: Vec::new(),
            local_names: BTreeMap::new(),
            global_names: BTreeMap::new(),
            scans: Vec::new(),
            first_defined: 0,
            bodies: Vec::new(),
        };
        let mut func_types: Vec<u32> = Vec::new();
        let mut names: BTreeMap<u32, String> = BTreeMap::new();
        for payload in Parser::new(0).parse_all(data) {
            match payload.map_err(bad)? {
                Payload::Version { num, .. } => m.version = u32::from(num),
                Payload::TypeSection(r) => {
                    for group in r {
                        for ty in group.map_err(bad)?.types() {
                            let (p, res) = match &ty.composite_type.inner {
                                CompositeInnerType::Func(f) => (
                                    f.params().iter().map(|t| t.to_string()).collect(),
                                    f.results().iter().map(|t| t.to_string()).collect(),
                                ),
                                _ => (vec!["(gc type)".to_string()], Vec::new()),
                            };
                            m.types.push((p, res));
                        }
                    }
                }
                Payload::ImportSection(r) => {
                    for imp in r {
                        let imp = imp.map_err(bad)?;
                        let (kind, func_type) = match imp.ty {
                            TypeRef::Func(t) => ("func", Some(t)),
                            TypeRef::Table(_) => ("table", None),
                            TypeRef::Memory(_) => ("memory", None),
                            TypeRef::Global(_) => ("global", None),
                            TypeRef::Tag(_) => ("tag", None),
                        };
                        let (module, name) = (imp.module.to_string(), imp.name.to_string());
                        if let Some(t) = func_type {
                            let index = m.funcs.len() as u32;
                            m.funcs.push(Func {
                                index,
                                type_idx: t,
                                import: Some((module.clone(), name.clone())),
                                name: None,
                                body: None,
                            });
                        }
                        m.imports.push(Import { module, name, kind });
                    }
                    m.first_defined = m.funcs.len() as u32;
                }
                Payload::FunctionSection(r) => {
                    for t in r {
                        func_types.push(t.map_err(bad)?);
                    }
                }
                Payload::TableSection(r) => {
                    for t in r {
                        let t = t.map_err(bad)?.ty;
                        let n = m.tables.len();
                        m.tables.push(format!(
                            "table[{n}] {} {}",
                            t.element_type,
                            limits(t.initial, t.maximum, "entries")
                        ));
                    }
                }
                Payload::MemorySection(r) => {
                    for t in r {
                        let t = t.map_err(bad)?;
                        let n = m.memories.len();
                        let extra = format!(
                            "{}{}",
                            if t.shared { " shared" } else { "" },
                            if t.memory64 { " 64-bit" } else { "" }
                        );
                        m.memories
                            .push(format!("memory[{n}] {}{extra}", limits(t.initial, t.maximum, "pages of 64 KiB")));
                    }
                }
                Payload::GlobalSection(r) => {
                    for g in r {
                        let g = g.map_err(bad)?;
                        let n = m.globals.len();
                        m.globals.push(format!(
                            "global[{n}] {}{}",
                            if g.ty.mutable { "mut " } else { "" },
                            g.ty.content_type
                        ));
                    }
                }
                Payload::ExportSection(r) => {
                    for e in r {
                        let e = e.map_err(bad)?;
                        m.exports.push(Export { name: e.name.to_string(), kind: kind_name(e.kind), index: e.index });
                    }
                }
                Payload::StartSection { func, .. } => m.start = Some(func),
                Payload::ElementSection(r) => {
                    for e in r {
                        let e = e.map_err(bad)?;
                        let _ = matches!(e.kind, ElementKind::Active { .. });
                        match e.items {
                            ElementItems::Functions(fs) => {
                                for f in fs {
                                    m.in_tables.push(f.map_err(bad)?);
                                }
                            }
                            ElementItems::Expressions(_, exprs) => {
                                for x in exprs {
                                    let x = x.map_err(bad)?;
                                    for op in x.get_operators_reader() {
                                        if let Ok(Operator::RefFunc { function_index }) = op {
                                            m.in_tables.push(function_index);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Payload::CodeSectionEntry(body) => m.bodies.push(body),
                Payload::DataSection(r) => {
                    for (index, d) in r.into_iter().enumerate() {
                        let d = d.map_err(bad)?;
                        let (_active, mem_addr) = match &d.kind {
                            DataKind::Passive => (false, None),
                            DataKind::Active { offset_expr, .. } => {
                                let mut addr = None;
                                for op in offset_expr.get_operators_reader() {
                                    match op {
                                        Ok(Operator::I32Const { value }) => addr = Some(u64::from(value as u32)),
                                        Ok(Operator::I64Const { value }) => addr = Some(value as u64),
                                        _ => {}
                                    }
                                }
                                (true, addr)
                            }
                        };
                        // `range` covers the segment header too; the bytes are the tail of it.
                        let file_off = d.range.end - d.data.len();
                        m.segments.push(Segment { index, mem_addr, file_off, len: d.data.len() });
                    }
                }
                Payload::CustomSection(c) => {
                    m.customs.push((c.name().to_string(), c.data_offset(), c.data().len()));
                    match c.as_known() {
                        KnownCustom::Name(r) => {
                            for n in r {
                                match n {
                                    Ok(Name::Function(map)) => {
                                        for nm in map.into_iter().flatten() {
                                            names.insert(nm.index, nm.name.to_string());
                                        }
                                    }
                                    Ok(Name::Local(map)) => {
                                        for f in map.into_iter().flatten() {
                                            for nm in f.names.into_iter().flatten() {
                                                m.local_names.insert((f.index, nm.index), nm.name.to_string());
                                            }
                                        }
                                    }
                                    Ok(Name::Global(map)) => {
                                        for nm in map.into_iter().flatten() {
                                            m.global_names.insert(nm.index, nm.name.to_string());
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        KnownCustom::Producers(r) => {
                            for f in r.into_iter().flatten() {
                                for v in f.values.into_iter().flatten() {
                                    m.producers.push((f.name.to_string(), v.name.to_string(), v.version.to_string()));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if func_types.len() != m.bodies.len() {
            return Err(Error::NotRead(format!(
                "not a readable WebAssembly module: {} function declarations but {} bodies",
                func_types.len(),
                m.bodies.len()
            )));
        }
        for (i, (t, body)) in func_types.iter().zip(&m.bodies).enumerate() {
            let range = body.range();
            m.funcs.push(Func {
                index: m.first_defined + i as u32,
                type_idx: *t,
                import: None,
                name: None,
                body: Some(range),
            });
        }
        for f in &mut m.funcs {
            f.name = names.get(&f.index).map(|n| demangle(n));
        }
        m.scans = m.bodies.iter().map(scan_body).collect();
        Ok(m)
    }

    /// `(i32, i32) -> i32` for a type index.
    pub fn sig(&self, type_idx: u32) -> String {
        match self.types.get(type_idx as usize) {
            Some((p, r)) => format!("({}) -> {}", p.join(", "), if r.is_empty() { "()".into() } else { r.join(", ") }),
            None => format!("type[{type_idx}] (not in the type section)"),
        }
    }

    pub fn func(&self, index: u32) -> Option<&Func> {
        self.funcs.get(index as usize)
    }

    pub fn scan(&self, index: u32) -> Option<&Scan> {
        index.checked_sub(self.first_defined).and_then(|i| self.scans.get(i as usize))
    }

    /// The best name for a function: the name section, an export, the import, else `func[N]`.
    pub fn name(&self, index: u32) -> String {
        let Some(f) = self.func(index) else { return format!("func[{index}] (not in the module)") };
        if let Some(n) = &f.name {
            return n.clone();
        }
        if let Some((m, n)) = &f.import {
            return format!("{m}.{n}");
        }
        match self.exports.iter().find(|e| e.kind == "func" && e.index == index) {
            Some(e) => e.name.clone(),
            None => format!("func[{index}]"),
        }
    }

    /// Where the name came from: `name`, `import`, `export` or `none`.
    pub fn name_source(&self, index: u32) -> &'static str {
        let Some(f) = self.func(index) else { return "none" };
        if f.name.is_some() {
            "name"
        } else if f.import.is_some() {
            "import"
        } else if self.exports.iter().any(|e| e.kind == "func" && e.index == index) {
            "export"
        } else {
            "none"
        }
    }

    /// The defined function whose body holds a file offset.
    pub fn func_at(&self, off: usize) -> Option<&Func> {
        self.funcs.iter().find(|f| f.body.as_ref().is_some_and(|b| b.contains(&off)))
    }

    /// Callers of each function: `callee -> [(caller, call offset)]`, sorted by caller.
    pub fn callers(&self) -> BTreeMap<u32, Vec<(u32, usize)>> {
        let mut map: BTreeMap<u32, Vec<(u32, usize)>> = BTreeMap::new();
        for (i, s) in self.scans.iter().enumerate() {
            for &(callee, off) in &s.calls {
                map.entry(callee).or_default().push((self.first_defined + i as u32, off));
            }
        }
        map
    }

    pub fn raw(&self, range: Range<usize>) -> &[u8] {
        &self.data[range.start.min(self.data.len())..range.end.min(self.data.len())]
    }

    pub fn body(&self, index: u32) -> Option<&FunctionBody<'a>> {
        index.checked_sub(self.first_defined).and_then(|i| self.bodies.get(i as usize))
    }

    /// The reader for a body's instructions with their file offsets.
    pub fn ops(&self, index: u32) -> Option<Result<Vec<(Operator<'a>, usize)>>> {
        let body = self.body(index)?;
        let run = || -> std::result::Result<Vec<_>, wasmparser::BinaryReaderError> {
            body.get_operators_reader()?.into_iter_with_offsets().collect()
        };
        Some(run().map_err(bad))
    }
}

fn scan_body(body: &FunctionBody<'_>) -> Scan {
    let mut s = Scan::default();
    let Ok(reader) = body.get_operators_reader() else {
        s.bad = Some((body.range().start, "locals are cut off".into()));
        return s;
    };
    for item in reader.into_iter_with_offsets() {
        match item {
            Ok((op, off)) => {
                s.ops += 1;
                match op {
                    Operator::Call { function_index } | Operator::ReturnCall { function_index } => {
                        s.calls.push((function_index, off))
                    }
                    Operator::CallIndirect { type_index, .. } | Operator::ReturnCallIndirect { type_index, .. } => {
                        s.indirect.push((type_index, off))
                    }
                    Operator::I32Const { value } => s.consts.push((i64::from(value), off)),
                    Operator::I64Const { value } => s.consts.push((value, off)),
                    Operator::RefFunc { function_index } => s.ref_funcs.push((function_index, off)),
                    _ => {}
                }
            }
            Err(e) => {
                s.bad = Some((e.offset(), e.message().to_string()));
                break;
            }
        }
    }
    s
}
