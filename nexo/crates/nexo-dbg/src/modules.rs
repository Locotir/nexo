//! The files loaded into the program, opened lazily with `nexo-bin`, and the arithmetic between
//! addresses in memory and addresses in a file. Also the call chain (`.eh_frame` first, the frame
//! pointer when a file has no unwind table).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use nexo_bin::{Image, Place};

use crate::platform::Target;
use crate::regs::Regs;

pub struct Module {
    pub path: String,
    pub base: u64,
    pub end: u64,
}

/// One frame of a call chain.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    pub pc: u64,
    /// The stack pointer in this frame.
    pub sp: u64,
    /// Where the caller's stack pointer will be after the return, when known.
    pub cfa: Option<u64>,
    /// The general registers as they are in this frame, in the order of `Regs::gp`.
    pub gp: [u64; 16],
    /// The registers whose value in this frame is not known (bits by `Regs::gp` index, [`LOST_FLAGS`] for eflags):
    /// in a caller's frame, those the call may have changed that no unwind rule gave back. Zero in the innermost.
    pub lost: u32,
    /// How this frame was found from the one it called (`.eh_frame`, `.pdata`, ...); empty for the innermost.
    pub how: &'static str,
}

/// The bit of [`Frame::lost`] for eflags, which every call may change and nothing saves.
pub const LOST_FLAGS: u32 = 1 << 16;

/// Registers a call may change without saving them (by `Regs::gp` index): rax, rcx, rdx, rsi, rdi and r8 to r11 in
/// the System V ABI (Linux, macOS); Windows x64 keeps rsi and rdi for the caller.
const SYSV_VOLATILE: u32 = 1 | 1 << 2 | 1 << 3 | 1 << 4 | 1 << 5 | 0xf << 8;
const WIN_VOLATILE: u32 = 1 | 1 << 2 | 1 << 3 | 0xf << 8;
/// eax, ecx and edx: what a call may change in a 32-bit program (cdecl, stdcall, fastcall alike).
const I386_VOLATILE: u32 = 1 | 1 << 2 | 1 << 3;
/// rbp and rsp: the two registers a frame pointer chain gives back.
const FP_KNOWN: u32 = 1 << 6 | 1 << 7;
/// How a frame found by the saved frame pointers is described.
const FP_CHAIN: &str = "the frame pointer chain (no unwind table covers the callee)";

/// One step out of a frame: the caller's pc and registers, the CFA, how the step was made, and which registers
/// (bits by `Regs::gp` index) an unwind rule set.
#[derive(Debug, Clone, Copy)]
struct Out {
    pc: u64,
    gp: [u64; 16],
    cfa: Option<u64>,
    how: &'static str,
    set: u32,
    /// Registers a rule says have no value in the caller.
    gone: u32,
}

impl Out {
    fn new((pc, gp, cfa): (u64, [u64; 16], Option<u64>), how: &'static str, set: u32) -> Out {
        Out { pc, gp, cfa, how, set: set | 1 << 7, gone: 0 }
    }
}

pub struct Mods {
    pub list: Vec<Module>,
    main: Image,
    main_idx: usize,
    others: HashMap<String, Option<Image>>,
    lines: HashMap<usize, Option<crate::lines::Lines>>,
    /// The program's variables and types, built on first use.
    vars: Option<Option<crate::vars::Vars>>,
}

/// DWARF register number to the index in `Regs::gp`.
pub(crate) const DW2GP: [usize; 16] = [0, 3, 2, 1, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// The same for 32-bit x86, whose DWARF numbers are eax, ecx, edx, ebx, esp, ebp, esi, edi; 8 is eip, the return
/// address column.
pub(crate) const DW2GP32: [usize; 8] = [0, 2, 3, 1, 7, 6, 4, 5];

impl Mods {
    pub fn new(main: Image) -> Mods {
        Mods { list: Vec::new(), main, main_idx: 0, others: HashMap::new(), lines: HashMap::new(), vars: None }
    }

    /// The program's image, with the names and notes kept for it.
    pub fn main_image(&self) -> &Image {
        &self.main
    }

    /// The name of the program's import whose slot is at memory address `slot` (a [`Place::ImportSlot`]), the
    /// name its library exports. A slot may be written `puts`, `puts@got`, `__imp_puts` or as a number, so the
    /// library is searched by this name, never by the text typed.
    pub fn import_of_slot(&self, slot: u64) -> Option<String> {
        let fa = slot.wrapping_sub(self.main_bias());
        self.main.imports.iter().find(|i| i.slot == Some(fa)).map(|i| i.name.clone())
    }

    /// A range of the program's file (`A+LEN`'s whole function or section forms of `nexo_bin::range_in`) as the
    /// addresses it has in memory.
    pub fn file_range(&mut self, text: &str) -> crate::Result<(u64, u64)> {
        let (lo, hi) = nexo_bin::range_in(&self.main, text)?;
        let bias = self.main_bias();
        Ok((lo.wrapping_add(bias), hi.wrapping_add(bias)))
    }

    /// Read the names and notes kept for the program again (after `name` or `note`). A reason when they cannot be read.
    pub fn reload_names(&mut self, dir: &Path) -> Option<String> {
        nexo_bin::attach_names(&mut self.main, dir)
    }

    pub fn exe_path(&self) -> &Path {
        &self.main.path
    }

    /// Read the list of loaded files again (after a stop where a library may have come in).
    pub fn refresh(&mut self, t: &mut dyn Target) {
        let maps = t.maps().unwrap_or_default();
        let mods = t.modules().unwrap_or_default();
        let canon = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
        let me = canon(&self.main.path.display().to_string());
        self.list = mods
            .iter()
            .map(|m| {
                let end = maps
                    .iter()
                    .filter(|x| x.path == m.path && x.start >= m.base)
                    .map(|x| x.end)
                    .max()
                    .unwrap_or(m.base);
                Module { path: m.path.clone(), base: m.base, end }
            })
            .collect();
        self.main_idx = self.list.iter().position(|m| canon(&m.path) == me).unwrap_or(0);
    }

    pub fn main_base(&self) -> u64 {
        self.list.get(self.main_idx).map_or(0, |m| m.base)
    }

    /// Add this to a file address of the program to get its address in memory.
    pub fn main_bias(&self) -> u64 {
        self.main_base().wrapping_sub(self.main.image_base)
    }

    /// The file address (the ones `nexo bin` prints) of a memory address inside the program's own image.
    pub fn in_main(&self, addr: u64) -> Option<u64> {
        let i = self.locate(addr)?;
        (i == self.main_idx).then(|| addr.wrapping_sub(self.main_bias()))
    }

    pub fn locate(&self, addr: u64) -> Option<usize> {
        self.list.iter().position(|m| addr >= m.base && addr < m.end)
    }

    fn ensure(&mut self, idx: usize) {
        if idx == self.main_idx {
            return;
        }
        if let Some(m) = self.list.get(idx) {
            if !self.others.contains_key(&m.path) {
                let img = Image::open_arch(Path::new(&m.path), crate::platform::NATIVE_SLICE).ok();
                self.others.insert(m.path.clone(), img);
            }
        }
    }

    fn image(&self, idx: usize) -> Option<&Image> {
        if idx == self.main_idx {
            return Some(&self.main);
        }
        self.others.get(&self.list.get(idx)?.path)?.as_ref()
    }

    fn bias(&self, idx: usize) -> u64 {
        match (self.list.get(idx), self.image(idx)) {
            (Some(m), Some(i)) => m.base.wrapping_sub(i.image_base),
            _ => 0,
        }
    }

    /// The image of loaded file `idx`, read on first use, and what to add to its file addresses.
    pub fn image_bias(&mut self, idx: usize) -> Option<(&Image, u64)> {
        self.ensure(idx);
        let bias = self.bias(idx);
        Some((self.image(idx)?, bias))
    }

    /// The short file name of a module.
    pub fn short(&self, idx: usize) -> String {
        self.list.get(idx).map(|m| m.path.rsplit(['/', '\\']).next().unwrap_or(&m.path).to_string()).unwrap_or_default()
    }

    /// `name+0x4` for code, or a few words for data; `None` when the address is in no known file.
    pub fn label(&mut self, addr: u64, memory: bool) -> Option<String> {
        let idx = self.locate(addr)?;
        self.ensure(idx);
        let bias = self.bias(idx);
        let img = self.image(idx)?;
        let fa = addr.wrapping_sub(bias);
        if idx == self.main_idx {
            return nexo_bin::describe_addr(img, fa, memory);
        }
        let short = self.short(idx);
        Some(match nexo_bin::nearest_symbol(img, fa) {
            Some((n, 0)) => format!("{short}!{n}"),
            Some((n, off)) => format!("{short}!{n}+{off:#x}"),
            None => format!("{short}+{:#x}", addr - self.list[idx].base),
        })
    }

    /// Where the function or symbol that holds `addr` starts, from the same symbols that name addresses.
    pub fn symbol_start(&mut self, addr: u64) -> Option<u64> {
        let idx = self.locate(addr)?;
        self.ensure(idx);
        let bias = self.bias(idx);
        let img = self.image(idx)?;
        let (_, off) = nexo_bin::nearest_symbol(img, addr.wrapping_sub(bias))?;
        addr.checked_sub(off)
    }

    /// The line table of the program itself, built once, with the bias to add to its addresses.
    pub fn main_lines(&mut self) -> Option<(&crate::lines::Lines, u64)> {
        let (idx, bias) = (self.main_idx, self.main_bias());
        if !self.lines.contains_key(&idx) {
            let t = crate::lines::Lines::of(&self.main);
            self.lines.insert(idx, t);
        }
        Some((self.lines.get(&idx)?.as_ref()?, bias))
    }

    /// The program's variables, built once, with the bias to add to its addresses.
    pub fn main_vars(&mut self) -> Option<(&crate::vars::Vars, u64)> {
        let bias = self.main_bias();
        if self.vars.is_none() {
            self.vars = Some(crate::vars::Vars::of(&self.main));
        }
        Some((self.vars.as_ref()?.as_ref()?, bias))
    }

    /// The program's variables and its line table together (each built once), with the bias to add to their
    /// addresses; the line table is `None` when the program has none.
    pub fn main_vars_lines(&mut self) -> Option<(&crate::vars::Vars, Option<&crate::lines::Lines>, u64)> {
        let _ = self.main_lines();
        self.main_vars()?;
        let lines = self.lines.get(&self.main_idx).and_then(Option::as_ref);
        Some((self.vars.as_ref()?.as_ref()?, lines, self.main_bias()))
    }

    /// The text of the source line of the code at `addr`, when the file is on this machine.
    pub fn source_text(&mut self, addr: u64) -> Option<String> {
        let idx = self.locate(addr)?;
        let bias = self.bias(idx);
        let w = self.lines.get(&idx)?.as_ref()?.at(addr.wrapping_sub(bias))?;
        let (file, line) = w.inside.clone().unwrap_or((w.file.clone(), w.line));
        crate::lines::source_text(&file, line).map(|t| format!("{line}  {t}"))
    }

    /// `src/main.rs:42` for the code at `addr`, when the file that holds it carries line info.
    pub fn source(&mut self, addr: u64) -> Option<String> {
        let idx = self.locate(addr)?;
        self.ensure(idx);
        let bias = self.bias(idx);
        if !self.lines.contains_key(&idx) {
            let t = self.image(idx).and_then(crate::lines::Lines::of);
            self.lines.insert(idx, t);
        }
        Some(self.lines.get(&idx)?.as_ref()?.at(addr.wrapping_sub(bias))?.text())
    }

    /// What a person typed as a place: a number, a name, or `name+off`. A number inside a section of
    /// the program is a file address (the ones `nexo bin` prints); any other number is a memory address.
    pub fn resolve(&mut self, text: &str) -> crate::Result<Place> {
        let t = text.trim();
        let bias = self.main_bias();
        if let Ok(a) = nexo_bin::parse_addr(t) {
            return Ok(match self.main.section_at(a) {
                Some(_) => match nexo_bin::place_of(&self.main, t)? {
                    Place::Code(x) => Place::Code(x.wrapping_add(bias)),
                    Place::ImportSlot(x) => Place::ImportSlot(x.wrapping_add(bias)),
                    Place::Data(x) => Place::Data(x.wrapping_add(bias)),
                },
                None => Place::Data(a),
            });
        }
        match nexo_bin::place_of(&self.main, t) {
            Ok(Place::Code(x)) => Ok(Place::Code(x.wrapping_add(bias))),
            Ok(Place::ImportSlot(x)) => Ok(Place::ImportSlot(x.wrapping_add(bias))),
            Ok(Place::Data(x)) => Ok(Place::Data(x.wrapping_add(bias))),
            Err(first) => {
                // a name from a library: `libc.so.6!puts` or just `puts`
                let (only, name) = match t.split_once('!') {
                    Some((m, n)) => (Some(m), n),
                    None => (None, t),
                };
                for idx in 0..self.list.len() {
                    if idx == self.main_idx || only.is_some_and(|o| !self.short(idx).starts_with(o)) {
                        continue;
                    }
                    self.ensure(idx);
                    let bias = self.bias(idx);
                    if let Some(img) = self.image(idx) {
                        let hit = img
                            .exports
                            .iter()
                            .find(|e| e.name == name)
                            .map(|e| e.addr)
                            .or_else(|| img.symbols.iter().find(|s| s.name == name && s.func).map(|s| s.addr));
                        if let Some(a) = hit {
                            return Ok(Place::Code(a.wrapping_add(bias)));
                        }
                    }
                }
                Err(first.into())
            }
        }
    }

    /// The call chain from these registers: at most `max` frames. `rd` reads 8 bytes of the program; a 32-bit
    /// program's frames use the low 4 of them.
    pub fn backtrace(&mut self, regs: &Regs, rd: &mut dyn FnMut(u64) -> Option<u64>, max: usize) -> Vec<Frame> {
        let x32 = self.x32();
        let volatile = if x32 {
            I386_VOLATILE
        } else if self.main.format == nexo_bin::Format::Pe {
            WIN_VOLATILE
        } else {
            SYSV_VOLATILE
        };
        let mut word = |a: u64| rd(a).map(|v| if x32 { v & 0xffff_ffff } else { v });
        let rd: &mut dyn FnMut(u64) -> Option<u64> = &mut word;
        let mut gp = regs.gp;
        let mut pc = regs.rip;
        let mut lost = 0u32;
        let mut how = "";
        let mut frames: Vec<Frame> = Vec::new();
        while frames.len() < max {
            let first = frames.is_empty();
            let next = self.unwind_one(pc, &gp, first, rd);
            frames.push(Frame { pc, sp: gp[7], cfa: next.and_then(|o| o.cfa), gp, lost, how });
            let Some(o) = next else { break };
            if o.pc == 0 || o.gp[7] <= gp[7] {
                break;
            }
            // A register no rule set keeps its value through the call only when the ABI makes the callee keep it;
            // the chain of frame pointers has no rules, so only rbp and rsp are known past it.
            lost = if o.how == FP_CHAIN {
                (0xffff & !FP_KNOWN) | LOST_FLAGS
            } else {
                ((lost | volatile) & !o.set) | o.gone | LOST_FLAGS
            };
            (pc, gp, how) = (o.pc, o.gp, o.how);
        }
        frames
    }

    /// The program is 32-bit x86 code: 4-byte stack slots, eax .. edi.
    fn x32(&self) -> bool {
        self.main.bits == 32
    }

    fn unwind_one(
        &mut self,
        pc: u64,
        gp: &[u64; 16],
        first: bool,
        rd: &mut dyn FnMut(u64) -> Option<u64>,
    ) -> Option<Out> {
        let look = if first { pc } else { pc.wrapping_sub(1) };
        let slot = if self.x32() { 4 } else { 8 };
        if let Some(idx) = self.locate(look) {
            self.ensure(idx);
            if let Some(img) = self.image(idx).filter(|i| i.format == nexo_bin::Format::Pe) {
                // The registers the codes restore are not told apart here, so a register unknown in the callee stays
                // unknown; the callee-saved ones it never touched keep their value as the ABI says.
                if let Some(s) = crate::pe_unwind::unwind(img, self.bias(idx), look, gp, rd) {
                    return Some(Out::new(s, ".pdata", 0));
                }
            }
            if let Some(r) = self.unwind_cfi(idx, look, gp, rd) {
                return r;
            }
            if first && self.in_plt(idx, look) {
                let ra = rd(gp[7])?;
                let mut n = *gp;
                n[7] += slot;
                return Some(Out::new((ra, n, Some(gp[7] + slot)), "the PLT stub's return address", 0));
            }
            if first {
                if let Some(s) = self.macho_prologue(idx, look, gp, rd) {
                    return Some(Out::new(s, "the return address at the function's entry", 1 << 6));
                }
            }
        }
        // the frame pointer chain
        let bp = gp[6];
        if bp == 0 || bp < gp[7] || bp & (slot - 1) != 0 {
            return None;
        }
        // the saved frame pointer, then the return address, one slot each
        let ra = rd(bp.checked_add(slot)?)?;
        let obp = rd(bp)?;
        let mut n = *gp;
        n[6] = obp;
        n[7] = bp + 2 * slot;
        Some(Out::new((ra, n, Some(bp + 2 * slot)), FP_CHAIN, FP_KNOWN))
    }

    /// A Mach-O program stopped on the first instruction of a function, or on the second after `push rbp`: its frame
    /// is not made yet, so the return address sits on top of the stack (under the pushed rbp on the second). Past
    /// `mov rbp, rsp` the frame pointer chain is right, as clang keeps frame pointers on macOS. The compact unwind
    /// table (`__unwind_info`), which functions without a frame pointer need, is not read.
    #[allow(clippy::type_complexity)]
    fn macho_prologue(
        &self,
        idx: usize,
        pc: u64,
        gp: &[u64; 16],
        rd: &mut dyn FnMut(u64) -> Option<u64>,
    ) -> Option<(u64, [u64; 16], Option<u64>)> {
        let img = self.image(idx).filter(|i| i.format == nexo_bin::Format::MachO)?;
        let (_, off) = nexo_bin::nearest_symbol(img, pc.wrapping_sub(self.bias(idx)))?;
        let (sp, mut n) = (gp[7], *gp);
        match off {
            0 => {
                n[7] = sp + 8;
                Some((rd(sp)?, n, Some(sp + 8)))
            }
            // 0x55 is `push rbp`
            1 if rd(pc - 1)? & 0xff == 0x55 => {
                n[6] = rd(sp)?;
                n[7] = sp + 16;
                Some((rd(sp + 8)?, n, Some(sp + 16)))
            }
            _ => None,
        }
    }

    fn in_plt(&self, idx: usize, addr: u64) -> bool {
        let Some(img) = self.image(idx) else { return false };
        let fa = addr.wrapping_sub(self.bias(idx));
        img.section_at(fa).is_some_and(|s| s.name.starts_with(".plt"))
    }

    /// `Some(None)`: the table says this is the outermost frame. `None`: no table covers the address.
    fn unwind_cfi(
        &self,
        idx: usize,
        look: u64,
        gp: &[u64; 16],
        rd: &mut dyn FnMut(u64) -> Option<u64>,
    ) -> Option<Option<Out>> {
        use gimli::{CfaRule, RegisterRule, UnwindSection};
        let img = self.image(idx)?;
        let sec = img.sections.iter().find(|s| s.name == ".eh_frame")?;
        let mut eh = gimli::EhFrame::new(img.section_bytes(sec), gimli::LittleEndian);
        // a 32-bit file's table holds 4-byte addresses
        let size: u8 = if img.bits == 32 { 4 } else { 8 };
        eh.set_address_size(size);
        let mut bases = gimli::BaseAddresses::default().set_eh_frame(sec.addr);
        for (name, set) in [(".text", 0), (".got", 1)] {
            if let Some(s) = img.sections.iter().find(|s| s.name == name) {
                bases = if set == 0 { bases.set_text(s.addr) } else { bases.set_got(s.addr) };
            }
        }
        let fa = look.wrapping_sub(self.bias(idx));
        let mut ctx = gimli::UnwindContext::new();
        let row = eh.unwind_info_for_address(&bases, &mut ctx, fa, gimli::EhFrame::cie_from_offset).ok()?;
        let (map, ra_col, mask): (&[usize], u16, u64) =
            if self.x32() { (&DW2GP32, 8, 0xffff_ffff) } else { (&DW2GP, 16, u64::MAX) };
        let reg = |r: u16| map.get(r as usize).and_then(|&i| gp.get(i).copied());
        let cfa = match row.cfa() {
            CfaRule::RegisterAndOffset { register, offset } => reg(register.0)?.wrapping_add_signed(*offset) & mask,
            // a function that realigns its stack (a 32-bit `main`) reads the CFA back from where it saved it
            CfaRule::Expression(x) => cfi_expr(x.get(&eh).ok()?, size, None, &reg, rd)? & mask,
        };
        let mut n = *gp;
        let (mut set, mut gone) = (0u32, 0u32);
        for (dw, &slot) in map.iter().enumerate() {
            match row.register(gimli::Register(dw as u16)) {
                Some(RegisterRule::Offset(o)) => n[slot] = rd(cfa.wrapping_add_signed(o) & mask)?,
                Some(RegisterRule::ValOffset(o)) => n[slot] = cfa.wrapping_add_signed(o) & mask,
                Some(RegisterRule::Register(r)) => n[slot] = *gp.get(*map.get(r.0 as usize)?)?,
                Some(RegisterRule::Expression(x)) => {
                    let at = cfi_expr(x.get(&eh).ok()?, size, Some(cfa), &reg, rd)? & mask;
                    n[slot] = rd(at)?
                }
                Some(RegisterRule::ValExpression(x)) => {
                    n[slot] = cfi_expr(x.get(&eh).ok()?, size, Some(cfa), &reg, rd)? & mask
                }
                Some(RegisterRule::Undefined) => {
                    gone |= 1 << slot;
                    continue;
                }
                _ => continue,
            }
            set |= 1 << slot;
        }
        let ra = match row.register(gimli::Register(ra_col)) {
            Some(RegisterRule::Offset(o)) => rd(cfa.wrapping_add_signed(o) & mask)?,
            Some(RegisterRule::Expression(x)) => {
                let at = cfi_expr(x.get(&eh).ok()?, size, Some(cfa), &reg, rd)? & mask;
                rd(at)?
            }
            Some(RegisterRule::Undefined) | None => return Some(None),
            _ => return None,
        };
        n[7] = cfa;
        Some(Some(Out { gone, ..Out::new((ra, n, Some(cfa)), ".eh_frame", set) }))
    }
}

/// A DWARF expression of an unwind rule, run against a frame's registers (`reg`, by DWARF number) and memory: the
/// CFA itself, or, with the CFA pushed first, where a register was saved. `size`: the program's address size.
fn cfi_expr(
    x: gimli::Expression<gimli::EndianSlice<'_, gimli::LittleEndian>>,
    size: u8,
    cfa: Option<u64>,
    reg: &dyn Fn(u16) -> Option<u64>,
    rd: &mut dyn FnMut(u64) -> Option<u64>,
) -> Option<u64> {
    use gimli::{EvaluationResult, Location, Value};
    let mut e = x.evaluation(gimli::Encoding { address_size: size, format: gimli::Format::Dwarf32, version: 4 });
    if let Some(c) = cfa {
        e.set_initial_value(c);
    }
    let mut res = e.evaluate().ok()?;
    loop {
        res = match res {
            EvaluationResult::Complete => break,
            EvaluationResult::RequiresMemory { address, size, .. } => {
                let v = rd(address)?;
                let v = if size >= 8 { v } else { v & ((1u64 << (u32::from(size) * 8)) - 1) };
                e.resume_with_memory(Value::Generic(v)).ok()?
            }
            EvaluationResult::RequiresRegister { register, .. } => {
                e.resume_with_register(Value::Generic(reg(register.0)?)).ok()?
            }
            _ => return None,
        };
    }
    match e.result().as_slice() {
        [p] => match p.location {
            Location::Address { address } => Some(address),
            _ => None,
        },
        _ => None,
    }
}
