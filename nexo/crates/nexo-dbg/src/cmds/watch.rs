//! Hardware watchpoints.

use super::*;

impl Session {
    fn slots(&self) -> [Option<HwSlot>; 4] {
        let mut s = [None; 4];
        for w in &self.wps {
            s[w.slot] = Some(w.hw);
        }
        s
    }

    pub(super) fn apply_watches(&mut self) -> Result<()> {
        if self.state == State::Exited {
            return Ok(());
        }
        let s = self.slots();
        self.t.set_watch(&s)
    }

    pub(super) fn cmd_watch(&mut self, a: &Args) -> Result<Output> {
        let Some(at) = a.pos.first() else { return usage("watch <address|expr> [length 1|2|4|8] [r|w|x]") };
        self.need_stopped()?;
        let addr = self.addr_of(at)?;
        let mut len = 8u8;
        let mut acc = Access::Write;
        for w in &a.pos[1..] {
            match w.as_str() {
                "w" => acc = Access::Write,
                "r" | "rw" => acc = Access::ReadWrite,
                "x" => acc = Access::Exec,
                n => {
                    len = parse_addr(n)
                        .ok()
                        .and_then(|v| u8::try_from(v).ok())
                        .filter(|v| [1, 2, 4, 8].contains(v))
                        .ok_or_else(|| crate::Error::Usage("length is 1, 2, 4 or 8".into()))?
                }
            }
        }
        if acc == Access::Exec {
            len = 1;
        }
        if addr % len as u64 != 0 {
            return usage(format!("{addr:#x} is not aligned to {len}: the CPU watches aligned pieces"));
        }
        let Some(slot) = (0..4).find(|i| !self.wps.iter().any(|w| w.slot == *i)) else {
            return fail("all 4 hardware watches are used: `delete <id>` one first");
        };
        let id = self.new_id();
        let last = self.read_vec(addr, len as usize);
        self.wps.push(Wp { id, slot, hw: HwSlot { addr, len, access: acc }, last, hits: 0 });
        if let Err(e) = self.apply_watches() {
            self.wps.pop();
            return Err(e);
        }
        Ok(rows("watch", vec![format!("watch #{id} {addr:#x} len {len}")], json!({"id": id})))
    }
}
