use super::*;

/// One task's line of the check table.
#[derive(Debug, Clone)]
pub struct TaskCheck {
    pub task: String,
    pub pairs: usize,
    pub family: String,
    pub obs_ratio: f64,
    pub pred_ratio: f64,
    pub ratio_tol: f64,
    pub obs_dcost: f64,
    pub pred_dcost: f64,
    pub ratio_ok: bool,
    pub cost_ok: bool,
    pub events: usize,
}

/// The §5.2 table.
pub struct Check {
    pub tasks: Vec<TaskCheck>,
    pub pooled_obs_ratio: f64,
    pub pooled_pred_ratio: f64,
    pub pooled_obs_dcost: f64,
    pub pooled_pred_dcost: f64,
    pub pooled_ratio_ok: bool,
    pub pooled_cost_ok: bool,
    /// Median error of the reverse price check (curator arm priced as sent vs recorded).
    pub reverse_err: f64,
    pub reverse_ok: bool,
    /// Capped curator runs: (task, rep, P[fail] predicted from the plain twin).
    pub capped: Vec<(String, i64, f64)>,
    pub capped_ok: bool,
    pub pairs: usize,
    pub half: String,
    pub price_cost_err: f64,
    pub price_cache_err: f64,
}

/// The tolerance of the steps ratio: 1.64 standard errors of a ratio of per-run means, with the per-run SD of
/// the spec (17 %) as the floor when the sample is too small to tell.
fn ratio_se(pairs: &[(f64, f64)]) -> f64 {
    let n = pairs.len().max(1) as f64;
    let floor = 0.17 * (2.0 / n).sqrt();
    if pairs.len() < 4 {
        return floor;
    }
    let ratios: Vec<f64> = pairs.iter().map(|(c, p)| c / p.max(1.0)).collect();
    let m = ratios.iter().sum::<f64>() / n;
    let sd = (ratios.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0)).sqrt();
    floor.max(sd / n.sqrt())
}

/// Checks the simulator on the pairs of `results` whose rep parity is `holdout` (all pairs for `None`).
pub fn check(
    results: &str,
    spec: &str,
    set: &ParamSet,
    holdout: Holdout,
    life: f64,
    slots: usize,
) -> Result<Check, String> {
    let rows = load_rows(&[results.to_string()])?;
    let units = pair_up(&rows);
    let valid: Vec<&Unit> = units
        .iter()
        .filter(|u| {
            u.plain.zip(u.cur).is_some_and(|(p, c)| rows[p].clean() && rows[c].clean())
                && rows[u.plain.unwrap()].transcript.is_some()
                && (holdout == Holdout::None || holdout.holds(u.eff_rep))
        })
        .collect();
    let preds = par_map(valid.len(), 4, |i| {
        let u = valid[i];
        predict_row(&rows[u.plain.unwrap()], spec, set, None, life, slots)
    });
    let mut by_task: BTreeMap<String, Vec<(&Unit, Prediction)>> = BTreeMap::new();
    for (u, p) in valid.iter().zip(preds) {
        by_task.entry(rows[u.plain.unwrap()].task.clone()).or_default().push((u, p?));
    }
    let mut tasks = Vec::new();
    let (mut os_c, mut os_p, mut ps_c, mut oc_c, mut oc_p, mut pc_c, mut pc_p) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for (task, items) in &by_task {
        let pairs: Vec<(f64, f64)> = items
            .iter()
            .map(|(u, _)| (rows[u.cur.unwrap()].steps as f64, rows[u.plain.unwrap()].steps as f64))
            .collect();
        let (sc, sp): (f64, f64) = pairs.iter().fold((0.0, 0.0), |a, x| (a.0 + x.0, a.1 + x.1));
        let pred_c: f64 = items.iter().map(|(u, p)| p.steps_cur(rows[u.plain.unwrap()].max_steps)).sum();
        let (cc, cp): (f64, f64) = items
            .iter()
            .fold((0.0, 0.0), |a, (u, _)| (a.0 + rows[u.cur.unwrap()].cost, a.1 + rows[u.plain.unwrap()].cost));
        let (pcc, pcp): (f64, f64) = items.iter().fold((0.0, 0.0), |a, (_, p)| (a.0 + p.cost_cur, a.1 + p.cost_plain));
        let (obs_ratio, pred_ratio) = (sc / sp.max(1.0), pred_c / sp.max(1.0));
        let tol = 1.64 * ratio_se(&pairs);
        let (obs_d, pred_d) = (100.0 * (cc / cp.max(1e-12) - 1.0), 100.0 * (pcc / pcp.max(1e-12) - 1.0));
        tasks.push(TaskCheck {
            task: task.clone(),
            pairs: items.len(),
            family: items[0].1.family.clone(),
            obs_ratio,
            pred_ratio,
            ratio_tol: tol,
            obs_dcost: obs_d,
            pred_dcost: pred_d,
            ratio_ok: (pred_ratio - obs_ratio).abs() <= tol,
            cost_ok: (pred_d - obs_d).abs() <= 8.0,
            events: items.iter().map(|(_, p)| p.events).sum(),
        });
        os_c += sc;
        os_p += sp;
        ps_c += pred_c;
        oc_c += cc;
        oc_p += cp;
        pc_c += pcc;
        pc_p += pcp;
    }
    let (pooled_obs_ratio, pooled_pred_ratio) = (os_c / os_p.max(1.0), ps_c / os_p.max(1.0));
    let (pooled_obs_dcost, pooled_pred_dcost) =
        (100.0 * (oc_c / oc_p.max(1e-12) - 1.0), 100.0 * (pc_c / pc_p.max(1e-12) - 1.0));
    // the reverse check: the price model on the curator transcripts as sent
    let mut errs = Vec::new();
    for u in &valid {
        let (pr, r) = (&rows[u.plain.unwrap()], &rows[u.cur.unwrap()]);
        let Some(ct) = &r.transcript else { continue };
        let ps = load(&pr.transcript.as_ref().unwrap().to_string_lossy())?;
        let (pm, ph, _) = plain_tokens(&ps.requests, life, slots);
        let scale = token_scale(pr, pm, ph);
        let s = load(&ct.to_string_lossy())?;
        let (miss, hit, o) = plain_tokens(&s.requests, life, slots);
        let p = prices_for(r);
        let pred = p.dollars((miss as f64 * scale) as i64, (hit as f64 * scale) as i64) + p.output(o);
        errs.push((pred - r.cost).abs() / r.cost.max(1e-12));
    }
    let curs = errs.clone();
    let reverse_err = median(&mut errs);
    // the capped curator runs: P[fail] from the plain twin
    let mut capped = Vec::new();
    for u in units.iter().filter(|u| holdout == Holdout::None || holdout.holds(u.eff_rep)) {
        let (Some(p), Some(c)) = (u.plain, u.cur) else { continue };
        if rows[c].stop.as_deref() == Some("max_steps") && rows[p].transcript.is_some() {
            let pr = predict_row(&rows[p], spec, set, None, life, slots)?;
            capped.push((rows[c].task.clone(), u.rep, pr.p_fail));
        }
    }
    let need = |n: usize| (n * 5).div_ceil(6).max(1);
    let t = tasks.len();
    let pr = price_check(&rows, life, slots, None);
    let (price_cost_err, price_cache_err) = price_medians(&pr);
    let _ = need;
    Ok(Check {
        pooled_ratio_ok: (pooled_pred_ratio - pooled_obs_ratio).abs() <= 0.03,
        pooled_cost_ok: (pooled_pred_dcost - pooled_obs_dcost).abs() <= 3.0,
        reverse_ok: errs_ok(reverse_err, curs.len()),
        capped_ok: capped.iter().all(|c| c.2 >= 0.5),
        pooled_obs_ratio,
        pooled_pred_ratio,
        pooled_obs_dcost,
        pooled_pred_dcost,
        reverse_err,
        capped,
        pairs: valid.len(),
        half: match holdout {
            Holdout::None => "all valid pairs".to_string(),
            Holdout::Even => "even reps".to_string(),
            Holdout::Odd => "odd reps".to_string(),
        },
        price_cost_err,
        price_cache_err,
        tasks: {
            let _ = t;
            tasks
        },
    })
}

fn errs_ok(err: f64, n: usize) -> bool {
    n > 0 && err <= 0.05
}

impl Check {
    /// Tasks that pass the steps ratio and the cost rule, and how many are needed (5 of 6, scaled).
    pub fn passes(&self) -> (usize, usize, usize) {
        let t = self.tasks.len();
        (
            self.tasks.iter().filter(|x| x.ratio_ok).count(),
            self.tasks.iter().filter(|x| x.cost_ok).count(),
            (t * 5).div_ceil(6),
        )
    }

    pub fn overall(&self) -> bool {
        let (a, b, need) = self.passes();
        !self.tasks.is_empty()
            && a >= need
            && b >= need
            && self.pooled_ratio_ok
            && self.pooled_cost_ok
            && self.reverse_ok
            && self.capped_ok
    }

    pub fn table(&self) -> String {
        let mut s = String::new();
        s += &format!("check on {} valid pairs ({})\n", self.pairs, self.half);
        s += &format!(
            "{:<22} {:<7} {:>5} {:>7} | {:>9} {:>9} {:>8} {:>5} | {:>8} {:>8} {:>8} {:>5}\n",
            "task", "family", "pairs", "events", "obs ratio", "pred", "tol", "ok", "obs dC%", "pred dC%", "diff", "ok"
        );
        for t in &self.tasks {
            s += &format!(
                "{:<22} {:<7} {:>5} {:>7} | {:>9.3} {:>9.3} {:>8.3} {:>5} | {:>+8.1} {:>+8.1} {:>8.1} {:>5}\n",
                t.task,
                t.family,
                t.pairs,
                t.events,
                t.obs_ratio,
                t.pred_ratio,
                t.ratio_tol,
                if t.ratio_ok { "pass" } else { "FAIL" },
                t.obs_dcost,
                t.pred_dcost,
                t.pred_dcost - t.obs_dcost,
                if t.cost_ok { "pass" } else { "FAIL" }
            );
        }
        let (a, b, need) = self.passes();
        s += &format!(
            "{:<22} {:<7} {:>5} {:>7} | {:>9.3} {:>9.3} {:>8.3} {:>5} | {:>+8.1} {:>+8.1} {:>8.1} {:>5}\n",
            "pooled",
            "",
            self.pairs,
            "",
            self.pooled_obs_ratio,
            self.pooled_pred_ratio,
            0.03,
            if self.pooled_ratio_ok { "pass" } else { "FAIL" },
            self.pooled_obs_dcost,
            self.pooled_pred_dcost,
            self.pooled_pred_dcost - self.pooled_obs_dcost,
            if self.pooled_cost_ok { "pass" } else { "FAIL" }
        );
        s += &format!(
            "tasks passing: steps ratio {a}, cost {b}, of {} (need {need}); pooled ratio within 0.03, cost within 3 pts\n",
            self.tasks.len()
        );
        s += &format!(
            "reverse price check (curator arm priced as sent vs recorded): median error {:.1} % (limit 5 %): {}\n",
            100.0 * self.reverse_err,
            if self.reverse_ok { "pass" } else { "FAIL" }
        );
        if self.capped.is_empty() {
            s += "capped curator runs: none in this half (P[fail] rule not exercised)\n";
        } else {
            for (t, rep, p) in &self.capped {
                s += &format!(
                    "capped curator run {t} rep {rep}: P[fail] {p:.3} (need >= 0.5): {}\n",
                    if *p >= 0.5 { "pass" } else { "FAIL" }
                );
            }
        }
        s += &format!(
            "price model on the plain arms: median cost error {:.1} %, median cache_read error {:.1} %\n",
            100.0 * self.price_cost_err,
            100.0 * self.price_cache_err
        );
        s += &format!("overall: {}\n", if self.overall() { "PASS" } else { "FAIL" });
        s
    }

    /// The markdown report (`sim/reports/h1-check.md`).
    pub fn markdown(&self, results: &str, params: &str, spec: &str, date: &str) -> String {
        let mut s = format!("# Simulator check against held-out pairs\n\nWritten {date} by `sim check`.\n\n");
        s += &format!(
            "- results: `{results}`\n- params: `{params}`\n- spec: `{spec}`\n- half checked: {}, {} valid pairs\n\n",
            self.half, self.pairs
        );
        s += "| task | family | pairs | obs steps ratio | predicted | tol | steps | obs dCost % | predicted | cost |\n|---|---|---|---|---|---|---|---|---|---|\n";
        for t in &self.tasks {
            s += &format!(
                "| {} | {} | {} | {:.3} | {:.3} | {:.3} | {} | {:+.1} | {:+.1} | {} |\n",
                t.task,
                t.family,
                t.pairs,
                t.obs_ratio,
                t.pred_ratio,
                t.ratio_tol,
                if t.ratio_ok { "pass" } else { "fail" },
                t.obs_dcost,
                t.pred_dcost,
                if t.cost_ok { "pass" } else { "fail" }
            );
        }
        s += &format!(
            "| **pooled** | | {} | {:.3} | {:.3} | 0.030 | {} | {:+.1} | {:+.1} | {} |\n\n",
            self.pairs,
            self.pooled_obs_ratio,
            self.pooled_pred_ratio,
            if self.pooled_ratio_ok { "pass" } else { "fail" },
            self.pooled_obs_dcost,
            self.pooled_pred_dcost,
            if self.pooled_cost_ok { "pass" } else { "fail" }
        );
        let (a, b, need) = self.passes();
        s += &format!("Tasks passing: steps ratio {a}, cost {b}, of {} (need {need}).\n\n", self.tasks.len());
        s += &format!(
            "Reverse price check: median error {:.1} % (limit 5 %), {}.\n\n",
            100.0 * self.reverse_err,
            if self.reverse_ok { "pass" } else { "fail" }
        );
        for (t, rep, p) in &self.capped {
            s += &format!(
                "Capped curator run {t} rep {rep}: predicted P[fail] {p:.3}, {}.\n",
                if *p >= 0.5 { "pass" } else { "fail" }
            );
        }
        s += &format!(
            "\nPrice model on the plain arms: median cost error {:.1} %, median cache_read error {:.1} %.\n\n**Overall: {}**\n\nTo swap halves run `sim fit --holdout <parity>` and `sim check --holdout <parity>` with the other parity and compare.\n",
            100.0 * self.price_cost_err,
            100.0 * self.price_cache_err,
            if self.overall() { "PASS" } else { "FAIL" }
        );
        s
    }
}
