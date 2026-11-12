//! Names and types from DWARF: function prototypes, struct members and the names of stack locals.
//!
//! Read once per file (`Image::dwarf`), from `.debug_info` and friends, with gimli. The result has the
//! same shape as what the person gives with `proto`, `struct` and `var`, so the decompiler merges it
//! under the person's own entries (theirs win).

use std::collections::{BTreeMap, HashMap};

use gimli::{AttributeValue, EndianSlice, LittleEndian, Operation, UnitOffset};
use object::{Object, ObjectSection};

type R<'a> = EndianSlice<'a, LittleEndian>;
type Die<'a> = gimli::DebuggingInformationEntry<R<'a>>;

/// Most functions, struct layouts and members kept; a file with more is cut here.
const MAX_ITEMS: usize = 200_000;
const MAX_TYPE_DEPTH: usize = 8;

#[derive(Debug, Default, Clone)]
pub struct Dwarf {
    /// C prototype text by function start address.
    pub protos: BTreeMap<u64, String>,
    /// `0:i32:id 8:node*:next` by struct (or typedef) name.
    pub structs: BTreeMap<String, String>,
    /// Stack local names by function start address: `local_c` -> `count`.
    pub vars: BTreeMap<u64, BTreeMap<String, String>>,
    /// C types of stack slots (locals and spilled arguments) by function address: `local_20` -> `struct item *`.
    pub slot_types: BTreeMap<u64, BTreeMap<String, String>>,
}

/// Read the DWARF of a whole file image (empty when there is none or it cannot be parsed).
pub fn read(data: &[u8]) -> Dwarf {
    let Ok(file) = object::File::parse(data) else { return Dwarf::default() };
    if file.section_by_name(".debug_info").is_none() || !file.is_little_endian() {
        return Dwarf::default();
    }
    let load = |id: gimli::SectionId| -> Result<R<'_>, gimli::Error> {
        let bytes: &[u8] = file.section_by_name(id.name()).and_then(|s| s.data().ok()).unwrap_or(&[]);
        Ok(EndianSlice::new(bytes, LittleEndian))
    };
    let Ok(dwarf) = gimli::Dwarf::load(load) else { return Dwarf::default() };
    let mut out = Dwarf::default();
    let mut units = dwarf.units();
    while let Ok(Some(header)) = units.next() {
        let Ok(unit) = dwarf.unit(header) else { continue };
        let _ = read_unit(&dwarf, &unit, &mut out);
        if out.protos.len() + out.structs.len() > MAX_ITEMS {
            break;
        }
    }
    out
}

struct Ctx<'d, 'a> {
    dwarf: &'d gimli::Dwarf<R<'a>>,
    unit: &'d gimli::Unit<R<'a>>,
}

impl<'a> Ctx<'_, 'a> {
    fn entry(&self, off: UnitOffset) -> Option<Die<'a>> {
        self.unit.entry(off).ok()
    }

    fn string(&self, e: &Die<'a>, at: gimli::DwAt) -> Option<String> {
        let v = e.attr_value(at)?;
        let s = self.dwarf.attr_string(self.unit, v).ok()?;
        Some(s.to_string_lossy().into_owned())
    }

    fn unit_ref(&self, e: &Die<'a>, at: gimli::DwAt) -> Option<UnitOffset> {
        match e.attr_value(at)? {
            AttributeValue::UnitRef(o) => Some(o),
            _ => None,
        }
    }

    /// The DIE itself or the one it is an instance of (`abstract_origin`, `specification`).
    fn origin(&self, e: &Die<'a>) -> Option<Die<'a>> {
        let o =
            self.unit_ref(e, gimli::DW_AT_abstract_origin).or_else(|| self.unit_ref(e, gimli::DW_AT_specification))?;
        self.entry(o)
    }

    /// An attribute from the DIE, or else from its origin.
    fn string_o(&self, e: &Die<'a>, at: gimli::DwAt) -> Option<String> {
        self.string(e, at).or_else(|| self.origin(e).and_then(|o| self.string(&o, at)))
    }

    fn type_of(&self, e: &Die<'a>) -> Option<UnitOffset> {
        self.unit_ref(e, gimli::DW_AT_type)
            .or_else(|| self.origin(e).and_then(|o| self.unit_ref(&o, gimli::DW_AT_type)))
    }

    /// A C type as short text in the style of the rest of the output: `i32`, `u8`, `char *`,
    /// `struct item *`, `size_t`.
    fn ctype(&self, off: Option<UnitOffset>, depth: usize) -> String {
        let Some(off) = off else { return "void".into() };
        if depth > MAX_TYPE_DEPTH {
            return "void".into();
        }
        let Some(e) = self.entry(off) else { return "void".into() };
        let inner = |s: &Self| s.ctype(s.unit_ref(&e, gimli::DW_AT_type), depth + 1);
        match e.tag() {
            gimli::DW_TAG_base_type => self.base(&e),
            gimli::DW_TAG_pointer_type | gimli::DW_TAG_reference_type => ptr_to(&inner(self)),
            gimli::DW_TAG_array_type => ptr_to(&inner(self)),
            gimli::DW_TAG_subroutine_type => "void *".into(),
            gimli::DW_TAG_const_type => {
                let t = inner(self);
                if t.ends_with('*') {
                    t
                } else {
                    format!("const {t}")
                }
            }
            gimli::DW_TAG_volatile_type | gimli::DW_TAG_restrict_type | gimli::DW_TAG_atomic_type => inner(self),
            gimli::DW_TAG_typedef => self.string(&e, gimli::DW_AT_name).unwrap_or_else(|| inner(self)),
            gimli::DW_TAG_structure_type | gimli::DW_TAG_class_type => match self.string(&e, gimli::DW_AT_name) {
                Some(n) => format!("struct {n}"),
                None => "struct".into(),
            },
            gimli::DW_TAG_union_type => match self.string(&e, gimli::DW_AT_name) {
                Some(n) => format!("union {n}"),
                None => "union".into(),
            },
            gimli::DW_TAG_enumeration_type => match self.string(&e, gimli::DW_AT_name) {
                Some(n) => format!("enum {n}"),
                None => "i32".into(),
            },
            _ => "void".into(),
        }
    }

    fn base(&self, e: &Die<'a>) -> String {
        let size = match e.attr_value(gimli::DW_AT_byte_size) {
            Some(AttributeValue::Udata(n)) => n,
            Some(AttributeValue::Data1(n)) => u64::from(n),
            _ => 0,
        };
        let enc = match e.attr_value(gimli::DW_AT_encoding) {
            Some(AttributeValue::Encoding(x)) => x,
            _ => gimli::DW_ATE_signed,
        };
        let name = self.string(e, gimli::DW_AT_name).unwrap_or_default();
        match enc {
            gimli::DW_ATE_boolean => "bool".into(),
            gimli::DW_ATE_float => match size {
                4 => "float".into(),
                8 => "double".into(),
                _ => "long double".into(),
            },
            gimli::DW_ATE_signed_char | gimli::DW_ATE_unsigned_char if name == "char" => "char".into(),
            gimli::DW_ATE_unsigned | gimli::DW_ATE_unsigned_char | gimli::DW_ATE_UTF => format!("u{}", size * 8),
            _ => format!("i{}", size * 8),
        }
    }
}

fn ptr_to(t: &str) -> String {
    if t.ends_with('*') {
        format!("{t}*")
    } else {
        format!("{t} *")
    }
}

fn decl(ty: &str, name: &str) -> String {
    if ty.ends_with('*') {
        format!("{ty}{name}")
    } else {
        format!("{ty} {name}")
    }
}

/// A member type as one token: no spaces, no qualifiers.
fn field_type(t: &str) -> String {
    let t = t.replace("const ", "");
    t.replace("struct ", "").replace("union ", "").replace("enum ", "").replace(' ', "")
}

struct Func {
    low: u64,
    name: String,
    ret: String,
    params: Vec<(String, String)>,
    variadic: bool,
    depth: isize,
    /// How far a frame-base-relative offset is from the stack pointer at entry.
    fb_bias: Option<i64>,
    locals: Vec<(i64, String)>,
    slot_types: Vec<(i64, String)>,
}

struct Agg {
    name: Option<String>,
    off: UnitOffset,
    depth: isize,
    fields: Vec<String>,
}

fn read_unit(dwarf: &gimli::Dwarf<R<'_>>, unit: &gimli::Unit<R<'_>>, out: &mut Dwarf) -> gimli::Result<()> {
    let cx = Ctx { dwarf, unit };
    let mut cur = unit.entries();
    let mut func: Option<Func> = None;
    let mut agg: Option<Agg> = None;
    // struct offset -> member text, and typedef name -> target offset, for aliases
    let mut by_off: HashMap<usize, String> = HashMap::new();
    let mut typedefs: Vec<(String, Option<UnitOffset>)> = Vec::new();
    while let Some(e) = cur.next_dfs()? {
        let depth = e.depth();
        if func.as_ref().is_some_and(|f| depth <= f.depth) {
            finish_func(func.take(), out);
        }
        if agg.as_ref().is_some_and(|a| depth <= a.depth) {
            finish_agg(agg.take(), out, &mut by_off);
        }
        let tag = e.tag();
        let e = e.clone();
        if tag == gimli::DW_TAG_subprogram && func.is_none() {
            func = begin_func(&cx, &e, depth);
        } else if let Some(f) = func.as_mut() {
            match tag {
                gimli::DW_TAG_formal_parameter if depth == f.depth + 1 => {
                    let ty = cx.ctype(cx.type_of(&e), 0);
                    let name = cx.string_o(&e, gimli::DW_AT_name).unwrap_or_default();
                    if let (Some(bias), Some(fb)) = (f.fb_bias, fbreg(&cx, &e)) {
                        f.slot_types.push((bias + fb, ty.clone()));
                    }
                    f.params.push((ty, name));
                }
                gimli::DW_TAG_unspecified_parameters if depth == f.depth + 1 => f.variadic = true,
                gimli::DW_TAG_variable => {
                    if let (Some(bias), Some(name)) = (f.fb_bias, cx.string_o(&e, gimli::DW_AT_name)) {
                        if let Some(fb) = fbreg(&cx, &e) {
                            f.slot_types.push((bias + fb, cx.ctype(cx.type_of(&e), 0)));
                            f.locals.push((bias + fb, name));
                        }
                    }
                }
                _ => {}
            }
        }
        if matches!(tag, gimli::DW_TAG_structure_type | gimli::DW_TAG_union_type | gimli::DW_TAG_class_type)
            && agg.is_none()
            && e.attr_value(gimli::DW_AT_declaration).is_none()
        {
            agg = Some(Agg { name: cx.string(&e, gimli::DW_AT_name), off: e.offset(), depth, fields: Vec::new() });
        } else if let Some(a) = agg.as_mut() {
            if tag == gimli::DW_TAG_member && depth == a.depth + 1 && e.attr_value(gimli::DW_AT_bit_size).is_none() {
                let off = match e.attr_value(gimli::DW_AT_data_member_location) {
                    Some(AttributeValue::Udata(n)) => Some(n),
                    Some(AttributeValue::Data1(n)) => Some(u64::from(n)),
                    Some(AttributeValue::Data2(n)) => Some(u64::from(n)),
                    Some(AttributeValue::Data4(n)) => Some(u64::from(n)),
                    Some(AttributeValue::Sdata(n)) if n >= 0 => Some(n as u64),
                    None if a.fields.is_empty() => Some(0),
                    _ => None,
                };
                if let (Some(off), Some(name)) = (off, cx.string(&e, gimli::DW_AT_name)) {
                    let ty = field_type(&cx.ctype(cx.type_of(&e), 0));
                    if !ty.is_empty() && !ty.contains(':') {
                        a.fields.push(format!("{off:#x}:{ty}:{name}"));
                    }
                }
            }
        }
        if tag == gimli::DW_TAG_typedef {
            if let Some(n) = cx.string(&e, gimli::DW_AT_name) {
                typedefs.push((n, cx.unit_ref(&e, gimli::DW_AT_type)));
            }
        }
    }
    finish_func(func.take(), out);
    finish_agg(agg.take(), out, &mut by_off);
    // a typedef of a struct (possibly anonymous) names the same members
    for (name, target) in typedefs {
        let mut t = target;
        for _ in 0..4 {
            let Some(o) = t else { break };
            if let Some(f) = by_off.get(&o.0) {
                out.structs.entry(name.clone()).or_insert_with(|| f.clone());
                break;
            }
            t = cx.entry(o).and_then(|d| match d.tag() {
                gimli::DW_TAG_const_type | gimli::DW_TAG_volatile_type | gimli::DW_TAG_typedef => {
                    cx.unit_ref(&d, gimli::DW_AT_type)
                }
                _ => None,
            });
        }
    }
    Ok(())
}

fn begin_func<'a>(cx: &Ctx<'_, 'a>, e: &Die<'a>, depth: isize) -> Option<Func> {
    if e.attr_value(gimli::DW_AT_declaration).is_some() {
        return None;
    }
    let low = match e.attr_value(gimli::DW_AT_low_pc)? {
        AttributeValue::Addr(a) => a,
        v => cx.dwarf.attr_address(cx.unit, v).ok().flatten()?,
    };
    let name = cx.string_o(e, gimli::DW_AT_name)?;
    let ret = cx.ctype(cx.type_of(e), 0);
    let fb_bias = match e.attr_value(gimli::DW_AT_frame_base) {
        Some(AttributeValue::Exprloc(x)) => match x.operations(cx.unit.encoding()).next() {
            // the CFA is the stack pointer at entry plus the return address
            Ok(Some(Operation::CallFrameCFA)) => Some(8),
            // rbp after `push rbp; mov rbp, rsp`
            Ok(Some(Operation::Register { register })) if register.0 == 6 => Some(-8),
            _ => None,
        },
        _ => None,
    };
    Some(Func {
        low,
        name,
        ret,
        params: Vec::new(),
        variadic: false,
        depth,
        fb_bias,
        locals: Vec::new(),
        slot_types: Vec::new(),
    })
}

/// The frame-base offset of a variable stored at `DW_OP_fbreg N` and nothing else.
fn fbreg<'a>(cx: &Ctx<'_, 'a>, e: &Die<'a>) -> Option<i64> {
    let AttributeValue::Exprloc(x) = e.attr_value(gimli::DW_AT_location)? else { return None };
    let mut ops = x.operations(cx.unit.encoding());
    let Ok(Some(Operation::FrameOffset { offset })) = ops.next() else { return None };
    matches!(ops.next(), Ok(None)).then_some(offset)
}

fn finish_func(f: Option<Func>, out: &mut Dwarf) {
    let Some(f) = f else { return };
    if out.protos.contains_key(&f.low) {
        return;
    }
    let mut used: Vec<String> = f.params.iter().map(|p| p.1.clone()).collect();
    let mut args: Vec<String> = Vec::new();
    for (i, (t, n)) in f.params.iter().enumerate() {
        let n = if n.is_empty() { format!("a{}", i + 1) } else { n.clone() };
        args.push(decl(t, &n));
    }
    if f.variadic {
        args.push("...".into());
    }
    let list = if args.is_empty() { "void".to_string() } else { args.join(", ") };
    let mut types = BTreeMap::new();
    for (slot, ty) in f.slot_types {
        if slot < 0 {
            types.entry(format!("local_{:x}", -slot)).or_insert(ty);
        }
    }
    if !types.is_empty() {
        out.slot_types.insert(f.low, types);
    }
    out.protos.insert(f.low, format!("{}({list})", decl(&f.ret, &f.name)));
    let mut vars = BTreeMap::new();
    for (slot, name) in f.locals {
        if slot >= 0 {
            continue;
        }
        let key = format!("local_{:x}", -slot);
        let mut n = name.clone();
        let mut k = 2;
        while used.contains(&n) {
            n = format!("{name}_{k}");
            k += 1;
        }
        used.push(n.clone());
        vars.entry(key).or_insert(n);
    }
    if !vars.is_empty() {
        out.vars.insert(f.low, vars);
    }
}

fn finish_agg(a: Option<Agg>, out: &mut Dwarf, by_off: &mut HashMap<usize, String>) {
    let Some(a) = a else { return };
    if a.fields.is_empty() {
        return;
    }
    let text = a.fields.join(" ");
    if let Some(n) = a.name {
        out.structs.entry(n).or_insert_with(|| text.clone());
    }
    by_off.insert(a.off.0, text);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decomp_window, Image};
    use std::path::PathBuf;
    use std::process::Command;

    const SRC: &str = r#"
struct item { int id; long count; struct item *next; const char *label; };
typedef struct { int x; int y; } point_t;

__attribute__((noinline)) int total(struct item *it, int limit) {
    int sum = 0;
    int i;
    for (i = 0; it && i < limit; i++) {
        sum += it->count;
        it = it->next;
    }
    return sum;
}

__attribute__((noinline)) long area(point_t *p, unsigned char scale, ...) {
    return (long)p->x * p->y * scale;
}

int main(int argc, char **argv) {
    struct item a = {1, 5, 0, "a"};
    point_t p = {argc, 3};
    return total(&a, argc) + (int)area(&p, 2);
}
"#;

    /// Compile the sample with `-g -O0`; None (with a message) when there is no C compiler.
    fn build(dir: &std::path::Path) -> Option<PathBuf> {
        let src = dir.join("prog.c");
        std::fs::write(&src, SRC).ok()?;
        let exe = dir.join("prog");
        let ok = ["cc", "gcc", "clang"].iter().any(|cc| {
            Command::new(cc)
                .args(["-g", "-O0", "-fno-pie", "-no-pie", "-o"])
                .arg(&exe)
                .arg(&src)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        });
        // a Windows compiler writes `prog.exe`: the sample is a Unix build
        if !ok || !exe.is_file() {
            eprintln!("skipped: no C compiler that builds the sample here");
            return None;
        }
        Some(exe)
    }

    #[test]
    fn dwarf_names_and_types_reach_decomp() {
        let dir = tempfile::tempdir().unwrap();
        let Some(exe) = build(dir.path()) else { return };
        let data = std::fs::read(&exe).unwrap();
        let d = read(&data);
        let total = d.protos.values().find(|p| p.contains(" total(")).expect("prototype of total");
        assert_eq!(total, "i32 total(struct item *it, i32 limit)");
        let area = d.protos.values().find(|p| p.contains(" area(")).expect("prototype of area");
        assert!(area.contains("point_t *p") && area.ends_with(", ...)"), "{area}");
        assert!(d.structs["item"].contains("0x8:i64:count") && d.structs["item"].contains("next"), "{:?}", d.structs);
        assert!(d.structs["point_t"].contains("0x4:i32:y"), "{:?}", d.structs);
        assert!(d.vars.values().any(|m| m.values().any(|n| n == "sum")), "{:?}", d.vars);

        let img = Image::open(&exe).unwrap();
        let data_dir = dir.path().join("data");
        let o = decomp_window(&img, &data_dir, &["total".to_string()], false, None, 200).unwrap();
        let text = o.text_all();
        println!("{text}");
        assert!(text.contains("i32 total(struct item *it, i32 limit)"), "{text}");
        assert!(text.contains("sum"), "{text}");
        assert!(text.contains("->count") && text.contains("->next"), "{text}");
    }

    #[test]
    fn person_wins_over_dwarf() {
        let dir = tempfile::tempdir().unwrap();
        let Some(exe) = build(dir.path()) else { return };
        let img = Image::open(&exe).unwrap();
        let data_dir = dir.path().join("data");
        crate::proto_set(&img, &data_dir, "total", Some("long total(struct item *head, int n)")).unwrap();
        let mut img = Image::open(&exe).unwrap();
        crate::attach_names(&mut img, &data_dir);
        let o = decomp_window(&img, &data_dir, &["total".to_string()], false, None, 200).unwrap();
        let text = o.text_all();
        assert!(text.contains("long total(struct item *head, int n)"), "{text}");
        assert!(!text.contains("limit"), "{text}");
    }

    #[test]
    fn a_file_without_dwarf_is_empty() {
        let d = read(&crate::fixtures::elf64(false).bytes);
        assert!(d.protos.is_empty() && d.structs.is_empty() && d.vars.is_empty());
    }
}
