//! The vector unit: the SSE moves and bit operations compiled C uses.

use iced_x86::Instruction;

use super::cpu::{Cpu, Event};
use super::mem::Mem;

/// Run one vector instruction; `Ok(false)` when it is not one this unit knows.
pub(super) fn exec(_cpu: &mut Cpu, _mem: &mut Mem, _ins: &Instruction) -> Result<bool, Event> {
    Ok(false)
}
