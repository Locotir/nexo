use super::*;

/// The prices that reproduce a recorded row best.
pub fn prices_for(r: &Row) -> Prices {
    let calc = |p: Prices| p.dollars(r.input as i64, r.cache_read as i64) + p.output(r.output as i64);
    let (a, b) = (calc(Prices::LIVE), calc(PRICES_OLD));
    if (a - r.cost).abs() <= (b - r.cost).abs() {
        Prices::LIVE
    } else {
        PRICES_OLD
    }
}

/// The plain arm priced by the prefix cache alone: input tokens as the engine estimates them, `life`
/// seconds and `slots` requests of cache. Returns (miss, hit, output tokens).
pub fn plain_tokens(requests: &[Request], life: f64, slots: usize) -> (i64, i64, i64) {
    let mut cache = Cache::new(life, slots);
    let (mut miss, mut hit, mut out) = (0, 0, 0);
    for (i, q) in requests.iter().enumerate() {
        let all = q.messages.iter().map(tokens_of).sum::<i64>() + q.system_tokens as i64;
        let h = cache.send(&q.messages, q.system_tokens as i64, q.now);
        // the provider caches what the model wrote with the prompt, so it is not billed as new input
        if let Some(said) = reply_to(requests, i) {
            cache.extend_last(said);
        }
        hit += h;
        miss += all - h;
        out += q.out_tokens.unwrap_or(0) as i64;
    }
    (miss, hit, out)
}

/// One plain run: predicted against recorded.
#[derive(Debug, Clone)]
pub struct PriceRow {
    pub path: String,
    pub recorded_cost: f64,
    pub predicted_cost: f64,
    pub recorded_cache: u64,
    pub predicted_cache: f64,
    pub scale: f64,
}

impl PriceRow {
    pub fn cost_err(&self) -> f64 {
        (self.predicted_cost - self.recorded_cost).abs() / self.recorded_cost.max(1e-12)
    }

    pub fn cache_err(&self) -> f64 {
        (self.predicted_cache - self.recorded_cache as f64).abs() / (self.recorded_cache as f64).max(1.0)
    }
}

pub(super) fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// The token scale of a plain run: recorded input tokens over the engine's estimate.
pub fn token_scale(r: &Row, miss: i64, hit: i64) -> f64 {
    let est = (miss + hit).max(1) as f64;
    let real = (r.input + r.cache_read) as f64;
    if real > 0.0 {
        real / est
    } else {
        1.0
    }
}

/// Checks the price model on the plain arms of the rows: the cost and the cache read it predicts against the
/// recorded ones. `scale` is the engine-estimate-to-provider-tokens factor (`None`: each run's own).
pub fn price_check(rows: &[Row], life: f64, slots: usize, scale: Option<f64>) -> Vec<PriceRow> {
    let plains: Vec<&Row> = rows.iter().filter(|r| r.kind == ArmKind::Plain && r.transcript.is_some()).collect();
    let mut out = Vec::new();
    for r in plains {
        let Ok(s) = load(&r.transcript.as_ref().unwrap().to_string_lossy()) else { continue };
        let (miss, hit, o) = plain_tokens(&s.requests, life, slots);
        let k = scale.unwrap_or_else(|| token_scale(r, miss, hit));
        let p = prices_for(r);
        let predicted = p.dollars((miss as f64 * k) as i64, (hit as f64 * k) as i64) + p.output(o);
        out.push(PriceRow {
            path: r.transcript.as_ref().unwrap().to_string_lossy().to_string(),
            recorded_cost: r.cost,
            predicted_cost: predicted,
            recorded_cache: r.cache_read,
            predicted_cache: hit as f64 * k,
            scale: k,
        });
    }
    out
}

/// Median cost error and median cache error of a price check.
pub fn price_medians(rows: &[PriceRow]) -> (f64, f64) {
    (
        median(&mut rows.iter().map(PriceRow::cost_err).collect::<Vec<_>>()),
        median(&mut rows.iter().map(PriceRow::cache_err).collect::<Vec<_>>()),
    )
}

// ---- predict and check ----
