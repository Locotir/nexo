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
}

pub struct Mods {
    pub list: Vec<Module>,
    main: Image,
    main_idx: usize,
    others: HashMap<String, Option<Image>>,
    lines: HashMap<usize, Option<crate::lines::Lines>>,
}

/// DWARF register number to the index in `Regs::gp`.
const DW2GP: [usize; 16] = [0, 3, 2, 1, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

impl Mods {
    pub fn new(main: Image) -> Mods {
        Mods { list: Vec::new(), main, main_idx: 0, others: HashMap::new(), lines: HashMap::new() }
    }

    /// The program's image, with the names and notes kept for it.
    pub fn main_image(&self) -> &Image {
        &self.main
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

    pub fn locate(&self, addr: u64) -> Option<usize> {
        self.list.iter().position(|m| addr >= m.base && addr < m.end)
    }

    fn ensure(&mut self, idx: usize) {
        if idx == self.main_idx {
            return;
        }
        if let Some(m) = self.list.get(idx) {
            if !self.others.contains_key(&m.path) {
                let img = Image::open(Path::new(&m.path)).ok();
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

    /// The call chain from these registers: at most `max` frames. `rd` reads 8 bytes of the program.
    pub fn backtrace(&mut self, regs: &Regs, rd: &mut dyn FnMut(u64) -> Option<u64>, max: usize) -> Vec<Frame> {
        let mut gp = regs.gp;
        let mut pc = regs.rip;
        let mut frames: Vec<Frame> = Vec::new();
        while frames.len() < max {
            let mut f = Frame { pc, sp: gp[7], cfa: None };
            let first = frames.is_empty();
            let next = self.unwind_one(pc, &gp, first, rd);
            if let Some((npc, ngp, cfa)) = &next {
                f.cfa = *cfa;
                let _ = (npc, ngp);
            }
            frames.push(f);
            let Some((npc, ngp, _)) = next else { break };
            if npc == 0 || ngp[7] <= gp[7] {
                break;
            }
            pc = npc;
            gp = ngp;
        }
        frames
    }

    #[allow(clippy::type_complexity)]
    fn unwind_one(
        &mut self,
        pc: u64,
        gp: &[u64; 16],
        first: bool,
        rd: &mut dyn FnMut(u64) -> Option<u64>,
    ) -> Option<(u64, [u64; 16], Option<u64>)> {
        let look = if first { pc } else { pc.wrapping_sub(1) };
        if let Some(idx) = self.locate(look) {
            self.ensure(idx);
            if let Some(r) = self.unwind_cfi(idx, look, gp, rd) {
                return r;
            }
            if first && self.in_plt(idx, look) {
                let ra = rd(gp[7])?;
                let mut n = *gp;
                n[7] += 8;
                return Some((ra, n, Some(gp[7] + 8)));
            }
        }
        // the frame pointer chain
        let bp = gp[6];
        if bp == 0 || bp < gp[7] || bp & 7 != 0 {
            return None;
        }
        let ra = rd(bp.checked_add(8)?)?;
        let obp = rd(bp)?;
        let mut n = *gp;
        n[6] = obp;
        n[7] = bp + 16;
        Some((ra, n, Some(bp + 16)))
    }

    fn in_plt(&self, idx: usize, addr: u64) -> bool {
        let Some(img) = self.image(idx) else { return false };
        let fa = addr.wrapping_sub(self.bias(idx));
        img.section_at(fa).is_some_and(|s| s.name.starts_with(".plt"))
    }

    /// `Some(None)`: the table says this is the outermost frame. `None`: no table covers the address.
    #[allow(clippy::type_complexity)]
    fn unwind_cfi(
        &self,
        idx: usize,
        look: u64,
        gp: &[u64; 16],
        rd: &mut dyn FnMut(u64) -> Option<u64>,
    ) -> Option<Option<(u64, [u64; 16], Option<u64>)>> {
        use gimli::{CfaRule, RegisterRule, UnwindSection};
        let img = self.image(idx)?;
        let sec = img.sections.iter().find(|s| s.name == ".eh_frame")?;
        let eh = gimli::EhFrame::new(img.section_bytes(sec), gimli::LittleEndian);
        let mut bases = gimli::BaseAddresses::default().set_eh_frame(sec.addr);
        for (name, set) in [(".text", 0), (".got", 1)] {
            if let Some(s) = img.sections.iter().find(|s| s.name == name) {
                bases = if set == 0 { bases.set_text(s.addr) } else { bases.set_got(s.addr) };
            }
        }
        let fa = look.wrapping_sub(self.bias(idx));
        let mut ctx = gimli::UnwindContext::new();
        let row = eh.unwind_info_for_address(&bases, &mut ctx, fa, gimli::EhFrame::cie_from_offset).ok()?;
        let CfaRule::RegisterAndOffset { register, offset } = row.cfa() else { return None };
        let base = *gp.get(*DW2GP.get(register.0 as usize)?)?;
        let cfa = base.wrapping_add_signed(*offset);
        let mut n = *gp;
        for (dw, &slot) in DW2GP.iter().enumerate() {
            match row.register(gimli::Register(dw as u16)) {
                Some(RegisterRule::Offset(o)) => n[slot] = rd(cfa.wrapping_add_signed(o))?,
                Some(RegisterRule::ValOffset(o)) => n[slot] = cfa.wrapping_add_signed(o),
                Some(RegisterRule::Register(r)) => n[slot] = *gp.get(*DW2GP.get(r.0 as usize)?)?,
                _ => {}
            }
        }
        let ra = match row.register(gimli::Register(16)) {
            Some(RegisterRule::Offset(o)) => rd(cfa.wrapping_add_signed(o))?,
            Some(RegisterRule::Undefined) | None => return Some(None),
            _ => return None,
        };
        n[7] = cfa;
        Some(Some((ra, n, Some(cfa))))
    }
}
