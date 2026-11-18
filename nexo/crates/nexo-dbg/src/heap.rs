//! `heap`: leaks, double frees and frees of bad pointers found without valgrind or ASan. It is built on the hooks:
//! one hook on each allocation function reads the arguments at the entry and the result at the return, and a hook on
//! `free` reads the pointer at the entry. The bookkeeping is [`Heap`], which knows nothing of the process.

use std::collections::{HashMap, VecDeque};

use nexo_bin::Output;
use serde_json::json;

use crate::callargs::int_regs;
use crate::cmds::Args;
use crate::engine::{rows, Session, Why};
use crate::hooks::{HeapCall, Hook};
use crate::platform::{Os, Tid};
use crate::regs::Regs;
use crate::trace::Kind;
use crate::{fail, usage, Result};

/// Live blocks tracked at most; past it new allocations are counted and not followed.
pub const LIVE_MAX: usize = 1_000_000;
/// Freed blocks remembered (with where they were freed), the oldest forgotten first.
pub const FREED_KEEP: usize = 4096;
/// Frames of the call chain kept for each block.
pub const FRAMES: usize = 6;
/// Allocation sites an answer lists.
pub const SITES_SHOWN: usize = 10;

/// The functions the check follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeapFn {
    Malloc,
    Calloc,
    Realloc,
    Free,
    PosixMemalign,
    AlignedAlloc,
    ReallocArray,
    Memalign,
}

/// Name, function, and whether the program must have it (the others are followed where present, on Linux).
const FUNCS: [(&str, HeapFn, bool); 8] = [
    ("malloc", HeapFn::Malloc, true),
    ("calloc", HeapFn::Calloc, true),
    ("realloc", HeapFn::Realloc, true),
    ("free", HeapFn::Free, true),
    ("posix_memalign", HeapFn::PosixMemalign, false),
    ("aligned_alloc", HeapFn::AlignedAlloc, false),
    ("reallocarray", HeapFn::ReallocArray, false),
    ("memalign", HeapFn::Memalign, false),
];

impl HeapFn {
    pub fn name(self) -> &'static str {
        FUNCS.iter().find(|f| f.1 == self).map_or("?", |f| f.0)
    }
}

#[derive(Debug, Clone)]
struct Block {
    size: u64,
    id: u64,
    bt: Vec<u64>,
}

#[derive(Debug, Clone)]
struct Freed {
    alloc: Vec<u64>,
    free: Vec<u64>,
}

/// Why a pointer given to `free` or `realloc` is wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bad {
    /// Not a block this check knows.
    Unknown,
    /// A block already freed: where it was allocated and where it was freed.
    Freed { alloc: Vec<u64>, freed: Vec<u64> },
}

/// One allocation site: the blocks with the same call chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub bt: Vec<u64>,
    pub count: u64,
    pub bytes: u64,
    /// The oldest block of the site: its address and allocation id.
    pub first: u64,
    pub first_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub blocks: u64,
    pub bytes: u64,
    /// The biggest sites by bytes.
    pub sites: Vec<Site>,
    /// Sites left out.
    pub more: usize,
}

/// The live blocks and the recently freed ones.
#[derive(Debug)]
pub struct Heap {
    live: HashMap<u64, Block>,
    freed: HashMap<u64, (u64, Freed)>,
    /// The order of `freed`: address and the sequence number it was freed with (a stale entry is skipped).
    order: VecDeque<(u64, u64)>,
    seq: u64,
    next_id: u64,
    live_max: usize,
    freed_keep: usize,
    /// The live cap was reached, and whether that was said.
    capped: bool,
    cap_told: bool,
    /// The check was turned on after the program started: a free of a block it never saw is not an error.
    late: bool,
    untracked: u64,
    foreign: u64,
    allocs: u64,
    frees: u64,
}

fn plural(n: u64, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// A call chain as a person reads it, nearest caller first.
pub fn chain(bt: &[u64], label: &mut dyn FnMut(u64) -> String) -> String {
    if bt.is_empty() {
        return "(no frames)".into();
    }
    bt.iter().map(|&pc| label(pc)).collect::<Vec<_>>().join(" <- ")
}

/// The stop line for a wrong pointer given to `op` (`free` or `realloc`).
pub fn describe_bad(op: &str, addr: u64, bad: &Bad, label: &mut dyn FnMut(u64) -> String) -> String {
    match bad {
        Bad::Unknown => format!("heap: {op} of {addr:#x} that is not a live block"),
        Bad::Freed { alloc, freed } if op == "free" => format!(
            "heap: double free of {addr:#x}, allocated at {}, first freed at {}",
            chain(alloc, label),
            chain(freed, label)
        ),
        Bad::Freed { alloc, freed } => format!(
            "heap: {op} of {addr:#x} after it was freed, allocated at {}, freed at {}",
            chain(alloc, label),
            chain(freed, label)
        ),
    }
}

impl Heap {
    pub fn new(late: bool) -> Heap {
        Heap::with_limits(late, LIVE_MAX, FREED_KEEP)
    }

    pub fn with_limits(late: bool, live_max: usize, freed_keep: usize) -> Heap {
        Heap {
            live: HashMap::new(),
            freed: HashMap::new(),
            order: VecDeque::new(),
            seq: 0,
            next_id: 0,
            live_max,
            freed_keep,
            capped: false,
            cap_told: false,
            late,
            untracked: 0,
            foreign: 0,
            allocs: 0,
            frees: 0,
        }
    }

    /// The cap was reached and this is the first time anyone asks: say it once.
    pub fn cap_news(&mut self) -> Option<usize> {
        (self.capped && !std::mem::replace(&mut self.cap_told, true)).then_some(self.live_max)
    }

    /// A block of `size` bytes at `addr` came from an allocation function. A null result is a failed call.
    pub fn alloc(&mut self, addr: u64, size: u64, bt: Vec<u64>) {
        if addr == 0 {
            return;
        }
        self.allocs += 1;
        self.freed.remove(&addr);
        if !self.live.contains_key(&addr) && self.live.len() >= self.live_max {
            self.untracked += 1;
            self.capped = true;
            return;
        }
        self.next_id += 1;
        self.live.insert(addr, Block { size, id: self.next_id, bt });
    }

    fn remember(&mut self, addr: u64, b: Block, free_bt: Vec<u64>) {
        self.seq += 1;
        let seq = self.seq;
        self.freed.insert(addr, (seq, Freed { alloc: b.bt, free: free_bt }));
        self.order.push_back((addr, seq));
        while self.order.len() > self.freed_keep {
            if let Some((a, s)) = self.order.pop_front() {
                if self.freed.get(&a).is_some_and(|f| f.0 == s) {
                    self.freed.remove(&a);
                }
            }
        }
    }

    /// What is wrong with a pointer that is not live, `None` when that is not an error (a block this check could not
    /// have seen).
    fn bad_pointer(&mut self, addr: u64) -> Option<Bad> {
        if let Some((_, f)) = self.freed.get(&addr) {
            return Some(Bad::Freed { alloc: f.alloc.clone(), freed: f.free.clone() });
        }
        if self.capped || self.late {
            self.foreign += 1;
            return None;
        }
        Some(Bad::Unknown)
    }

    /// `free(addr)` was called, from `bt`. `free(NULL)` is fine.
    pub fn free(&mut self, addr: u64, bt: Vec<u64>) -> std::result::Result<(), Bad> {
        if addr == 0 {
            return Ok(());
        }
        self.frees += 1;
        match self.live.remove(&addr) {
            Some(b) => {
                self.remember(addr, b, bt);
                Ok(())
            }
            None => self.bad_pointer(addr).map_or(Ok(()), Err),
        }
    }

    /// `realloc(old, size)` returned `new` (not null), called from `bt`: the entry moves with its first call chain.
    pub fn realloc(&mut self, old: u64, new: u64, size: u64, bt: Vec<u64>) -> std::result::Result<(), Bad> {
        if new == 0 {
            return Ok(());
        }
        if old == 0 {
            self.alloc(new, size, bt);
            return Ok(());
        }
        match self.live.remove(&old) {
            Some(mut b) => {
                b.size = size;
                if new != old {
                    self.remember(old, b.clone(), bt);
                }
                self.freed.remove(&new);
                self.live.insert(new, b);
                Ok(())
            }
            // an inner call (`reallocarray` calling `realloc`) already moved it
            None if self.live.contains_key(&new) => Ok(()),
            None => {
                let bad = self.bad_pointer(old);
                self.alloc(new, size, bt);
                bad.map_or(Ok(()), Err)
            }
        }
    }

    /// Live blocks and their bytes.
    pub fn totals(&self) -> (u64, u64) {
        (self.live.len() as u64, self.live.values().fold(0u64, |a, b| a.saturating_add(b.size)))
    }

    /// The live blocks grouped by call chain, the `top` biggest by bytes.
    pub fn summary(&self, top: usize) -> Summary {
        let mut by: HashMap<&[u64], Site> = HashMap::new();
        let (blocks, bytes) = self.totals();
        for (&addr, b) in &self.live {
            let s = by.entry(b.bt.as_slice()).or_insert_with(|| Site {
                bt: b.bt.clone(),
                count: 0,
                bytes: 0,
                first: addr,
                first_id: b.id,
            });
            s.count += 1;
            s.bytes = s.bytes.saturating_add(b.size);
            if b.id < s.first_id {
                s.first = addr;
                s.first_id = b.id;
            }
        }
        let mut sites: Vec<Site> = by.into_values().collect();
        sites.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(b.count.cmp(&a.count)).then(a.bt.cmp(&b.bt)));
        let more = sites.len().saturating_sub(top);
        sites.truncate(top);
        Summary { blocks, bytes, sites, more }
    }

    /// The answer of `heap` and `heap leaks`.
    pub fn lines(&self, s: &Summary, leaks: bool, label: &mut dyn FnMut(u64) -> String) -> Vec<String> {
        let counts = format!("{}, {}", plural(self.allocs, "allocation"), plural(self.frees, "free"));
        let mut l = Vec::new();
        if s.blocks == 0 {
            l.push(if leaks {
                format!("heap leaks: none, every tracked block was freed ({counts})")
            } else {
                format!("heap: no live blocks ({counts})")
            });
        } else {
            let what = if leaks { "not freed" } else { "live" };
            l.push(format!(
                "heap{}: {} {what}, {} ({counts})",
                if leaks { " leaks" } else { "" },
                plural(s.blocks, "block"),
                plural(s.bytes, "byte")
            ));
            for (n, x) in s.sites.iter().enumerate() {
                l.push(format!(
                    "  {}. {} in {}, first {:#x} (allocation #{}), from {}",
                    n + 1,
                    plural(x.bytes, "byte"),
                    plural(x.count, "block"),
                    x.first,
                    x.first_id,
                    chain(&x.bt, label)
                ));
            }
            if s.more > 0 {
                l.push(format!("  ... {} more allocation sites with fewer bytes", s.more));
            }
        }
        if self.capped {
            l.push(format!(
                "heap: the cap of {} tracked blocks was reached: {} later allocations are not followed, so these numbers are a floor",
                self.live_max, self.untracked
            ));
        }
        if self.foreign > 0 {
            l.push(format!(
                "heap: {} not followed (allocated before the check or past the cap) were freed and not reported",
                plural(self.foreign, "block")
            ));
        }
        l
    }

    /// The line the exit answer carries.
    pub fn exit_line(&self) -> String {
        let (n, b) = self.totals();
        format!("heap: {}, {} still live; heap leaks shows where", plural(n, "block"), plural(b, "byte"))
    }
}

impl Session {
    /// `heap on` | `heap off` | `heap` | `heap leaks`.
    pub(crate) fn cmd_heap(&mut self, a: &Args) -> Result<Output> {
        match a.pos.first().map(String::as_str) {
            Some("on") => self.heap_on(),
            Some("off") => {
                if self.tr.heap.take().is_none() {
                    return fail("the heap check is not on");
                }
                self.tr.hooks.retain(|h| h.heap.is_none());
                self.tr.pending.retain(|p| p.heap.is_none());
                Ok(rows("heap", vec!["heap check off, what it knew is dropped".into()], json!({})))
            }
            None => self.heap_report(false),
            Some("leaks") => self.heap_report(true),
            Some(w) => usage(format!("heap {w:?}: heap on | heap | heap leaks | heap off")),
        }
    }

    fn heap_on(&mut self) -> Result<Output> {
        self.need_stopped()?;
        if self.tr.heap.is_some() {
            return fail("the heap check is already on: `heap` lists the live blocks, `heap off` stops it");
        }
        if self.bits() == 32 {
            return fail("the heap check reads the arguments and results of 64-bit calls; this program is 32-bit");
        }
        let linux = self.t.os() == Os::Linux;
        let mut placed: Vec<String> = Vec::new();
        let mut waiting = 0;
        for (name, f, required) in FUNCS {
            if !required && !linux {
                continue;
            }
            let (addr, call_len, hooked, _) = match self.hook_place(name) {
                Ok(p) => p,
                Err(e) if required => {
                    self.tr.hooks.retain(|h| h.heap.is_none());
                    return Err(e);
                }
                Err(_) => continue,
            };
            // two names for one function (musl's `memalign` and `aligned_alloc`) are followed once
            if addr != 0 && self.tr.hooks.iter().any(|h| h.heap.is_some() && h.addr == addr) {
                continue;
            }
            let also = (addr != 0 && call_len.is_none() && self.lib_sites().contains_key(&addr)).then(Vec::new);
            let id = self.new_id();
            self.tr.hooks.push(Hook {
                id,
                addr,
                name: hooked,
                sig: None,
                call_len,
                bt: 0,
                stop: false,
                set: Vec::new(),
                ret: None,
                hits: 0,
                returns: 0,
                also,
                heap: Some(f),
            });
            if addr == 0 {
                waiting += 1;
            }
            placed.push(name.to_string());
        }
        let late = self.last_stop.as_ref().is_none_or(|s| s.event != "entry");
        self.tr.heap = Some(Heap::new(late));
        let mut l = vec![format!("heap check on: {}", placed.join(" "))];
        if waiting > 0 {
            l.push(format!("{waiting} of them wait for a library that has them"));
        }
        if late {
            l.push("it was turned on after the program started: blocks allocated before now are not known, and a free of one is ignored".into());
        }
        l.push("each call stops the process for a moment, so the program runs much slower".into());
        l.push("`cont`: a free of a pointer that is not live stops the program; `heap` lists the live blocks, `heap leaks` after the exit".into());
        Ok(rows("heap", l, json!({"functions": placed})))
    }

    fn heap_report(&mut self, leaks: bool) -> Result<Output> {
        let Some(h) = self.tr.heap.take() else {
            return fail("the heap check is off: `heap on` turns it on, then `cont`");
        };
        let s = h.summary(SITES_SHOWN);
        let l = h.lines(&s, leaks, &mut |pc| self.heap_label(pc));
        self.tr.heap = Some(h);
        Ok(rows("heap", l, json!({"blocks": s.blocks, "bytes": s.bytes})))
    }

    fn heap_label(&mut self, pc: u64) -> String {
        self.mods.label(pc.wrapping_sub(1), false).unwrap_or_else(|| format!("{pc:#x}"))
    }

    /// The line the answer to the program's end carries when the check is on.
    pub(crate) fn heap_exit_note(&self) -> Option<String> {
        self.tr.heap.as_ref().map(Heap::exit_line)
    }

    /// The caller frames of a thread at a function's entry, as return addresses.
    fn heap_frames(&mut self, r: &Regs) -> Vec<u64> {
        let frames = {
            let Session { t, mods, .. } = self;
            let mut rd = |a: u64| {
                let mut b = [0u8; 8];
                (t.read(a, &mut b) == 8).then(|| u64::from_le_bytes(b))
            };
            mods.backtrace(r, &mut rd, FRAMES + 1)
        };
        frames.iter().skip(1).take(FRAMES).map(|f| f.pc).collect()
    }

    /// The stop for a wrong pointer, and the one-time word that the cap was reached.
    fn heap_verdict(&mut self, op: &str, addr: u64, res: std::result::Result<(), Bad>) -> Option<Why> {
        if let Some(max) = self.tr.heap.as_mut().and_then(Heap::cap_news) {
            self.log_line(format!("heap: {max} live blocks are tracked, the cap: later allocations are not followed"));
        }
        let bad = res.err()?;
        let text = describe_bad(op, addr, &bad, &mut |pc| self.heap_label(pc));
        Some(Why::Heap(text))
    }

    /// A thread entered a followed function. `Some`: the program stops (a free of a bad pointer).
    pub(crate) fn heap_entry(
        &mut self,
        tid: Tid,
        id: u32,
        f: HeapFn,
        r: &Regs,
        call_len: Option<u8>,
    ) -> Result<Option<Why>> {
        if self.tr.heap.is_none() {
            return Ok(None);
        }
        let regs = int_regs(self.t.os());
        let arg = |i: usize| regs.get(i).and_then(|n| r.get(n)).unwrap_or(0);
        if f == HeapFn::Free {
            let p = arg(0);
            if p == 0 {
                return Ok(None);
            }
            let bt = self.heap_frames(r);
            let res = match self.tr.heap.as_mut() {
                Some(h) => h.free(p, bt),
                None => return Ok(None),
            };
            return Ok(self.heap_verdict("free", p, res));
        }
        let (old, size, out) = match f {
            HeapFn::Calloc => (0, arg(0).saturating_mul(arg(1)), 0),
            HeapFn::Realloc => (arg(0), arg(1), 0),
            HeapFn::ReallocArray => (arg(0), arg(1).saturating_mul(arg(2)), 0),
            HeapFn::PosixMemalign => (0, arg(2), arg(0)),
            HeapFn::AlignedAlloc | HeapFn::Memalign => (0, arg(1), 0),
            HeapFn::Malloc | HeapFn::Free => (0, arg(0), 0),
        };
        let bt = self.heap_frames(r);
        let call = HeapCall { f, old, size, out, bt };
        self.pend(tid, r, call_len, Kind::Hook, Some(id), f.name(), String::new(), None, Some(call))?;
        Ok(None)
    }

    /// A followed function returned. `Some`: the program stops (a `realloc` of a bad pointer).
    pub(crate) fn heap_returned(&mut self, r: &Regs, call: &HeapCall) -> Option<Why> {
        let mut got = r.gp[0];
        if call.f == HeapFn::PosixMemalign {
            // 0 on success, with the block's address stored where the first argument pointed
            if got != 0 {
                return None;
            }
            let b = self.read_vec(call.out, 8);
            got = u64::from_le_bytes(<[u8; 8]>::try_from(b.as_slice()).ok()?);
        }
        let h = self.tr.heap.as_mut()?;
        let res = match call.f {
            // `realloc(p, 0)` frees p and answers NULL
            HeapFn::Realloc | HeapFn::ReallocArray if got == 0 && call.size == 0 && call.old != 0 => {
                h.free(call.old, call.bt.clone())
            }
            HeapFn::Realloc | HeapFn::ReallocArray => h.realloc(call.old, got, call.size, call.bt.clone()),
            _ => {
                h.alloc(got, call.size, call.bt.clone());
                Ok(())
            }
        };
        self.heap_verdict("realloc", call.old, res)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bt(n: u64) -> Vec<u64> {
        vec![0x1000 + n, 0x2000]
    }

    fn name(pc: u64) -> String {
        format!("f{pc:x}")
    }

    #[test]
    fn alloc_and_free_leave_nothing_live() {
        let mut h = Heap::new(false);
        h.alloc(0x10, 8, bt(1));
        h.alloc(0x20, 16, bt(2));
        assert_eq!(h.totals(), (2, 24));
        assert_eq!(h.free(0x10, bt(3)), Ok(()));
        assert_eq!(h.free(0x20, bt(3)), Ok(()));
        assert_eq!(h.totals(), (0, 0));
        assert!(h.exit_line().starts_with("heap: 0 blocks, 0 bytes still live"));
    }

    #[test]
    fn free_of_null_is_fine_and_a_null_result_is_not_a_block() {
        let mut h = Heap::new(false);
        assert_eq!(h.free(0, bt(1)), Ok(()));
        h.alloc(0, 8, bt(1));
        assert_eq!(h.totals(), (0, 0));
    }

    #[test]
    fn a_free_of_an_unknown_pointer_is_named() {
        let mut h = Heap::new(false);
        let bad = h.free(0x99, bt(1)).unwrap_err();
        assert_eq!(bad, Bad::Unknown);
        assert_eq!(describe_bad("free", 0x99, &bad, &mut name), "heap: free of 0x99 that is not a live block");
    }

    #[test]
    fn a_double_free_says_where_it_was_allocated_and_first_freed() {
        let mut h = Heap::new(false);
        h.alloc(0x10, 8, bt(1));
        h.free(0x10, bt(2)).unwrap();
        let bad = h.free(0x10, bt(3)).unwrap_err();
        let t = describe_bad("free", 0x10, &bad, &mut name);
        assert_eq!(t, "heap: double free of 0x10, allocated at f1001 <- f2000, first freed at f1002 <- f2000");
    }

    #[test]
    fn an_address_that_is_allocated_again_is_not_a_double_free() {
        let mut h = Heap::new(false);
        h.alloc(0x10, 8, bt(1));
        h.free(0x10, bt(2)).unwrap();
        h.alloc(0x10, 8, bt(3));
        assert_eq!(h.free(0x10, bt(4)), Ok(()));
    }

    #[test]
    fn realloc_moves_the_entry_and_keeps_its_first_site() {
        let mut h = Heap::new(false);
        h.alloc(0x10, 8, bt(1));
        h.realloc(0x10, 0x50, 64, bt(2)).unwrap();
        assert_eq!(h.totals(), (1, 64));
        let s = h.summary(10);
        assert_eq!(s.sites[0].bt, bt(1));
        assert_eq!(s.sites[0].first, 0x50);
        // the old address is a freed block now
        let bad = h.free(0x10, bt(3)).unwrap_err();
        assert!(matches!(bad, Bad::Freed { .. }), "{bad:?}");
        // in place, and from NULL
        h.realloc(0x50, 0x50, 128, bt(4)).unwrap();
        assert_eq!(h.totals(), (1, 128));
        h.realloc(0, 0x70, 5, bt(5)).unwrap();
        assert_eq!(h.totals(), (2, 133));
    }

    #[test]
    fn realloc_of_a_freed_or_unknown_pointer_is_reported_and_its_result_followed() {
        let mut h = Heap::new(false);
        h.alloc(0x10, 8, bt(1));
        h.free(0x10, bt(2)).unwrap();
        let bad = h.realloc(0x10, 0x30, 8, bt(3)).unwrap_err();
        let t = describe_bad("realloc", 0x10, &bad, &mut name);
        assert!(t.starts_with("heap: realloc of 0x10 after it was freed, allocated at f1001"), "{t}");
        assert_eq!(h.totals(), (1, 8));
        assert_eq!(h.realloc(0x77, 0x90, 8, bt(4)), Err(Bad::Unknown));
        // a failed realloc (NULL) changes nothing
        assert_eq!(h.realloc(0x30, 0, 8, bt(5)), Ok(()));
        assert_eq!(h.totals(), (2, 16));
    }

    #[test]
    fn an_inner_realloc_that_already_moved_the_block_is_not_an_error() {
        let mut h = Heap::new(false);
        h.alloc(0x10, 8, bt(1));
        h.realloc(0x10, 0x50, 64, bt(2)).unwrap();
        assert_eq!(h.realloc(0x10, 0x50, 64, bt(3)), Ok(()));
        assert_eq!(h.totals(), (1, 64));
    }

    #[test]
    fn the_summary_groups_by_call_chain_and_lists_the_biggest_first() {
        let mut h = Heap::new(false);
        for i in 0..3 {
            h.alloc(0x100 + i * 0x10, 10, bt(1));
        }
        h.alloc(0x900, 100, bt(2));
        h.alloc(0xa00, 5, bt(3));
        let s = h.summary(2);
        assert_eq!((s.blocks, s.bytes, s.more), (5, 135, 1));
        assert_eq!(s.sites.len(), 2);
        assert_eq!((s.sites[0].bytes, s.sites[0].count, s.sites[0].first), (100, 1, 0x900));
        assert_eq!((s.sites[1].bytes, s.sites[1].count, s.sites[1].first), (30, 3, 0x100));
        let l = h.lines(&s, true, &mut name);
        assert_eq!(l[0], "heap leaks: 5 blocks not freed, 135 bytes (5 allocations, 0 frees)");
        assert_eq!(l[1], "  1. 100 bytes in 1 block, first 0x900 (allocation #4), from f1002 <- f2000");
        assert!(l[2].starts_with("  2. 30 bytes in 3 blocks, first 0x100 (allocation #1)"), "{}", l[2]);
        assert_eq!(l[3], "  ... 1 more allocation sites with fewer bytes");
    }

    #[test]
    fn an_empty_heap_has_no_leaks() {
        let h = Heap::new(false);
        let l = h.lines(&h.summary(10), true, &mut name);
        assert_eq!(l, vec!["heap leaks: none, every tracked block was freed (0 allocations, 0 frees)".to_string()]);
    }

    #[test]
    fn the_freed_ring_is_bounded_and_forgets_the_oldest() {
        let mut h = Heap::with_limits(false, 100, 3);
        for i in 1..=5u64 {
            h.alloc(i * 0x10, 8, bt(i));
            h.free(i * 0x10, bt(9)).unwrap();
        }
        assert!(h.freed.len() <= 3 && h.order.len() <= 3, "{} {}", h.freed.len(), h.order.len());
        // the newest is still known as freed, the oldest is no longer known
        assert!(matches!(h.free(0x50, bt(9)), Err(Bad::Freed { .. })));
        assert_eq!(h.free(0x10, bt(9)), Err(Bad::Unknown));
    }

    #[test]
    fn a_ring_entry_of_a_reused_address_does_not_drop_the_new_one() {
        let mut h = Heap::with_limits(false, 100, 2);
        h.alloc(0x10, 8, bt(1));
        h.free(0x10, bt(1)).unwrap();
        h.alloc(0x10, 8, bt(1));
        h.free(0x10, bt(1)).unwrap();
        h.alloc(0x20, 8, bt(2));
        h.free(0x20, bt(2)).unwrap();
        // the first entry of 0x10 went out of the ring; the second one is the live record
        assert!(matches!(h.free(0x10, bt(3)), Err(Bad::Freed { .. })));
    }

    #[test]
    fn the_cap_stops_tracking_says_so_once_and_never_blames_an_untracked_block() {
        let mut h = Heap::with_limits(false, 2, 8);
        h.alloc(0x10, 1, bt(1));
        h.alloc(0x20, 1, bt(1));
        assert_eq!(h.cap_news(), None);
        h.alloc(0x30, 1, bt(1));
        h.alloc(0x40, 1, bt(1));
        assert_eq!(h.cap_news(), Some(2));
        assert_eq!(h.cap_news(), None);
        assert_eq!(h.totals(), (2, 2));
        // the untracked block is freed: not an error
        assert_eq!(h.free(0x30, bt(2)), Ok(()));
        // a tracked block can still be freed and room opens again
        assert_eq!(h.free(0x10, bt(2)), Ok(()));
        h.alloc(0x50, 1, bt(1));
        assert_eq!(h.totals(), (2, 2));
        let l = h.lines(&h.summary(10), false, &mut name);
        assert!(l.iter().any(|x| x.contains("cap of 2 tracked blocks") && x.contains("2 later allocations")), "{l:?}");
    }

    #[test]
    fn a_check_turned_on_late_ignores_blocks_it_never_saw_but_still_finds_double_frees() {
        let mut h = Heap::new(true);
        assert_eq!(h.free(0x99, bt(1)), Ok(()));
        h.alloc(0x10, 8, bt(1));
        h.free(0x10, bt(2)).unwrap();
        assert!(matches!(h.free(0x10, bt(3)), Err(Bad::Freed { .. })));
    }

    #[test]
    fn a_chain_without_frames_says_so() {
        assert_eq!(chain(&[], &mut name), "(no frames)");
    }
}
