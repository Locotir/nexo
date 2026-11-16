//! `syms`: what the file's own tables list, one row each: the libraries it needs, its imports (with the library,
//! the GOT/IAT slot the loader fills and the stub that jumps through it), its exports, its function and data
//! symbols, its sections, its slots and its relocations. Nothing here is found from the code except the stubs,
//! so a stripped file lists what is left and says where the rest is found.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use crate::disasm::describe;
use crate::funcs::Func;
use crate::image::{Format, Image};
use crate::ops::command;
use crate::page::{cut, Output, Row};
use crate::place::slot_name;
use crate::syms_read::{self, reloc_type, reloc_value, Tables};
use crate::{Error, Result};

/// The kinds of row `syms` lists, in the order it lists them.
pub const SYM_KINDS: [&str; 8] = ["needed", "import", "export", "func", "data", "section", "got", "reloc"];
/// The words `--kind` takes: the kinds, `plt` and `iat` for `got`, and `all`.
pub const SYM_KIND_WORDS: [&str; 11] =
    ["needed", "import", "export", "func", "data", "section", "got", "reloc", "plt", "iat", "all"];
/// The kinds listed when none is asked for.
const DEFAULT: [&str; 5] = ["needed", "import", "export", "func", "data"];

/// One line of the answer.
enum Entry {
    Text(String),
    Needed(usize),
    Import(usize),
    /// A PE delay-load import, by its place in [`Tables::delay`].
    Delay(usize),
    Export(usize),
    Sym(usize),
    Section(usize),
    /// An import's slot, by the import's index.
    Got(usize),
    /// An ELF or PE relocation, by its place in [`Tables::relocs`].
    Reloc(usize),
    /// A Mach-O rebase: a pointer the loader slides by the load address.
    Rebase(usize),
    /// A Mach-O bind: a slot the loader fills with an import's address, by the import's index.
    Bind(usize),
}

/// The kinds `asked` names, in [`SYM_KINDS`] order; none asked gives [`DEFAULT`].
fn kinds(asked: &[String]) -> Result<Vec<&'static str>> {
    if asked.is_empty() {
        return Ok(DEFAULT.to_vec());
    }
    let mut want = HashSet::new();
    for w in asked {
        let w = w.trim().to_ascii_lowercase();
        match w.as_str() {
            "all" => want.extend(SYM_KINDS),
            "plt" | "iat" => {
                want.insert("got");
            }
            _ => match SYM_KINDS.iter().find(|k| **k == w) {
                Some(k) => {
                    want.insert(*k);
                }
                None => {
                    return Err(Error::Usage(format!(
                        "{w:?} is not a kind of row; the kinds are {} (plt and iat are got)",
                        SYM_KIND_WORDS.join(", ")
                    )))
                }
            },
        }
    }
    Ok(SYM_KINDS.iter().copied().filter(|k| want.contains(k)).collect())
}

/// What a row needs besides the image.
struct Ctx<'a> {
    img: &'a Image,
    t: Tables,
    /// Import index to the stub that jumps through its slot.
    stubs: HashMap<usize, &'a Func>,
}

fn addr_col(a: Option<u64>) -> String {
    a.map_or_else(|| format!("{:<10}", "-"), |a| format!("{a:#010x}"))
}

fn size_col(s: Option<u64>) -> String {
    s.filter(|&s| s > 0).map_or_else(|| "-".into(), |s| s.to_string())
}

/// The name as the file writes it: `raw` when the image keeps one (the name was demangled), else `name`.
fn written<'a>(raw: &'a str, name: &'a str) -> &'a str {
    if raw.is_empty() {
        name
    } else {
        raw
    }
}

fn nonempty(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

fn line(addr: Option<u64>, kind: &str, size: Option<u64>, rest: &str) -> String {
    format!("{}  {kind:<7} {:>6}  {rest}", addr_col(addr), size_col(size))
}

impl Ctx<'_> {
    /// `libc.so.6 GLIBC_2.2.5`, `KERNEL32.dll`, or nothing for an ELF import whose version names no library.
    fn lib_of(&self, i: usize) -> (Option<String>, Option<String>) {
        let imp = &self.img.imports[i];
        match self.t.versions.get(written(&imp.raw, &imp.name)) {
            Some((file, v)) => (imp.lib.clone().or_else(|| file.clone()), Some(v.clone())),
            None => (imp.lib.clone(), None),
        }
    }

    fn render(&self, e: &Entry) -> Row {
        let img = self.img;
        let (text, j) = match *e {
            Entry::Text(ref s) => return Row { key: 0, text: s.clone(), json: None },
            Entry::Needed(i) => {
                let n = &self.t.needed[i];
                (line(None, "needed", None, n), json!({"kind": "needed", "name": n}))
            }
            Entry::Import(i) => {
                let imp = &img.imports[i];
                let (lib, ver) = self.lib_of(i);
                let stub = self.stubs.get(&i);
                let mut s = imp.name.clone();
                for w in [&lib, &ver].into_iter().flatten() {
                    s += "  ";
                    s += w;
                }
                if let Some(f) = stub {
                    s += &format!("  stub {}", f.name);
                }
                if let Some(slot) = imp.slot {
                    s += &format!("  slot {slot:#x} {}", slot_name(img, imp));
                }
                let at = stub.map(|f| f.start).or(imp.slot);
                (
                    line(at, "import", stub.map(|f| f.size()), &s),
                    json!({"kind": "import", "addr": at, "name": imp.name, "raw": nonempty(&imp.raw), "lib": lib, "version": ver,
                        "stub": stub.map(|f| f.start), "stub_name": stub.map(|f| f.name.as_str()), "slot": imp.slot,
                        "slot_name": imp.slot.map(|_| slot_name(img, imp))}),
                )
            }
            Entry::Delay(i) => {
                let (lib, name, slot) = &self.t.delay[i];
                (
                    line(Some(*slot), "import", None, &format!("{name}  {lib}  delay-loaded  slot {slot:#x}")),
                    json!({"kind": "import", "addr": slot, "name": name, "lib": lib, "slot": slot, "delay": true}),
                )
            }
            Entry::Export(i) => {
                let x = &img.exports[i];
                (
                    line(Some(x.addr), "export", None, &x.name),
                    json!({"kind": "export", "addr": x.addr, "name": x.name, "raw": nonempty(&x.raw)}),
                )
            }
            Entry::Sym(i) => {
                let s = &img.symbols[i];
                let kind = if s.func { "func" } else { "data" };
                (
                    line(Some(s.addr), kind, Some(s.size), &s.name),
                    json!({"kind": kind, "addr": s.addr, "size": s.size, "name": s.name, "raw": nonempty(&s.raw)}),
                )
            }
            Entry::Section(i) => {
                let s = &img.sections[i];
                let at = s.alloc.then_some(s.addr);
                let rest = format!("{}  {}  file {:#x}+{:#x}", s.name, s.flags(), s.file_off, s.file_size);
                (
                    line(at, "section", Some(s.size), &rest),
                    json!({"kind": "section", "addr": s.addr, "size": s.size, "name": s.name, "flags": s.flags(),
                        "file_off": s.file_off, "file_size": s.file_size}),
                )
            }
            Entry::Got(i) | Entry::Bind(i) => {
                let imp = &img.imports[i];
                let slot = imp.slot.unwrap_or(0);
                let (lib, _) = self.lib_of(i);
                let kind = match (e, img.format) {
                    (Entry::Bind(_), _) => "bind",
                    (_, Format::Pe) => "iat",
                    _ => "got",
                };
                let name = slot_name(img, imp);
                let mut s = format!("{name}  slot of {}", imp.name);
                if let Some(l) = &lib {
                    s += &format!(" ({l})");
                }
                if let Some(f) = self.stubs.get(&i) {
                    s += &format!("  stub {:#x} {}", f.start, f.name);
                }
                (
                    line(Some(slot), kind, Some(img.ptr_size() as u64), &s),
                    json!({"kind": kind, "addr": slot, "size": img.ptr_size(), "name": name, "import": imp.name,
                        "lib": lib, "stub": self.stubs.get(&i).map(|f| f.start)}),
                )
            }
            Entry::Rebase(i) => {
                let (slot, v) = img.rel_ptrs[i];
                let to = describe(img, v, false).map_or_else(String::new, |d| format!(" ({d})"));
                (
                    line(Some(slot), "rebase", Some(img.ptr_size() as u64), &format!("-> {v:#x}{to}")),
                    json!({"kind": "rebase", "addr": slot, "size": img.ptr_size(), "target": v}),
                )
            }
            Entry::Reloc(i) => {
                let r = &self.t.relocs[i];
                let ty = reloc_type(img, r.ty);
                let sym = r.sym.and_then(|s| self.t.dynsym.get(s as usize)).filter(|s| !s.is_empty());
                let value = reloc_value(img, r);
                let mut s = ty.clone();
                if let Some(n) = sym {
                    s += &format!("  {n}");
                    if r.addend != 0 {
                        s += &format!("{:+#x}", r.addend);
                    }
                }
                if let Some(v) = value {
                    s += &format!("  -> {v:#x}");
                    if let Some(d) = describe(img, v, false) {
                        s += &format!(" ({d})");
                    }
                }
                if let Some(&k) = img.slots.get(&r.at) {
                    s += &format!("  [{}]", slot_name(img, &img.imports[k]));
                }
                (
                    line(Some(r.at), "reloc", Some(u64::from(r.size)), &s),
                    json!({"kind": "reloc", "addr": r.at, "size": r.size, "type": ty, "symbol": sym, "addend": r.addend,
                        "target": value}),
                )
            }
        };
        Row { key: 0, text, json: Some(j) }
    }
}

/// The rows of `syms` for `img`: the kinds `asked` names (see [`SYM_KIND_WORDS`]), or the needed libraries,
/// imports, exports and symbols when none is named. Only the rows a page of `budget` lines from `from` shows are
/// built.
pub fn syms_window(img: &Image, asked: &[String], from: Option<u64>, budget: usize) -> Result<Output> {
    let kinds = kinds(asked)?;
    let has = |k: &str| kinds.contains(&k);
    let t = syms_read::read(img, has("reloc"));
    // the stubs come from the decoded code; a stub is named after the import it jumps for
    let funcs = img.funcs();
    let by_name: HashMap<&str, usize> = img.imports.iter().enumerate().map(|(i, m)| (m.name.as_str(), i)).collect();
    let mut stubs = HashMap::new();
    for f in funcs {
        let key = if img.format == Format::Pe { Some(f.name.as_str()) } else { f.name.strip_suffix("@plt") };
        if let Some(&i) = key.and_then(|k| by_name.get(k)) {
            if Some(f.start) != img.imports[i].slot {
                stubs.entry(i).or_insert(f);
            }
        }
    }
    // the symbol table and the dynamic one name many of the same places
    let mut seen = HashSet::new();
    let mut syms: Vec<usize> =
        (0..img.symbols.len()).filter(|&i| seen.insert((img.symbols[i].addr, img.symbols[i].name.as_str()))).collect();
    syms.sort_by_key(|&i| (img.symbols[i].addr, !img.symbols[i].func));
    let n_func = syms.iter().filter(|&&i| img.symbols[i].func).count();
    let n_data = syms.len() - n_func;
    let mut slots: Vec<usize> = (0..img.imports.len()).filter(|&i| img.imports[i].slot.is_some()).collect();
    slots.sort_by_key(|&i| img.imports[i].slot);
    let macho = img.format == Format::MachO;
    let n_relocs = if macho { img.rel_ptrs.len() + slots.len() } else { t.reloc_count };
    let slot_word = if img.format == Format::Pe { "IAT" } else { "GOT" };
    let file = img.path.display().to_string();

    let mut e = Vec::new();
    let text = |e: &mut Vec<Entry>, s: String| e.push(Entry::Text(s));
    text(
        &mut e,
        format!(
            "syms {file}: {} {}; {} needed, {} imports, {} exports, {n_func} function symbols, {n_data} data symbols, \
             {} sections, {} {slot_word} slots, {n_relocs} relocations",
            img.format_name(),
            img.arch_name(),
            t.needed.len(),
            img.imports.len() + t.delay.len(),
            img.exports.len(),
            img.sections.len(),
            slots.len()
        ),
    );
    let others: Vec<&str> = SYM_KINDS.iter().copied().filter(|k| !has(k)).collect();
    let mut listed = format!("listed: {}", kinds.join(", "));
    if !others.is_empty() {
        listed += &format!("; also --kind {} or all (several: --kind import,got)", others.join(", "));
    }
    text(&mut e, listed);
    if n_func == 0 && (has("func") || has("data")) {
        text(
            &mut e,
            format!(
                "no function symbols (stripped); {} functions were found from the code: {}; main and entry work as \
                 places: {}",
                funcs.len(),
                command(img, "funcs", ""),
                command(img, "fn", "main")
            ),
        );
    }
    if let Some(s) = &t.soname {
        text(&mut e, format!("soname {s}"));
    }
    if let Some(s) = &t.search {
        text(&mut e, s.clone());
    }
    if img.format == Format::Elf && !t.dynamic {
        text(
            &mut e,
            "no dynamic segment: the program is statically linked, so it loads no library and imports nothing; the \
             library code it calls is inside the file"
                .into(),
        );
    }
    if img.format == Format::Elf
        && has("import")
        && img.imports.iter().any(|m| !t.versions.contains_key(written(&m.raw, &m.name)))
    {
        text(
            &mut e,
            "an ELF import names its library only through a symbol version; the loader looks for the others in the \
             needed libraries, in order"
                .into(),
        );
    }
    text(&mut e, format!("{:<10}  {:<7} {:>6}  name", "addr", "kind", "size"));
    let start = e.len();
    for k in &kinds {
        let before = e.len();
        match *k {
            "needed" => e.extend((0..t.needed.len()).map(Entry::Needed)),
            "import" => {
                e.extend((0..img.imports.len()).map(Entry::Import));
                e.extend((0..t.delay.len()).map(Entry::Delay));
            }
            "export" => e.extend((0..img.exports.len()).map(Entry::Export)),
            "func" => e.extend(syms.iter().filter(|&&i| img.symbols[i].func).map(|&i| Entry::Sym(i))),
            "data" => e.extend(syms.iter().filter(|&&i| !img.symbols[i].func).map(|&i| Entry::Sym(i))),
            "section" => e.extend((0..img.sections.len()).map(Entry::Section)),
            "got" => e.extend(slots.iter().map(|&i| Entry::Got(i))),
            "reloc" if macho => {
                e.extend((0..img.rel_ptrs.len()).map(Entry::Rebase));
                e.extend(slots.iter().map(|&i| Entry::Bind(i)));
            }
            "reloc" => e.extend((0..t.relocs.len()).map(Entry::Reloc)),
            _ => {}
        }
        if e.len() == before {
            text(&mut e, format!("{k}: none in this file"));
        }
    }
    if e.len() == start {
        text(&mut e, "nothing listed".into());
    }

    let rest = if asked.is_empty() { String::new() } else { format!("--kind {}", kinds.join(",")) };
    let meta = json!({
        "file": file, "format": img.format_name(), "arch": img.arch_name(), "kinds": kinds,
        "counts": {"needed": t.needed.len(), "import": img.imports.len() + t.delay.len(), "export": img.exports.len(),
            "func": n_func, "data": n_data, "section": img.sections.len(), "got": slots.len(), "reloc": n_relocs},
        "soname": t.soname, "search": t.search,
    });
    let ctx = Ctx { img, t, stubs };
    let (rows, tail) = cut(&e, from, budget, |x| ctx.render(x));
    Ok(Output { op: "syms", meta, rows, tail, command: command(img, "syms", &rest) })
}

/// `syms` for a WebAssembly module: its imports (module and kind), exports and named functions.
pub(crate) fn wasm_syms(
    m: &crate::wasm::Module<'_>,
    path: &str,
    asked: &[String],
    from: Option<u64>,
    budget: usize,
) -> Result<Output> {
    // a module has no libraries, data symbols or slots, so by default only what it has is listed
    let kinds = if asked.is_empty() { vec!["import", "export", "func"] } else { kinds(asked)? };
    let has = |k: &str| kinds.contains(&k);
    let named: Vec<&crate::wasm::Func> = m.funcs.iter().filter(|f| f.body.is_some() && f.name.is_some()).collect();
    let mut rows: Vec<(String, Option<Value>)> = vec![(
        format!(
            "syms {path}: WebAssembly module; {} imports, {} exports, {} named functions of {}",
            m.imports.len(),
            m.exports.len(),
            named.len(),
            m.funcs.len()
        ),
        None,
    )];
    let others: Vec<&str> = ["import", "export", "func"].into_iter().filter(|k| !has(k)).collect();
    let mut listed = format!("listed: {}", kinds.join(", "));
    if !others.is_empty() {
        listed += &format!("; also --kind {}", others.join(", "));
    }
    rows.push((listed, None));
    rows.push((format!("{:<10}  {:<7} {:>6}  name", "offset", "kind", "size"), None));
    for k in &kinds {
        let before = rows.len();
        match *k {
            "import" => {
                // function imports come first in the function index space, in the import order
                let mut fi = 0u32;
                for imp in &m.imports {
                    let index = (imp.kind == "func").then(|| {
                        fi += 1;
                        fi - 1
                    });
                    let at = index.map_or_else(String::new, |i| format!("  func[{i}]"));
                    rows.push((
                        line(None, "import", None, &format!("{}  {}  {}{at}", imp.name, imp.module, imp.kind)),
                        Some(json!({"kind": "import", "name": imp.name, "lib": imp.module, "what": imp.kind,
                            "index": index})),
                    ));
                }
            }
            "export" => {
                for x in &m.exports {
                    rows.push((
                        line(None, "export", None, &format!("{}  {}[{}]", x.name, x.kind, x.index)),
                        Some(json!({"kind": "export", "name": x.name, "what": x.kind, "index": x.index})),
                    ));
                }
            }
            "func" => {
                for f in &named {
                    let b = f.body.clone().unwrap_or_default();
                    let name = f.name.as_deref().unwrap_or("");
                    rows.push((
                        line(Some(b.start as u64), "func", Some(b.len() as u64), &format!("{name}  func[{}]", f.index)),
                        Some(json!({"kind": "func", "addr": b.start, "size": b.len(), "name": name, "index": f.index})),
                    ));
                }
            }
            _ => {}
        }
        if rows.len() == before {
            let what =
                if ["import", "export", "func"].contains(k) { "none in this module" } else { "not in WebAssembly" };
            rows.push((format!("{k}: {what}"), None));
        }
    }
    let rest = if asked.is_empty() { String::new() } else { format!(" --kind {}", kinds.join(",")) };
    let meta = json!({"file": path, "format": "wasm", "kinds": kinds,
        "counts": {"import": m.imports.len(), "export": m.exports.len(), "func": named.len()}});
    let (rows, tail) = cut(&rows, from, budget, |(t, j)| Row { key: 0, text: t.clone(), json: j.clone() });
    Ok(Output {
        op: "syms",
        meta,
        rows,
        tail,
        command: format!("nexo bin syms {}{rest}", crate::page::shell_word(path)),
    })
}
