//! Whether a local's value at a stop is its own yet. Its stack slot exists for the whole call, so before the code
//! reaches its declaration, or outside the lexical block it is declared in, the slot holds whatever an earlier
//! call or an earlier pass left there. The value is still shown, with that fact beside it; a parameter is stored
//! by the prologue and is never marked.

use super::scope::eval;
use super::*;
use crate::lines::short_path;

/// Whether `pc` falls in one of `ranges`.
fn holds(ranges: &[(u64, u64)], pc: u64) -> bool {
    ranges.iter().any(|&(lo, hi)| pc >= lo && pc < hi)
}

/// A variable at a fixed address (a `static` in a function or a block): it is set before the program runs.
fn fixed(v: &Var) -> bool {
    // DW_OP_addr, DW_OP_addrx, DW_OP_GNU_addr_index; a thread-local ends in a push_tls_address
    matches!(&v.loc, Loc::Expr(e) if matches!(e.first(), Some(0x03 | 0xa1 | 0xfb)) || matches!(e.last(), Some(0x9b | 0xe0)))
}

impl Vars {
    /// The locals of the function at `pc` (a file address) declared in a lexical block that does not hold `pc`:
    /// not live here, in the order DWARF lists them. A `static` in such a block is left out.
    pub(super) fn out_of_block(&self, pc: u64) -> Vec<(&Var, gimli::Encoding, Option<&[u8]>)> {
        let Some(f) = self.func_at(pc) else { return Vec::new() };
        let gone = |v: &&Var| !fixed(v) && v.scope.as_ref().is_some_and(|r| !holds(r, pc));
        f.vars.iter().filter(gone).map(|v| (v, f.enc, f.frame_base.as_deref())).collect()
    }

    /// Why `v`'s value at this stop is not its own: `not live here: its block is p.c:16-18, stopped at p.c:20`, or
    /// `not assigned yet: declared at p.c:9, stopped at p.c:8`. `None` when the stop is past its declaration, for a
    /// parameter or a `static`, and without a line table.
    pub(super) fn unassigned(&self, v: &Var, regs: &mut Regs) -> Option<String> {
        if v.param || fixed(v) {
            return None;
        }
        let lines = regs.lines?;
        let pc = regs.rip.wrapping_sub(regs.bias);
        let f = self.func_at(pc)?;
        // in inlined code this is the line of the outermost call, a line of this function
        let here = lines.at(pc)?;
        let stop = format!("{}:{}", short_path(&here.file), here.line);
        // a block's address ranges say exactly where its variables are live, whatever order its code is in
        if let Some(r) = v.scope.as_ref().filter(|r| !holds(r, pc)) {
            let block = match lines.span_of(r) {
                Some((file, a, b)) if a == b => format!("{}:{a}", short_path(file)),
                Some((file, a, b)) => format!("{}:{a}-{b}", short_path(file)),
                None => format!("{:#x}..{:#x}", r[0].0, r[0].1),
            };
            return Some(format!("not live here: its block is {block}, stopped at {stop}"));
        }
        // Here the stop is inside the block that declares `v` (or `v` belongs to the whole function), so the line
        // comparison is sound: statements of one block run in source order, and nested blocks run where their
        // source sits, so code on a line before the declaration runs before it on this pass through the block. A
        // loop body is a block of its own (DW_TAG_lexical_block) and C runs a declaration's initialiser each time
        // it is reached, so on a later pass the slot holds the last pass's value: "in this pass" says so. A backward
        // `goto` over a declaration in the function's own block is the one case where an earlier pass assigned it.
        // Only a stack slot is judged: a location list (optimised code) says itself where the value is live, and
        // optimised code does not run in line order. A stop before the prologue ends already says the slots are
        // not stored yet.
        if regs.early.is_some() || !Self::in_slot(v) {
            return None;
        }
        let (file, line) = v.decl?;
        if self.files.get(file as usize) != Some(&here.file) {
            return None;
        }
        let pass = if v.scope.is_some() { " in this pass" } else { "" };
        let decl = format!("{}:{line}", short_path(&here.file));
        if line > here.line {
            return Some(format!("not assigned yet{pass}: declared at {decl}, stopped at {stop}"));
        }
        // the declaration's own line: before its first instruction in the block nothing of it has run (`break f`
        // stops there when the first statement declares a local); later in the line the initialiser may have
        // run (a for loop's condition and step share the line of its `int i = 0`), so nothing is said
        let within = v.scope.as_deref().unwrap_or(&f.ranges);
        if line == here.line && here.inside.is_none() && lines.line_start_in(&here.file, line, within) == Some(pc) {
            return Some(format!("not assigned yet{pass}: declared at {decl}, stopped at the start of that line"));
        }
        if line == here.line {
            return self.by_address(v, f, regs, pass, &decl);
        }
        None
    }

    /// The line cannot say (the declaration is on the stop's line, past its first instruction: a whole function
    /// on one line, or a `for` header), so the code does: does an instruction between the function's entry and the
    /// stop write to the local's slot? A fact read from the binary when it is found; otherwise the doubt is said.
    fn by_address(&self, v: &Var, f: &Func, regs: &mut Regs, pass: &str, decl: &str) -> Option<String> {
        use nexo_bin::SlotUse;
        let doubt = || Some(format!("may not be assigned yet{pass}: its line, {decl}, is the stop line"));
        let Loc::Expr(e) = &v.loc else { return doubt() };
        let (Some(&(lo, hi)), Some(At::Mem(slot))) = (f.ranges.first(), eval(e, f.enc, f.frame_base.as_deref(), regs))
        else {
            return doubt();
        };
        let pc = regs.rip.wrapping_sub(regs.bias);
        let size = self.size(View(v.ty, 0));
        let Some(code) = (regs.rd)(lo.wrapping_add(regs.bias), hi.saturating_sub(lo).min(1 << 16) as usize) else {
            return doubt();
        };
        // a frame pointer holds the CFA less 16 for the whole body; at the stop `leave` may have restored it
        let Some(rbp) = regs.cfa.map(|c| c.wrapping_sub(16)) else { return doubt() };
        // the bytes are the running program's: the code is at its load address
        match nexo_bin::slot_use(&code, lo.wrapping_add(regs.bias), pc.wrapping_add(regs.bias), slot, size, rbp) {
            SlotUse::Stored => None,
            SlotUse::Untouched => Some(format!(
                "not assigned yet{pass}: declared at {decl}, the stop's own line, and no instruction between the function's entry and the stop writes to its slot"
            )),
            SlotUse::AddressTaken => Some(format!(
                "may not be assigned yet{pass}: its line, {decl}, is the stop line, and its address was taken before the stop"
            )),
            SlotUse::Unclear => doubt(),
        }
    }

    /// A variable's value as text, with why it is not its own yet when that is known (see `unassigned`).
    pub(super) fn shown_noted(
        &self,
        v: &Var,
        at: Option<At>,
        regs: &mut Regs,
        depth: usize,
        inreg: &[super::entry::InReg],
    ) -> String {
        let val = self.shown(v, at, regs, depth, inreg);
        match self.unassigned(v, regs) {
            Some(why) => format!("{val} ({why})"),
            None => val,
        }
    }
}
