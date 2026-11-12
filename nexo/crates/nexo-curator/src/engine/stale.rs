//! Reads that a later edit of their file has made out of date.

use rustc_hash::FxHashSet;

use crate::hindsight::UKind;
use crate::msg::get_str;
use crate::textops::{self};
use crate::tools::{full_write, heredoc_writes, Kind};

use super::files::{command_of, same_file};
use super::Engine;

/// The name the program a debugger drives goes by where a file's name goes: a call that moves it is an edit of it.
pub(super) const DEBUG_STATE: &str = "debug:";

impl Engine {
    /// The file a read result `i` holds and the message of the first later edit or write of it that did not
    /// fail: the read no longer shows what the file holds (`None`: not a read, or not changed since). A shell
    /// command that only printed files (`shell_reads`) reads them as a read tool does.
    pub(super) fn superseded_by(&self, i: usize) -> Option<(String, usize)> {
        self.read_superseded(i, false)
    }

    /// As `superseded_by`; with `shell`, a shell command that only printed files counts as a read of them.
    pub(super) fn read_superseded(&self, i: usize, shell: bool) -> Option<(String, usize)> {
        let u = self.tr.units.get(i)?;
        let info = self.fz.info.get(&i)?;
        if u.kind != UKind::Result || info.error {
            return None;
        }
        if let Some(session) = self.dbg_looks.get(&i) {
            // a move names a session or none; none could be any, so it ages every look, and a look naming none
            // could be of any session, so every move ages it
            return self.edits.iter().find_map(|(p, mi, id)| {
                let moved = p.strip_prefix(DEBUG_STATE)?;
                let same = moved.is_empty() || session.is_empty() || moved == session;
                (*mi > u.msg && same && self.edit_ok.get(id) == Some(&true)).then(|| (DEBUG_STATE.to_string(), *mi))
            });
        }
        let paths: Vec<&str> = match info.kind {
            Kind::Read => info.path.as_deref().filter(|p| !p.is_empty()).into_iter().collect(),
            Kind::Exec if shell => self.shell_reads.get(&i)?.iter().map(String::as_str).collect(),
            _ => return None,
        };
        paths.into_iter().find_map(|path| {
            self.edits
                .iter()
                .find(|(p, mi, id)| *mi > u.msg && same_file(path, p) && self.edit_ok.get(id) == Some(&true))
                .map(|(_, mi, _)| (path.to_string(), *mi))
        })
    }

    /// The read results of the messages `vis` that a later edit of their file made out of date
    /// (`Params.stale`; empty when it is off).
    pub(super) fn stale_reads(&self, vis: &[usize]) -> FxHashSet<usize> {
        if self.p.stale == 0 || self.edits.is_empty() {
            return FxHashSet::default();
        }
        vis.iter().flat_map(|&mi| self.fz.by_msg(mi)).copied().filter(|&i| self.superseded_by(i).is_some()).collect()
    }

    /// The lines that call ``i`` (in message ``mi``) wrote whole to a file written whole again later.
    pub(super) fn superseded(&self, i: usize, mi: usize) -> FxHashSet<String> {
        let b = self.tr.block(i);
        let info = self.fz.info.get(&i);
        let mut bodies: Vec<(String, String)> = match info.map(|x| x.kind) {
            Some(Kind::Write) => full_write(get_str(b, "name"), b.get("input"))
                .map(|t| (info.and_then(|x| x.path.clone()).unwrap_or_default(), t.to_string()))
                .into_iter()
                .collect(),
            Some(Kind::Exec) => heredoc_writes(&command_of(b)),
            _ => Vec::new(),
        };
        bodies.retain(|(p, _)| self.wrote.iter().any(|(k, w)| w.mi > mi && same_file(k, p)));
        bodies.iter().flat_map(|(_, t)| t.split('\n').map(textops::squash_ws)).collect()
    }
}
