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

fn reg_of(spec: &str, name: &str) -> Result<usize> {
    reg_named(name)
        .ok_or_else(|| Error::Usage(format!("--hook {spec:?}: {name} is not a 64-bit general register (rax..r15)")))
}

/// `ADDR=print`, `ADDR=dump:REG:LEN` or `ADDR=set:REG=VALUE`. The value may be anything `--reg` takes.
pub(super) fn parse(img: &Image, m: &mut Machine, s: &str) -> Result<(u64, Action)> {
    let usage = || {
        Error::Usage(format!(
            "--hook {s:?}: write ADDR=print, ADDR=dump:REG:LEN or ADDR=set:REG=VALUE, like --hook 0x401230=dump:rdi:64"
        ))
    };
    let (a, act) = s.split_once('=').ok_or_else(usage)?;
    let addr = crate::place::any_addr_in(img, a.trim()).map_err(|e| Error::Usage(format!("--hook {s:?}: {e}")))?;
    let act = act.trim();
    let action = if act == "print" {
        Action::Print
    } else if let Some(t) = act.strip_prefix("dump:") {
        let (r, l) = t.split_once(':').ok_or_else(usage)?;
        let reg = reg_of(s, r)?;
        let len = parse_addr(l.trim()).map_err(|_| usage())?;
        if len == 0 || len > MAX_DUMP as u64 {
            return Err(Error::Usage(format!("--hook {s:?}: LEN is 1 to {MAX_DUMP}")));
        }
        Action::Dump(reg, len as usize)
    } else if let Some(t) = act.strip_prefix("set:") {
        let (r, v) = t.split_once('=').ok_or_else(usage)?;
        let reg = reg_of(s, r)?;
        if reg == RSP {
            return Err(Error::Usage("--hook set:rsp: the stack pointer is set by emu".into()));
        }
        let (val, _) = value_in(m, v.trim(), format!("hook {}", GPR_NAMES[reg]))?;
        Action::Set(reg, val)
    } else {
        return Err(usage());
    };
    Ok((addr, action))
}

impl Machine<'_> {
    /// Run the hooks at `rip` (the caller checked that there are some).
    pub(super) fn run_hooks(&mut self, rip: u64) {
        let list = match self.hooks.at.get(&rip) {
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
                        let regs: Vec<String> = (0..16)
                            .filter(|&r| self.cpu.gpr[r] != 0)
                            .map(|r| format!("{}={:#x}", GPR_NAMES[r], self.cpu.gpr[r]))
                            .collect();
                        lines.push(format!("print  rip={rip:#x} {}", regs.join(" ")));
                    }
                }
                Action::Dump(r, len) => {
                    if room {
                        let at = self.cpu.gpr[r];
                        lines.push(format!("dump   {}={at:#x} +{len}", GPR_NAMES[r]));
                        lines.extend(hex_lines(&self.mem, at, len));
                    }
                }
                Action::Set(r, v) => {
                    let old = self.cpu.gpr[r];
                    self.cpu.gpr[r] = v;
                    if room {
                        lines.push(format!("set    {} = {v:#x} (was {old:#x})", GPR_NAMES[r]));
                    }
                }
            }
            if room {
                self.hooks.log.push(HookRec { addr: rip, lines });
            }
        }
    }
}
