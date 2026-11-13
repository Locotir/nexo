//! `calls`: the real target of every indirect call.

use super::*;

impl Session {
    /// `calls` logs the real target of every `call reg` / `call [mem]` in the program as it runs; `calls off` stops.
    pub(super) fn cmd_calls(&mut self, a: &Args) -> Result<Output> {
        let n_on = self.bps.iter().filter(|b| b.call).count();
        if a.pos.first().is_some_and(|w| w == "off") {
            self.bps.retain(|b| !b.call);
            return Ok(rows("calls", vec![format!("indirect-call logging off ({n_on} sites)")], json!({})));
        }
        if !a.pos.is_empty() {
            // named sites only: `calls 0x40114b main+0x20`. Check every one before changing anything.
            let mut sites: Vec<u64> = Vec::new();
            let mut repeated = 0;
            for w in &a.pos {
                let at = self.addr_of(w)?;
                if !self.is_indirect_call(at) {
                    return fail(format!(
                        "{w} ({at:#x}) is not an indirect call (`call reg` or `call [mem]`); logging was left as it was. \
                         `dis {w}` shows what is there, or `calls` alone logs every indirect call in the program"
                    ));
                }
                if sites.contains(&at) {
                    repeated += 1;
                } else {
                    sites.push(at);
                }
            }
            self.bps.retain(|b| !b.call);
            let n = sites.len();
            for at in sites {
                let id = self.new_id();
                self.bps.push(Bp {
                    id,
                    addr: at,
                    enabled: true,
                    cond: None,
                    after: 0,
                    hits: 0,
                    log: None,
                    once: false,
                    call: true,
                    actions: Vec::new(),
                });
            }
            let mut l = vec![format!(
                "logging {n} indirect call site{}: `cont`, then `log` shows each target; they are saved for `nexo bin decomp`",
                if n == 1 { "" } else { "s" }
            )];
            if repeated > 0 {
                l.push(format!("{repeated} repeated site{} ignored", if repeated == 1 { "" } else { "s" }));
            }
            if n_on > 0 {
                l.push(format!(
                    "replaced the {n_on} site{} that {} being logged",
                    if n_on == 1 { "" } else { "s" },
                    if n_on == 1 { "was" } else { "were" }
                ));
            }
            return Ok(rows("calls", l, json!({"sites": n, "replaced": n_on, "repeated": repeated})));
        }
        if n_on > 0 {
            return Ok(rows(
                "calls",
                vec![format!("logging {n_on} indirect calls: `log` shows the targets, `calls off` stops")],
                json!({"sites": n_on}),
            ));
        }
        let (sites, more) = nexo_bin::indirect_calls(self.mods.main_image());
        if sites.is_empty() {
            return fail("no `call reg` or `call [mem]` in this program's code: every call goes to a fixed place (`dis` or `nexo bin` shows them)");
        }
        let bias = self.mods.main_bias();
        let n = sites.len();
        for fa in sites {
            let id = self.new_id();
            self.bps.push(Bp {
                id,
                addr: fa.wrapping_add(bias),
                enabled: true,
                cond: None,
                after: 0,
                hits: 0,
                log: None,
                once: false,
                call: true,
                actions: Vec::new(),
            });
        }
        let mut l = vec![format!(
            "logging {n} indirect calls: `cont`, then `log` shows each target by name; `calls off` stops"
        )];
        if more > 0 {
            l.push(format!("{more} more sites past the cap were left out"));
        }
        Ok(rows("calls", l, json!({"sites": n})))
    }

    /// Whether the instruction at `addr` is a `call reg` / `call [mem]` (our own int3 on its first byte is looked through).
    fn is_indirect_call(&mut self, addr: u64) -> bool {
        let mut bytes = self.read_vec(addr, 16);
        if let (Some(b), Some(o)) = (bytes.first_mut(), self.inserted_byte(addr)) {
            *b = o;
        }
        nexo_bin::indirect_call_target(&bytes, addr, &|_| Some(0), &|_| Some(0)).is_some()
    }
}
