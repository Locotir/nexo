//! `overview`: the map of a file in one bounded answer. Format, hardening, the program's real main with
//! its evidence, imports by capability, the most referenced strings, the most called functions, and the
//! next commands worth running, with addresses.

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::caps;
use crate::mainfind::{find_main, MainHit};
use crate::ops::command;
use crate::page::{cut, shell_word, Output, Row};
use crate::refs::Kind;
use crate::strings::shorten;
use crate::{Image, Result};

/// Rows of each list (the rest is one command away).
const TOP: usize = 8;
/// Imports named in a capability group before "and N more".
const PER_GROUP: usize = 8;
/// Longest string shown (characters).
const SHOWN: usize = 56;

struct Sec {
    title: String,
    lines: Vec<String>,
}

/// How many places call or use an import: its PLT stub (ELF) and its slot (the table the loader fills).
fn import_sites(img: &Image, stubs: &HashMap<&str, u64>, name: &str, slot: Option<u64>) -> usize {
    let refs = img.refs();
    let mut n = 0;
    let used = |k: Kind| matches!(k, Kind::Call | Kind::Jump | Kind::Read | Kind::Addr);
    if let Some(&s) = stubs.get(name) {
        n += refs.at(s).iter().filter(|r| matches!(r.kind, Kind::Call | Kind::Jump | Kind::Addr)).count();
    }
    if let Some(s) = slot {
        // the stub's own jump through the slot is not a use by the program
        n += refs.at(s).iter().filter(|r| used(r.kind) && !is_stub_body(img, r.src)).count();
    }
    n
}

fn is_stub_body(img: &Image, src: u64) -> bool {
    img.func_at(src).is_some_and(|f| f.name.ends_with("@plt"))
}

/// The map of the file.
pub fn overview(img: &Image) -> Result<Output> {
    overview_window(img, None, usize::MAX)
}

/// Like [`overview`], cut to a page.
pub fn overview_window(img: &Image, from: Option<u64>, budget: usize) -> Result<Output> {
    let refs = img.refs();
    let funcs = img.funcs();
    let strings = img.strings();
    let file = shell_word(&img.path.display().to_string()) + &crate::macho::arch_flag(img);
    let mut secs: Vec<Sec> = Vec::new();
    let mut next: Vec<String> = Vec::new();

    // the file
    let hard: Vec<String> = img.hardening.items.iter().map(|(k, v)| format!("{}: {v}", k.replace('_', " "))).collect();
    let mut head = Sec { title: String::new(), lines: Vec::new() };
    head.lines.push(format!(
        "overview {}  {} {}, {} bytes, sha256 {}",
        img.path.display(),
        img.format_name(),
        img.arch_name(),
        img.data.len(),
        &img.sha256[..16.min(img.sha256.len())]
    ));
    head.lines.push(format!("hardening   {}", hard.join(" | ")));
    let entry_name = img.label(img.entry).unwrap_or_else(|| "?".into());
    head.lines.push(format!(
        "entry       {:#x} ({entry_name}); {} functions, {} imports, {} exports, {} strings, {} symbols",
        img.entry,
        funcs.len(),
        img.imports.len(),
        img.exports.len(),
        strings.len(),
        img.symbols.len()
    ));
    if img.symbols.is_empty() && img.dwarf().protos.is_empty() {
        head.lines.push("symbols     none: the file is stripped, functions are sub_<address>".into());
    }
    secs.push(head);

    // main
    let mains = find_main(img);
    let mut m = Sec { title: String::new(), lines: Vec::new() };
    for h in mains.iter().take(3) {
        m.lines.push(main_line(img, h));
    }
    if mains.len() > 3 {
        m.lines.push(format!("  ... {} more candidates", mains.len() - 3));
    }
    if mains.is_empty() {
        m.lines.push(format!(
            "main        not found: no call to __libc_start_main, std::rt::lang_start or a Windows C runtime \
             start was recognised from the entry {:#x}; start at the entry: nexo bin decomp {file} {:#x}",
            img.entry, img.entry
        ));
    }
    secs.push(m);

    // imports by capability
    let stubs: HashMap<&str, u64> =
        funcs.iter().filter_map(|f| f.name.strip_suffix("@plt").map(|n| (n, f.start))).collect();
    let mut groups: Vec<(&'static str, Vec<(String, usize)>)> = caps::group_names().map(|g| (g, Vec::new())).collect();
    let mut other = 0usize;
    let mut seen_names: Vec<&str> = Vec::new();
    for imp in &img.imports {
        if seen_names.contains(&imp.name.as_str()) {
            continue;
        }
        seen_names.push(&imp.name);
        match caps::group_of(&imp.name) {
            Some(g) => {
                let sites = import_sites(img, &stubs, &imp.name, imp.slot);
                if let Some(e) = groups.iter_mut().find(|e| e.0 == g) {
                    e.1.push((imp.name.split('@').next().unwrap_or(&imp.name).to_string(), sites));
                }
            }
            None => other += 1,
        }
    }
    let classified: usize = groups.iter().map(|g| g.1.len()).sum();
    let mut imps = Sec {
        title: format!("imports by capability ({classified} of {} named by what they do)", seen_names.len()),
        lines: Vec::new(),
    };
    for (g, list) in groups.iter_mut() {
        if list.is_empty() {
            continue;
        }
        list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let shown: Vec<String> = list.iter().take(PER_GROUP).map(|(n, s)| format!("{n}({s})")).collect();
        let more = if list.len() > PER_GROUP { format!(" and {} more", list.len() - PER_GROUP) } else { String::new() };
        imps.lines.push(format!("  {g:<16}{}{more}", shown.join(" ")));
    }
    if classified == 0 {
        imps.lines.push("  none of the imports is a network, file, process, crypto, registry or loading call".into());
    }
    imps.lines.push(format!(
        "  ({other} other imports; the number in () is the places that use the import; list all: nexo bin xref {file} <name>)"
    ));
    if groups.iter().any(|g| g.0 == "dynamic loading" && !g.1.is_empty()) {
        imps.lines.push("  note: dynamic loading is used, so some calls are not in the import table".into());
    }
    let first_in = |g: &str| groups.iter().find(|e| e.0 == g).and_then(|e| e.1.first()).map(|x| x.0.clone());
    for g in ["network", "processes", "crypto"] {
        if let Some(n) = first_in(g) {
            next.push(format!("who uses {n}: nexo bin xref {file} {}", shell_word(&n)));
            break;
        }
    }
    secs.push(imps);

    // strings
    let mut ranked: Vec<(usize, usize)> = Vec::new();
    for (i, s) in strings.iter().enumerate() {
        let n = refs.within(s.addr, s.byte_len).iter().filter(|r| r.kind != Kind::Ptr).count();
        if n > 0 {
            ranked.push((n, i));
        }
    }
    ranked.sort_by_key(|&(n, i)| (std::cmp::Reverse(n), i));
    let mut st = Sec {
        title: format!("strings used most ({} of {} are referenced by code)", ranked.len(), strings.len()),
        lines: Vec::new(),
    };
    for &(n, i) in ranked.iter().take(TOP) {
        let s = &strings[i];
        let w = if s.wide { "L" } else { "" };
        st.lines.push(format!("  {:#010x}  {n:>3} refs  {w}\"{}\"", s.addr, shorten(&s.text, SHOWN)));
    }
    if ranked.is_empty() {
        st.lines.push("  none".into());
    }
    if ranked.len() > TOP {
        st.lines.push(format!("  ... all: nexo bin strings {file} --sort refs"));
    }
    if let Some(&(_, i)) = ranked.first() {
        next.push(format!("who uses the top string: nexo bin xref {file} {:#x}", strings[i].addr));
    }
    secs.push(st);

    // functions
    let mut called: Vec<(usize, usize)> = funcs
        .iter()
        .enumerate()
        .filter(|(_, f)| !is_boilerplate(img, f))
        .map(|(i, f)| (refs.callers(f.start), i))
        .filter(|&(c, _)| c > 0)
        .collect();
    called.sort_by_key(|&(c, i)| (std::cmp::Reverse(c), i));
    let mut fs = Sec { title: format!("functions called most ({} functions in all)", funcs.len()), lines: Vec::new() };
    for &(c, i) in called.iter().take(TOP) {
        let f = &funcs[i];
        let tag = img.sig_label(f.start).map(|s| format!("  ; {s}")).unwrap_or_default();
        fs.lines.push(format!("  {:#010x}  {c:>3} callers  {:>6} bytes  {}{tag}", f.start, f.size(), img.func_name(f)));
    }
    if called.is_empty() {
        fs.lines.push("  none is called directly".into());
    }
    if let Some(&(_, i)) = called.first() {
        next.push(format!("who calls the most called: nexo bin calls {file} {:#x}", funcs[i].start));
    }
    secs.push(fs);

    // next commands
    let mut suggestions: Vec<String> = Vec::new();
    if let Some(h) = mains.first() {
        suggestions.push(format!("read the {}: nexo bin decomp {file} {:#x}", h.role, h.addr));
        suggestions.push(format!("what the {} calls: nexo bin calls {file} {:#x}", h.role, h.addr));
        suggestions.push(format!("the call tree under the {}: nexo bin tree {file} {:#x}", h.role, h.addr));
    }
    suggestions.extend(next);
    suggestions.push(format!("biggest functions: nexo bin funcs {file} --sort size"));
    suggestions.truncate(6);
    let mut nx = Sec { title: "next".into(), lines: Vec::new() };
    nx.lines.extend(suggestions.iter().map(|s| format!("  {s}")));
    if let Some(l) = crate::xops::indirect_line(img) {
        nx.lines.push(format!("  note: {l}, so the call counts above are lower bounds"));
    }
    secs.push(nx);

    // rows
    let mut items: Vec<String> = Vec::new();
    for s in &secs {
        if !s.title.is_empty() {
            items.push(s.title.clone());
        }
        items.extend(s.lines.iter().cloned());
    }
    let (rows, tail) = cut(&items, from, budget, |t| Row { key: 0, text: t.clone(), json: None });
    let meta = meta_json(img, &mains, &groups, other, &ranked, &called, &suggestions);
    Ok(Output { op: "overview", meta, rows, tail, command: command(img, "overview", "") })
}

fn main_line(img: &Image, h: &MainHit) -> String {
    let name = img.func_at(h.addr).map(|f| img.func_name(f).to_string()).unwrap_or_else(|| "?".into());
    format!("{:<11} {:#x} {name}  [{}] {}", h.role, h.addr, h.strength, h.evidence)
}

fn meta_json(
    img: &Image,
    mains: &[MainHit],
    groups: &[(&'static str, Vec<(String, usize)>)],
    other: usize,
    strings: &[(usize, usize)],
    funcs: &[(usize, usize)],
    next: &[String],
) -> Value {
    let hardening: serde_json::Map<String, Value> =
        img.hardening.items.iter().map(|(k, v)| ((*k).to_string(), json!(v))).collect();
    let imports: serde_json::Map<String, Value> = groups
        .iter()
        .filter(|g| !g.1.is_empty())
        .map(|(g, l)| {
            ((*g).to_string(), json!(l.iter().map(|(n, s)| json!({"name": n, "sites": s})).collect::<Vec<_>>()))
        })
        .collect();
    json!({
        "file": img.path.display().to_string(), "format": img.format_name(), "arch": img.arch_name(),
        "size": img.data.len(), "sha256": img.sha256, "entry": img.entry, "hardening": hardening,
        "main": mains.iter().map(|h| json!({"addr": h.addr, "role": h.role, "strength": h.strength, "evidence": h.evidence})).collect::<Vec<_>>(),
        "imports": imports, "imports_other": other,
        "top_strings": strings.iter().take(TOP).map(|&(n, i)| {
            let s = &img.strings()[i];
            json!({"addr": s.addr, "refs": n, "text": shorten(&s.text, 200)})
        }).collect::<Vec<_>>(),
        "top_functions": funcs.iter().take(TOP).map(|&(c, i)| {
            let f = &img.funcs()[i];
            json!({"addr": f.start, "callers": c, "size": f.size(), "name": img.func_name(f)})
        }).collect::<Vec<_>>(),
        "next": next,
    })
}

/// Linker and C runtime pieces that every program has: PLT stubs and the start-up helpers.
fn is_boilerplate(img: &Image, f: &crate::funcs::Func) -> bool {
    const CRT: [&str; 9] = [
        "deregister_tm_clones",
        "register_tm_clones",
        "frame_dummy",
        "__do_global_dtors_aux",
        "_init",
        "_fini",
        "_start",
        "__libc_csu_init",
        "__libc_csu_fini",
    ];
    f.name.ends_with("@plt")
        || CRT.contains(&f.name.as_str())
        || img.section_at(f.start).is_some_and(|s| s.name.starts_with(".plt"))
}
