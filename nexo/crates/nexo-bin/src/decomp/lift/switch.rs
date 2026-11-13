//! Jump-table (switch) recovery: the bound compare, the table, and indirect jumps that dispatch through one.

use super::*;

/// Look at the instructions before an indirect `jmp` for a jump table.
pub(super) fn find_switch(env: &dyn Env, d: &Decoded, jmp_idx: usize, start: u64, end: u64) -> Option<SwitchInfo> {
    use Mnemonic as M;
    let jmp = &d.ins[jmp_idx];
    let in_fn = |a: u64| a >= start && a < end;
    let lo = jmp_idx.saturating_sub(10);
    // Absolute table: jmp [table + idx*8]
    if jmp.op0_kind() == OpKind::Memory
        && jmp.memory_index() != Register::None
        && jmp.memory_index_scale() == 8
        && jmp.memory_base() == Register::None
    {
        let table = jmp.memory_displacement64();
        let idx = jmp.memory_index();
        let bound = find_bound(d, jmp_idx, idx);
        let count = bound.map(|x| x.0 + 1).unwrap_or(256);
        let mut targets = Vec::new();
        for k in 0..count {
            let raw = env.read(table + k * 8, 8)?;
            let t = u64::from_le_bytes(raw.try_into().ok()?);
            if !in_fn(t) {
                if bound.is_some() {
                    return None;
                }
                break;
            }
            targets.push(t);
        }
        if targets.is_empty() {
            return None;
        }
        return Some(SwitchInfo {
            hide_from: jmp_idx,
            targets,
            idx_reg: gpr_number(idx),
            idx_bits: if idx.is_gpr32() { 32 } else { 64 },
            bound: bound.map(|x| x.0),
        });
    }
    // jmp reg, reg = base + sext(dword [base + idx*4])
    if jmp.op0_kind() == OpKind::Register {
        let target_reg = jmp.op0_register().full_register();
        // Absolute table read into the register just before the jump (an unoptimised non-PIE build):
        // mov reg, [table + idx*8] ; jmp reg
        let load = (lo..jmp_idx).rev().take_while(|&k| d.ins[k].flow_control() == FlowControl::Next).find(|&k| {
            let i = &d.ins[k];
            i.op0_kind() == OpKind::Register
                && i.op0_register().full_register() == target_reg
                && !matches!(i.mnemonic(), M::Cmp | M::Test)
        });
        if let Some(l) = load {
            let ld = &d.ins[l];
            if ld.mnemonic() == M::Mov
                && ld.op0_register().size() == 8
                && ld.op1_kind() == OpKind::Memory
                && ld.memory_index() != Register::None
                && ld.memory_index_scale() == 8
                && ld.memory_base() == Register::None
                && !ld.is_ip_rel_memory_operand()
            {
                let table = ld.memory_displacement64();
                let idx = ld.memory_index();
                let bound = find_bound(d, l, idx).or_else(|| find_bound_of_reloaded(d, l, idx));
                let count = bound.map(|x| x.0 + 1).unwrap_or(256);
                let mut targets = Vec::new();
                for k in 0..count {
                    let t = u64::from_le_bytes(env.read(table + k * 8, 8)?.try_into().ok()?);
                    if !in_fn(t) {
                        if bound.is_some() {
                            return None;
                        }
                        break;
                    }
                    targets.push(t);
                }
                if targets.is_empty() {
                    return None;
                }
                return Some(SwitchInfo {
                    hide_from: l,
                    targets,
                    idx_reg: gpr_number(idx),
                    idx_bits: if idx.is_gpr32() { 32 } else { 64 },
                    bound: bound.map(|x| x.0),
                });
            }
        }
        let (mut add_i, mut load_i, mut lea_i) = (None, None, None);
        // the other register of `add target, addend`: the offset may be loaded into it instead of into the target
        let mut addend = Register::None;
        let mut k = jmp_idx;
        while k > lo {
            k -= 1;
            let i = &d.ins[k];
            if i.flow_control() != FlowControl::Next {
                break;
            }
            match i.mnemonic() {
                M::Add
                    if i.op0_register().full_register() == target_reg
                        && i.op1_kind() == OpKind::Register
                        && add_i.is_none() =>
                {
                    add_i = Some(k);
                    addend = i.op1_register().full_register();
                }
                M::Movsxd | M::Mov
                    if i.op0_kind() == OpKind::Register
                        && (i.op0_register().full_register() == target_reg
                            || i.op0_register().full_register() == addend)
                        && i.op1_kind() == OpKind::Memory
                        && load_i.is_none()
                        && add_i.is_some() =>
                {
                    load_i = Some(k)
                }
                M::Lea if load_i.is_some() && lea_i.is_none() && i.is_ip_rel_memory_operand() => lea_i = Some(k),
                _ => {}
            }
        }
        let (a, l) = (add_i?, load_i?);
        let load = &d.ins[l];
        let add = &d.ins[a];
        if load.memory_index() == Register::None {
            return None;
        }
        // a table of 32-bit offsets read as [table + idx*4], or (an unoptimised mingw build) as [scaled + table]
        // with the index scaled by an earlier `lea scaled, [idx*4]` and the table address in a register
        let mut scaled_i = None;
        let idx = if load.memory_index_scale() == 4 {
            load.memory_index()
        } else if load.memory_index_scale() == 1 && load.memory_base() != Register::None && lea_i.is_some() {
            let table_reg = d.ins[lea_i?].op0_register().full_register();
            let regs = [load.memory_base().full_register(), load.memory_index().full_register()];
            if !regs.contains(&table_reg) {
                return None;
            }
            let scaled = regs.into_iter().find(|&r| r != table_reg)?;
            let s = (lo..l).rev().take_while(|&k| d.ins[k].flow_control() == FlowControl::Next).find(|&k| {
                let i = &d.ins[k];
                i.mnemonic() == M::Lea && i.op0_register().full_register() == scaled
            })?;
            let sc = &d.ins[s];
            if sc.memory_base() != Register::None
                || sc.memory_index() == Register::None
                || sc.memory_index_scale() != 4
                || sc.memory_displacement64() != 0
            {
                return None;
            }
            scaled_i = Some(s);
            sc.memory_index()
        } else {
            return None;
        };
        // the table base: a lea, or (PE) a register holding the image base plus a displacement
        let (table, base) = if let Some(le) = lea_i {
            let lea = &d.ins[le];
            let extra = if scaled_i.is_some() { load.memory_displacement64() } else { 0 };
            (lea.ip_rel_memory_address().wrapping_add(extra), 0u64)
        } else if env.is_pe() {
            (env.image_base() + load.memory_displacement64(), env.image_base())
        } else {
            return None;
        };
        let _ = add;
        let from = scaled_i.unwrap_or(l);
        let bound = find_bound(d, from, idx).or_else(|| find_bound_of_reloaded(d, from, idx));
        let count = bound.map(|x| x.0 + 1).unwrap_or(256);
        let mut targets = Vec::new();
        for k in 0..count {
            let raw = env.read(table + k * 4, 4)?;
            let v = i32::from_le_bytes(raw.try_into().ok()?);
            let t =
                if env.is_pe() && base != 0 { base + (v as u32 as u64) } else { table.wrapping_add(v as i64 as u64) };
            if !in_fn(t) {
                if bound.is_some() {
                    return None;
                }
                break;
            }
            targets.push(t);
        }
        if targets.is_empty() {
            return None;
        }
        let first = lea_i.map_or(l, |x| x.min(l)).min(a).min(scaled_i.unwrap_or(usize::MAX));
        return Some(SwitchInfo {
            hide_from: first,
            targets,
            idx_reg: gpr_number(idx),
            idx_bits: if idx.is_gpr32() { 32 } else { 64 },
            bound: bound.map(|x| x.0),
        });
    }
    None
}

/// The highest valid index from a `cmp idx, N; ja default` before position `at`.
fn find_bound(d: &Decoded, at: usize, idx: Register) -> Option<(u64, u64)> {
    let mut k = at;
    let mut steps = 0;
    while k > 0 && steps < 14 {
        k -= 1;
        steps += 1;
        let i = &d.ins[k];
        if i.mnemonic() == Mnemonic::Cmp
            && i.op0_kind() == OpKind::Register
            && i.op0_register().full_register() == idx.full_register()
            && i.op1_kind() != OpKind::Register
            && i.op1_kind() != OpKind::Memory
        {
            let n = i.immediate(1);
            // the branch right after the compare
            let j = d.ins.get(k + 1)?;
            if matches!(j.mnemonic(), Mnemonic::Ja | Mnemonic::Jae) {
                let n = if j.mnemonic() == Mnemonic::Ja { n } else { n.wrapping_sub(1) };
                return Some((n & 0xffff_ffff, j.near_branch_target()));
            }
            return None;
        }
    }
    None
}

/// The highest valid index when the index register was reloaded from a stack slot just before the table read and
/// the guard compared that slot: `cmp [slot], N ; ja default ; ... ; mov idx, [slot]` (an unoptimised build).
fn find_bound_of_reloaded(d: &Decoded, at: usize, idx: Register) -> Option<(u64, u64)> {
    let reload = (at.saturating_sub(4)..at).rev().find(|&k| {
        let i = &d.ins[k];
        i.mnemonic() == Mnemonic::Mov
            && i.op0_kind() == OpKind::Register
            && i.op0_register().full_register() == idx.full_register()
            && i.op1_kind() == OpKind::Memory
    })?;
    let slot = &d.ins[reload];
    let same_slot = |i: &Instruction| {
        i.memory_base() == slot.memory_base()
            && i.memory_index() == slot.memory_index()
            && i.memory_displacement64() == slot.memory_displacement64()
    };
    let mut k = reload;
    let mut steps = 0;
    while k > 0 && steps < 14 {
        k -= 1;
        steps += 1;
        let i = &d.ins[k];
        // `mov reg, idx ; mov [slot], reg ; sub reg, N ; ja default` (clang): the guard works on a copy of the index
        if i.mnemonic() == Mnemonic::Sub
            && i.op0_kind() == OpKind::Register
            && i.op1_kind() != OpKind::Register
            && i.op1_kind() != OpKind::Memory
        {
            let j = d.ins.get(k + 1);
            let stored = (k.saturating_sub(3)..k).any(|m| {
                let st = &d.ins[m];
                st.mnemonic() == Mnemonic::Mov
                    && st.op0_kind() == OpKind::Memory
                    && st.op1_kind() == OpKind::Register
                    && st.op1_register().full_register() == i.op0_register().full_register()
                    && same_slot(st)
            });
            if let Some(j) = j.filter(|j| stored && matches!(j.mnemonic(), Mnemonic::Ja | Mnemonic::Jae)) {
                let n = i.immediate(1);
                let n = if j.mnemonic() == Mnemonic::Ja { n } else { n.wrapping_sub(1) };
                return Some((n & 0xffff_ffff, j.near_branch_target()));
            }
        }
        if i.mnemonic() == Mnemonic::Cmp
            && i.op0_kind() == OpKind::Memory
            && same_slot(i)
            && i.op1_kind() != OpKind::Register
            && i.op1_kind() != OpKind::Memory
        {
            let j = d.ins.get(k + 1)?;
            if matches!(j.mnemonic(), Mnemonic::Ja | Mnemonic::Jae) {
                let n = i.immediate(1);
                let n = if j.mnemonic() == Mnemonic::Ja { n } else { n.wrapping_sub(1) };
                return Some((n & 0xffff_ffff, j.near_branch_target()));
            }
            return None;
        }
    }
    None
}

/// Whether a first table entry at `entry` is an address inside the function (so the table is a switch of it).
/// An entry that cannot be read is not known to be anything else, so it counts.
fn entry_is_here(env: &dyn Env, entry: u64, start: u64, end: u64) -> bool {
    let e = env.read(entry, 8).and_then(|r| <[u8; 8]>::try_from(r).ok());
    e.is_none_or(|e| (start..end).contains(&u64::from_le_bytes(e)))
}

/// An indirect `jmp` at instruction `k` that reads like a dispatch through a table of this function, which
/// `find_switch` could not recover. A memory operand `[table + idx*8]`, or a register loaded in the 64
/// instructions before it from such an operand, counts unless the table is readable and holds addresses
/// outside the function (a table of function pointers, a real tail call). So does the PIE pattern: a
/// rip-relative `lea` for the table base and an `add jump_reg, reg` (or `add jump_reg, [table+idx*8]`) that adds it to
/// the offset. The table base may sit in a register loaded by a rip-relative `lea` (how every 64-bit Windows image reaches
/// a table, as its addresses do not fit a 32-bit displacement): its first pointer is read the same way.
pub(super) fn jumps_through_a_table(env: &dyn Env, d: &Decoded, k: usize, start: u64, end: u64) -> bool {
    let jmp = &d.ins[k];
    // The address of the table a 64-bit read at instruction `at` starts from, when it is known: the
    // displacement of an absolute table, or a register base loaded by a rip-relative `lea` before it.
    let table_of = |x: &Instruction, at: usize| -> Option<u64> {
        if x.is_ip_rel_memory_operand() {
            return None;
        }
        if x.memory_base() == Register::None {
            return Some(x.memory_displacement64());
        }
        let base = x.memory_base().full_register();
        let lea = d.ins[at.saturating_sub(64)..at].iter().rev().find(|l| {
            l.mnemonic() == Mnemonic::Lea
                && l.op0_kind() == OpKind::Register
                && l.op0_register().full_register() == base
        })?;
        (lea.is_ip_rel_memory_operand()).then(|| lea.memory_displacement64().wrapping_add(x.memory_displacement64()))
    };
    // `None`: not an indexed read; `Some(false)`: a table of addresses outside this function; `Some(true)`: a
    // switch of it, or a table not known to be anything else (a register base we could not follow, 32-bit offsets)
    let indexed = |x: &Instruction, at: usize, wide: bool| -> Option<bool> {
        if x.memory_index() == Register::None {
            return None;
        }
        match table_of(x, at) {
            Some(table) if wide => Some(entry_is_here(env, table, start, end)),
            _ => Some(true),
        }
    };
    if jmp.op0_kind() == OpKind::Memory {
        return indexed(jmp, k, true).unwrap_or(false);
    }
    if jmp.op0_kind() != OpKind::Register {
        return false;
    }
    let target = jmp.op0_register().full_register();
    let from = k.saturating_sub(64);
    let loaded = d.ins[from..k].iter().enumerate().any(|(i, x)| {
        matches!(x.mnemonic(), Mnemonic::Mov | Mnemonic::Movsxd)
            && x.op0_kind() == OpKind::Register
            && x.op0_register().full_register() == target
            && x.op1_kind() == OpKind::Memory
            && indexed(x, from + i, x.mnemonic() == Mnemonic::Mov && x.op0_register().size() == 8).unwrap_or(false)
    });
    let lea = d.ins[from..k].iter().any(|x| x.mnemonic() == Mnemonic::Lea && x.is_ip_rel_memory_operand());
    let add = d.ins[from..k].iter().any(|x| {
        x.mnemonic() == Mnemonic::Add
            && x.op0_kind() == OpKind::Register
            && x.op0_register().full_register() == target
            && (x.op1_kind() == OpKind::Register
                || (x.op1_kind() == OpKind::Memory && x.memory_index() != Register::None))
    });
    loaded || (lea && add)
}
