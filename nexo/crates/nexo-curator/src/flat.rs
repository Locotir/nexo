//! Per-step file names kept up to date as steps arrive, so that asking for them costs what changed, not
//! the length of the session.

use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

/// The files each step read, each by the parts of its name the other files read do not share (see
/// `Engine::step_files`). A step is added once; a part that becomes shared (two files hold it) redoes
/// only the steps that hold that part.
#[derive(Clone, Default)]
pub struct StepFiles {
    known: FxHashSet<Vec<String>>,
    parts: FxHashMap<String, usize>,
    at: FxHashMap<String, Vec<usize>>,
    raw: Vec<Vec<Vec<String>>>,
    own: Arc<Vec<Vec<Vec<String>>>>,
}

impl StepFiles {
    /// How many steps were taken in.
    pub fn seen(&self) -> usize {
        self.raw.len()
    }

    fn own_of(&self, files: &[Vec<String>]) -> Vec<Vec<String>> {
        let shared = |p: &String| self.parts.get(p).copied().unwrap_or(0) > 1;
        files
            .iter()
            .map(|f| {
                let own: Vec<String> = f.iter().filter(|p| !shared(p)).cloned().collect();
                if own.is_empty() {
                    f.clone()
                } else {
                    own
                }
            })
            .collect()
    }

    /// Takes in the next step's files.
    pub fn push(&mut self, files: Vec<Vec<String>>) {
        let idx = self.raw.len();
        let mut flipped: Vec<String> = Vec::new();
        let mut held: FxHashSet<&String> = FxHashSet::default();
        for f in &files {
            held.extend(f.iter());
            if self.known.insert(f.clone()) {
                let mut parts = f.clone();
                parts.sort();
                parts.dedup();
                for p in parts {
                    let c = self.parts.entry(p.clone()).or_default();
                    *c += 1;
                    if *c == 2 {
                        flipped.push(p);
                    }
                }
            }
        }
        for p in held {
            self.at.entry(p.clone()).or_default().push(idx);
        }
        let mine = self.own_of(&files);
        self.raw.push(files);
        let own = Arc::make_mut(&mut self.own);
        own.push(mine);
        for p in flipped {
            if let Some(steps) = self.at.get(&p) {
                for &s in steps {
                    let fresh = {
                        let shared = |q: &String| self.parts.get(q).copied().unwrap_or(0) > 1;
                        self.raw[s]
                            .iter()
                            .map(|f| {
                                let own: Vec<String> = f.iter().filter(|q| !shared(q)).cloned().collect();
                                if own.is_empty() {
                                    f.clone()
                                } else {
                                    own
                                }
                            })
                            .collect::<Vec<_>>()
                    };
                    own[s] = fresh;
                }
            }
        }
    }

    /// The files of every step, as `Engine::step_files` gives them.
    pub fn all(&self) -> Arc<Vec<Vec<Vec<String>>>> {
        self.own.clone()
    }
}

/// What `Engine::leads` needs of the whole session, kept as units arrive: the forms of the names the
/// agent wrote in its text and calls, and in how many outputs each name appears.
#[derive(Clone, Default)]
pub struct LeadIndex {
    pub seen: usize,
    pub forms: FxHashSet<String>,
    pub results: FxHashMap<u32, u32>,
}
