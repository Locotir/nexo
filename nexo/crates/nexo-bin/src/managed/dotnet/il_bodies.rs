//! Method bodies: header, exception sections and IL decoding.

use super::*;

pub(super) fn decode_body(
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

pub(super) fn decode_sections(d: &[u8], mut p: usize, cx: &Ctx<'_>, c: &mut Code) {
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
