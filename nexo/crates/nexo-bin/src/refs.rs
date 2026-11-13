//! References: who calls, jumps to, reads, writes, takes the address of, or points at an address.
//!
//! Built in memory on every call from the one decode pass the function finder already makes (see
//! `funcs::find_with`), plus a scan of the data sections for pointers. Nothing is kept on disk.

use std::time::{Duration, Instant};

use iced_x86::{
    FlowControl, Instruction, InstructionInfoFactory, InstructionInfoOptions, Mnemonic, OpAccess, OpKind, Register,
};

use crate::funcs::Func;
use crate::Image;

/// The most references kept. When the list is longer, the rest are dropped and `Refs::capped` says so.
pub const MAX_REFS: usize = 8_000_000;
/// Smallest value taken for an address (an immediate or a data word): below it, numbers are noise.
const MIN_ADDR: u64 = 0x1000;

#[cfg(test)]
thread_local! {
    /// A smaller cap for tests, so a cut can be seen on a small file.
    pub(crate) static TEST_MAX: std::cell::Cell<usize> = const { std::cell::Cell::new(MAX_REFS) };
}

fn max_refs() -> usize {
    #[cfg(test)]
    {
        TEST_MAX.with(|c| c.get())
    }
    #[cfg(not(test))]
    {
        MAX_REFS
    }
}

/// What a reference does, in the order an answer lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Call,
    Jump,
    Read,
    Write,
    /// The address is taken (`lea`).
    Addr,
    /// The address is written inside a memory operand next to a register (`[reg+address]`): a table
    /// or an array that starts here, read, written or jumped through at an index.
    Base,
    /// A number in an instruction that is also an address of a mapped section: maybe an address.
    Imm,
    /// A pointer stored in a data section.
    Ptr,
}

impl Kind {
    pub const ALL: [Kind; 8] =
        [Kind::Call, Kind::Jump, Kind::Read, Kind::Write, Kind::Addr, Kind::Base, Kind::Imm, Kind::Ptr];

    /// The word in a row.
    pub fn word(self) -> &'static str {
        match self {
            Kind::Call => "call",
            Kind::Jump => "jump",
            Kind::Read => "read",
            Kind::Write => "write",
            Kind::Addr => "addr",
            Kind::Base => "base",
            Kind::Imm => "imm",
            Kind::Ptr => "ptr",
        }
    }

    /// What the group is called in a heading.
    pub fn title(self) -> &'static str {
        match self {
            Kind::Call => "calls",
            Kind::Jump => "jumps from another function",
            Kind::Read => "reads",
            Kind::Write => "writes",
            Kind::Addr => "address taken (lea)",
            Kind::Base => "used with a register added",
            Kind::Imm => "numbers that may be this address",
            Kind::Ptr => "pointers in data",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ref {
    pub target: u64,
    pub src: u64,
    pub kind: Kind,
}

/// Every reference found, in two views: sorted by target and (by index) sorted by source.
pub struct Refs {
    by_target: Vec<Ref>,
    by_src: Vec<u32>,
    /// True when the list hit [`MAX_REFS`] and some references are missing.
    pub capped: bool,
    /// True when the cut dropped code references too (data pointers are added last, so they go first).
    pub capped_code: bool,
    /// Indirect calls (`call rax`, `call [rax+0x18]`): the targets are not known.
    pub indirect_calls: u64,
    /// Indirect jumps that are not through an import slot (`jmp rax`, `jmp [rax*8+table]`): the
    /// jump tables this version does not recover.
    pub indirect_jumps: u64,
    /// How long the build took (decode pass included when `refs` ran it).
    pub build: Duration,
}

impl Refs {
    pub fn len(&self) -> usize {
        self.by_target.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_target.is_empty()
    }

    /// References whose target is exactly `t`, by source address.
    pub fn at(&self, t: u64) -> &[Ref] {
        let a = self.by_target.partition_point(|r| r.target < t);
        let b = self.by_target.partition_point(|r| r.target <= t);
        &self.by_target[a..b]
    }

    /// Calls and cross-function jumps to a function start: its callers, counted by call site.
    pub fn callers(&self, start: u64) -> usize {
        self.at(start).iter().filter(|r| matches!(r.kind, Kind::Call | Kind::Jump)).count()
    }

    /// References whose target is in `lo..lo+len`.
    pub fn within(&self, lo: u64, len: u64) -> &[Ref] {
        let hi = lo.saturating_add(len);
        let a = self.by_target.partition_point(|r| r.target < lo);
        let b = self.by_target.partition_point(|r| r.target < hi);
        &self.by_target[a..b.max(a)]
    }

    /// References whose source is in `lo..hi`, by source address.
    pub fn from(&self, lo: u64, hi: u64) -> impl Iterator<Item = &Ref> + '_ {
        let a = self.by_src.partition_point(|&i| self.by_target[i as usize].src < lo);
        let b = self.by_src.partition_point(|&i| self.by_target[i as usize].src < hi);
        self.by_src[a..b.max(a)].iter().map(|&i| &self.by_target[i as usize])
    }
}

/// Collects references while the code is decoded.
pub(crate) struct Collector {
    list: Vec<Ref>,
    max: usize,
    capped: bool,
    capped_code: bool,
    pub(crate) indirect_calls: u64,
    pub(crate) indirect_jumps: u64,
    info: InstructionInfoFactory,
    /// AArch64: a load from an import slot into a register, waiting for the `blr`/`br` that uses it:
    /// (register, slot, address of the load).
    pub(crate) a64_load: Option<(u8, u64, u64)>,
    /// x86-32: the registers that hold the GOT or another known address.
    pic: crate::pic32::Pic32,
}

impl Collector {
    pub fn new() -> Collector {
        Collector {
            list: Vec::new(),
            max: max_refs(),
            capped: false,
            capped_code: false,
            indirect_calls: 0,
            indirect_jumps: 0,
            info: InstructionInfoFactory::new(),
            a64_load: None,
            pic: crate::pic32::Pic32::new(),
        }
    }

    /// False when the list is full and the reference was dropped.
    pub(crate) fn push(&mut self, target: u64, src: u64, kind: Kind) -> bool {
        if self.list.len() >= self.max {
            self.capped = true;
            self.capped_code |= kind != Kind::Ptr;
            false
        } else {
            self.list.push(Ref { target, src, kind });
            true
        }
    }

    /// Record what one instruction refers to. A jump is kept here whatever it jumps to; `build`
    /// drops the ones that stay inside their own function once the functions are known.
    pub fn instruction(&mut self, img: &Image, ins: &Instruction) {
        if img.bits == 32 {
            let pic = self.pic.resolve(img, ins).filter(|&(a, _)| a >= MIN_ADDR);
            self.record(img, ins, pic);
            self.pic.step(img, ins);
        } else {
            self.record(img, ins, None);
        }
    }

    /// [`instruction`](Self::instruction), with the address x86-32 position-independent code names
    /// through a register of known value (`pic`).
    fn record(&mut self, img: &Image, ins: &Instruction, pic: Option<(u64, bool)>) {
        if ins.is_invalid() {
            return;
        }
        let src = ins.ip();
        let mapped = |a: u64| img.section_at(a).is_some();
        match ins.flow_control() {
            FlowControl::Call | FlowControl::UnconditionalBranch | FlowControl::ConditionalBranch => {
                let kind = if ins.flow_control() == FlowControl::Call { Kind::Call } else { Kind::Jump };
                if matches!(ins.op0_kind(), OpKind::NearBranch64 | OpKind::NearBranch32) {
                    let t = ins.near_branch_target();
                    if mapped(t) {
                        self.push(t, src, kind);
                    }
                }
                return;
            }
            FlowControl::IndirectCall | FlowControl::IndirectBranch => {
                let kind = if ins.flow_control() == FlowControl::IndirectCall { Kind::Call } else { Kind::Jump };
                if ins.is_ip_rel_memory_operand() {
                    // through a slot: an import, or a function pointer kept in a global
                    let slot = ins.ip_rel_memory_address();
                    if mapped(slot) {
                        self.push(slot, src, kind);
                    }
                } else {
                    match pic.or_else(|| written_address(ins)).filter(|&(a, _)| mapped(a)) {
                        // through a slot at a fixed address, as above
                        Some((slot, true)) => {
                            self.push(slot, src, kind);
                        }
                        // through a table at an index: the table is named, where it lands is not known
                        other => {
                            if let Some((table, _)) = other {
                                self.push(table, src, Kind::Base);
                            }
                            if kind == Kind::Jump {
                                self.indirect_jumps += 1;
                            } else {
                                self.indirect_calls += 1;
                            }
                        }
                    }
                }
                return;
            }
            _ => {}
        }
        let named = if ins.is_ip_rel_memory_operand() {
            Some((ins.ip_rel_memory_address(), true))
        } else {
            pic.or_else(|| written_address(ins))
        };
        if let Some((a, alone)) = named.filter(|&(a, _)| mapped(a)) {
            let kind = if !alone {
                Kind::Base
            } else if ins.mnemonic() == Mnemonic::Lea {
                Kind::Addr
            } else {
                let info = self.info.info_options(ins, InstructionInfoOptions::NO_REGISTER_USAGE);
                match info
                    .used_memory()
                    .iter()
                    .find(|m| {
                        m.index() == Register::None
                            && if pic.is_some() { m.base() == ins.memory_base() } else { m.displacement() == a }
                    })
                    .map(|m| m.access())
                {
                    Some(OpAccess::Write | OpAccess::CondWrite | OpAccess::ReadWrite | OpAccess::ReadCondWrite) => {
                        Kind::Write
                    }
                    Some(OpAccess::NoMemAccess) => Kind::Addr,
                    _ => Kind::Read,
                }
            };
            self.push(a, src, kind);
        }
        if !matches!(
            ins.mnemonic(),
            Mnemonic::Add
                | Mnemonic::Sub
                | Mnemonic::And
                | Mnemonic::Or
                | Mnemonic::Xor
                | Mnemonic::Adc
                | Mnemonic::Sbb
                | Mnemonic::Test
                | Mnemonic::Imul
        ) {
            for i in 0..ins.op_count() {
                // x86-32 code passes addresses as plain numbers (`push 0x804a008`)
                if matches!(ins.op_kind(i), OpKind::Immediate32 | OpKind::Immediate32to64 | OpKind::Immediate64) {
                    let v = ins.immediate(i);
                    if v >= MIN_ADDR && mapped(v) {
                        self.push(v, src, Kind::Imm);
                    }
                }
            }
        }
    }
}

/// The address a memory operand carries in its own bytes (`[0x429a80]`, `[rdx+0x429a80]`,
/// `[rax*8+0x429a80]`; not the RIP-relative form), and whether it stands alone. None when the
/// operand has no 4-byte offset, when the offset is too small to be an address, for a stack slot
/// (`[rsp+...]`) and for thread storage (`fs:`, `gs:`). The caller still checks that it is mapped.
pub(crate) fn written_address(ins: &Instruction) -> Option<(u64, bool)> {
    if ins.is_ip_rel_memory_operand() || ins.memory_displ_size() < 4 {
        return None;
    }
    if !(0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Memory) {
        return None;
    }
    if ins.memory_base().full_register() == Register::RSP || matches!(ins.memory_segment(), Register::FS | Register::GS)
    {
        return None;
    }
    let a = ins.memory_displacement64();
    let alone = ins.memory_base() == Register::None && ins.memory_index() == Register::None;
    (a >= MIN_ADDR).then_some((a, alone))
}

/// Sections whose words are not pointers of the program (tables the loader reads).
fn skipped(name: &str) -> bool {
    name.starts_with(".rela")
        || name.starts_with(".rel.")
        || name == ".dynsym"
        || name.starts_with(".eh_frame")
        || name.ends_with(",__eh_frame")
        || name.ends_with(",__unwind_info")
}

/// Every aligned pointer-sized word (8 bytes, 4 in x86-32 files) of the data sections that lands
/// inside a mapped section, plus the pointers the loader fills in (ELF relative relocations).
fn data_pointers(img: &Image, c: &mut Collector) {
    let w = img.ptr_size();
    for sec in img.sections.iter().filter(|s| s.alloc && !s.exec && s.file_size > 0 && !skipped(&s.name)) {
        let data = img.section_bytes(sec);
        let first = (w - (sec.addr % w as u64) as usize) % w;
        let mut i = first;
        while i + w <= data.len() {
            #[cfg(test)]
            SCANNED.with(|c| c.set(c.get() + 1));
            let mut b = [0u8; 8];
            b[..w].copy_from_slice(&data[i..i + w]);
            let v = u64::from_le_bytes(b);
            if v >= MIN_ADDR && img.section_at(v).is_some() {
                if let Some(src) = sec.addr.checked_add(i as u64) {
                    if !c.push(v, src, Kind::Ptr) {
                        return;
                    }
                }
            }
            i += w;
        }
    }
    for &(slot, value) in &img.rel_ptrs {
        if value >= MIN_ADDR && img.section_at(value).is_some() && !c.push(value, slot, Kind::Ptr) {
            return;
        }
    }
}

fn holder(funcs: &[Func], addr: u64) -> Option<u64> {
    let n = funcs.partition_point(|f| f.start <= addr);
    funcs[..n].last().filter(|f| addr < f.end).map(|f| f.start)
}

/// Finish: data pointers, drop jumps that stay in their function, sort, build the source view.
pub(crate) fn build(img: &Image, mut c: Collector, t0: Instant) -> Refs {
    let funcs = img.funcs();
    // code references first: data pointers only take the room they leave
    let mut list = std::mem::take(&mut c.list);
    list.retain(|r| {
        if r.kind != Kind::Jump {
            return true;
        }
        let s = holder(funcs, r.src);
        s.is_none() || s != holder(funcs, r.target)
    });
    c.list = list;
    data_pointers(img, &mut c);
    let (mut list, capped) = (c.list, c.capped);
    list.sort_unstable_by_key(|r| (r.target, r.src, r.kind));
    list.dedup();
    // the cap keeps the index inside u32
    let mut by_src: Vec<u32> = (0..list.len() as u32).collect();
    by_src.sort_unstable_by_key(|&i| {
        let r = &list[i as usize];
        (r.src, r.target, r.kind)
    });
    Refs {
        by_target: list,
        by_src,
        capped,
        capped_code: c.capped_code,
        indirect_calls: c.indirect_calls,
        indirect_jumps: c.indirect_jumps,
        build: t0.elapsed(),
    }
}

#[cfg(test)]
thread_local! {
    /// Words of the data sections looked at by the last pointer scans of this thread.
    pub(crate) static SCANNED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
impl Refs {
    /// A list made by hand, for tests of answers over long lists.
    pub(crate) fn for_test(mut list: Vec<Ref>) -> Refs {
        list.sort_unstable_by_key(|r| (r.target, r.src, r.kind));
        list.dedup();
        let mut by_src: Vec<u32> = (0..list.len() as u32).collect();
        by_src.sort_unstable_by_key(|&i| {
            let r = &list[i as usize];
            (r.src, r.target, r.kind)
        });
        Refs {
            by_target: list,
            by_src,
            capped: false,
            capped_code: false,
            indirect_calls: 0,
            indirect_jumps: 0,
            build: Duration::ZERO,
        }
    }
}
