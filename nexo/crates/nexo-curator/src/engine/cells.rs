//! The memory map of a reply, kept from one request to the next: only the cells of the messages that are in
//! the view, or were in the last one, are looked at again.

use std::sync::Arc;

use rustc_hash::FxHashSet;

use crate::goal::head_slice;
use crate::memory::{Cell, Memory, State, Zone};

use super::Engine;

/// The map as last given, and what the last request changed in it.
#[derive(Clone, Default)]
pub(super) struct Cells {
    zones: Arc<Vec<Zone>>,
    cells: Arc<Vec<Cell>>,
    /// The messages of the view the map was last given for: the only ones whose state may differ now, with
    /// the ones of the view as it is now.
    vis: Arc<Vec<usize>>,
    /// Whether every topic's zone is active and none is a card: so it is while no topic is left out.
    plain: bool,
    /// The cells whose state the last map changed (a new message that is in the view too), and their state.
    changed: Vec<(usize, State)>,
    /// How many cells the map before the last one had.
    before: usize,
    /// Whether the last map was made from nothing: a new engine (the first request, or a history rewritten),
    /// or no request yet to open a topic.
    rebuilt: bool,
}

fn zone(label: &str) -> Zone {
    Zone { label: label.into(), active: true, card: false }
}

impl Engine {
    /// Whether a message of the view is sent with parts cut or whole.
    fn cell_state(&self, mi: usize) -> State {
        if self.fz.by_msg(mi).iter().any(|i| self.dropped.contains_key(i)) {
            State::Cut
        } else {
            State::Whole
        }
    }

    /// Everything the engine holds, by zone, and what of it the next call takes.
    pub fn memory(&mut self) -> Memory {
        let n = self.tr.msgs.len();
        let vis = self.visible();
        if self.goal.turns.is_empty() {
            // no request opened a topic yet: the view is every message, and nothing is kept
            self.cells = Cells { rebuilt: true, ..Cells::default() };
            let cell = |mi: usize| Cell {
                zone: usize::from(mi > 0),
                tokens: self.sizes[mi].max(1),
                state: self.cell_state(mi),
            };
            let zones = vec![zone("anchor"), zone("conversation")];
            return Memory { zones: Arc::new(zones), cells: Arc::new((0..n).map(cell).collect()) };
        }
        let mut c = std::mem::take(&mut self.cells);
        c.rebuilt = c.zones.is_empty();
        c.before = c.cells.len();
        c.changed.clear();
        let g = &self.goal;
        // the zones: a topic's label is the request that opened it, and it never changes
        if c.zones.len() < g.members.len() + 1 {
            let zones = Arc::make_mut(&mut c.zones);
            if zones.is_empty() {
                zones.push(zone("anchor"));
            }
            for members in &g.members[zones.len() - 1..] {
                zones.push(zone(&g.ask(&self.tr, &self.fz, members[0])));
            }
        }
        if self.apart() {
            let a = g.active();
            let others = g.others();
            let shown = head_slice(others.len(), self.p.cards);
            let carded: FxHashSet<usize> = others[..shown].iter().copied().collect();
            for (k, z) in Arc::make_mut(&mut c.zones).iter_mut().skip(1).enumerate() {
                (z.active, z.card) = (k == a, carded.contains(&k));
            }
            c.plain = false;
        } else if !c.plain {
            for z in Arc::make_mut(&mut c.zones).iter_mut().skip(1) {
                (z.active, z.card) = (true, false);
            }
            c.plain = true;
        }
        // the cells: a message's zone (the topic of the turn that holds it) and its tokens as it arrived are
        // settled when it comes; its state changes only by entering or leaving the view, or by a cut while in it
        let seen: FxHashSet<usize> = vis.iter().copied().collect();
        let cells = Arc::make_mut(&mut c.cells);
        for &mi in c.vis.iter() {
            if cells[mi].state != State::Out && !seen.contains(&mi) {
                cells[mi].state = State::Out;
                c.changed.push((mi, State::Out));
            }
        }
        for mi in cells.len()..n {
            let zone = if mi == 0 { 0 } else { g.thread_at(mi) + 1 };
            cells.push(Cell { zone, tokens: self.sizes[mi].max(1), state: State::Out });
        }
        for &mi in vis.iter() {
            let state = self.cell_state(mi);
            if cells[mi].state != state {
                cells[mi].state = state;
                c.changed.push((mi, state));
            }
        }
        c.vis = vis;
        let out = Memory { zones: c.zones.clone(), cells: c.cells.clone() };
        self.cells = c;
        out
    }

    /// The map made from nothing, every message read: what [`Engine::memory`] is to give, for the tests.
    #[cfg(test)]
    pub(crate) fn memory_whole(&mut self) -> Memory {
        let n = self.tr.msgs.len();
        let seen: FxHashSet<usize> = self.visible().iter().copied().collect();
        let g = &self.goal;
        let mut zones = vec![zone("anchor")];
        let mut at = vec![1; n];
        if g.turns.is_empty() {
            zones.push(zone("conversation"));
        } else {
            let (a, others, apart) = (g.active(), g.others(), self.apart());
            let shown = if apart { head_slice(others.len(), self.p.cards) } else { 0 };
            for (k, members) in g.members.iter().enumerate() {
                let label = g.ask(&self.tr, &self.fz, members[0]).into();
                zones.push(Zone { label, active: k == a || !apart, card: others[..shown].contains(&k) });
                for &j in members {
                    let (x, y) = g.span(j, n);
                    at[x..y].fill(k + 1);
                }
            }
        }
        let cell = |mi: usize| Cell {
            zone: if mi == 0 { 0 } else { at[mi] },
            tokens: self.sizes[mi].max(1),
            state: if seen.contains(&mi) { self.cell_state(mi) } else { State::Out },
        };
        Memory { zones: Arc::new(zones), cells: Arc::new((0..n).map(cell).collect()) }
    }

    /// What the last [`Engine::memory`] changed when it went on from the map before it: how many cells that
    /// one had, and the cells whose state differs now with that state. `None` when it made the map from
    /// nothing, and every cell is to be read.
    pub(crate) fn memory_changed(&self) -> Option<(usize, &[(usize, State)])> {
        (!self.cells.rebuilt).then_some((self.cells.before, &self.cells.changed[..]))
    }
}
