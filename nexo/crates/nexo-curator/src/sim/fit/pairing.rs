use super::*;

/// A curator run and its plain twin (same task, model, rep and commit), either of which may be missing.
#[derive(Debug, Clone)]
pub struct Unit {
    pub plain: Option<usize>,
    pub cur: Option<usize>,
    /// The rep as recorded, and the one parity is taken from (see `pair_up`).
    pub rep: i64,
    pub eff_rep: i64,
}

/// Pairs the plain and curator rows by (task, model, rep, commit); inside a group the pair from the same
/// directory goes first. Every run directory of the older benches counts its reps from 0, so when a task's
/// reps collide the parity is taken from the position of the pair among its task's pairs (directory order)
/// instead of the recorded rep; with unique reps it is the recorded rep.
pub fn pair_up(rows: &[Row]) -> Vec<Unit> {
    type Group = (Vec<usize>, Vec<usize>);
    let mut groups: BTreeMap<(String, String, i64, String), Group> = BTreeMap::new();
    for (i, r) in rows.iter().enumerate() {
        let key = (r.task.clone(), r.model.clone(), r.rep, r.commit.clone());
        match r.kind {
            ArmKind::Plain => groups.entry(key).or_default().0.push(i),
            ArmKind::Curator => groups.entry(key).or_default().1.push(i),
            ArmKind::Other => {}
        }
    }
    let mut units = Vec::new();
    for ((_, _, rep, _), (mut plains, mut curs)) in groups {
        let mut out = Vec::new();
        for &c in &curs {
            if let Some(k) = plains.iter().position(|&p| rows[p].dir == rows[c].dir) {
                out.push((Some(plains.remove(k)), Some(c)));
            }
        }
        curs.retain(|c| !out.iter().any(|(_, x)| *x == Some(*c)));
        for c in curs {
            let p = (!plains.is_empty()).then(|| plains.remove(0));
            out.push((p, Some(c)));
        }
        for p in plains {
            out.push((Some(p), None));
        }
        for (plain, cur) in out {
            units.push(Unit { plain, cur, rep, eff_rep: rep });
        }
    }
    // effective rep: the recorded reps when each task's are unique; else, since the older run directories
    // each count their reps from 0, the ordinal of the unit in (task, model, directory, rep) order, which
    // splits the whole set into two balanced halves
    let mut by_task: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (k, u) in units.iter().enumerate() {
        let r = &rows[u.cur.or(u.plain).expect("a unit holds a row")];
        by_task.entry((r.task.clone(), r.model.clone())).or_default().push(k);
    }
    let collide = by_task.values().any(|ks| ks.iter().map(|&k| units[k].rep).collect::<BTreeSet<_>>().len() < ks.len());
    if collide {
        let mut all: Vec<usize> = (0..units.len()).collect();
        all.sort_by_key(|&k| {
            let r = &rows[units[k].cur.or(units[k].plain).unwrap()];
            (r.task.clone(), r.model.clone(), r.dir.clone(), units[k].rep)
        });
        for (n, k) in all.into_iter().enumerate() {
            units[k].eff_rep = n as i64;
        }
    }
    units
}

/// Which reps stay out of the fit (they are the ones to check on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Holdout {
    None,
    Odd,
    Even,
}

impl Holdout {
    pub fn parse(s: &str) -> Option<Holdout> {
        match s {
            "odd" => Some(Holdout::Odd),
            "even" => Some(Holdout::Even),
            "none" | "all" => Some(Holdout::None),
            _ => None,
        }
    }

    /// Whether a unit with this rep is held out.
    pub fn holds(self, eff_rep: i64) -> bool {
        match self {
            Holdout::None => false,
            Holdout::Odd => eff_rep.rem_euclid(2) == 1,
            Holdout::Even => eff_rep.rem_euclid(2) == 0,
        }
    }
}

// ---- transcripts: induced re-fetches ----
