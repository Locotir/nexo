//! x86-32 position-independent code: the registers that hold a known address in a straight run of code.
//!
//! 32-bit x86 has no RIP-relative operand. Position-independent code loads its own address
//! (`call __x86.get_pc_thunk.bx`, or `call next; pop ebx`), adds the distance to the GOT, and then names
//! its data as `[ebx-0x1fec]`; a PLT stub of a PIE jumps through `[ebx+0x10]`, with ebx set to the GOT
//! by its caller. The tracker follows those steps in decode order. A register is forgotten as soon as
//! anything else writes it, a call forgets the registers a call may change (eax, ecx, edx), and a return,
//! a jump or a byte that is not code forgets everything.

use iced_x86::{
    FlowControl, Instruction, InstructionInfoFactory, InstructionInfoOptions, Mnemonic, OpAccess, OpKind, Register,
};

use crate::Image;

/// The value of each 32-bit general register (eax, ecx, edx, ebx, esp, ebp, esi, edi) when it is known.
pub(crate) struct Pic32 {
    known: [Option<u32>; 8],
    /// After `call next`: the address it pushed, for the `pop` that takes it.
    pushed: Option<u32>,
    info: InstructionInfoFactory,
}

/// The slot of a register in `known` (any part of eax..edi counts as the whole register).
fn index(r: Register) -> Option<usize> {
    Some(match r.full_register32() {
        Register::EAX => 0,
        Register::ECX => 1,
        Register::EDX => 2,
        Register::EBX => 3,
        Register::ESP => 4,
        Register::EBP => 5,
        Register::ESI => 6,
        Register::EDI => 7,
        _ => return None,
    })
}

/// The register a get-pc thunk at `at` loads (`mov reg, [esp]; ret`), when the bytes there are one.
fn thunk(img: &Image, at: u64) -> Option<usize> {
    let b = img.read_at(at, 4)?;
    // 8b /r with mod 00, rm 100 (a SIB byte follows), SIB 0x24 = [esp], then ret
    let reg = usize::from((b[1] >> 3) & 7);
    (b[0] == 0x8b && b[1] & 0xc7 == 0x04 && b[2] == 0x24 && b[3] == 0xc3 && reg != 4).then_some(reg)
}

/// Whether `ip` is in a PLT section, where the ABI keeps the GOT in ebx.
fn in_plt(img: &Image, ip: u64) -> bool {
    img.section_at(ip).is_some_and(|s| s.name.starts_with(".plt"))
}

/// An immediate operand that fits in 32 bits.
fn imm32(ins: &Instruction, op: u32) -> Option<u32> {
    matches!(ins.op_kind(op), OpKind::Immediate32 | OpKind::Immediate8to32).then(|| ins.immediate(op) as u32)
}

impl Pic32 {
    pub(crate) fn new() -> Pic32 {
        Pic32 { known: [None; 8], pushed: None, info: InstructionInfoFactory::new() }
    }

    /// The value of `r` now, when known.
    pub(crate) fn value(&self, r: Register) -> Option<u32> {
        index(r).and_then(|i| self.known[i])
    }

    /// The address the memory operand of `ins` names through a register with a known value, and whether
    /// it stands alone (no index register). Ask before [`step`](Self::step) gives the same instruction.
    pub(crate) fn resolve(&self, img: &Image, ins: &Instruction) -> Option<(u64, bool)> {
        if !(0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Memory) {
            return None;
        }
        let base = ins.memory_base();
        if !base.is_gpr32() || matches!(ins.memory_segment(), Register::FS | Register::GS) {
            return None;
        }
        let v = match self.value(base) {
            Some(v) => v,
            None if base == Register::EBX && in_plt(img, ins.ip()) => u32::try_from(img.got?).ok()?,
            None => return None,
        };
        let a = v.wrapping_add(ins.memory_displacement32());
        Some((u64::from(a), ins.memory_index() == Register::None))
    }

    fn forget(&mut self) {
        self.known = [None; 8];
        self.pushed = None;
    }

    /// Follow one instruction.
    pub(crate) fn step(&mut self, img: &Image, ins: &Instruction) {
        let pushed = self.pushed.take();
        if ins.is_invalid() || ins.mnemonic() == Mnemonic::Hlt {
            self.forget();
            return;
        }
        match ins.flow_control() {
            FlowControl::Return
            | FlowControl::UnconditionalBranch
            | FlowControl::IndirectBranch
            | FlowControl::Interrupt
            | FlowControl::Exception => {
                self.forget();
                return;
            }
            FlowControl::Call | FlowControl::IndirectCall => {
                self.known[0] = None;
                self.known[1] = None;
                self.known[2] = None;
                if ins.op0_kind() == OpKind::NearBranch32 {
                    let t = ins.near_branch_target();
                    if t == ins.next_ip() {
                        self.pushed = Some(ins.next_ip32());
                    } else if let Some(r) = thunk(img, t) {
                        self.known[r] = Some(ins.next_ip32());
                    }
                }
                return;
            }
            _ => {}
        }
        let dst = (ins.op_count() > 0 && ins.op0_kind() == OpKind::Register && ins.op0_register().is_gpr32())
            .then(|| index(ins.op0_register()))
            .flatten();
        match (ins.mnemonic(), dst) {
            (Mnemonic::Pop, Some(r)) if pushed.is_some() => {
                self.known[r] = pushed;
                return;
            }
            (Mnemonic::Add | Mnemonic::Sub, Some(r)) if imm32(ins, 1).is_some() => {
                let k = imm32(ins, 1).unwrap_or(0);
                self.known[r] = self.known[r].map(|v| {
                    if ins.mnemonic() == Mnemonic::Add {
                        v.wrapping_add(k)
                    } else {
                        v.wrapping_sub(k)
                    }
                });
                return;
            }
            (Mnemonic::Mov, Some(r)) if ins.op1_kind() == OpKind::Register && ins.op1_register().is_gpr32() => {
                self.known[r] = self.value(ins.op1_register());
                return;
            }
            (Mnemonic::Mov, Some(r)) if imm32(ins, 1).is_some() => {
                self.known[r] = imm32(ins, 1);
                return;
            }
            (Mnemonic::Lea, Some(r)) => {
                self.known[r] = self.resolve(img, ins).filter(|x| x.1).map(|x| x.0 as u32);
                return;
            }
            _ => {}
        }
        if self.known.iter().all(Option::is_none) {
            return;
        }
        // anything else that writes a register makes it unknown
        let info = self.info.info_options(ins, InstructionInfoOptions::NO_MEMORY_USAGE);
        for u in info.used_registers() {
            if matches!(
                u.access(),
                OpAccess::Write | OpAccess::CondWrite | OpAccess::ReadWrite | OpAccess::ReadCondWrite
            ) {
                if let Some(i) = index(u.register()) {
                    self.known[i] = None;
                }
            }
        }
    }
}
