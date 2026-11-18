//! `--break` and `--hook`: stop, or act, when the program counter reaches an address.

use std::collections::{HashMap, HashSet};

use super::*;

/// What a hook does when execution reaches its address.
#[derive(Debug, Clone, Copy)]
pub(super) enum Action {
    /// Log the registers and go on.
    Print,
    /// Log `len` bytes at the address in register `reg`, and go on.
    Dump(usize, usize),
    /// Write a value into register `reg` and go on.
    Set(usize, u64),
}

/// One hit of a hook: the address and the lines it logged (the first is the summary).
pub(super) struct HookRec {
    pub addr: u64,
    pub lines: Vec<String>,
}

#[derive(Default)]
pub(super) struct Hooks {
    pub breaks: HashSet<u64>,
    pub at: HashMap<u64, Vec<Action>>,
    pub log: Vec<HookRec>,
    /// Every hit, logged or not.
    pub total: usize,
}

/// How one architecture names its registers for `--hook`: the hook code is the same for both.
#[derive(Clone, Copy)]
pub(super) struct Regs {
    /// A register by name.
    pub find: fn(&str) -> Option<usize>,
    /// The name of register `r`.
    pub name: fn(usize) -> String,
    /// The program counter's name in the log (`rip`, `pc`).
    pub pc: &'static str,
    /// What the error says a register may be (`rax..r15`).
    pub range: &'static str,
    /// The index of the stack pointer, which a hook may not set.
    pub sp: usize,
    /// How many registers `print` lists.
    pub count: usize,
}

fn x86_name(r: usize) -> String {
    GPR_NAMES[r].to_string()
}

/// The x86-64 general registers.
pub(super) const X86: Regs = Regs { find: reg_named, name: x86_name, pc: "rip", range: "rax..r15", sp: RSP, count: 16 };

fn reg_of(spec: &str, name: &str, r: &Regs) -> Result<usize> {
    (r.find)(name)
        .ok_or_else(|| Error::Usage(format!("--hook {spec:?}: {name} is not a 64-bit general register ({})", r.range)))
}

/// `ADDR=print`, `ADDR=dump:REG:LEN` or `ADDR=set:REG=VALUE`. The value may be anything `--reg` takes.
pub(super) fn parse(img: &Image, m: &mut Machine, s: &str, regs: &Regs) -> Result<(u64, Action)> {
    let usage = || {
        Error::Usage(format!(
            "--hook {s:?}: write ADDR=print, ADDR=dump:REG:LEN or ADDR=set:REG=VALUE, like --hook 0x401230=dump:{}:64",
            if regs.pc == "rip" { "rdi" } else { "x0" }
        ))
    };
    let (a, act) = s.split_once('=').ok_or_else(usage)?;
    let addr = crate::place::any_addr_in(img, a.trim()).map_err(|e| Error::Usage(format!("--hook {s:?}: {e}")))?;
    let act = act.trim();
    let action = if act == "print" {
        Action::Print
    } else if let Some(t) = act.strip_prefix("dump:") {
        let (r, l) = t.split_once(':').ok_or_else(usage)?;
        let reg = reg_of(s, r, regs)?;
        let len = parse_addr(l.trim()).map_err(|_| usage())?;
        if len == 0 || len > MAX_DUMP as u64 {
            return Err(Error::Usage(format!("--hook {s:?}: LEN is 1 to {MAX_DUMP}")));
        }
        Action::Dump(reg, len as usize)
    } else if let Some(t) = act.strip_prefix("set:") {
        let (r, v) = t.split_once('=').ok_or_else(usage)?;
        let reg = reg_of(s, r, regs)?;
        if reg == regs.sp {
            return Err(Error::Usage(format!("--hook set:{}: the stack pointer is set by emu", (regs.name)(reg))));
        }
        let (val, _) = value_in(m, v.trim(), format!("hook {}", (regs.name)(reg)))?;
        Action::Set(reg, val)
    } else {
        return Err(usage());
    };
    Ok((addr, action))
}

impl Machine<'_> {
    /// Run the hooks at `pc` (the caller checked that there are some) on `gpr`, the registers of the
    /// architecture running.
    pub(super) fn run_hooks(&mut self, pc: u64, gpr: &mut [u64], regs: &Regs) {
        let list = match self.hooks.at.get(&pc) {
            Some(l) => l.clone(),
            None => return,
        };
        for act in list {
            self.hooks.total += 1;
            let room = self.hooks.log.len() < self.max_calls;
            let mut lines = Vec::new();
            match act {
                Action::Print => {
                    if room {
                        let shown: Vec<String> = (0..regs.count)
                            .filter(|&r| gpr[r] != 0)
                            .map(|r| format!("{}={:#x}", (regs.name)(r), gpr[r]))
                            .collect();
                        lines.push(format!("print  {}={pc:#x} {}", regs.pc, shown.join(" ")));
                    }
                }
                Action::Dump(r, len) => {
                    if room {
                        let at = gpr[r];
                        lines.push(format!("dump   {}={at:#x} +{len}", (regs.name)(r)));
                        lines.extend(hex_lines(&self.mem, at, len));
                    }
                }
                Action::Set(r, v) => {
                    let old = gpr[r];
                    gpr[r] = v;
                    if room {
                        lines.push(format!("set    {} = {v:#x} (was {old:#x})", (regs.name)(r)));
                    }
                }
            }
            if room {
                self.hooks.log.push(HookRec { addr: pc, lines });
            }
        }
    }
}
