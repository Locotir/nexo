//! An image at the address its module has in a running program: every address the file gives moves by one
//! amount, so `--at`, names and answers all speak live addresses (see `nexo bin --module`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::OnceLock;

use super::{Format, Image};
use crate::{Error, Result};

fn moved<V>(m: &mut BTreeMap<u64, V>, d: u64) {
    *m = std::mem::take(m).into_iter().map(|(k, v)| (k.wrapping_add(d), v)).collect();
}

impl Image {
    /// Move the image to `live_base` (where its first loaded byte sits in memory) from where the file puts it
    /// (`image_base`). A file address `a` becomes `a + (live_base - image_base)`, wrapping; sliding again by the
    /// opposite amount gives the file back. Not for Mach-O files, whose addresses are spread over more tables.
    pub fn slide_to(&mut self, live_base: u64) -> Result<u64> {
        let delta = live_base.wrapping_sub(self.image_base);
        self.slide(delta)?;
        Ok(delta)
    }

    /// Move every address by `delta` (wrapping).
    pub fn slide(&mut self, delta: u64) -> Result<()> {
        if self.format == Format::MachO {
            return Err(Error::NotRead("a Mach-O file cannot be moved to a live address yet".into()));
        }
        if delta == 0 {
            return Ok(());
        }
        let add = |a: u64| a.wrapping_add(delta);
        self.entry = add(self.entry);
        self.image_base = add(self.image_base);
        for s in &mut self.sections {
            s.addr = add(s.addr);
        }
        for s in &mut self.symbols {
            s.addr = add(s.addr);
        }
        for e in &mut self.exports {
            e.addr = add(e.addr);
        }
        for i in &mut self.imports {
            i.slot = i.slot.map(add);
        }
        self.got = self.got.map(add);
        for p in &mut self.rel_ptrs {
            *p = (add(p.0), add(p.1));
        }
        let u = &mut self.user;
        moved(&mut u.names, delta);
        moved(&mut u.notes, delta);
        moved(&mut u.protos, delta);
        moved(&mut u.vars, delta);
        moved(&mut u.targets, delta);
        u.targets_more = std::mem::take(&mut u.targets_more).into_iter().map(add).collect::<BTreeSet<u64>>();
        if let Some(p) = &mut self.pdb {
            for f in &mut p.funcs {
                f.0 = add(f.0);
            }
            for l in &mut p.lines {
                l.addr = add(l.addr);
            }
            p.types.slide(delta);
        }
        self.slid = self.slid.wrapping_add(delta);
        // the lookup tables are built again from the moved parts; what was computed from addresses is dropped
        self.names = BTreeMap::new();
        self.sizes = HashMap::new();
        self.raws = HashMap::new();
        self.finish();
        self.funcs = OnceLock::new();
        self.strings = OnceLock::new();
        self.refs = OnceLock::new();
        self.dwarf = OnceLock::new();
        Ok(())
    }
}
