use super::*;

/// One curator run seen against its plain twin.
#[derive(Debug, Clone)]
pub struct Obs {
    pub family: String,
    pub model: String,
    pub task: String,
    pub dir: String,
    pub rep: i64,
    pub eff_rep: i64,
    /// Events on the plain twin with this run's spec: `cited` + `denied`, and `blind` for the record.
    pub events: Option<usize>,
    pub blind: usize,
    pub fetched: usize,
    pub recall_calls: usize,
    pub expo_out: u64,
    pub has_recall: bool,
    pub steps_cur: usize,
    pub steps_plain: Option<usize>,
    pub score_cur: f64,
    pub score_plain: Option<f64>,
    /// Both arms ended on their own.
    pub clean_pair: bool,
    /// The curator run hit a step cap.
    pub capped: bool,
    pub rewrites: u64,
}

impl Obs {
    fn unfetched(&self) -> f64 {
        (self.events.unwrap_or(0) as f64 - self.fetched as f64).max(0.0)
    }
}

/// The in-view repeat rate of a set of analyses: repeats per prior call per request.
pub(super) fn baseline(an: &[&Analysis]) -> f64 {
    let (rep, expo): (u64, u64) = an.iter().fold((0, 0), |(r, e), a| (r + a.repeats_in, e + a.expo_in));
    if expo == 0 {
        0.0
    } else {
        rep as f64 / expo as f64
    }
}

/// The fitted behaviour of one group of runs.
#[derive(Debug, Clone)]
pub struct GroupFit {
    pub runs: usize,
    pub pairs: usize,
    pub events: usize,
    pub fetched: usize,
    pub unpaired: usize,
    pub baseline: f64,
    pub r: Est,
    pub beta: Est,
    pub phi: Est,
    pub pi: Est,
}

/// Fits one group: the curator runs `obs` (those with a twin carry events) and the baseline `h`.
pub fn fit_group(obs: &[&Obs], h: f64, seed: u64) -> GroupFit {
    let with_events: Vec<&Obs> = obs.iter().copied().filter(|o| o.events.is_some()).collect();
    let pairs: Vec<&Obs> = with_events.iter().copied().filter(|o| o.steps_plain.is_some()).collect();
    let events: usize = with_events.iter().map(|o| o.events.unwrap()).sum();
    let fetched: usize = with_events.iter().map(|o| o.fetched).sum();

    // r
    let r_stat = |u: &[&Obs]| -> Option<f64> {
        let e: f64 = u.iter().map(|o| o.events.unwrap() as f64).sum();
        let f: f64 = u.iter().map(|o| o.fetched as f64).sum();
        let x: f64 = u.iter().map(|o| o.expo_out as f64).sum();
        (e > 0.0).then(|| ((f - h * x) / e).clamp(0.0, 1.0))
    };
    let all: &[&Obs] = &with_events;
    let r = if events == 0 {
        Est::NONE
    } else {
        let value = r_stat(all).unwrap_or(0.0);
        if fetched == 0 {
            Est { value: 0.0, lo: 0.0, hi: (3.0 / events as f64).min(1.0), n: events as f64, rule3: true }
        } else {
            let (lo, hi) = percentile_ci(&with_events, seed, r_stat);
            Est { value, lo: lo.min(value), hi: hi.max(value), n: events as f64, rule3: false }
        }
    };

    // beta over the pairs that have an unfetched event
    let bp: Vec<&Obs> = pairs.iter().copied().filter(|o| o.unfetched() > 0.0).collect();
    let b_stat = |u: &[&Obs]| -> Option<f64> {
        let d: f64 = u.iter().map(|o| o.steps_cur as f64 - o.steps_plain.unwrap() as f64 - o.fetched as f64).sum();
        let n: f64 = u.iter().map(|o| o.unfetched()).sum();
        (n > 0.0).then(|| (d / n).max(0.0))
    };
    let beta = if bp.is_empty() {
        Est::NONE
    } else {
        let ball: &[&Obs] = &bp;
        let value = b_stat(ball).unwrap_or(0.0);
        let (lo, hi) = percentile_ci(&bp, seed ^ 0xB, b_stat);
        Est { value, lo: lo.min(value), hi: hi.max(value), n: bp.iter().map(|o| o.unfetched()).sum(), rule3: false }
    };

    // phi: score drops among clean pairs
    let cp: Vec<&Obs> = pairs.iter().copied().filter(|o| o.clean_pair).collect();
    let dropped = |o: &Obs| f64::from(o.score_cur + 1e-9 < o.score_plain.unwrap_or(0.0));
    let p_stat = |u: &[&Obs]| -> Option<f64> {
        let a: Vec<&&Obs> = u.iter().filter(|o| o.unfetched() > 0.0).collect();
        if a.is_empty() {
            return None;
        }
        let b: Vec<&&Obs> = u.iter().filter(|o| o.events == Some(0)).collect();
        let sa = a.iter().map(|o| dropped(o)).sum::<f64>() / a.len() as f64;
        let sb = if b.is_empty() { 0.0 } else { b.iter().map(|o| dropped(o)).sum::<f64>() / b.len() as f64 };
        let k = a.iter().map(|o| o.unfetched()).sum::<f64>() / a.len() as f64;
        let x = (sa - sb).clamp(0.0, 1.0);
        Some(1.0 - (1.0 - x).powf(1.0 / k.max(1.0)))
    };
    let at_risk: f64 = cp.iter().map(|o| o.unfetched()).sum();
    let phi = if at_risk == 0.0 {
        Est::NONE
    } else {
        let call: &[&Obs] = &cp;
        let value = p_stat(call).unwrap_or(0.0);
        let drops: f64 = cp.iter().filter(|o| o.unfetched() > 0.0).map(|o| dropped(o)).sum();
        if drops == 0.0 {
            Est { value: 0.0, lo: 0.0, hi: (3.0 / at_risk).min(1.0), n: at_risk, rule3: true }
        } else {
            let (lo, hi) = percentile_ci(&cp, seed ^ 0xF, p_stat);
            Est { value, lo: lo.min(value), hi: hi.max(value), n: at_risk, rule3: false }
        }
    };

    // pi: recall calls over fetches, in runs that had the tool
    let rr: Vec<&Obs> = with_events.iter().copied().filter(|o| o.has_recall).collect();
    let rf: f64 = rr.iter().map(|o| o.fetched as f64).sum();
    let pi = if rf == 0.0 {
        Est::NONE
    } else {
        let s = |u: &[&Obs]| -> Option<f64> {
            let f: f64 = u.iter().map(|o| o.fetched as f64).sum();
            let c: f64 = u.iter().map(|o| o.recall_calls as f64).sum();
            (f > 0.0).then(|| (c / f).clamp(0.0, 1.0))
        };
        let value = s(&rr).unwrap_or(0.0);
        let (lo, hi) = percentile_ci(&rr, seed ^ 0xA, s);
        Est { value, lo: lo.min(value), hi: hi.max(value), n: rf, rule3: false }
    };

    // fewer than 10 events say nothing about the cost of a miss or the chance of failure
    let (beta, phi) = if events < 10 { (Est::NONE, Est::NONE) } else { (beta, phi) };
    GroupFit {
        runs: obs.len(),
        pairs: pairs.len(),
        events,
        fetched,
        unpaired: obs.len() - with_events.len(),
        baseline: h,
        r,
        beta,
        phi,
        pi,
    }
}

// ---- observing a unit ----

/// Everything the fit needs from one unit: loads its transcripts, runs the curator over the plain twin.
pub(super) fn observe(rows: &[Row], u: &Unit) -> Result<Option<(Obs, Analysis, Option<Analysis>)>, String> {
    let Some(ci) = u.cur else { return Ok(None) };
    let (c, p) = (&rows[ci], u.plain.map(|k| &rows[k]));
    let Some(ct) = &c.transcript else { return Ok(None) };
    let cs = load(&ct.to_string_lossy())?;
    let an = analyse(&cs.requests);
    let family = family_of_task(&c.task).map_or_else(|| auto_family(&cs.requests).to_string(), str::to_string);
    let mut obs = Obs {
        family,
        model: c.model.clone(),
        task: c.task.clone(),
        dir: c.dir.to_string_lossy().to_string(),
        rep: u.rep,
        eff_rep: u.eff_rep,
        events: None,
        blind: 0,
        fetched: an.fetched.len(),
        recall_calls: an.recall_calls,
        expo_out: an.expo_out,
        has_recall: an.has_recall,
        steps_cur: c.steps,
        steps_plain: p.map(|p| p.steps),
        score_cur: c.score,
        score_plain: p.map(|p| p.score),
        clean_pair: p.is_some_and(|p| p.clean()) && c.clean(),
        capped: c.stop.as_deref() == Some("max_steps"),
        rewrites: c.rewrites,
    };
    let mut plain_an = None;
    if let Some(p) = p {
        if let Some(pt) = &p.transcript {
            let ps = load(&pt.to_string_lossy())?;
            let spec = if c.spec.is_empty() { DEFAULT_SPEC } else { &c.spec };
            let r = run(&ps.requests, spec, Prices::LIVE, 0.0, 0)?;
            obs.events = Some(r.events.iter().filter(|e| matches!(e.kind, Kind::Cited | Kind::Denied)).count());
            obs.blind = r.events.iter().filter(|e| e.kind == Kind::Blind).count();
            plain_an = Some(analyse(&ps.requests));
        }
    }
    Ok(Some((obs, an, plain_an)))
}
