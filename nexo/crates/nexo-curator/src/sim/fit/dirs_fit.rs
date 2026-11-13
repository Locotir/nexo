use super::*;

/// What `fit` does.
#[derive(Debug, Clone)]
pub struct FitConfig {
    pub holdout: Holdout,
    /// Fit the parameters of this model; `None` pools every model.
    pub model: Option<String>,
    /// Families with fewer events fall back to the pooled fit.
    pub min_events: usize,
    pub seed: u64,
    pub workers: usize,
}

impl Default for FitConfig {
    fn default() -> FitConfig {
        FitConfig {
            holdout: Holdout::None,
            model: Some("deepseek-flash".to_string()),
            min_events: 20,
            seed: 1,
            workers: 4,
        }
    }
}

/// The result of a fit: every (family, model) group, the pooled fit per model filter, and the baselines.
pub struct Fit {
    pub obs: Vec<Obs>,
    /// By (family, model).
    pub groups: BTreeMap<(String, String), GroupFit>,
    /// By family over the models the config selects.
    pub families: BTreeMap<String, GroupFit>,
    pub global: GroupFit,
    pub sources: Vec<String>,
    pub config: FitConfig,
    pub held_out: usize,
}

/// Runs `f` over the indices `0..n` on up to `workers` threads, keeping the order.
pub(super) fn par_map<T: Send>(n: usize, workers: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<T>>> = (0..n).map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..workers.clamp(1, n.max(1)) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if i >= n {
                    break;
                }
                *slots[i].lock().unwrap() = Some(f(i));
            });
        }
    });
    slots.into_iter().map(|m| m.into_inner().unwrap().expect("every slot is filled")).collect()
}

/// Fits the parameters from the run directories.
pub fn fit_dirs(dirs: &[String], cfg: &FitConfig) -> Result<Fit, String> {
    let rows = load_rows(dirs)?;
    let units = pair_up(&rows);
    let held_out = units.iter().filter(|u| u.cur.is_some() && cfg.holdout.holds(u.eff_rep)).count();
    let kept: Vec<&Unit> = units.iter().filter(|u| !cfg.holdout.holds(u.eff_rep)).collect();
    let seen = par_map(kept.len(), cfg.workers, |i| observe(&rows, kept[i]));
    let mut obs = Vec::new();
    let mut ans: Vec<(String, String, Analysis)> = Vec::new();
    for (k, s) in seen.into_iter().enumerate() {
        let Some((o, a, pa)) = s? else { continue };
        let _ = k;
        ans.push((o.family.clone(), o.model.clone(), a));
        if let Some(pa) = pa {
            ans.push((o.family.clone(), o.model.clone(), pa));
        }
        obs.push(o);
    }
    // plain runs without a curator twin still tell the in-view rate
    for u in kept.iter().filter(|u| u.cur.is_none()) {
        let r = &rows[u.plain.unwrap()];
        if let Some(t) = &r.transcript {
            let s = load(&t.to_string_lossy())?;
            let family = family_of_task(&r.task).map_or_else(|| auto_family(&s.requests).to_string(), str::to_string);
            ans.push((family, r.model.clone(), analyse(&s.requests)));
        }
    }
    let chosen = |model: &str| cfg.model.as_deref().is_none_or(|m| m == model);
    let h_of = |f: Option<&str>, m: Option<&str>| {
        let sel: Vec<&Analysis> = ans
            .iter()
            .filter(|(fa, mo, _)| f.is_none_or(|x| x == fa) && m.map_or_else(|| chosen(mo), |x| x == mo))
            .map(|(_, _, a)| a)
            .collect();
        baseline(&sel)
    };
    let mut groups = BTreeMap::new();
    let keys: BTreeSet<(String, String)> = obs.iter().map(|o| (o.family.clone(), o.model.clone())).collect();
    for (f, m) in &keys {
        let sel: Vec<&Obs> = obs.iter().filter(|o| &o.family == f && &o.model == m).collect();
        groups.insert((f.clone(), m.clone()), fit_group(&sel, h_of(Some(f), Some(m)), cfg.seed));
    }
    let mut families = BTreeMap::new();
    let fams: BTreeSet<&String> = obs.iter().filter(|o| chosen(&o.model)).map(|o| &o.family).collect();
    for f in fams {
        let sel: Vec<&Obs> = obs.iter().filter(|o| &o.family == f && chosen(&o.model)).collect();
        families.insert(f.clone(), fit_group(&sel, h_of(Some(f), None), cfg.seed));
    }
    let all: Vec<&Obs> = obs.iter().filter(|o| chosen(&o.model)).collect();
    let global = fit_group(&all, h_of(None, None), cfg.seed);
    let mut sources: Vec<String> = rows.iter().map(|r| r.dir.to_string_lossy().to_string()).collect();
    sources.sort();
    sources.dedup();
    Ok(Fit { obs, groups, families, global, sources, config: cfg.clone(), held_out })
}

impl Fit {
    /// The `params.json` value: `{family: {r, beta, phi, pi, mu, n_runs, n_events, ci, sources, date}}` for
    /// the families with at least `min_events` events; a family with fewer takes the pooled fit and says so.
    pub fn params_json(&self, date: &str) -> Value {
        let entry = |g: &GroupFit, own: &GroupFit, fallback: bool| {
            json!({
                "r": round(g.r.value), "beta": round(g.beta.value), "phi": round(g.phi.value), "pi": round(g.pi.value),
                "mu": 2.0,
                "n_runs": own.runs, "n_events": own.events,
                "ci": { "r": g.r.json(), "beta": g.beta.json(), "phi": g.phi.json(), "pi": g.pi.json() },
                "validated": !fallback,
                "fallback": if fallback { json!("global") } else { Value::Null },
                "own": { "r": round(own.r.value), "beta": round(own.beta.value), "phi": round(own.phi.value),
                         "r_ci": own.r.json(), "beta_ci": own.beta.json(), "phi_ci": own.phi.json() },
                "sources": self.sources, "date": date,
            })
        };
        let mut o = serde_json::Map::new();
        let mut g = entry(&self.global, &self.global, false);
        g["r_den"] = json!(0.9);
        o.insert("global".to_string(), g);
        for (f, g) in &self.families {
            let fb = g.events < self.config.min_events;
            o.insert(f.clone(), if fb { entry(&self.global, g, true) } else { entry(g, g, false) });
        }
        let mut by_model = serde_json::Map::new();
        for ((f, m), g) in &self.groups {
            by_model.insert(format!("{f}@{m}"), entry(g, g, g.events < self.config.min_events));
        }
        o.insert("by_model".to_string(), Value::Object(by_model));
        o.insert("r_den".to_string(), json!(0.9));
        Value::Object(o)
    }

    /// The per-family table.
    pub fn table(&self) -> String {
        let mut s = String::new();
        let sel = self.config.model.clone().unwrap_or_else(|| "all models".to_string());
        s += &format!(
            "fit on {} curator runs ({} held out by rep parity: {:?}); parameters for {sel}; baseline = in-view repeat rate per call per request; * = rule of three\n",
            self.obs.len(),
            self.held_out,
            self.config.holdout
        );
        s += &format!(
            "{:<8} {:<16} {:>4} {:>5} {:>6} {:>5} {:>9}  {:<24} {:<24} {:<24} {:<22} {}\n",
            "family", "model", "runs", "pairs", "events", "fetch", "baseline", "r", "beta", "phi", "pi", "status"
        );
        let row = |s: &mut String, f: &str, m: &str, g: &GroupFit, status: &str| {
            *s += &format!(
                "{:<8} {:<16} {:>4} {:>5} {:>6} {:>5} {:>9.5}  {:<24} {:<24} {:<24} {:<22} {}\n",
                f,
                m,
                g.runs,
                g.pairs,
                g.events,
                g.fetched,
                g.baseline,
                g.r.show(),
                g.beta.show(),
                g.phi.show(),
                g.pi.show(),
                status
            );
        };
        let status = |g: &GroupFit, min: usize| {
            if g.events >= min {
                "validated".to_string()
            } else {
                format!("unvalidated (n events = {})", g.events)
            }
        };
        for ((f, m), g) in &self.groups {
            row(&mut s, f, m, g, &status(g, self.config.min_events));
        }
        s += "pooled over the selected model:\n";
        for (f, g) in &self.families {
            row(&mut s, f, "(pooled)", g, &status(g, self.config.min_events));
        }
        row(&mut s, "global", "(pooled)", &self.global, "fallback");
        s
    }
}

// ---- the price model ----
