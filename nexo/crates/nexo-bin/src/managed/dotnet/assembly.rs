//! Reading a whole assembly: type definitions, strings and facts.

use super::*;

/// TypeDef visibility (ECMA-335 II.23.1.15): the low three flag bits tell top-level from nested.
pub(super) fn type_vis(tf: u32) -> &'static str {
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

pub(super) fn quote(s: &str) -> String {
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
