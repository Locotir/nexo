use super::*;

/// The result of one cold session (N, hops).
#[derive(Debug, Clone)]
pub struct ColdRow {
    pub topics: usize,
    pub hops: usize,
    pub questions: usize,
    pub answered: usize,
    /// Questions of one fact and answered ones, and of two or three facts and answered ones.
    pub by_hops: [usize; 4],
    /// Standing facts planted (0: the session has none), tasks that suppose their defaults, and tasks whose view held the
    /// source line of the fact.
    pub standing: usize,
    pub stand: [usize; 2],
    pub calls: usize,
    pub view_max: i64,
    /// Input tokens sent (missed plus cached), with the system prompt.
    pub sent: i64,
    pub usd: f64,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub rss_mb: f64,
    /// The plain arm (the whole history every request): its largest request, input tokens sent, and cost.
    pub plain_max: i64,
    pub plain_sent: i64,
    pub plain_usd: f64,
    /// Median over the calls of the share of each request's prefix equal to the previous request's, in percent: this
    /// arm's and the plain arm's (the `prefijo` column).
    pub prefix: f64,
    pub plain_prefix: f64,
    /// The window the arm ran at (0: none) and, for the sierra arm, the compactions it made.
    pub window: u64,
    pub compactions: usize,
    /// The curator was called on every this-many-th request (1: all; more: the totals above count the calls made).
    pub stride: usize,
    /// Tokens of the history when the session ended (the whole session unless it was stopped).
    pub tokens: i64,
    /// Why the session was stopped early (time or memory), with where the time and the memory went.
    pub stopped: Option<String>,
    /// Seconds in `Curator::view`, in the generator and oracle, in the pricing, and in all.
    pub secs: [f64; 4],
}

impl ColdRow {
    pub fn recall(&self) -> f64 {
        100.0 * self.answered as f64 / self.questions.max(1) as f64
    }

    /// Percent of the standing tasks whose view held the fact they suppose (`fijos`).
    pub fn fijos(&self) -> f64 {
        100.0 * self.stand[0] as f64 / self.stand[1].max(1) as f64
    }

    /// The `fijos` column with its separator, empty for a session without standing facts.
    fn fijos_col(&self) -> String {
        if self.standing > 0 {
            format!(" | fijos {:.1}", self.fijos())
        } else {
            String::new()
        }
    }

    /// The ladder line (`--tokens`): tokens totales | N | % marcas en 1 salto | % en 2-3 saltos | vista máx | enviados |
    /// DeepSeek $ | ms/petición p50/p99 | RSS máx MB | plain, and, for a stopped session, why and where it went.
    pub fn line_tokens(&self) -> String {
        let r = |hit: usize, n: usize| {
            if n == 0 {
                "-".to_string()
            } else {
                format!("{:.1}", 100.0 * hit as f64 / n as f64)
            }
        };
        let mut s = format!(
            "{:.2}M tokens | {} topics | {} | {}{} | {} | {:.2}M | {:.4} | prefijo {:.1} | {:.2}/{:.2} | {:.0} | plain máx {} {:.2}M $ prefijo {:.1}{}",
            self.tokens as f64 / 1e6,
            self.topics,
            r(self.by_hops[0], self.by_hops[1]),
            r(self.by_hops[2], self.by_hops[3]),
            self.fijos_col(),
            self.view_max,
            self.sent as f64 / 1e6,
            self.usd,
            self.prefix,
            self.p50_ms,
            self.p99_ms,
            self.rss_mb,
            self.plain_max,
            self.plain_sent as f64 / 1e6,
            self.plain_prefix,
            if self.stride > 1 { format!(" | stride {}", self.stride) } else { String::new() },
        );
        if let Some(why) = &self.stopped {
            s += &format!(
                " | PARADO: {why}; s: curator {:.0}, generador y oráculo {:.0}, precio {:.0}, total {:.0}",
                self.secs[0], self.secs[1], self.secs[2], self.secs[3]
            );
        }
        s
    }

    /// The recall column: the percent of questions answered, or for a session whose questions take 1, 2 and 3 facts in
    /// turn (`hops` 0) the percent at one hop and at two or three.
    fn recall_col(&self) -> String {
        if self.hops == 0 {
            let r = |hit: usize, n: usize| 100.0 * hit as f64 / n.max(1) as f64;
            format!("{:.1}/{:.1}", r(self.by_hops[0], self.by_hops[1]), r(self.by_hops[2], self.by_hops[3]))
        } else {
            format!("{:.1}", self.recall())
        }
    }

    /// The window the arm ran at, as `128k` or `1M`.
    pub fn window_label(w: u64) -> String {
        match w {
            0 => "sin tope".to_string(),
            w if w >= 1_000_000 && w.is_multiple_of(1_000_000) => format!("{}M", w / 1_000_000),
            w if w >= 1000 && w.is_multiple_of(1000) => format!("{}k", w / 1000),
            w => w.to_string(),
        }
    }

    /// The one-line report: N | hops | recall % | view máx | enviados | DeepSeek $ | prefijo % | ms/request p50/p99 |
    /// RSS máx MB, then the plain arm (unlimited): largest request, sent, $, prefix, and whether it fits 128k and 1M
    /// windows. `hops` 0 shows the recall at one hop and at two or three (`a/b`).
    pub fn line(&self) -> String {
        let fit = |w: i64| if self.plain_max <= w { "cabe" } else { "no cabe" };
        format!(
            "{} | {} | {}{} | {} | {:.2}M | {:.4} | prefijo {:.1} | {:.2}/{:.2} | {:.0} | plain máx {} ({} 128k, {} 1M) enviados {:.2}M {:.4} $ prefijo {:.1}{}{}",
            self.topics,
            self.hops,
            self.recall_col(),
            self.fijos_col(),
            self.view_max,
            self.sent as f64 / 1e6,
            self.usd,
            self.prefix,
            self.p50_ms,
            self.p99_ms,
            self.rss_mb,
            self.plain_max,
            fit(128_000),
            fit(1_000_000),
            self.plain_sent as f64 / 1e6,
            self.plain_usd,
            self.plain_prefix,
            if self.window > 0 { format!(" | ventana {}", Self::window_label(self.window)) } else { String::new() },
            if self.stride > 1 { format!(" | stride {}", self.stride) } else { String::new() },
        )
    }

    /// The sierra arm's line, in the columns of `line()` up to RSS, then the window and the compactions it made.
    pub fn line_saw(&self) -> String {
        format!(
            "{} | {} | {}{} | {} | {:.2}M | {:.4} | prefijo {:.1} | {:.2}/{:.2} | {:.0} | sierra ventana {}, {} compactaciones{}",
            self.topics,
            self.hops,
            self.recall_col(),
            self.fijos_col(),
            self.view_max,
            self.sent as f64 / 1e6,
            self.usd,
            self.prefix,
            self.p50_ms,
            self.p99_ms,
            self.rss_mb,
            Self::window_label(self.window),
            self.compactions,
            if self.stride > 1 { format!(" | stride {}", self.stride) } else { String::new() },
        )
    }

    /// A row before any request.
    pub(super) fn blank(p: &ColdParams) -> ColdRow {
        ColdRow {
            topics: p.topics,
            hops: p.hops,
            questions: 0,
            answered: 0,
            calls: 0,
            view_max: 0,
            sent: 0,
            usd: 0.0,
            p50_ms: 0.0,
            p99_ms: 0.0,
            rss_mb: 0.0,
            plain_max: 0,
            plain_sent: 0,
            plain_usd: 0.0,
            prefix: 0.0,
            plain_prefix: 0.0,
            window: p.window,
            compactions: 0,
            stride: p.stride.max(1),
            by_hops: [0; 4],
            standing: p.standing,
            stand: [0; 2],
            tokens: 0,
            stopped: None,
            secs: [0.0; 4],
        }
    }

    /// Counts one call: its view's size, and whether it held the lines of the question or of the standing task.
    pub(super) fn judge(
        &mut self,
        view: &[Value],
        tokens: i64,
        question: Option<Vec<String>>,
        task: Option<Vec<String>>,
    ) {
        self.calls += 1;
        self.view_max = self.view_max.max(tokens);
        if let Some(lines) = question {
            self.questions += 1;
            let ok = usize::from(holds(view, &lines));
            self.answered += ok;
            let k = if lines.len() == 1 { 0 } else { 2 };
            self.by_hops[k] += ok;
            self.by_hops[k + 1] += 1;
        }
        if let Some(lines) = task {
            self.stand[0] += usize::from(holds(view, &lines));
            self.stand[1] += 1;
        }
    }
}

/// The tokens of the system prompt in the priced requests: 9000, less in a small window (an eighth of it at most).
pub(super) fn system_tokens(window: u64) -> i64 {
    if window == 0 {
        9000
    } else {
        9000.min(window as i64 / 8)
    }
}

/// The window of the sierra arm when `--window` says none.
pub const SAW_WINDOW: u64 = 128_000;

/// The share of a request's tokens (system prompt included) equal to the previous request's, in percent.
pub(super) fn share(all: i64, cached: i64) -> f64 {
    100.0 * cached as f64 / all.max(1) as f64
}

/// Runs the cold session: the curator sees every request (or every `stride`-th, and every question), the
/// oracle checks each question's view, and both arms are priced at the replay's DeepSeek prices.
pub fn run_cold(p: &ColdParams) -> ColdRow {
    use nexo_curator::sim::price::{tokens_of, Prices};
    let system = system_tokens(p.window);
    let prices = Prices::LIVE;
    let mut g = Cold::new(p.clone());
    let mut cur = Curator::new(&spec()).expect("spec");
    if p.window > 0 {
        cur.window(p.window);
    }
    let mut msgs: Vec<Value> = Vec::new();
    let mut sent = Sent::default();
    let (mut now, mut requests) = (0.0, 0usize);
    let mut row = ColdRow::blank(p);
    let (mut miss, mut hit, mut plain_total, mut plain_before) = (0i64, 0i64, 0i64, 0i64);
    let (mut times, mut shares, mut plain_shares) = (Vec::new(), Vec::new(), Vec::new());
    // the peak memory counted below is this session's alone
    let _ = std::fs::write("/proc/self/clear_refs", "5");
    let start = Instant::now();
    let (mut view_s, mut price_s) = (0.0, 0.0);
    // `SCALE_CHECK=1`: a hash of every reply of the session goes to stderr (the same number before and after a change that
    // must not change what the curator does)
    let (check, mut fold) = (std::env::var_os("SCALE_CHECK").is_some(), 0u64);
    'session: while let Some(b) = g.next_block() {
        let mut ask = b.ask;
        let mut stand = b.stand;
        for m in b.msgs {
            let user = m["role"] == "user";
            plain_total += tokens_of(&m);
            msgs.push(m);
            if !user {
                continue;
            }
            requests += 1;
            if requests.is_multiple_of(512) {
                let secs = start.elapsed().as_secs_f64();
                let rss = rss_mb();
                if (p.max_secs > 0.0 && secs > p.max_secs) || (p.max_rss_mb > 0.0 && rss > p.max_rss_mb) {
                    let what = if rss > p.max_rss_mb && p.max_rss_mb > 0.0 { "RSS" } else { "tiempo" };
                    row.stopped = Some(format!(
                        "{what} sobre el tope en el {:.1} % de la sesión ({} peticiones, {} llamadas), RSS {:.0} MB, historia ≈ {:.0} MB de texto",
                        100.0 * g.position() as f64 / g.total_blocks() as f64,
                        requests,
                        row.calls,
                        rss,
                        plain_total as f64 * 4.0 / 1e6
                    ));
                    break 'session;
                }
            }
            let question = ask.take();
            let task = stand.take();
            if question.is_none() && task.is_none() && !requests.is_multiple_of(p.stride.max(1)) {
                continue;
            }
            now += 20.0;
            let t = Instant::now();
            let reply = cur.view(system as u64, &msgs, now).expect("view");
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            view_s += t.elapsed().as_secs_f64();
            if check {
                // everything a call returns, in order: a change of behavior shows as another number
                let mut h = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hash::hash(&(fold, serde_json::to_string(&reply.view).unwrap_or_default()), &mut h);
                std::hash::Hash::hash(
                    &(reply.tokens, reply.memory.to_string(), format!("{:?}{:?}", reply.stats, reply.curated)),
                    &mut h,
                );
                std::hash::Hash::hash(&(reply.added, reply.removed), &mut h);
                fold = std::hash::Hasher::finish(&h);
            }
            let priced = Instant::now();
            row.judge(&reply.view, reply.tokens, question, task);
            let (all, cached) = sent.send(reply.view);
            let (all, cached) = (all + system, if row.calls == 1 { 0 } else { cached + system });
            miss += all - cached;
            hit += cached;
            let plain_all = plain_total + system;
            if row.calls > 1 {
                shares.push(share(all, cached));
                plain_shares.push(share(plain_all, plain_before.min(plain_all)));
            }
            row.plain_max = row.plain_max.max(plain_all);
            row.plain_sent += plain_all;
            row.plain_usd += prices.dollars(plain_all - plain_before.min(plain_all), plain_before.min(plain_all));
            plain_before = plain_all;
            price_s += priced.elapsed().as_secs_f64();
        }
    }
    if check {
        eprintln!("replies hash {fold:016x}");
    }
    row.sent = miss + hit;
    row.usd = prices.dollars(miss, hit);
    row.p50_ms = pct(&times, 0.5);
    row.p99_ms = pct(&times, 0.99);
    row.prefix = pct(&shares, 0.5);
    row.plain_prefix = pct(&plain_shares, 0.5);
    row.rss_mb = peak_rss_mb();
    row.tokens = plain_total;
    let total = start.elapsed().as_secs_f64();
    row.secs = [view_s, (total - view_s - price_s).max(0.0), price_s, total];
    row
}

// ---------------------------------------------------------------------------------------------------------
// The sierra arm: what other harnesses do. The whole history goes to the model until the request would pass 0.9 of the
// window; then everything but the last messages is replaced by a summary of a fixed size made by rules (the first
// request, the last requests cut short, the commands run, and only the last `SAW_KV` lines of the form `key = value`)
// and the session goes on from there: the view grows, drops, grows. What the summary does not keep is lost.
