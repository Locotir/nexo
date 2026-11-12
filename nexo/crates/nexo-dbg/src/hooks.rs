//! Calls into libraries (`ltrace`) and declared hooks on functions (`hook`): a breakpoint at the
//! entry reads the arguments with their types, a breakpoint at the return address reads the result.
//! A call is written down when it returns; one that never returned says so.

use std::collections::BTreeMap;

use nexo_bin::{Output, Proto};
use serde_json::json;

use crate::cmds::Args;
use crate::engine::{rows, Session, Why};
use crate::platform::{Os, Tid};
use crate::regs::Regs;
use crate::trace::Kind;
use crate::{fail, usage, Result};

/// Calls in flight at once (deeper recursion is not followed to its return).
const PENDING_MAX: usize = 4096;

/// A library function as the program reaches it.
#[derive(Debug, Clone)]
pub struct Site {
    pub name: String,
    /// `None`: a stub entered by a call. `Some(len)`: a `call [slot]` of this length.
    pub call_len: Option<u8>,
}

/// `ltrace` is on: the sites it watches (memory address) and the names it keeps.
#[derive(Debug, Clone, Default)]
pub struct Lib {
    pub sites: BTreeMap<u64, Site>,
    pub names: Vec<String>,
}

/// A call whose entry was seen, waiting for its return.
#[derive(Debug, Clone)]
pub struct Pending {
    pub tid: Tid,
    pub ret_addr: u64,
    /// The stack pointer right after the return.
    pub sp_after: u64,
    pub kind: Kind,
    pub name: String,
    pub text: String,
    pub ret_ty: Option<String>,
}

/// The registers that carry the first integer arguments.
fn arg_regs(os: Os) -> &'static [&'static str] {
    match os {
        Os::Linux => &["rdi", "rsi", "rdx", "rcx", "r8", "r9"],
        Os::Windows => &["rcx", "rdx", "r8", "r9"],
    }
}

fn is_float(ty: &str) -> bool {
    let t = ty.trim();
    !t.contains('*') && (t.contains("double") || t.contains("float"))
}

fn is_ptr(ty: &str) -> bool {
    ty.contains('*')
}

fn is_text(ty: &str) -> bool {
    let t = ty.replace(' ', "");
    t == "constchar*" || t == "charconst*"
}

/// Integer types by how many bytes they have and whether they carry a sign.
fn int_kind(ty: &str) -> (u32, bool) {
    let t = ty.trim();
    let unsigned = t.contains("unsigned") || t.starts_with("size_t") || t.starts_with("uint") || t == "bool";
    let bytes = if t.contains("long") || t.contains("size_t") || t.contains("64") || t == "intptr_t" {
        8
    } else if t.contains("short") || t.contains("16") {
        2
    } else if t == "char" || t.contains("int8") || t == "bool" || t.ends_with(" char") {
        1
    } else {
        4
    };
    (bytes, unsigned)
}

impl Session {
    /// A value of type `ty` as a person reads it. Text is read from memory now.
    fn typed(&mut self, ty: &str, v: u64) -> String {
        if is_float(ty) {
            return "(floating point, not read)".into();
        }
        if is_text(ty) {
            if v == 0 {
                return "NULL".into();
            }
            return self.cstr(v);
        }
        if is_ptr(ty) {
            return if v == 0 { "NULL".into() } else { format!("{v:#x}") };
        }
        let (bytes, unsigned) = int_kind(ty);
        let mask = if bytes == 8 { u64::MAX } else { (1u64 << (bytes * 8)) - 1 };
        let x = v & mask;
        if unsigned {
            x.to_string()
        } else {
            let shift = 64 - bytes * 8;
            (((x << shift) as i64) >> shift).to_string()
        }
    }

    /// `name(arg, ...)` from the registers at the entry. `named`: `x=12` instead of `12`.
    fn call_args(&mut self, name: &str, proto: Option<&Proto>, r: &Regs, named: bool) -> (String, Option<String>) {
        let regs = arg_regs(self.t.os());
        let reg = |i: usize| regs.get(i).and_then(|n| r.get(n));
        if let Some(p) = proto {
            let mut parts = Vec::new();
            // floating point arguments travel in xmm registers, not in the integer ones
            let mut gi = 0;
            for (ty, an) in &p.args {
                let shown = if is_float(ty) {
                    "(floating point, not read)".to_string()
                } else {
                    let v = reg(gi);
                    gi += 1;
                    match v {
                        Some(v) => self.typed(ty, v),
                        None => "(on the stack, not read)".into(),
                    }
                };
                parts.push(if named && !an.is_empty() { format!("{an}={shown}") } else { shown });
            }
            if p.variadic {
                parts.push("...".into());
            }
            return (format!("{name}({})", parts.join(", ")), Some(p.ret.clone()));
        }
        if let Some((n, _)) = nexo_bin::arg_count(name) {
            let v: Vec<String> = (0..n).map(|i| reg(i).map(|x| format!("{x:#x}")).unwrap_or("?".into())).collect();
            return (format!("{name}({})", v.join(", ")), None);
        }
        let v: Vec<String> = (0..3).map(|i| format!("{}={:#x}", regs[i], reg(i).unwrap_or(0))).collect();
        (format!("{name}({}; no prototype)", v.join(", ")), None)
    }

    fn ret_text(&mut self, ty: Option<&str>, rax: u64) -> String {
        match ty.map(str::trim) {
            Some("void") => "(void)".into(),
            Some(t) if is_float(t) => "(floating point, not read)".into(),
            Some(t) if is_ptr(t) && t.replace(' ', "").ends_with("char*") && rax != 0 => {
                format!("{rax:#x} {}", self.cstr(rax))
            }
            Some(t) => self.typed(t, rax),
            None => format!("{rax:#x}"),
        }
    }

    /// Entry and return addresses the trace and the hooks need in memory now.
    pub(crate) fn hook_addrs(&self) -> Vec<u64> {
        let mut v: Vec<u64> = self.tr.pending.iter().map(|p| p.ret_addr).collect();
        if let Some(l) = &self.tr.lib {
            v.extend(l.sites.keys());
        }
        v
    }

    /// A thread is at an address the trace or a hook watches. `Some`: a hook stops the program.
    pub(crate) fn hook_hit(&mut self, tid: Tid, addr: u64) -> Result<Option<(Tid, Why)>> {
        let verdict = None;
        // a return first: the same address may also be an entry
        if self.tr.pending.iter().any(|p| p.tid == tid && p.ret_addr == addr) {
            let r = self.t.regs(tid)?;
            let sp = r.rsp();
            if let Some(i) =
                self.tr.pending.iter().rposition(|p| p.tid == tid && p.ret_addr == addr && p.sp_after == sp)
            {
                let p = self.tr.pending.remove(i);
                let ret = self.ret_text(p.ret_ty.as_deref(), r.gp[0]);
                self.tr.push(p.kind, &p.name, format!("{} = {ret}", p.text));
            }
        }
        let site = self
            .tr
            .lib
            .as_ref()
            .and_then(|l| l.sites.get(&addr).filter(|s| l.names.is_empty() || l.names.contains(&s.name)).cloned());
        if let Some(s) = site {
            let r = self.t.regs(tid)?;
            let proto = nexo_bin::libc_proto(&s.name);
            let (text, ret_ty) = self.call_args(&s.name, proto.as_ref(), &r, false);
            self.pend(tid, &r, s.call_len, Kind::Lib, &s.name, text, ret_ty)?;
        }
        Ok(verdict)
    }

    #[allow(clippy::too_many_arguments)]
    fn pend(
        &mut self,
        tid: Tid,
        r: &Regs,
        call_len: Option<u8>,
        kind: Kind,
        name: &str,
        text: String,
        ret_ty: Option<String>,
    ) -> Result<()> {
        let (ret_addr, sp_after) = match call_len {
            None => {
                let mut b = [0u8; 8];
                if self.t.read(r.rsp(), &mut b) != 8 {
                    self.tr.push(kind, name, format!("{text} = ? (return address unreadable)"));
                    return Ok(());
                }
                (u64::from_le_bytes(b), r.rsp() + 8)
            }
            Some(len) => (r.rip + len as u64, r.rsp()),
        };
        if self.tr.pending.len() >= PENDING_MAX {
            self.tr.push(kind, name, format!("{text} = ? (too many calls in flight to follow this one)"));
            return Ok(());
        }
        self.tr.pending.push(Pending { tid, ret_addr, sp_after, kind, name: name.into(), text, ret_ty });
        Ok(())
    }

    /// The program ended: calls still in flight never returned.
    pub(crate) fn hooks_end(&mut self) {
        for p in std::mem::take(&mut self.tr.pending) {
            self.tr.push(p.kind, &p.name, format!("{} = ? (its return was not seen)", p.text));
        }
    }

    /// The import calls of the program, by memory address.
    fn lib_sites(&self) -> BTreeMap<u64, Site> {
        let bias = self.mods.main_bias();
        nexo_bin::import_sites(self.mods.main_image())
            .into_iter()
            .map(|s| (s.addr.wrapping_add(bias), Site { name: s.name, call_len: s.call_len }))
            .collect()
    }

    /// `ltrace [names]` | `ltrace off` | `ltrace show [names]`.
    pub(crate) fn cmd_ltrace(&mut self, a: &Args) -> Result<Output> {
        match a.pos.first().map(String::as_str) {
            Some("off") => {
                self.tr.lib = None;
                self.tr.pending.retain(|p| p.kind != Kind::Lib);
                Ok(rows("ltrace", vec!["library call trace off (`ltrace show` keeps the lines)".into()], json!({})))
            }
            Some("show") => {
                let names: Vec<String> = a.pos[1..].iter().flat_map(|w| w.split(',')).map(str::to_string).collect();
                Ok(self.show(
                    Kind::Lib,
                    &|n: &str| names.is_empty() || names.iter().any(|x| x == n),
                    "`ltrace` turns it on",
                ))
            }
            _ => {
                self.need_stopped()?;
                let names: Vec<String> = a.pos.iter().flat_map(|w| w.split(',')).map(str::to_string).collect();
                let sites = self.lib_sites();
                if sites.is_empty() {
                    return fail("this program calls no library function through an import (a static program?): `break <function>` stops in one");
                }
                let known: Vec<&str> = sites.values().map(|s| s.name.as_str()).collect();
                if let Some(bad) = names.iter().find(|n| !known.contains(&n.as_str())) {
                    let mut k = known.clone();
                    k.sort();
                    k.dedup();
                    return usage(format!("the program imports no {bad:?}; it imports: {}", k.join(" ")));
                }
                let n = sites.values().filter(|s| names.is_empty() || names.contains(&s.name)).count();
                self.tr.lib = Some(crate::hooks::Lib { sites, names: names.clone() });
                let what = if names.is_empty() { "every imported function".to_string() } else { names.join(",") };
                Ok(rows(
                    "ltrace",
                    vec![format!(
                        "tracing library calls: {what} ({n} call sites); `cont`, then `ltrace show`; a call is written when it returns"
                    )],
                    json!({"sites": n}),
                ))
            }
        }
    }
}
