//! A test-only counter of the curator's work, so that a test can check how the work of a view grows with the
//! history without reading a clock.
//!
//! `work!(Name, k)` adds `k` to the counter `Name` of the running thread under `cfg(test)`, and expands to
//! nothing otherwise: a release or default build compiles the code it compiled before. The counters live in a
//! thread-local, so tests that run on parallel threads do not see each other's (the curator spawns no thread).

/// Adds `$k` to the counter `$w` (a variant of `Work`) under `cfg(test)`; nothing otherwise.
#[cfg(test)]
macro_rules! work {
    ($w:ident, $k:expr) => {
        $crate::work::add($crate::work::Work::$w, ($k) as u64)
    };
}

#[cfg(not(test))]
macro_rules! work {
    ($w:ident, $k:expr) => {};
}

#[cfg(test)]
pub(crate) use counters::*;

#[cfg(test)]
mod counters {
    use std::cell::Cell;

    /// What is counted. Each counter is one unit of the work the name says; the guard compares a counter at
    /// two history sizes, so only how it grows matters.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Work {
        /// Entries of the sizes vector built (messages) and visible messages weighed, by `sizes_now`.
        SizesCell,
        /// Edits examined by a stale-read lookup, plus one for each lookup.
        StaleCmp,
        /// Edits indexed by `track_edits` plus one for each result looked up.
        EditCmp,
        /// Turns scored by `Threads::open` (kept quadratic, see the perf SPEC 4.3).
        TurnScored,
        /// Entries of the maps an epoch try copies (`Engine::clone`).
        CloneUnit,
        /// Characters of the view `page` walked over.
        PageByte,
        /// Cells copied because the last reply still held them.
        CellCopy,
        /// Messages `visible` scanned when it rebuilt its answer.
        VisibleScan,
        /// Calls of `sizes_now`.
        SizesCalls,
        /// Epoch tries (`Engine::clone` in `decide`).
        CloneCalls,
        /// Passes of `page` over the view.
        PageCalls,
        /// Times the cells were copied because the last reply still held them.
        CellCopies,
    }

    pub(crate) const K: usize = 12;

    impl Work {
        /// The event counter a work counter is divided by (work per event is what grows with the history), if it has one.
        pub(crate) fn per(self) -> Option<Work> {
            match self {
                Work::SizesCell => Some(Work::SizesCalls),
                Work::CloneUnit => Some(Work::CloneCalls),
                Work::PageByte => Some(Work::PageCalls),
                Work::CellCopy => Some(Work::CellCopies),
                _ => None,
            }
        }

        /// Whether the counter counts events, not work.
        pub(crate) fn is_event(self) -> bool {
            matches!(self, Work::SizesCalls | Work::CloneCalls | Work::PageCalls | Work::CellCopies)
        }

        pub(crate) const ALL: [Work; K] = [
            Work::SizesCell,
            Work::StaleCmp,
            Work::EditCmp,
            Work::TurnScored,
            Work::CloneUnit,
            Work::PageByte,
            Work::CellCopy,
            Work::VisibleScan,
            Work::SizesCalls,
            Work::CloneCalls,
            Work::PageCalls,
            Work::CellCopies,
        ];
    }

    thread_local! {
        static WORK: [Cell<u64>; K] = const { [const { Cell::new(0) }; K] };
    }

    pub(crate) fn add(w: Work, k: u64) {
        WORK.with(|c| c[w as usize].set(c[w as usize].get().saturating_add(k)));
    }

    /// The counters of this thread, which start again from zero.
    pub(crate) fn take() -> [u64; K] {
        WORK.with(|c| std::array::from_fn(|i| c[i].replace(0)))
    }
}
