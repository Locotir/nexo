//! Libraries the program loads and unloads while it runs (`dlopen`, `LoadLibrary`, a delay-loaded DLL): breakpoints
//! and hooks on a name no loaded file has yet wait for it (pending), `catch load` and `catch unload` stop there, and
//! `ltrace` and the hooks on an import take the import calls of a library that comes in late.
//!
//! The loader tells each change at one place: Linux's `ld.so` calls `_dl_debug_state` before and after it changes
//! its list, macOS's dyld calls the function its image list names, and Windows sends the debugger an event. A
//! breakpoint there (or the event) makes the engine compare the loaded files with the ones it saw last.

use nexo_bin::{Output, Place};
use serde_json::json;

use crate::cmds::Args;
use crate::engine::{rows, Session, Why};
use crate::hooks::Site;
use crate::platform::{Os, Tid};
use crate::{usage, Result};

/// Imports through which a program loads a library by itself; a name it does not import may come with one.
const LOADERS: [&str; 8] = [
    "dlopen",
    "dlmopen",
    "__libc_dlopen_mode",
    "LoadLibraryA",
    "LoadLibraryW",
    "LoadLibraryExA",
    "LoadLibraryExW",
    "LdrLoadDll",
];

/// A loaded file as the last look saw it: path, start and end in memory.
type Seen = (String, u64, u64);

/// What the debugger keeps about the program's libraries between stops.
#[derive(Debug, Default)]
pub struct Loads {
    /// The loader's hook in this program, once looked up (`Some(None)`: it has none the debugger knows).
    hook: Option<Option<u64>>,
    /// The loader's hook while a breakpoint there is wanted.
    pub armed: Option<u64>,
    /// `catch load` and `catch unload`: a text the library's file name holds (empty: any library).
    pub catch_load: Option<String>,
    pub catch_unload: Option<String>,
    /// The loaded files at the last look, while loads are watched.
    seen: Option<Vec<Seen>>,
}

impl Loads {
    /// The program was replaced (exec): its loader and its files are new; what to catch stays.
    pub fn forget(&mut self) {
        self.hook = None;
        self.armed = None;
        self.seen = None;
    }
}

/// The name a pending breakpoint or hook is looked up by: in libraries only, since the program's own image is
/// known from the start.
fn lookup(name: &str) -> String {
    if name.contains('!') {
        name.to_string()
    } else {
        format!("!{name}")
    }
}

/// A file's short name.
fn short(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Whether a place someone typed is a name a library loaded later may have: `name`, `lib!name` or `!name`.
pub(crate) fn may_load_later(spec: &str) -> bool {
    let name = spec.rsplit('!').next().unwrap_or(spec);
    let mut c = name.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
        && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
        && crate::regs::Regs::default().get(name).is_none()
}

impl Session {
    /// Whether anything waits on a library load or unload.
    fn loads_wanted(&self) -> bool {
        self.libs.catch_load.is_some()
            || self.libs.catch_unload.is_some()
            || self.tr.lib.is_some()
            || self.bps.iter().any(|b| b.addr == 0 || b.name.is_some())
            || self.tr.hooks.iter().any(|h| h.addr == 0 || h.also.is_some())
    }

    /// Before the program runs: watch the loader while something waits on a library, and let it be otherwise.
    pub(crate) fn arm_loads(&mut self) {
        let want = self.loads_wanted();
        if self.t.stop_at_loads(want) {
            self.libs.armed = None;
        } else {
            self.libs.armed = if want { self.loader_hook() } else { None };
        }
        if !want {
            self.libs.seen = None;
        } else if self.libs.seen.is_none() {
            // a library that came in while nothing watched counts as loaded now
            self.mods.refresh(&mut *self.t);
            self.libs.seen = Some(self.loaded_now());
            let mut l = Vec::new();
            self.set_pending(&mut l);
            for line in l {
                self.log_line(line);
            }
        }
    }

    /// Where the loader tells a change, looked up once per program.
    fn loader_hook(&mut self) -> Option<u64> {
        if let Some(h) = self.libs.hook {
            return h;
        }
        let h = self.t.load_hook().or_else(|| match self.t.os() {
            Os::Linux => match self.mods.resolve("!_dl_debug_state") {
                Ok(Place::Code(a)) => Some(a),
                _ => None,
            },
            _ => None,
        });
        self.libs.hook = Some(h);
        h
    }

    fn loaded_now(&self) -> Vec<Seen> {
        self.mods.list.iter().map(|m| (m.path.clone(), m.base, m.end)).collect()
    }

    /// The import calls of the loaded file `idx`, by memory address.
    fn import_sites_at(&mut self, idx: usize) -> Vec<(u64, Site)> {
        let Some((img, bias)) = self.mods.image_bias(idx) else { return Vec::new() };
        nexo_bin::import_sites(img)
            .into_iter()
            .map(|s| (s.addr.wrapping_add(bias), Site { name: s.name, call_len: s.call_len }))
            .collect()
    }

    /// The loader added or removed libraries (its hook was hit, or the system said so). `Some` when `catch load`
    /// or `catch unload` stops here.
    pub(crate) fn on_load(&mut self, tid: Tid) -> Option<(Tid, Why)> {
        let before = self.libs.seen.clone()?;
        self.mods.refresh(&mut *self.t);
        let mut now = self.loaded_now();
        // a load is its file at its start: the end a look finds may grow (the program maps the same file again above
        // it and its headers in memory cannot be read) while the library stays where it was, with our bytes in it.
        // A load never grows, so it keeps the smallest end seen, and an unload forgets only what was in it.
        let same = |a: &Seen, b: &Seen| a.0 == b.0 && a.1 == b.1;
        for m in now.iter_mut() {
            if let Some(b) = before.iter().find(|b| same(b, m)) {
                m.2 = m.2.min(b.2);
            }
        }
        let has = |list: &[Seen], m: &Seen| list.iter().any(|x| same(x, m));
        let gone: Vec<Seen> = before.iter().filter(|m| !has(&now, m)).cloned().collect();
        let new: Vec<Seen> = now.iter().filter(|m| !has(&before, m)).cloned().collect();
        self.libs.seen = Some(now);
        if gone.is_empty() && new.is_empty() {
            return None;
        }
        let mut l = Vec::new();
        for (path, lo, hi) in &gone {
            self.lib_gone(path, *lo, *hi, &mut l);
        }
        for (path, base, _) in &new {
            self.lib_new(path, *base, &mut l);
        }
        self.set_pending(&mut l);
        for line in l {
            self.log_line(line);
        }
        let caught = |c: &Option<String>, list: &[Seen]| -> Vec<String> {
            let Some(c) = c else { return Vec::new() };
            list.iter()
                .filter(|m| short(&m.0).contains(c.as_str()))
                .map(|m| format!("{} at {:#x} ({})", short(&m.0), m.1, m.0))
                .collect()
        };
        let mut said = Vec::new();
        let came = caught(&self.libs.catch_load, &new);
        if !came.is_empty() {
            said.push(format!("library loaded: {}", came.join(", ")));
        }
        let went = caught(&self.libs.catch_unload, &gone);
        if !went.is_empty() {
            said.push(format!("library unloaded: {}", went.join(", ")));
        }
        (!said.is_empty()).then(|| (tid, Why::Load(said.join("; "))))
    }

    /// A library went away: our bytes in it went too, a breakpoint or hook set by name waits for it again, one set
    /// by address is disabled, and the trace forgets its call sites there.
    fn lib_gone(&mut self, path: &str, lo: u64, hi: u64, l: &mut Vec<String>) {
        let inside = |a: u64| a >= lo && a < hi;
        self.forget_inserted(lo, hi);
        let (mut wait, mut off) = (Vec::new(), Vec::new());
        for b in self.bps.iter_mut().filter(|b| inside(b.addr)) {
            if b.name.is_some() {
                b.addr = 0;
                wait.push(format!("#{}", b.id));
            } else if b.enabled {
                b.enabled = false;
                off.push(format!("#{}", b.id));
            }
        }
        for h in self.tr.hooks.iter_mut() {
            if inside(h.addr) {
                h.addr = 0;
                wait.push(format!("hook #{}", h.id));
            }
            if let Some(also) = &mut h.also {
                also.retain(|(a, _)| !inside(*a));
            }
        }
        if let Some(lib) = &mut self.tr.lib {
            lib.sites.retain(|a, _| !inside(*a));
        }
        let (stay, cut): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.tr.pending).into_iter().partition(|p| !inside(p.ret_addr));
        self.tr.pending = stay;
        for p in cut.into_iter().filter(|p| p.heap.is_none()) {
            self.tr.push(
                p.kind,
                &p.name,
                format!("{} = ? (it returns into {}, which was unloaded)", p.text, short(path)),
            );
        }
        let mut s = format!("{} unloaded", short(path));
        if !wait.is_empty() {
            s.push_str(&format!("; {} wait for it to load again", wait.join(" ")));
        }
        if !off.is_empty() {
            s.push_str(&format!("; breakpoints {} were addresses in it and are disabled", off.join(" ")));
        }
        l.push(s);
    }

    /// A library came in: `ltrace` and the hooks on the program's own import stubs take its import calls.
    fn lib_new(&mut self, path: &str, base: u64, l: &mut Vec<String>) {
        let Some(idx) = self.mods.list.iter().position(|m| m.path == path && m.base == base) else { return };
        let mut s = format!("{} loaded at {base:#x}", short(path));
        let hooked: Vec<usize> = (0..self.tr.hooks.len()).filter(|&i| self.tr.hooks[i].also.is_some()).collect();
        if self.tr.lib.is_none() && hooked.is_empty() {
            l.push(s);
            return;
        }
        let sites = self.import_sites_at(idx);
        if let Some(lib) = &mut self.tr.lib {
            let n = sites.iter().filter(|(_, x)| lib.names.is_empty() || lib.names.contains(&x.name)).count();
            lib.sites.extend(sites.iter().cloned());
            s.push_str(&format!("; ltrace follows its {n} import call sites"));
        }
        for i in hooked {
            let h = &mut self.tr.hooks[i];
            let more: Vec<(u64, Option<u8>)> =
                sites.iter().filter(|(_, x)| x.name == h.name).map(|(a, x)| (*a, x.call_len)).collect();
            if !more.is_empty() {
                s.push_str(&format!("; hook #{} takes its {} call sites of {}", h.id, more.len(), h.name));
                h.also.get_or_insert_with(Vec::new).extend(more);
            }
        }
        l.push(s);
    }

    /// Breakpoints and hooks waiting on a name are set where a loaded library has it.
    fn set_pending(&mut self, l: &mut Vec<String>) {
        let waiting: Vec<(u32, String)> =
            self.bps.iter().filter(|b| b.addr == 0).filter_map(|b| Some((b.id, b.name.clone()?))).collect();
        for (id, name) in waiting {
            if let Ok(Place::Code(a)) = self.mods.resolve(&lookup(&name)) {
                if let Some(b) = self.bps.iter_mut().find(|b| b.id == id) {
                    b.addr = a;
                }
                let at = self.mods.label(a, false).unwrap_or_default();
                l.push(format!("breakpoint #{id} on {name} is set at {a:#x} {at}"));
            }
        }
        let waiting: Vec<(u32, String)> =
            self.tr.hooks.iter().filter(|h| h.addr == 0).map(|h| (h.id, h.name.clone())).collect();
        for (id, name) in waiting {
            if let Ok(Place::Code(a)) = self.mods.resolve(&lookup(&name)) {
                if let Some(h) = self.tr.hooks.iter_mut().find(|h| h.id == id) {
                    h.addr = a;
                }
                let at = self.mods.label(a, false).unwrap_or_default();
                l.push(format!("hook #{id} on {name} is set at {a:#x} {at}"));
            }
        }
    }

    /// Whether the program imports a way to load a library by itself.
    pub(crate) fn loads_libraries(&self) -> bool {
        self.mods.main_image().imports.iter().any(|i| LOADERS.contains(&i.name.as_str()))
    }

    /// The files loaded now, for an answer about a name none of them has.
    pub(crate) fn loaded_names(&self) -> String {
        const SHOWN: usize = 6;
        let mut n: Vec<&str> = self.mods.list.iter().map(|m| short(&m.path)).collect();
        if n.len() > SHOWN {
            let more = n.len() - SHOWN;
            n.truncate(SHOWN);
            return format!("{} and {more} more", n.join(", "));
        }
        n.join(", ")
    }

    /// `catch load [NAME]`, `catch unload [NAME]`, `catch load off`.
    pub(crate) fn cmd_catch_load(&mut self, a: &Args) -> Result<Output> {
        let load = a.pos[0] == "load";
        let what = a.pos.get(1).cloned();
        let slot = if load { &mut self.libs.catch_load } else { &mut self.libs.catch_unload };
        let verb = if load { "loads" } else { "is unloaded" };
        let line = match what.as_deref() {
            Some("off") => {
                *slot = None;
                format!("no stop when a library {verb}")
            }
            _ if a.pos.len() > 2 => return usage("catch load|unload [NAME|off]"),
            Some(n) => {
                *slot = Some(n.to_string());
                format!("the program stops when a library whose file name holds {n:?} {verb}; `cont` goes on")
            }
            None => {
                *slot = Some(String::new());
                format!("the program stops when any library {verb}; `cont` goes on")
            }
        };
        Ok(rows("catch", vec![line], json!({"load": self.libs.catch_load, "unload": self.libs.catch_unload})))
    }
}
