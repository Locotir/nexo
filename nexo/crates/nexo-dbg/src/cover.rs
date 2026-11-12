//! Code coverage of the program's own file: a one-shot breakpoint at the start of every basic block;
//! the first pass through a block records it and the breakpoint goes. Snapshots are kept by name and
//! compared with `cover diff A B`. Only blocks whose start was passed are counted: code reached only
//! through a jump into the middle of a block (a jump table the reading did not see) is not.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use nexo_bin::Output;
use serde_json::json;

use crate::engine::{rows, Session};
use crate::{fail, usage, Result};

/// Lines a listing shows before it says how many more there are.
const SHOW_MAX: usize = 200;

/// Coverage of the program's own code, in file addresses.
#[derive(Debug, Clone, Default)]
pub struct Cover {
    /// Block start -> the function it is in (index into `funcs`).
    pub blocks: BTreeMap<u64, usize>,
    /// Functions measured: (start, name).
    pub funcs: Vec<(u64, String)>,
    /// Block starts passed.
    pub hit: BTreeSet<u64>,
    /// Still watching (breakpoints at blocks not yet passed).
    pub on: bool,
}

fn snap_dir(sha: &str) -> PathBuf {
    crate::state_dir().join("cover").join(sha)
}

fn snap_name_ok(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && n != "now"
}

impl Session {
    /// Blocks not yet passed, as memory addresses.
    pub(crate) fn cover_addrs(&self) -> Vec<u64> {
        let Some(c) = self.tr.cover.as_ref().filter(|c| c.on) else { return Vec::new() };
        let bias = self.mods.main_bias();
        c.blocks.keys().filter(|a| !c.hit.contains(a)).map(|a| a.wrapping_add(bias)).collect()
    }

    /// A thread is at `addr`: a block start passed for the first time is recorded.
    pub(crate) fn cover_hit(&mut self, addr: u64) {
        let bias = self.mods.main_bias();
        if let Some(c) = self.tr.cover.as_mut().filter(|c| c.on) {
            let fa = addr.wrapping_sub(bias);
            if c.blocks.contains_key(&fa) {
                c.hit.insert(fa);
            }
        }
    }

    /// `cover [names]` | `cover show [names]` | `cover save <A>` | `cover diff <A> <B>` | `cover off`.
    pub(crate) fn cmd_cover(&mut self, words: &[String]) -> Result<Output> {
        let w: Vec<&str> = words.iter().map(String::as_str).collect();
        match w.first().copied() {
            Some("off") => {
                let Some(c) = self.tr.cover.as_mut() else { return fail("coverage is not on: `cover` turns it on") };
                c.on = false;
                Ok(rows("cover", vec!["coverage off (`cover show` keeps what was measured)".into()], json!({})))
            }
            Some("show") => self.cover_show(&w[1..]),
            Some("save") => self.cover_save(w.get(1).copied()),
            Some("diff") => match (w.get(1), w.get(2)) {
                (Some(a), Some(b)) => self.cover_diff(a, b),
                _ => usage("cover diff <A> <B> (`now` is the coverage being measured)"),
            },
            _ => self.cover_on(&w),
        }
    }

    fn cover_on(&mut self, names: &[&str]) -> Result<Output> {
        self.need_stopped()?;
        let img = self.mods.main_image();
        let code: Vec<(u64, u64)> = img.code_sections().map(|s| (s.addr, s.addr + s.file_size)).collect();
        let in_code = |a: u64| code.iter().any(|&(s, e)| a >= s && a < e);
        let mut funcs = Vec::new();
        let mut blocks = BTreeMap::new();
        for f in img.funcs() {
            if f.name.ends_with("@plt") || f.size() == 0 || !in_code(f.start) {
                continue;
            }
            if !names.is_empty() && !names.contains(&f.name.as_str()) {
                continue;
            }
            let i = funcs.len();
            funcs.push((f.start, f.name.clone()));
            for b in nexo_bin::block_starts(img, f.start, f.end) {
                blocks.entry(b).or_insert(i);
            }
        }
        if let Some(bad) = names.iter().find(|n| !funcs.iter().any(|(_, f)| f == *n)) {
            return usage(format!("no function {bad:?} in the program (`nexo bin funcs` lists them)"));
        }
        if blocks.is_empty() {
            return fail("no code to measure in the program's file");
        }
        let (nb, nf) = (blocks.len(), funcs.len());
        self.tr.cover = Some(Cover { blocks, funcs, hit: BTreeSet::new(), on: true });
        Ok(rows(
            "cover",
            vec![format!(
                "measuring coverage from now on: {nb} blocks in {nf} functions; `cont`, then `cover show`, `cover save <name>`, `cover diff <A> <B>`"
            )],
            json!({"blocks": nb, "funcs": nf}),
        ))
    }

    fn cover_show(&mut self, names: &[&str]) -> Result<Output> {
        let Some(c) = self.tr.cover.clone() else { return fail("no coverage measured: `cover` turns it on") };
        let bias = self.mods.main_bias();
        let mut per: Vec<(usize, usize)> = vec![(0, 0); c.funcs.len()];
        for (b, &i) in &c.blocks {
            per[i].1 += 1;
            if c.hit.contains(b) {
                per[i].0 += 1;
            }
        }
        let total = c.blocks.len();
        let pct = |h: usize, t: usize| if t == 0 { 0.0 } else { h as f64 * 100.0 / t as f64 };
        let mut l = vec![format!(
            "{} of {total} blocks passed ({:.1}%), {} of {} functions entered{}; a block reached only by a jump into its middle is not counted",
            c.hit.len(),
            pct(c.hit.len(), total),
            per.iter().filter(|p| p.0 > 0).count(),
            c.funcs.len(),
            if c.on { "" } else { " (coverage is off)" }
        )];
        let mut more = 0;
        for (i, (start, name)) in c.funcs.iter().enumerate() {
            if !names.is_empty() && !names.contains(&name.as_str()) {
                continue;
            }
            if l.len() > SHOW_MAX {
                more += 1;
                continue;
            }
            let (h, t) = per[i];
            l.push(format!("{name} {start:#x}: {h}/{t} blocks"));
            if !names.is_empty() {
                // the blocks of a named function: which were not passed
                let missed: Vec<u64> =
                    c.blocks.iter().filter(|(b, &j)| j == i && !c.hit.contains(b)).map(|(b, _)| *b).collect();
                for b in missed.iter().take(SHOW_MAX) {
                    let lab = self.mods.label(b.wrapping_add(bias), false).unwrap_or_default();
                    l.push(format!("  not passed {b:#x} {lab}"));
                }
            }
        }
        if !names.is_empty() {
            if let Some(bad) = names.iter().find(|n| !c.funcs.iter().any(|(_, f)| f == *n)) {
                return usage(format!("{bad:?} is not a measured function"));
            }
        }
        if more > 0 {
            l.push(format!("... {more} more functions (`cover show <name>` for one)"));
        }
        Ok(rows("cover", l, json!({"passed": c.hit.len(), "blocks": total})))
    }

    fn cover_save(&mut self, name: Option<&str>) -> Result<Output> {
        let Some(name) = name.filter(|n| snap_name_ok(n)) else {
            return usage("cover save <name>: letters, digits, - _ . (not `now`)");
        };
        let Some(c) = self.tr.cover.as_ref() else { return fail("no coverage measured: `cover` turns it on") };
        let dir = snap_dir(&self.mods.main_image().sha256);
        std::fs::create_dir_all(&dir).map_err(|e| crate::Error::Fail(format!("{}: {e}", dir.display())))?;
        let mut text = format!("# blocks {}\n", c.blocks.len());
        for a in &c.hit {
            text.push_str(&format!("{a:#x}\n"));
        }
        let p = dir.join(format!("{name}.txt"));
        std::fs::write(&p, text).map_err(|e| crate::Error::Fail(format!("{}: {e}", p.display())))?;
        Ok(rows(
            "cover",
            vec![format!("saved {name}: {} blocks passed (`cover diff {name} now`)", c.hit.len())],
            json!({}),
        ))
    }

    fn cover_load(&self, name: &str) -> Result<BTreeSet<u64>> {
        if name == "now" {
            return match &self.tr.cover {
                Some(c) => Ok(c.hit.clone()),
                None => fail("no coverage measured: `cover` turns it on"),
            };
        }
        if !snap_name_ok(name) {
            return usage(format!("{name:?} is not a snapshot name"));
        }
        let p = snap_dir(&self.mods.main_image().sha256).join(format!("{name}.txt"));
        let text = std::fs::read_to_string(&p).map_err(|_| {
            crate::Error::Fail(format!("no snapshot {name:?} for this program: `cover save {name}` makes one"))
        })?;
        Ok(text.lines().filter(|l| !l.starts_with('#')).filter_map(|l| nexo_bin::parse_addr(l.trim()).ok()).collect())
    }

    fn cover_diff(&mut self, a: &str, b: &str) -> Result<Output> {
        let (sa, sb) = (self.cover_load(a)?, self.cover_load(b)?);
        let bias = self.mods.main_bias();
        let only_a: Vec<u64> = sa.difference(&sb).copied().collect();
        let only_b: Vec<u64> = sb.difference(&sa).copied().collect();
        let mut l = vec![format!(
            "{a}: {} blocks, {b}: {} blocks; only in {a}: {}, only in {b}: {}, in both: {}",
            sa.len(),
            sb.len(),
            only_a.len(),
            only_b.len(),
            sa.intersection(&sb).count()
        )];
        for (who, list) in [(a, &only_a), (b, &only_b)] {
            for x in list.iter().take(SHOW_MAX / 2) {
                let lab = self.mods.label(x.wrapping_add(bias), false).unwrap_or_default();
                l.push(format!("only {who} {x:#x} {lab}"));
            }
            if list.len() > SHOW_MAX / 2 {
                l.push(format!("... {} more only in {who}", list.len() - SHOW_MAX / 2));
            }
        }
        Ok(rows("cover", l, json!({"only_a": only_a.len(), "only_b": only_b.len()})))
    }
}
