//! Managed bytecode: JVM `.class` and `.jar`, and (in `dotnet`) .NET assemblies. Both are read into one
//! small model (types, members, methods with disassembly, strings) that the questions below answer
//! in bounded pieces, in the same page format as the native ops.

mod jvm;
#[cfg(any(test, feature = "fixtures"))]
pub mod testkit;
#[cfg(test)]
mod tests_jvm;
mod zip;

use crate::page::{cut, shell_word, Output, Row};
use crate::{Error, Result};
use serde_json::json;

#[derive(Default, Clone)]
pub(crate) struct Insn {
    pub off: u32,
    pub text: String,
    /// `Owner.name` plus the raw descriptor in parentheses, for calls.
    pub callee: Option<String>,
    pub line: Option<u32>,
    /// More lines that belong to this instruction (switch cases).
    pub cont: Vec<String>,
}

#[derive(Default)]
pub(crate) struct Code {
    pub size: usize,
    pub info: String,
    pub insns: Vec<Insn>,
    pub handlers: Vec<String>,
    /// Set when decoding stopped or a table was damaged: said in the answer, never hidden.
    pub note: Option<String>,
}

pub(crate) struct FieldDef {
    pub name: String,
    pub ty: String,
    pub raw: String,
    pub flags: String,
    pub constant: Option<String>,
}

pub(crate) struct MethodDef {
    pub name: String,
    pub sig: String,
    pub raw: String,
    pub flags: String,
    /// `Type.name` plus the raw descriptor: what selects exactly this method.
    pub key: String,
    pub code: Option<Code>,
}

pub(crate) struct TypeDef {
    pub name: String,
    pub kind: String,
    pub flags: String,
    pub extends: Option<String>,
    pub implements: Vec<String>,
    pub fields: Vec<FieldDef>,
    pub methods: Vec<MethodDef>,
    /// The archive entry it came from (empty for a lone file).
    pub origin: String,
}

pub(crate) struct Module {
    /// "JVM class", "JAR", ".NET assembly".
    pub kind: String,
    pub file: String,
    pub types: Vec<TypeDef>,
    /// `(origin, text)`.
    pub strings: Vec<(String, String)>,
    /// Facts worth a header line: main class, version, entries skipped and why.
    pub facts: Vec<String>,
}

/// What to ask of a file.
#[derive(Debug, Clone, Default)]
pub struct ManagedQuery<'a> {
    /// A type (`com.foo.Bar` or `Bar`) lists its members; a method (`Bar.run`, `Bar.run(I)V`) disassembles.
    pub target: Option<&'a str>,
    /// List the strings (constant-pool strings and the ones code loads).
    pub strings: bool,
    /// List every call site of this method (`Owner.name`, with or without the descriptor).
    pub callers: Option<&'a str>,
    /// A regular expression filtering types, members or strings.
    pub grep: Option<&'a str>,
}

/// True when the first bytes say the file is managed bytecode this module reads.
pub fn is_managed(head: &[u8]) -> bool {
    is_class(head) || zip::looks_like_zip(head)
}

/// A class file and a Mach-O universal binary share the CAFEBABE magic: the next four bytes are the
/// class version (45 or more) or the architecture count (a handful).
fn is_class(d: &[u8]) -> bool {
    d.starts_with(&[0xCA, 0xFE, 0xBA, 0xBE])
        && d.get(4..8).is_some_and(|v| u32::from_be_bytes([v[0], v[1], v[2], v[3]]) >= 45)
}

fn text_row(t: impl Into<String>) -> Row {
    Row { key: 0, text: t.into(), json: None }
}

pub(crate) fn load(path: &std::path::Path, data: &[u8]) -> Result<Module> {
    let file = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    if is_class(data) {
        let p = jvm::parse(data).map_err(|e| Error::NotRead(format!("{file}: not a readable class file: {e}")))?;
        let mut facts = vec![version_fact(p.major)];
        facts.retain(|f| !f.is_empty());
        return Ok(Module { kind: "JVM class".into(), file, types: vec![p.ty], strings: p.strings, facts });
    }
    if zip::looks_like_zip(data) {
        return load_jar(file, data);
    }
    Err(Error::NotRead(format!("{file} is not a .class, .jar or .NET assembly")))
}

fn version_fact(major: u16) -> String {
    if major >= 45 {
        format!("class version {major} (Java {})", if major >= 49 { major - 44 } else { 1 })
    } else {
        String::new()
    }
}

fn load_jar(file: String, data: &[u8]) -> Result<Module> {
    let z = zip::Zip::open(data)?;
    let mut m = Module { kind: "JAR".into(), file, types: vec![], strings: vec![], facts: vec![] };
    let (mut others, mut multi, mut max_major) = (0usize, 0usize, 0u16);
    let mut bad: Vec<String> = Vec::new();
    for e in &z.entries {
        if e.name.ends_with('/') {
            continue;
        }
        if e.name == "META-INF/MANIFEST.MF" {
            if let Ok(b) = z.read(e) {
                let t = String::from_utf8_lossy(&b).replace("\r\n ", "").replace("\n ", "");
                if let Some(l) = t.lines().find_map(|l| l.strip_prefix("Main-Class:")) {
                    m.facts.push(format!("Main-Class {}", l.trim()));
                }
            }
            others += 1;
            continue;
        }
        if !e.name.ends_with(".class") {
            others += 1;
            continue;
        }
        if e.name.starts_with("META-INF/versions/") {
            multi += 1;
            continue;
        }
        let parsed = z.read(e).and_then(|b| jvm::parse(&b));
        match parsed {
            Ok(mut p) => {
                p.ty.origin = e.name.clone();
                max_major = max_major.max(p.major);
                m.strings.extend(p.strings);
                m.types.push(p.ty);
            }
            Err(why) => bad.push(format!("{}: {why}", e.name)),
        }
    }
    m.facts.push(version_fact(max_major));
    m.facts.retain(|f| !f.is_empty());
    if others > 0 {
        m.facts.push(format!("{others} non-class entries not listed"));
    }
    if multi > 0 {
        m.facts.push(format!("{multi} classes under META-INF/versions/ not read (multi-release overrides)"));
    }
    if !bad.is_empty() {
        let shown: Vec<_> = bad.iter().take(3).cloned().collect();
        m.facts.push(format!(
            "{} classes could not be read: {}{}",
            bad.len(),
            shown.join("; "),
            if bad.len() > 3 { "; ..." } else { "" }
        ));
    }
    if m.types.is_empty() {
        return Err(Error::NotRead(format!("{} holds no .class files this reads", m.file)));
    }
    Ok(m)
}

fn matches_name(full: &str, want: &str) -> bool {
    full == want || full.strip_suffix(want).is_some_and(|p| p.ends_with('.') || p.ends_with('$'))
}

/// Answer one question about a `.class`, `.jar` or .NET assembly.
pub fn managed_window(
    path: &std::path::Path,
    data: &[u8],
    q: &ManagedQuery<'_>,
    from: Option<u64>,
    budget: usize,
) -> Result<Output> {
    let m = load(path, data)?;
    let re =
        q.grep.map(regex::Regex::new).transpose().map_err(|e| Error::Bad(format!("bad regular expression: {e}")))?;
    let mut base = format!("nexo bin managed {}", shell_word(&path.display().to_string()));
    let (rows, tail, op, meta);
    if let Some(t) = q.callers {
        base.push_str(&format!(" --callers {}", shell_word(t)));
        let mut sites: Vec<(String, String, String)> = Vec::new();
        for ty in &m.types {
            for me in &ty.methods {
                for i in me.code.iter().flat_map(|c| &c.insns) {
                    if i.callee.as_deref().is_some_and(|c| callee_matches(c, t)) {
                        sites.push((me.key.clone(), format!("{:#06x}", i.off), i.text.clone()));
                    }
                }
            }
        }
        let n = sites.len();
        let (r, tl) = cut(&sites, from, budget, |(k, off, text)| Row {
            key: 0,
            text: format!("{k} +{off}  {text}"),
            json: Some(json!({"method": k, "offset": off, "insn": text})),
        });
        (rows, tail, op, meta) = (r, tl, "managed-callers", json!({"target": t, "call_sites": n}));
        let mut rows = rows;
        if n == 0 {
            rows.push(text_row(format!(
                "no call to {t} in {}: give Owner.name (like java.io.PrintStream.println); the callees of a method are in its disassembly",
                m.file
            )));
        } else {
            rows.insert(0, text_row(format!("{n} call sites of {t}")));
        }
        return Ok(Output { op, meta, rows, tail, command: base });
    }
    if q.strings {
        base.push_str(" --strings");
        if let Some(g) = q.grep {
            base.push_str(&format!(" --grep {}", shell_word(g)));
        }
        let list: Vec<&(String, String)> =
            m.strings.iter().filter(|(_, s)| re.as_ref().is_none_or(|r| r.is_match(s))).collect();
        let n = list.len();
        let (r, tl) = cut(&list, from, budget, |(o, s)| Row {
            key: 0,
            text: format!("{o}  {}", quote(s)),
            json: Some(json!({"origin": o, "text": s})),
        });
        let mut rows = r;
        rows.insert(
            0,
            text_row(format!("{n} strings (origin = where code loads it; the type alone = in the constant pool only)")),
        );
        return Ok(Output { op: "managed-strings", meta: json!({"strings": n}), rows, tail: tl, command: base });
    }
    if let Some(t) = q.target {
        base.push_str(&format!(" {}", shell_word(t)));
        return target_view(&m, t, re.as_ref(), base, from, budget);
    }
    // overview: the types
    if let Some(g) = q.grep {
        base.push_str(&format!(" --grep {}", shell_word(g)));
    }
    let types: Vec<&TypeDef> = m.types.iter().filter(|t| re.as_ref().is_none_or(|r| r.is_match(&t.name))).collect();
    let (nm, nf) =
        (m.types.iter().map(|t| t.methods.len()).sum::<usize>(), m.types.iter().map(|t| t.fields.len()).sum::<usize>());
    let mut head = format!(
        "{} {}: {} types, {nm} methods, {nf} fields, {} strings",
        m.kind,
        m.file,
        m.types.len(),
        m.strings.len()
    );
    for f in &m.facts {
        head.push_str(&format!("; {f}"));
    }
    let (r, tl) = cut(&types, from, budget, |t| Row {
        key: 0,
        text: format!(
            "{} {}  {}m {}f{}",
            t.kind,
            t.name,
            t.methods.len(),
            t.fields.len(),
            t.extends
                .as_deref()
                .filter(|e| *e != "java.lang.Object")
                .map_or(String::new(), |e| format!("  extends {e}"))
        ),
        json: Some(
            json!({"name": t.name, "kind": t.kind, "methods": t.methods.len(), "fields": t.fields.len(), "extends": t.extends, "origin": t.origin}),
        ),
    });
    let mut rows = r;
    rows.insert(0, text_row(head));
    rows.insert(
        1,
        text_row(format!(
            "next: {base} TYPE (members) | TYPE.method (disassembly) | --strings | --callers Owner.name | --grep RE"
        )),
    );
    if types.is_empty() {
        rows.push(text_row(format!("no type matches {}", q.grep.unwrap_or(""))));
    }
    Ok(Output {
        op: "managed",
        meta: json!({"kind": m.kind, "types": m.types.len(), "methods": nm, "fields": nf, "facts": m.facts}),
        rows,
        tail: tl,
        command: base,
    })
}

fn quote(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars().take(200) {
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
    if s.chars().count() > 200 {
        o.push_str(&format!("...({} chars)", s.chars().count()));
    }
    o.push('"');
    o
}

fn callee_matches(callee: &str, want: &str) -> bool {
    if want.contains('(') {
        return callee == want || matches_name(callee, want);
    }
    let bare = callee.split('(').next().unwrap_or(callee);
    matches_name(bare, want)
}

fn target_view(
    m: &Module,
    t: &str,
    re: Option<&regex::Regex>,
    base: String,
    from: Option<u64>,
    budget: usize,
) -> Result<Output> {
    let t = t.replace('/', ".");
    // 1. a type by exact or short name
    let exact: Vec<&TypeDef> = m.types.iter().filter(|x| x.name == t).collect();
    let by_type: Vec<&TypeDef> =
        if exact.is_empty() { m.types.iter().filter(|x| matches_name(&x.name, &t)).collect() } else { exact };
    // 2. a method: Type.name or Type.name(desc)
    let (bare, sig) = match t.find('(') {
        Some(i) => (&t[..i], Some(&t[i..])),
        None => (t.as_str(), None),
    };
    let mut meths: Vec<(&TypeDef, &MethodDef)> = Vec::new();
    for ty in &m.types {
        for me in &ty.methods {
            let full = format!("{}.{}", ty.name, me.name);
            let hit = matches_name(&full, bare);
            if hit && sig.is_none_or(|s| me.raw == s) {
                meths.push((ty, me));
            }
        }
    }
    if sig.is_none() && by_type.len() == 1 {
        return type_view(m, by_type[0], re, base, from, budget);
    }
    match meths.len() {
        1 => method_view(m, meths[0].0, meths[0].1, base, from, budget),
        0 if by_type.len() == 1 => type_view(m, by_type[0], re, base, from, budget),
        0 if by_type.len() > 1 => Err(Error::Bad(format!(
            "{t} names {} types: {}; write the full name",
            by_type.len(),
            by_type.iter().take(8).map(|x| x.name.as_str()).collect::<Vec<_>>().join(", ")
        ))),
        0 => Err(Error::Bad(format!(
            "no type or method named {t} in {}: list the types with `{}` (--grep RE to filter), methods as Type.name",
            m.file,
            base.rsplit_once(' ').map_or(base.as_str(), |(b, _)| b)
        ))),
        n => Err(Error::Bad(format!(
            "{t} names {n} methods; pick one: {}{}",
            meths.iter().take(8).map(|(_, me)| me.key.as_str()).collect::<Vec<_>>().join(" | "),
            if n > 8 { " | ..." } else { "" }
        ))),
    }
}

fn type_view(
    m: &Module,
    ty: &TypeDef,
    re: Option<&regex::Regex>,
    base: String,
    from: Option<u64>,
    budget: usize,
) -> Result<Output> {
    enum Item<'a> {
        F(&'a FieldDef),
        M(&'a MethodDef),
    }
    let items: Vec<Item> = ty
        .fields
        .iter()
        .map(Item::F)
        .chain(ty.methods.iter().map(Item::M))
        .filter(|i| {
            re.is_none_or(|r| match i {
                Item::F(f) => r.is_match(&f.name),
                Item::M(me) => r.is_match(&me.name),
            })
        })
        .collect();
    let (r, tail) = cut(&items, from, budget, |i| match i {
        Item::F(f) => Row {
            key: 0,
            text: format!(
                "field  {}{}: {}{}",
                if f.flags.is_empty() { String::new() } else { format!("{} ", f.flags) },
                f.name,
                f.ty,
                f.constant.as_ref().map_or(String::new(), |c| format!(" = {c}"))
            ),
            json: Some(
                json!({"kind": "field", "name": f.name, "type": f.ty, "desc": f.raw, "flags": f.flags, "const": f.constant}),
            ),
        },
        Item::M(me) => Row {
            key: 0,
            text: format!(
                "method {} {}{}{}",
                me.flags,
                me.name,
                me.sig,
                match &me.code {
                    Some(c) => format!("  {} insns, {} bytes", c.insns.len(), c.size),
                    None => "  (no code)".to_string(),
                }
            ),
            json: Some(
                json!({"kind": "method", "key": me.key, "name": me.name, "sig": me.sig, "desc": me.raw, "flags": me.flags, "bytes": me.code.as_ref().map(|c| c.size)}),
            ),
        },
    });
    let mut rows = r;
    let mut head = format!("{} {} [{}]", ty.kind, ty.name, ty.flags);
    if let Some(e) = &ty.extends {
        head.push_str(&format!(" extends {e}"));
    }
    if !ty.implements.is_empty() {
        head.push_str(&format!(" implements {}", ty.implements.join(", ")));
    }
    if !ty.origin.is_empty() {
        head.push_str(&format!("  ({})", ty.origin));
    }
    rows.insert(0, text_row(head));
    rows.insert(
        1,
        text_row(format!(
            "{} fields, {} methods; disassemble with `{} {}.METHOD` (overloads: add the descriptor)",
            ty.fields.len(),
            ty.methods.len(),
            base.rsplit_once(' ').map_or(base.as_str(), |(b, _)| b),
            ty.name
        )),
    );
    let _ = m;
    Ok(Output {
        op: "managed-type",
        meta: json!({"type": ty.name, "fields": ty.fields.len(), "methods": ty.methods.len()}),
        rows,
        tail,
        command: base,
    })
}

fn method_view(
    m: &Module,
    ty: &TypeDef,
    me: &MethodDef,
    base: String,
    from: Option<u64>,
    budget: usize,
) -> Result<Output> {
    let _ = m;
    let Some(code) = &me.code else {
        return Ok(Output {
            op: "managed-method",
            meta: json!({"method": me.key}),
            rows: vec![text_row(format!(
                "{} {} {}{}  has no code ({})",
                ty.kind,
                ty.name,
                me.name,
                me.sig,
                if me.flags.contains("native") { "native" } else { "abstract" }
            ))],
            tail: 0,
            command: base,
        });
    };
    let mut calls: Vec<String> = Vec::new();
    for c in code.insns.iter().filter_map(|i| i.callee.as_deref()) {
        let b = c.split('(').next().unwrap_or(c).to_string();
        if !calls.contains(&b) {
            calls.push(b);
        }
    }
    let mut lines: Vec<(Option<u32>, String)> = Vec::new();
    let mut last_line = None;
    for i in &code.insns {
        let mut t = format!("{:#06x}  {}", i.off, i.text);
        if let Some(l) = i.line {
            if last_line != Some(l) {
                t.push_str(&format!("  ; line {l}"));
                last_line = Some(l);
            }
        }
        lines.push((Some(i.off), t));
        for c in &i.cont {
            lines.push((None, format!("          {c}")));
        }
    }
    let (r, tail) = cut(&lines, from, budget, |(off, t)| Row {
        key: 0,
        text: t.clone(),
        json: off.map(|o| json!({"offset": o, "text": t})),
    });
    let mut rows = r;
    let mut head_rows =
        vec![text_row(format!("{} [{}] {}  {} bytes, {}", me.key, me.flags, me.sig, code.size, code.info))];
    if !calls.is_empty() {
        let shown: Vec<&str> = calls.iter().take(12).map(String::as_str).collect();
        head_rows.push(text_row(format!(
            "calls {}: {}{}",
            calls.len(),
            shown.join(", "),
            if calls.len() > 12 {
                format!(", ... {} more (all are in the listing)", calls.len() - 12)
            } else {
                String::new()
            }
        )));
    }
    for h in &code.handlers {
        head_rows.push(text_row(h.clone()));
    }
    if let Some(n) = &code.note {
        head_rows.push(text_row(format!("WARNING {n}")));
    }
    head_rows.append(&mut rows);
    Ok(Output {
        op: "managed-method",
        meta: json!({"method": me.key, "bytes": code.size, "instructions": code.insns.len(), "calls": calls, "note": code.note}),
        rows: head_rows,
        tail,
        command: base,
    })
}
