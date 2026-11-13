use super::*;

/// The simulator's prediction for one plain run.
#[derive(Debug, Clone)]
pub struct Prediction {
    pub task: String,
    pub rep: i64,
    pub eff_rep: i64,
    pub family: String,
    pub steps_plain: usize,
    pub extra_steps: f64,
    pub cost_plain: f64,
    pub cost_cur: f64,
    pub p_fail: f64,
    pub events: usize,
}

impl Prediction {
    pub fn steps_cur(&self, max_steps: usize) -> f64 {
        (self.steps_plain as f64 + self.extra_steps).min(max_steps as f64)
    }
}

/// Predicts the curator arm of one plain run: its transcript through the curator with `spec`, the behaviour
/// layer on top. Costs are the simulator's own on both sides (priced at the run's recorded prices, tokens
/// scaled by `scale`, or by the run's own recorded usage when `None`).
pub fn predict_row(
    r: &Row,
    spec: &str,
    set: &ParamSet,
    scale: Option<f64>,
    life: f64,
    slots: usize,
) -> Result<Prediction, String> {
    let t = r.transcript.as_ref().ok_or("no transcript")?;
    let s = load(&t.to_string_lossy())?;
    let family = family_of_task(&r.task).map_or_else(|| auto_family(&s.requests).to_string(), str::to_string);
    let (miss, hit, o) = plain_tokens(&s.requests, life, slots);
    let k = scale.unwrap_or_else(|| token_scale(r, miss, hit));
    let base = prices_for(r);
    let prices = Prices { miss: base.miss * k, hit: base.hit * k, out: base.out };
    let cost_plain = prices.dollars(miss, hit) + prices.output(o);
    let mut cur = run(&s.requests, spec, prices, life, slots)?;
    probe(&mut cur, &s.requests, spec, life, slots)?;
    let cx = Context::measure(&s.requests, spec, prices, r.max_steps);
    let a = analytic(&cur, set.get(&family), set.r_den, &cx);
    Ok(Prediction {
        task: r.task.clone(),
        rep: r.rep,
        eff_rep: r.rep,
        family,
        steps_plain: r.steps,
        extra_steps: a.extra_steps,
        cost_plain,
        cost_cur: cur.cost + a.extra_cost,
        p_fail: a.p_fail,
        events: a.events,
    })
}

/// `sim predict`: the plain rows of `results`, each fed through the simulator.
pub fn predict(
    results: &str,
    arm: &str,
    spec: &str,
    set: &ParamSet,
    life: f64,
    slots: usize,
) -> Result<Vec<Prediction>, String> {
    let rows = load_rows(&[results.to_string()])?;
    let sel: Vec<(usize, &Row)> =
        rows.iter().enumerate().filter(|(_, r)| r.arm == arm && r.transcript.is_some()).collect();
    let all = pair_up(&rows);
    let scale: Option<f64> = None;
    par_map(sel.len(), 4, |i| {
        let mut p = predict_row(sel[i].1, spec, set, scale, life, slots)?;
        if let Some(u) = all.iter().find(|u| u.plain == Some(sel[i].0)) {
            p.eff_rep = u.eff_rep;
        }
        Ok(p)
    })
    .into_iter()
    .collect()
}
