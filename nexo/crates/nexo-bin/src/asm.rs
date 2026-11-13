//! Small Intel syntax front end for the iced-x86 encoder. No external assembler is run.
use std::{collections::BTreeMap, sync::OnceLock};

use iced_x86::{Code, Encoder, Formatter, Instruction, OpCodeOperandKind as K, OpKind, Register};
use serde_json::json;

use crate::{disasm, ops::hex, parse_number, Error, Output, Result, Row};

const MAX_SOURCE: usize = 262144;

#[derive(Clone, Debug)]
enum Operand {
    Reg(Register),
    Imm(i128),
    Label(String),
    Mem { base: Register, index: Register, scale: u32, disp: i64, size: Option<usize> },
}

fn bad(text: impl Into<String>) -> Error {
    Error::Bad(format!("asm: {}", text.into()))
}

fn register(s: &str) -> Option<Register> {
    Register::values().find(|r| format!("{r:?}").eq_ignore_ascii_case(s) && *r != Register::None)
}

fn label(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .enumerate()
            .all(|(i, c)| c.is_ascii_alphabetic() || c == '_' || c == '.' || (i > 0 && c.is_ascii_digit()))
}

fn operand(s: &str) -> Result<Operand> {
    let s = s.trim().to_ascii_lowercase();
    if s.contains('[') {
        let (prefix, rest) = s.split_once('[').ok_or_else(|| bad("expected ["))?;
        let inner = rest.strip_suffix(']').ok_or_else(|| bad("expected ]"))?;
        let size = match prefix.trim().trim_end_matches("ptr").trim() {
            "" => None,
            "byte" => Some(1),
            "word" => Some(2),
            "dword" => Some(4),
            "qword" => Some(8),
            _ => return Err(bad("memory size must be byte, word, dword or qword")),
        };
        let (mut base, mut index, mut scale, mut disp) = (Register::None, Register::None, 1, 0i64);
        let expr = inner.replace(' ', "").replace('-', "+-");
        for term in expr.split('+').filter(|s| !s.is_empty()) {
            if let Some((r, n)) = term.split_once('*') {
                let r = register(r).ok_or_else(|| bad("invalid index register"))?;
                if index != Register::None || !matches!(n, "1" | "2" | "4" | "8") {
                    return Err(bad("invalid index or scale"));
                }
                index = r;
                scale = n.parse().map_err(|_| bad("invalid scale"))?;
            } else if let Some(r) = register(term) {
                if base == Register::None {
                    base = r;
                } else if index == Register::None {
                    index = r;
                } else {
                    return Err(bad("too many memory registers"));
                }
            } else {
                let n = i64::try_from(parse_number(term)?).map_err(|_| bad("displacement is too large"))?;
                disp = disp.checked_add(n).ok_or_else(|| bad("displacement overflow"))?;
            }
        }
        if inner.trim().is_empty() {
            return Err(bad("empty memory operand"));
        }
        return Ok(Operand::Mem { base, index, scale, disp, size });
    }
    if let Some(r) = register(&s) {
        return Ok(Operand::Reg(r));
    }
    if let Ok(n) = parse_number(&s) {
        return Ok(Operand::Imm(n));
    }
    if label(&s) {
        return Ok(Operand::Label(s));
    }
    Err(bad(format!("unsupported operand {s:?}")))
}

fn reg_matches(k: K, r: Register) -> bool {
    match k {
        K::r8_reg | K::r8_opcode | K::r8_or_mem => r.is_gpr8(),
        K::r16_reg | K::r16_opcode | K::r16_rm | K::r16_or_mem => r.is_gpr16(),
        K::r32_reg | K::r32_opcode | K::r32_rm | K::r32_or_mem | K::r32_vvvv => r.is_gpr32(),
        K::r64_reg | K::r64_opcode | K::r64_rm | K::r64_or_mem | K::r64_vvvv => r.is_gpr64(),
        K::xmm_reg | K::xmm_rm | K::xmm_or_mem | K::xmm_vvvv => r.is_xmm(),
        K::ymm_reg | K::ymm_rm | K::ymm_or_mem | K::ymm_vvvv => r.is_ymm(),
        K::zmm_reg | K::zmm_rm | K::zmm_or_mem | K::zmm_vvvv => r.is_zmm(),
        K::al => r == Register::AL,
        K::cl => r == Register::CL,
        K::ax => r == Register::AX,
        K::dx => r == Register::DX,
        K::eax => r == Register::EAX,
        K::rax => r == Register::RAX,
        _ => false,
    }
}

fn immediate(k: K, n: i128) -> Option<OpKind> {
    let (kind, min, max) = match k {
        K::imm8 => (OpKind::Immediate8, -128, 255),
        K::imm8_const_1 if n == 1 => (OpKind::Immediate8, 1, 1),
        K::imm8sex16 => (OpKind::Immediate8to16, -128, 127),
        K::imm8sex32 => (OpKind::Immediate8to32, -128, 127),
        K::imm8sex64 => (OpKind::Immediate8to64, -128, 127),
        K::imm16 => (OpKind::Immediate16, -32768, 65535),
        K::imm32 => (OpKind::Immediate32, i32::MIN as i128, u32::MAX as i128),
        K::imm32sex64 => (OpKind::Immediate32to64, i32::MIN as i128, i32::MAX as i128),
        K::imm64 => (OpKind::Immediate64, i64::MIN as i128, u64::MAX as i128),
        _ => return None,
    };
    (n >= min && n <= max).then_some(kind)
}

fn encode(
    mnemonic: &str,
    operands: &[Operand],
    bits: u32,
    ip: u64,
    labels: &BTreeMap<String, u64>,
    sizing: bool,
) -> Result<Vec<u8>> {
    let mut best: Option<Vec<u8>> = None;
    static CODES: OnceLock<BTreeMap<String, Vec<Code>>> = OnceLock::new();
    let codes = CODES.get_or_init(|| {
        let mut codes: BTreeMap<String, Vec<Code>> = BTreeMap::new();
        for code in Code::values() {
            codes.entry(format!("{:?}", code.mnemonic()).to_ascii_lowercase()).or_default().push(code);
        }
        codes
    });
    for &code in codes.get(mnemonic).into_iter().flatten() {
        let info = code.op_code();
        if !info.is_instruction()
            || (bits == 64 && !info.mode64())
            || (bits == 32 && !info.mode32())
            || info.op_count() as usize != operands.len()
        {
            continue;
        }
        let mut ins = Instruction::default();
        ins.set_code(code);
        let inferred = operands.iter().find_map(|o| match o {
            Operand::Reg(r) => Some(r.size()),
            _ => None,
        });
        let mut rip_disp = None;
        let ok = operands.iter().enumerate().all(|(i, o)| {
            let i = i as u32;
            let k = info.op_kind(i);
            match o {
                Operand::Reg(r) if reg_matches(k, *r) => {
                    ins.set_op_register(i, *r);
                    true
                }
                Operand::Mem { base, index, scale, disp, size } => {
                    if !matches!(
                        k,
                        K::mem
                            | K::r8_or_mem
                            | K::r16_or_mem
                            | K::r32_or_mem
                            | K::r64_or_mem
                            | K::xmm_or_mem
                            | K::ymm_or_mem
                            | K::zmm_or_mem
                    ) {
                        return false;
                    }
                    let want = size.or(inferred);
                    if mnemonic != "lea" && want != Some(info.memory_size().size()) {
                        return false;
                    }
                    if !matches!(base, Register::None | Register::RIP | Register::EIP)
                        && !base.is_gpr32()
                        && !base.is_gpr64()
                    {
                        return false;
                    }
                    if *index != Register::None && !index.is_gpr32() && !index.is_gpr64() {
                        return false;
                    }
                    ins.set_op_kind(i, OpKind::Memory);
                    ins.set_memory_base(*base);
                    ins.set_memory_index(*index);
                    ins.set_memory_index_scale(*scale);
                    ins.set_memory_displacement64(*disp as u64);
                    ins.set_memory_displ_size(if *disp == 0 && *base != Register::None { 0 } else { 1 });
                    if matches!(base, Register::RIP | Register::EIP) {
                        rip_disp = Some(*disp);
                        ins.set_memory_displ_size(4);
                        ins.set_memory_displacement64(ip);
                    }
                    true
                }
                Operand::Imm(_) | Operand::Label(_) => {
                    let n = match o {
                        Operand::Imm(n) => *n,
                        Operand::Label(l) => match labels.get(l) {
                            Some(n) => *n as i128,
                            None if sizing => ip as i128,
                            None => return false,
                        },
                        _ => unreachable!(),
                    };
                    // Always use rel32: instruction lengths stay fixed while labels are laid out.
                    if matches!(k, K::br32_4 | K::br64_4) && n >= 0 && n <= u64::MAX as i128 {
                        if bits == 32 && n > u32::MAX as i128 {
                            return false;
                        }
                        ins.set_op_kind(i, if bits == 64 { OpKind::NearBranch64 } else { OpKind::NearBranch32 });
                        ins.set_near_branch64(n as u64);
                        return true;
                    }
                    if matches!(o, Operand::Label(_)) {
                        return false;
                    }
                    if let Some(kind) = immediate(k, n) {
                        ins.set_op_kind(i, kind);
                        return ins.try_set_immediate_u64(i, n as u64).is_ok();
                    }
                    false
                }
                _ => false,
            }
        });
        if !ok {
            continue;
        }
        let mut encoder = Encoder::new(bits);
        if let Ok(len) = encoder.encode(&ins, ip) {
            if let Some(disp) = rip_disp {
                ins.set_memory_displacement64(ip.wrapping_add(len as u64).wrapping_add(disp as u64));
                encoder = Encoder::new(bits);
                if encoder.encode(&ins, ip).is_err() {
                    continue;
                }
            }
            let bytes = encoder.take_buffer();
            if best.as_ref().is_none_or(|b| bytes.len() < b.len()) {
                best = Some(bytes);
            }
        }
    }
    best.ok_or_else(|| bad(format!("cannot encode {mnemonic} {operands:?} in {bits}-bit mode (unknown label, unsupported syntax, operand size or branch range)")))
}

/// Assemble Intel instructions separated by semicolons or newlines. Labels use `name:`;
/// branches use rel32. Numbers are decimal or `0x` hex. Memory accepts size ptr [base+index*scale+disp].
/// Directives, macros and implicit memory sizes without a register are not accepted.
pub fn assemble(source: &str, at: u64, bits: u32) -> Result<Vec<u8>> {
    if !matches!(bits, 32 | 64) {
        return Err(bad("bits must be 32 or 64"));
    }
    if bits == 32 && at > u32::MAX as u64 {
        return Err(bad("address exceeds 32 bits"));
    }
    if source.len() > MAX_SOURCE {
        return Err(bad("source exceeds 262144 bytes"));
    }
    let mut labels = BTreeMap::new();
    let mut instructions = Vec::new();
    let mut ip = at;
    for statement in source.split([';', '\n']) {
        let mut text = statement.trim();
        while let Some((name, rest)) = text.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            if !label(&name) {
                return Err(bad("invalid label"));
            }
            if labels.insert(name.clone(), ip).is_some() {
                return Err(bad(format!("duplicate label {name}")));
            }
            text = rest.trim();
        }
        if text.is_empty() {
            continue;
        }
        let (m, args) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let m = match m.to_ascii_lowercase().as_str() {
            "jz" => "je".into(),
            "jnz" => "jne".into(),
            other => other.to_string(),
        };
        let operands = if args.trim().is_empty() {
            Vec::new()
        } else {
            args.split(',').map(operand).collect::<Result<Vec<_>>>()?
        };
        let bytes = encode(&m, &operands, bits, ip, &labels, true)?;
        instructions.push((m, operands, ip, bytes.len()));
        ip = ip.checked_add(bytes.len() as u64).ok_or_else(|| bad("address overflow"))?;
        if bits == 32 && ip > u32::MAX as u64 {
            return Err(bad("assembly exceeds 32-bit address space"));
        }
    }
    if instructions.is_empty() {
        return Err(bad("give at least one instruction"));
    }
    let mut bytes = Vec::new();
    for (m, operands, ip, len) in instructions {
        let encoded = encode(&m, &operands, bits, ip, &labels, false)?;
        if encoded.len() != len {
            return Err(bad("label changed instruction size"));
        }
        bytes.extend(encoded);
    }
    Ok(bytes)
}

pub(crate) fn disassembly(bits: u32, bytes: &[u8], at: u64, limit: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut formatter = disasm::formatter();
    disasm::walk_bits(bits, bytes, at, |ins, raw| {
        if lines.len() < limit {
            let mut text = String::new();
            formatter.format(ins, &mut text);
            lines.push(format!("{:#x}  {:<32} {text}", ins.ip(), hex(raw)));
        }
    });
    lines
}

/// Assemble and return the bytes and a short disassembly.
pub fn assembly(source: &str, at: u64, bits: u32) -> Result<Output> {
    let bytes = assemble(source, at, bits)?;
    let mut lines = vec![format!("asm at {at:#x} ({bits}-bit): {}", hex(&bytes))];
    lines.extend(disassembly(bits, &bytes, at, usize::MAX));
    let rows = lines
        .into_iter()
        .enumerate()
        .map(|(i, text)| Row { key: i as u64, json: Some(json!({"text": text})), text })
        .collect();
    Ok(Output {
        op: "asm",
        meta: json!({"at": at, "bits": bits, "bytes": hex(&bytes)}),
        rows,
        tail: 0,
        command: String::new(),
    })
}
