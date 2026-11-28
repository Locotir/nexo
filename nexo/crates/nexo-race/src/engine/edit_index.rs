//! A per-file index of the edits and writes (`Engine::edits`), so that «the first edit of this file after
//! message `m`» costs the depth of the path and the edits of that file, not every edit of the session.

use crate::sorted::{FxHashMap, FxHashSet};

use super::{files::PathSet, stale::DEBUG_STATE, Shared};

/// Positions in `Engine::edits`, in push order, of the edits that name a path as `same_file` reads it. Not part of
/// a snapshot: it is built again from `edits`, lazily, as they come (`sync_edit_caches`).
#[derive(Clone, Default)]
pub(super) struct EditIndex {
    /// How many edits it covers.
    at: usize,
    /// The path as `same_file` reads it (`PathSet::norm`), and the positions of the edits that name it.
    whole: FxHashMap<String, Vec<usize>>,
    /// Every tail of those paths that starts after a `/`, and the positions of the edits whose path has it.
    tails: FxHashMap<String, Vec<usize>>,
    /// The edits that move a debugged program (`DEBUG_STATE`).
    debug: Vec<usize>,
    /// The message of the last edit covered, and whether the messages ever went down along the positions (they
    /// do not in an engine that took its units in order: then a search can start where the message is).
    last: usize,
    unsorted: bool,
}

impl EditIndex {
    /// How many edits it covers.
    pub(super) fn at(&self) -> usize {
        self.at
    }

    /// Covers the edits it has not seen yet.
    pub(super) fn extend(&mut self, edits: &[(String, usize, String)]) {
        if self.at > edits.len() {
            *self = EditIndex::default();
        }
        for (pos, (path, mi, _)) in edits.iter().enumerate().skip(self.at) {
            if *mi < self.last {
                self.unsorted = true;
            }
            self.last = *mi;
            let p = PathSet::norm(path);
            for (i, _) in p.match_indices('/') {
                self.tails.entry(p[i + 1..].to_string()).or_default().push(pos);
            }
            self.whole.entry(p).or_default().push(pos);
            if path.starts_with(DEBUG_STATE) {
                self.debug.push(pos);
            }
        }
        self.at = edits.len();
    }

    /// Where a search of `list` for an edit after message `after` starts.
    fn start(&self, list: &[usize], edits: &[(String, usize, String)], after: usize) -> usize {
        if self.unsorted {
            0
        } else {
            list.partition_point(|&pos| edits[pos].1 <= after)
        }
    }

    /// The position of the first edit of `list` after message `after` that `ok` takes (its call's id).
    fn first_in(
        &self,
        list: &[usize],
        edits: &[(String, usize, String)],
        after: usize,
        ok: &dyn Fn(&str) -> bool,
    ) -> Option<usize> {
        let from = self.start(list, edits, after);
        for &pos in &list[from..] {
            work!(StaleCmp, 1);
            let (_, mi, id) = &edits[pos];
            if *mi > after && ok(id) {
                return Some(pos);
            }
        }
        None
    }

    /// The first edit (in push order) after message `after` of a file `same_file` as `path` that `ok` takes.
    pub(super) fn first_file(
        &self,
        edits: &[(String, usize, String)],
        path: &str,
        after: usize,
        ok: &dyn Fn(&str) -> bool,
    ) -> Option<usize> {
        work!(StaleCmp, 1);
        let q = PathSet::norm(path);
        let own = [self.whole.get(&q), self.tails.get(&q)];
        // an edit path that is `q`, one that has `q` as a tail, and one that is a tail of `q`
        let shorter = q.match_indices('/').map(|(i, _)| self.whole.get(&q[i + 1..]));
        own.into_iter().chain(shorter).flatten().filter_map(|list| self.first_in(list, edits, after, ok)).min()
    }

    /// The first edit that moves the debugged program after message `after`, that `ok` takes, and that a look at
    /// `session` is out of date by (a move that names none, or a look that does, is any session).
    pub(super) fn first_debug(
        &self,
        edits: &[(String, usize, String)],
        session: &str,
        after: usize,
        ok: &dyn Fn(&str) -> bool,
    ) -> Option<usize> {
        work!(StaleCmp, 1);
        let from = self.start(&self.debug, edits, after);
        self.debug[from..].iter().copied().find(|&pos| {
            work!(StaleCmp, 1);
            let (p, mi, id) = &edits[pos];
            let moved = p.strip_prefix(DEBUG_STATE).unwrap_or("");
            *mi > after && (moved.is_empty() || session.is_empty() || moved == session) && ok(id)
        })
    }
}

/// The ids of `edits` (`Engine::edit_ids`: how many are covered, and the set) and the index of them, both
/// covering the edits pushed since the last call: each edit is looked at once, whatever the number of calls.
pub(super) fn sync_edit_caches(
    ids: &mut Shared<(usize, FxHashSet<String>)>,
    index: &mut Shared<EditIndex>,
    edits: &[(String, usize, String)],
) {
    if (**ids).0 > edits.len() {
        *ids = Default::default();
    }
    // nothing new: no write, so a shared copy stays shared
    if (**ids).0 < edits.len() {
        let covered = (**ids).0;
        let own = ids.mt();
        for e in &edits[covered..] {
            own.1.insert(e.2.clone());
        }
        work!(EditCmp, edits.len() - covered);
        own.0 = edits.len();
    }
    if index.at() != edits.len() {
        index.mt().extend(edits);
    }
}
