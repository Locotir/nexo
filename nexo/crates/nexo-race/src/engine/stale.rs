//! Reads that a later edit of their file has made out of date.

use crate::sorted::FxHashSet;

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
        // an engine just read back from a snapshot has not indexed its edits yet (`take_history` does it): until
        // then the search goes over them all, with the same answer
        self.superseded_with(i, shell, self.edit_index.at() == self.edits.len())
    }

    /// `read_superseded` by a search over every edit, as it was before the index: what the index must answer.
    #[cfg(test)]
    pub(super) fn read_superseded_linear(&self, i: usize, shell: bool) -> Option<(String, usize)> {
        self.superseded_with(i, shell, false)
    }

    /// `read_superseded`, finding the first later edit through `edit_index` or by a search over every edit.
    fn superseded_with(&self, i: usize, shell: bool, indexed: bool) -> Option<(String, usize)> {
        let u = self.tr.units.get(i)?;
        let info = self.fz.info.get(&i)?;
        if u.kind != UKind::Result || info.error {
            return None;
        }
        let ok = |id: &str| self.edit_ok.get(id) == Some(&true);
        if let Some(session) = self.dbg_looks.get(&i) {
            // a move names a session or none; none could be any, so it ages every look, and a look naming none
            // could be of any session, so every move ages it
            let pos = if indexed {
                self.edit_index.first_debug(&self.edits, session, u.msg, &ok)
            } else {
                self.edits.iter().position(|(p, mi, id)| {
                    work!(StaleCmp, 1);
                    p.strip_prefix(DEBUG_STATE).is_some_and(|moved| {
                        *mi > u.msg && (moved.is_empty() || session.is_empty() || moved == session) && ok(id)
                    })
                })
            };
            return pos.map(|pos| (DEBUG_STATE.to_string(), self.edits[pos].1));
        }
        let paths: Vec<&str> = match info.kind {
            Kind::Read => info.path.as_deref().filter(|p| !p.is_empty()).into_iter().collect(),
            Kind::Exec if shell => self.shell_reads.get(&i)?.iter().map(String::as_str).collect(),
            _ => return None,
        };
        paths.into_iter().find_map(|path| {
            let pos = if indexed {
                self.edit_index.first_file(&self.edits, path, u.msg, &ok)
            } else {
                self.edits.iter().position(|(p, mi, id)| {
                    work!(StaleCmp, 1);
                    *mi > u.msg && same_file(path, p) && ok(id)
                })
            };
            pos.map(|pos| (path.to_string(), self.edits[pos].1))
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
