//! The behaviour layer: what the agent does after the view lacked something, as an analytic expectation
//! and as a Monte Carlo that inserts the real calls into the history and runs the curator again.
//!
//! After a `cited` or `denied` event the agent fetches the content back (probability `r`, one more step),
//! or goes on and wanders (`(1 - r) * delta` with `delta = beta / mu`, a geometric number of detour steps of
//! mean `mu`), or goes on blind and fails (`(1 - r) * phi`), or nothing happens. A run that takes more steps
//! than `max_steps` fails. `blind` events carry no branch.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::events::{Event, Foot, Kind, Run, Stepper};
use super::input::{reply_to, text_of, Request};
use super::price::{tokens_of, Prices};

/// The fitted behaviour of one family of sessions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    /// Chance the agent fetches back what a `cited` event cut.
    pub r: f64,
    /// Expected detour steps per unfetched event (`delta * mu`).
    pub beta: f64,
    /// Chance it fails when it goes on without the content, per unfetched event.
    pub phi: f64,
    /// Share of fetches that use `recall` (kept for the fit; it does not change the price).
    pub pi: f64,
    /// Mean length of a detour, in steps (at least 1).
    pub mu: f64,
}

impl Params {
    /// Priors for a family nothing was fitted for.
    pub const PRIOR: Params = Params { r: 0.1, beta: 0.1, phi: 0.02, pi: 0.0, mu: 2.0 };

    /// The chance of a detour, kept so that `delta + phi <= 1`.
    pub fn delta(&self) -> f64 {
        let mu = self.mu.max(1.0);
        (self.beta.max(0.0) / mu).min((1.0 - self.phi).max(0.0))
    }

    /// Detour steps per unfetched event after that clamp.
    pub fn beta_eff(&self) -> f64 {
        self.delta() * self.mu.max(1.0)
    }
}

/// The parameters of every family, a global fallback and the fetch chance after a `denied` event.
#[derive(Debug, Clone)]
pub struct ParamSet {
    pub families: BTreeMap<String, Params>,
    pub global: Params,
    pub r_den: f64,
}

impl Default for ParamSet {
    fn default() -> ParamSet {
        ParamSet { families: BTreeMap::new(), global: Params::PRIOR, r_den: 0.9 }
    }
}

fn params_from(v: &Value, fallback: Params) -> Option<Params> {
    let o = v.as_object()?;
    let f = |k: &str, d: f64| o.get(k).and_then(Value::as_f64).unwrap_or(d);
    o.get("r").and_then(Value::as_f64)?;
    Some(Params {
        r: f("r", fallback.r),
        beta: f("beta", fallback.beta),
        phi: f("phi", fallback.phi),
        pi: f("pi", fallback.pi),
        mu: f("mu", fallback.mu),
    })
}

impl ParamSet {
    /// One set of parameters for every family.
    pub fn uniform(p: Params, r_den: f64) -> ParamSet {
        ParamSet { families: BTreeMap::new(), global: p, r_den }
    }

    /// Reads `params.json`: `{family: {r, beta, phi, pi, mu?, ...}}` with an optional `global` entry (and
    /// `r_den` there or at the top). A family without `r` is skipped; a missing key takes the global value.
    pub fn load(path: &str) -> Result<ParamSet, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
        let o = v.as_object().ok_or_else(|| format!("{path}: expected an object"))?;
        let mut set = ParamSet::default();
        if let Some(g) = o.get("global") {
            set.global = params_from(g, Params::PRIOR).unwrap_or(Params::PRIOR);
            if let Some(x) = g.get("r_den").and_then(Value::as_f64) {
                set.r_den = x;
            }
        }
        if let Some(x) = o.get("r_den").and_then(Value::as_f64) {
            set.r_den = x;
        }
        for (k, val) in o.iter().filter(|(k, _)| k.as_str() != "global") {
            if let Some(p) = params_from(val, set.global) {
                set.families.insert(k.clone(), p);
            }
        }
        Ok(set)
    }

    /// The parameters of `family`, the global ones when it has none.
    pub fn get(&self, family: &str) -> Params {
        self.families.get(family).copied().unwrap_or(self.global)
    }
}

/// What the price of a branch depends on besides the events.
#[derive(Debug, Clone, Copy)]
pub struct Context {
    pub prices: Prices,
    /// Requests a fetched result stays in the view (the spec's `protect`).
    pub protect: usize,
    /// Output tokens of a tool-call reply.
    pub c_out: f64,
    pub max_steps: usize,
}

/// Output tokens of a tool call when the session records none (flash: 60 to 120).
pub const DEFAULT_C_OUT: f64 = 90.0;

/// The value of `key` in `spec`, else in the defaults.
pub fn spec_int(spec: &str, key: &str) -> Option<i64> {
    let find = |s: &str| {
        s.split(';')
            .filter_map(|kv| kv.split_once('='))
            .rev()
            .find(|(k, _)| k.trim() == key)
            .map(|(_, v)| v.trim().to_string())
    };
    find(spec).or_else(|| find(crate::D_SPEC)).and_then(|v| v.parse().ok())
}

impl Context {
    /// Measures `c_out` from the transcript (the mean output of the requests whose reply calls a tool) and
    /// reads `protect` from the spec.
    pub fn measure(requests: &[Request], spec: &str, prices: Prices, max_steps: usize) -> Context {
        let (mut sum, mut n) = (0u64, 0u64);
        let (mut all, mut m) = (0u64, 0u64);
        for (i, q) in requests.iter().enumerate() {
            let Some(o) = q.out_tokens.filter(|o| *o > 0) else { continue };
            all += o;
            m += 1;
            let calls = reply_to(requests, i)
                .and_then(|r| r["content"].as_array())
                .is_some_and(|bs| bs.iter().any(|b| b["type"] == "tool_use"));
            if calls {
                sum += o;
                n += 1;
            }
        }
        let c_out = match (n, m) {
            (0, 0) => DEFAULT_C_OUT,
            (0, _) => all as f64 / m as f64,
            _ => sum as f64 / n as f64,
        };
        let protect = spec_int(spec, "protect").unwrap_or(12).max(0) as usize;
        Context { prices, protect, c_out, max_steps }
    }
}

/// The analytic expectation for one session.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Expect {
    pub extra_steps: f64,
    pub extra_cost: f64,
    /// The part of `extra_cost` that the fetches cost (the rest is the wandering steps).
    pub fetch_cost: f64,
    pub p_fail: f64,
    /// The part of `p_fail` that comes from passing `max_steps`.
    pub p_cap: f64,
    /// The `cited` and `denied` events the branches apply to.
    pub events: usize,
    /// Variance of the extra steps.
    pub var_steps: f64,
}

/// The standard normal upper tail beyond `x` (Abramowitz and Stegun 7.1.26).
fn upper_tail(x: f64) -> f64 {
    let z = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly =
        t * (0.254_829_592 + t * (-0.284_496_736 + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let half = 0.5 * poly * (-z * z).exp();
    if x >= 0.0 {
        half
    } else {
        1.0 - half
    }
}

/// Whether an event gets a branch.
fn branches(e: &Event) -> bool {
    matches!(e.kind, Kind::Cited | Kind::Denied)
}

/// Whether a session of `s` recorded steps plus `extra` passes the limit. A session recorded with more than
/// `max_steps` steps ran under another limit, so none applies to it.
fn over(s: usize, extra: usize, max_steps: usize) -> bool {
    s <= max_steps && s + extra > max_steps
}

/// One replay of the probe: its task number, the extra (miss, hit) input tokens it measured and how many
/// branching events it met.
type Tally = (usize, (i64, i64), usize);

/// Fills `run.feet`: for each `cited` or `denied` event the session is replayed with the call and result that
/// fetch it, first that one only (`alone`), then it and every event before it (`joint` is the difference to
/// the replay without it), and the extra input tokens over the open loop are kept. The curator reacts to what
/// was fetched (it cuts the new result, ages it, and may drop the copy the history holds), so what a fetch
/// costs is what the replay shows, and fetches that share content cost less together. `analytic` weighs the
/// two by how many of the other events are fetched. The same reaction can make later events disappear (the
/// content is back in the view, so nothing is cut again): an event the replay of every earlier fetch no longer
/// meets is marked `gone`, and `analytic` leaves its step out when those fetches are made. Run it once per
/// session and spec: the footprints do not depend on the parameters or the prices.
pub fn probe(run: &mut Run, requests: &[Request], spec: &str, life: f64, slots: usize) -> Result<(), String> {
    let sites = run.events.iter().filter(|e| branches(e)).count();
    let tasks = 2 * sites;
    let threads = std::thread::available_parallelism().map_or(1, |t| t.get()).min(tasks).max(1);
    let base = &*run;
    let found: Vec<Result<Vec<Tally>, String>> = std::thread::scope(|sc| {
        let handles: Vec<_> = (0..threads)
            .map(|w| {
                sc.spawn(move || {
                    (w..tasks)
                        .step_by(threads)
                        .map(|t| {
                            let k = t / 2;
                            let pick = move |j: usize| if t % 2 == 0 { j == k } else { j <= k };
                            replay(requests, spec, life, slots, pick)
                                .map(|(m, h, met)| (t, (m - base.miss, h - base.hit), met))
                        })
                        .collect::<Result<Vec<_>, String>>()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("a probe thread")).collect()
    });
    let mut total = vec![((0, 0), 0); tasks];
    for part in found {
        for (t, f, met) in part? {
            total[t] = (f, met);
        }
    }
    run.feet = (0..sites)
        .map(|k| {
            let before = if k == 0 { ((0, 0), sites) } else { total[2 * k - 1] };
            let after = total[2 * k + 1].0;
            Foot {
                alone: total[2 * k].0,
                joint: (after.0 - before.0 .0, after.1 - before.0 .1),
                // with every earlier event fetched, the replay no longer meets as many as the open loop
                gone: before.1 <= k,
            }
        })
        .collect();
    Ok(())
}

/// The session with the fetch of every branching event `j` (counted as the replay meets them) for which
/// `pick(j)` holds, and nothing else: the (miss, hit) input tokens it sends and how many branching events it met.
fn replay(
    requests: &[Request],
    spec: &str,
    life: f64,
    slots: usize,
    pick: impl Fn(usize) -> bool,
) -> Result<(i64, i64, usize), String> {
    let s = requests.len();
    let mut st = Stepper::new(spec, life, slots)?;
    let (mut k, mut inserts) = (0usize, Vec::<(usize, Vec<Value>)>::new());
    for (i, q) in requests.iter().enumerate() {
        let mut msgs = splice(&q.messages, &inserts);
        let events = st.send(q, &msgs, reply_to(requests, i), i + 1, s, q.out_tokens.unwrap_or(0))?;
        for e in events.iter().filter(|e| branches(e)) {
            if pick(k) {
                inserts.push((q.messages.len(), source_pair(&msgs, e.mi, &format!("simx{k}"))));
                msgs = splice(&q.messages, &inserts);
                st.send(q, &msgs, None, i + 1, s, 0)?;
            }
            k += 1;
        }
    }
    Ok((st.run.miss, st.run.hit, k))
}

/// The expectation of the extra steps, the extra cost and the chance of failing, from the events of an
/// open-loop pass (`SIMULATOR-SPEC.md` 2.4). The cap uses a normal approximation of the extra steps.
/// A fetch costs its measured footprint (`probe`) when `run.feet` holds one per event, else an estimate from
/// the event alone (the missing tokens once as a miss, the protected requests as hits), which is rough:
/// the curator cuts what is fetched and drops copies the history holds, so the estimate misses by tens of percent.
pub fn analytic(run: &Run, p: Params, r_den: f64, cx: &Context) -> Expect {
    let n = run.requests.max(1) as f64;
    let (v_bar, m_bar) = (run.hit as f64 / n, run.miss as f64 / n);
    let o_bar = if run.out > 0 { run.out as f64 / n } else { cx.c_out };
    let step = cx.prices.hit * v_bar + cx.prices.miss * m_bar + cx.prices.out * o_bar;
    let (beta, mu) = (p.beta_eff(), p.mu.max(1.0));
    let (mut steps, mut cost, mut var, mut ok, mut count) = (0.0, 0.0, 0.0, 1.0, 0);
    let mut fetch_cost = 0.0;
    // the chance that every event so far was either not met or fetched, which makes the later `gone` ones disappear
    let mut all_fetched = 1.0;
    // how likely each other event is fetched
    let sites = run.events.iter().filter(|e| branches(e)).count().max(1) as f64;
    let w = run
        .events
        .iter()
        .filter(|e| branches(e))
        .map(|e| if e.kind == Kind::Denied { r_den } else { p.r }.clamp(0.0, 1.0))
        .sum::<f64>()
        / sites;
    for e in run.events.iter().filter(|e| branches(e)) {
        let r = if e.kind == Kind::Denied { r_den } else { p.r }.clamp(0.0, 1.0);
        let v_t = run.views.get(e.t.wrapping_sub(1)).copied().unwrap_or(0) as f64;
        let input = match run.feet.get(count) {
            // the replay: the tokens the fetch request and the requests after it sent more
            Some(f) => {
                let tokens = |a: (i64, i64)| cx.prices.miss * a.0 as f64 + cx.prices.hit * a.1 as f64;
                (1.0 - w) * tokens(f.alone) + w * tokens(f.joint)
            }
            None => {
                let s_e = e.s_e as f64;
                cx.prices.hit * (v_t + s_e * cx.protect.min(e.s_rest) as f64) + cx.prices.miss * (cx.c_out + s_e)
            }
        };
        let fetch = input + cx.prices.out * cx.c_out;
        // a `gone` event happens unless every event before it was fetched (all of them is the closed loop of
        // `r = 1`; none, the open loop)
        let here = if run.feet.get(count).is_some_and(|f| f.gone) { 1.0 - all_fetched } else { 1.0 };
        all_fetched *= 1.0 - here * (1.0 - r);
        steps += here * (r + (1.0 - r) * beta);
        cost += here * (r * fetch + (1.0 - r) * beta * step);
        fetch_cost += here * r * fetch;
        var += here * (r * (1.0 - r) + (1.0 - r) * beta * (1.0 + mu));
        ok *= 1.0 - here * (1.0 - r) * p.phi;
        count += 1;
    }
    let room = cx.max_steps as f64 - run.requests as f64;
    let p_cap = if run.requests > cx.max_steps {
        0.0
    } else if var <= 1e-12 {
        f64::from(steps > room + 0.5)
    } else {
        upper_tail((room + 0.5 - steps) / var.sqrt())
    };
    Expect {
        extra_steps: steps,
        extra_cost: cost / 1e6,
        fetch_cost: fetch_cost / 1e6,
        p_fail: 1.0 - ok * (1.0 - p_cap),
        p_cap,
        events: count,
        var_steps: var,
    }
}

/// The family of a session for `--family auto` (`family::tag`, `SIMULATOR-SPEC.md` 7.2).
pub fn auto_family(requests: &[Request]) -> &'static str {
    super::family::tag(requests)
}

// ---- Monte Carlo ----

/// A xorshift64* generator, so a seed gives the same samples everywhere.
#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64, stream: u64) -> Rng {
        let mut r = Rng(seed ^ stream.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x2545_F491_4F6C_DD1D);
        for _ in 0..4 {
            r.next_u64();
        }
        r
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// The result of `n` sampled sessions.
#[derive(Debug, Clone, PartialEq)]
pub struct McResult {
    pub samples: usize,
    pub extra_steps: f64,
    pub extra_cost: f64,
    pub p_fail: f64,
    pub p_cap: f64,
    /// Standard deviation of the extra steps over the samples.
    pub steps_sd: f64,
    /// Events per sample the closed loop met (it can differ from the open loop's).
    pub events: f64,
    pub wall_secs: f64,
}

/// `messages` with the inserted pairs: each `(at, pair)` goes before the recorded message `at`, at the end
/// when `at` is the length. Pairs beyond the end are left out.
fn splice(messages: &[Value], inserts: &[(usize, Vec<Value>)]) -> Vec<Value> {
    if inserts.is_empty() {
        return messages.to_vec();
    }
    let mut out = Vec::with_capacity(messages.len() + 2 * inserts.len());
    for at in 0..=messages.len() {
        for (_, pair) in inserts.iter().filter(|(a, _)| *a == at) {
            out.extend(pair.iter().cloned());
        }
        if let Some(m) = messages.get(at) {
            out.push(m.clone());
        }
    }
    out
}

/// A call and its result, as two messages, with a new id.
fn pair(id: &str, name: &str, input: &Value, result: &Value) -> Vec<Value> {
    vec![
        json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": result}]}),
    ]
}

/// The call that made the largest result in message `mi`, with its result, found among the messages before:
/// the same tool, the same arguments, the same result. A message without a result is fetched with `recall`.
fn source_pair(history: &[Value], mi: usize, id: &str) -> Vec<Value> {
    let Some(m) = history.get(mi) else { return pair(id, "recall", &json!({}), &json!("")) };
    let best = m["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "tool_result")
        .max_by_key(|b| tokens_of(&json!({"content": [b]})));
    if let Some(b) = best {
        let uid = b["tool_use_id"].as_str().unwrap_or("");
        for h in history[..mi].iter().rev() {
            for u in h["content"].as_array().into_iter().flatten() {
                if u["type"] == "tool_use" && u["id"].as_str() == Some(uid) {
                    return pair(id, u["name"].as_str().unwrap_or("bash"), &u["input"], &b["content"]);
                }
            }
        }
        return pair(id, "recall", &json!({"id": mi}), &b["content"]);
    }
    pair(id, "recall", &json!({"id": mi}), &json!(text_of(m)))
}

/// The session's median step: the call and result of the median-sized result in the longest request.
fn median_step(requests: &[Request]) -> (String, Value, Value) {
    let fallback = ("bash".to_string(), json!({"command": "true"}), json!("ok"));
    let Some(q) = requests.iter().max_by_key(|q| q.messages.len()) else { return fallback };
    let mut steps: Vec<(i64, String, Value, Value)> = Vec::new();
    for (k, m) in q.messages.iter().enumerate() {
        for b in m["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "tool_result") {
            let uid = b["tool_use_id"].as_str().unwrap_or("");
            let used = q.messages[..k]
                .iter()
                .rev()
                .flat_map(|h| h["content"].as_array().into_iter().flatten())
                .find(|u| u["type"] == "tool_use" && u["id"].as_str() == Some(uid));
            if let Some(u) = used {
                steps.push((
                    tokens_of(&json!({"content": [b]})),
                    u["name"].as_str().unwrap_or("bash").to_string(),
                    u["input"].clone(),
                    b["content"].clone(),
                ));
            }
        }
    }
    if steps.is_empty() {
        return fallback;
    }
    steps.sort_by_key(|s| s.0);
    let (_, n, i, c) = steps.swap_remove(steps.len() / 2);
    (n, i, c)
}

struct Sample {
    extra_steps: usize,
    extra_cost: f64,
    failed: bool,
    capped: bool,
    events: usize,
}

struct Plan<'a> {
    requests: &'a [Request],
    spec: &'a str,
    cx: &'a Context,
    life: f64,
    slots: usize,
    p: Params,
    r_den: f64,
    base_input: f64,
    o_bar: f64,
    median: (String, Value, Value),
    /// For each event slot, a permutation of the samples: event `k` of sample `j` draws from stratum `perm[k][j]`.
    perm: Vec<Vec<u32>>,
    n: usize,
    seed: u64,
}

fn sample(plan: &Plan, j: usize) -> Result<Sample, String> {
    let (reqs, cx, p) = (plan.requests, plan.cx, plan.p);
    let s = reqs.len();
    let mut rng = Rng::new(plan.seed, j as u64 + 1);
    let mut st = Stepper::new(plan.spec, plan.life, plan.slots)?;
    let (mut inserts, mut extra, mut extra_out, mut failed, mut k, mut next_id) =
        (Vec::<(usize, Vec<Value>)>::new(), 0usize, 0.0f64, false, 0usize, 0usize);
    let (delta, beta_mu) = (p.delta(), p.mu.max(1.0));
    for (i, q) in reqs.iter().enumerate() {
        let mut msgs = splice(&q.messages, &inserts);
        let events = st.send(q, &msgs, reply_to(reqs, i), i + 1, s, q.out_tokens.unwrap_or(0))?;
        for e in events.iter().filter(|e| branches(e)) {
            let u = match plan.perm.get(k) {
                Some(pm) => (f64::from(pm[j]) + rng.unit()) / plan.n as f64,
                None => rng.unit(),
            };
            k += 1;
            let r = if e.kind == Kind::Denied { plan.r_den } else { p.r }.clamp(0.0, 1.0);
            let wander = (1.0 - r) * delta;
            let mut add = Vec::new();
            if u < r {
                next_id += 1;
                add.push((source_pair(&msgs, e.mi, &format!("simx{next_id}")), cx.c_out));
            } else if u < r + wander {
                let w = ((u - r) / wander.max(1e-12)).clamp(0.0, 1.0 - 1e-12);
                let steps =
                    if beta_mu <= 1.0 { 1 } else { 1 + ((1.0 - w).ln() / (1.0 - 1.0 / beta_mu).ln()).floor() as usize };
                for _ in 0..steps.min(200) {
                    next_id += 1;
                    let (name, input, result) = &plan.median;
                    add.push((pair(&format!("simx{next_id}"), name, input, result), plan.o_bar));
                }
            } else if u < r + (1.0 - r) * (delta + p.phi) {
                failed = true;
            }
            for (pr, out) in add {
                inserts.push((q.messages.len(), pr));
                msgs = splice(&q.messages, &inserts);
                st.send(q, &msgs, None, i + 1, s, 0)?;
                extra += 1;
                extra_out += out;
            }
        }
    }
    let input = cx.prices.dollars(st.run.miss, st.run.hit);
    Ok(Sample {
        extra_steps: extra,
        extra_cost: input - plan.base_input + cx.prices.output(extra_out.round() as i64),
        failed,
        capped: over(s, extra, cx.max_steps),
        events: k,
    })
}

/// Samples `n` sessions: the branches of each event are drawn (stratified over the samples, so a few
/// samples already match the probabilities), the real calls are inserted into the history, and the
/// curator is run again on the result, so that later cuts react to what was fetched. Samples run on
/// threads. `blind` events are not branched.
#[allow(clippy::too_many_arguments)]
pub fn monte_carlo(
    requests: &[Request],
    spec: &str,
    cx: &Context,
    life: f64,
    slots: usize,
    p: Params,
    r_den: f64,
    n: usize,
    seed: u64,
) -> Result<McResult, String> {
    let t0 = std::time::Instant::now();
    let n = n.max(1);
    let base = super::events::run(requests, spec, cx.prices, life, slots)?;
    let sites = base.events.iter().filter(|e| branches(e)).count();
    let perm = (0..sites + 8)
        .map(|k| {
            let mut v: Vec<u32> = (0..n as u32).collect();
            let mut g = Rng::new(seed, 1_000_003 + k as u64);
            for i in (1..v.len()).rev() {
                v.swap(i, (g.next_u64() % (i as u64 + 1)) as usize);
            }
            v
        })
        .collect();
    let o_bar = if base.out > 0 { base.out as f64 / base.requests.max(1) as f64 } else { cx.c_out };
    let plan = Plan {
        requests,
        spec,
        cx,
        life,
        slots,
        p,
        r_den,
        base_input: base.input_cost,
        o_bar,
        median: median_step(requests),
        perm,
        n,
        seed,
    };
    let threads = std::thread::available_parallelism().map_or(1, |t| t.get()).min(n);
    let results: Vec<Result<Sample, String>> = std::thread::scope(|sc| {
        let plan = &plan;
        let handles: Vec<_> = (0..threads)
            .map(|w| sc.spawn(move || (w..n).step_by(threads).map(|j| sample(plan, j)).collect::<Vec<_>>()))
            .collect();
        handles.into_iter().flat_map(|h| h.join().expect("a sample thread")).collect()
    });
    let mut all = Vec::with_capacity(n);
    for r in results {
        all.push(r?);
    }
    let nf = all.len() as f64;
    let mean = |f: &dyn Fn(&Sample) -> f64| all.iter().map(f).sum::<f64>() / nf;
    let steps = mean(&|s| s.extra_steps as f64);
    let sd = (mean(&|s| (s.extra_steps as f64 - steps).powi(2))).sqrt();
    Ok(McResult {
        samples: all.len(),
        extra_steps: steps,
        extra_cost: mean(&|s| s.extra_cost),
        p_fail: mean(&|s| f64::from(s.failed || s.capped)),
        p_cap: mean(&|s| f64::from(s.capped)),
        steps_sd: sd,
        events: mean(&|s| s.events as f64),
        wall_secs: t0.elapsed().as_secs_f64(),
    })
}
