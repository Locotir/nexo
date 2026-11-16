//! A walk of a folder that reads a fixed number of directory entries however big the tree is.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// A walk of a tree with the `ignore` crate's filters (hidden files and those in `.gitignore` left out) that
/// reads at most `cap` directory entries in all. The cap counts every entry of each folder the walk enters,
/// the ones its filters leave out included: the crate reads each of those and matches it against the ignore
/// rules before it drops it, without giving it, so a cap on the entries the walk gives would let a folder of a
/// million ignored files or dot files be read in full. Each folder is counted before the walk enters it; the
/// first one that does not fit in what is left is still given, so its name is known, and the walk ends there
/// without reading it.
pub struct BoundedWalk {
    walk: Option<ignore::Walk>,
    budget: Arc<Budget>,
}

#[derive(Default)]
struct Budget {
    left: AtomicUsize,
    read: AtomicUsize,
    stopped: AtomicBool,
}

impl BoundedWalk {
    /// Walks `root` as `builder` (made for `root`) is set, down to `max_depth`. The builder's own entry filter
    /// is replaced by the one that keeps the count.
    pub fn new(root: &Path, mut builder: ignore::WalkBuilder, max_depth: Option<usize>, cap: usize) -> BoundedWalk {
        let budget = Arc::new(Budget::default());
        // the walk reads the root's entries before it can ask about any of them, so the root is counted here
        let first = count_entries(root, cap.saturating_add(1));
        budget.read.store(first, Ordering::Relaxed);
        if first > cap {
            budget.stopped.store(true, Ordering::Relaxed);
            return BoundedWalk { walk: None, budget };
        }
        budget.left.store(cap - first, Ordering::Relaxed);
        let b = Arc::clone(&budget);
        builder.max_depth(max_depth).filter_entry(move |e| {
            // a folder at the deepest level is given but not read, and a file was counted with its folder
            let entered = e.file_type().is_some_and(|t| t.is_dir()) && max_depth.is_none_or(|m| e.depth() < m);
            if entered {
                let left = b.left.load(Ordering::Relaxed);
                let n = count_entries(e.path(), left.saturating_add(1));
                b.read.fetch_add(n, Ordering::Relaxed);
                if n > left {
                    b.left.store(0, Ordering::Relaxed);
                    b.stopped.store(true, Ordering::Relaxed);
                } else {
                    b.left.store(left - n, Ordering::Relaxed);
                }
            }
            true
        });
        BoundedWalk { walk: Some(builder.build()), budget }
    }

    /// Whether the walk stopped at its cap before the end of the tree.
    pub fn stopped(&self) -> bool {
        self.budget.stopped.load(Ordering::Relaxed)
    }

    /// The directory entries the walk counted, the ones its filters leave out included. It reads each of them
    /// at most twice (once to count it, once to walk it), so this bounds its work.
    pub fn read(&self) -> usize {
        self.budget.read.load(Ordering::Relaxed)
    }
}

impl Iterator for BoundedWalk {
    type Item = ignore::DirEntry;

    fn next(&mut self) -> Option<ignore::DirEntry> {
        // the folder that did not fit was the last entry given: one more step would read it
        if self.stopped() {
            self.walk = None;
        }
        let walk = self.walk.as_mut()?;
        // a folder it cannot read is passed over, as the rest of the walk goes on without it
        walk.find_map(Result::ok)
    }
}

/// The entries of the folder at `dir`, counting no further than `most`; none when it cannot be read.
fn count_entries(dir: &Path, most: usize) -> usize {
    std::fs::read_dir(dir).map_or(0, |r| r.take(most).count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".gitignore"), "*.npy\n").unwrap();
        std::fs::create_dir(d.path().join("data")).unwrap();
        for i in 0..500 {
            std::fs::write(d.path().join(format!("data/x{i}.npy")), "").unwrap();
            std::fs::write(d.path().join(format!("data/.cache{i}")), "").unwrap();
        }
        std::fs::create_dir(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/lib.rs"), "").unwrap();
        d
    }

    #[test]
    fn ignored_and_hidden_entries_count_towards_the_cap() {
        let d = tree();
        let builder = || {
            let mut b = ignore::WalkBuilder::new(d.path());
            b.require_git(false);
            b
        };
        // the walk gives five entries at most, but a thousand sit in data/ for it to read and drop
        let mut walk = BoundedWalk::new(d.path(), builder(), None, 200);
        let given: Vec<_> = walk.by_ref().collect();
        assert!(walk.stopped(), "{given:?}");
        assert!(walk.read() <= 201, "{}", walk.read());
        assert!(given.len() <= 4, "{given:?}");
        let mut whole = BoundedWalk::new(d.path(), builder(), None, 20_000);
        let all: Vec<_> = whole.by_ref().map(|e| e.path().to_path_buf()).collect();
        assert!(!whole.stopped());
        assert_eq!(whole.read(), 1004, "the root's 3, data/'s 1000, src/'s 1");
        let plain: Vec<_> = builder().build().flatten().map(|e| e.path().to_path_buf()).collect();
        assert_eq!(all, plain, "under its cap the walk gives what the crate's own walk gives");
    }

    #[test]
    fn a_folder_past_the_cap_is_given_but_not_read() {
        let d = tree();
        let mut walk = BoundedWalk::new(d.path(), ignore::WalkBuilder::new(d.path()), None, 50);
        let given: Vec<_> = walk.by_ref().map(|e| e.path().strip_prefix(d.path()).unwrap().to_path_buf()).collect();
        assert!(walk.stopped());
        assert_eq!(given.last().map(|p| p.as_os_str()), Some("data".as_ref()), "{given:?}");
        assert!(walk.read() <= 51, "{}", walk.read());
    }

    #[test]
    fn a_root_past_the_cap_is_not_walked() {
        let d = tree();
        let mut walk =
            BoundedWalk::new(&d.path().join("data"), ignore::WalkBuilder::new(d.path().join("data")), None, 100);
        assert_eq!(walk.next().map(|e| e.path().to_path_buf()), None);
        assert!(walk.stopped() && walk.read() == 101, "{}", walk.read());
    }

    #[test]
    fn a_folder_at_the_deepest_level_is_not_counted() {
        let d = tree();
        let mut walk = BoundedWalk::new(d.path(), ignore::WalkBuilder::new(d.path()), Some(1), 10);
        let given = walk.by_ref().count();
        assert!(!walk.stopped(), "data/ is at depth 1 and is not read");
        assert_eq!((given, walk.read()), (3, 3), "the root, data/ and src/");
    }
}
