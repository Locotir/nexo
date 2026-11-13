use super::*;

/// Lines `key = value` the summary keeps, requests it quotes (each cut to `SAW_CUT` characters), commands it lists, the
/// messages that stay after a compaction at most, and the characters of the summary at most (about 2,000 tokens).
pub(super) const SAW_KV: usize = 40;
const SAW_REQS: usize = 8;
const SAW_CUT: usize = 300;
const SAW_FILES: usize = 40;
const SAW_KEEP: usize = 20;
const SAW_CHARS: usize = 8000;

/// The sierra arm's memory: what the summary is made of, the summary, and the messages after it.
#[derive(Default)]
pub struct Saw {
    first: String,
    reqs: VecDeque<String>,
    files: VecDeque<String>,
    kv: VecDeque<String>,
    summary: Option<(Value, Value)>,
    live: Vec<Value>,
    live_tokens: i64,
    summary_tokens: i64,
    pub compactions: usize,
}

/// Is `m` a request of the person (a user message that is not a tool result)?
fn is_request(m: &Value) -> bool {
    m["role"] == "user"
        && match &m["content"] {
            Value::Array(blocks) => !blocks.iter().any(|b| b["type"] == "tool_result"),
            _ => true,
        }
}

fn keep_last(q: &mut VecDeque<String>, item: String, n: usize) {
    if q.back() != Some(&item) {
        q.retain(|x| *x != item);
        q.push_back(item);
    }
    while q.len() > n {
        q.pop_front();
    }
}

impl Saw {
    /// Tokens of the view (the summary and what follows it), without the system prompt.
    pub fn tokens(&self) -> i64 {
        self.summary_tokens + self.live_tokens
    }

    /// The view: the summary (with its acknowledgement) and the messages after it.
    pub fn view(&self) -> Vec<Value> {
        let mut v: Vec<Value> = self.summary.iter().flat_map(|(a, b)| [a.clone(), b.clone()]).collect();
        v.extend(self.live.iter().cloned());
        v
    }

    pub fn push(&mut self, m: Value) {
        self.live_tokens += nexo_curator::sim::price::tokens_of(&m);
        self.live.push(m);
    }

    /// Compacts when the view and the system prompt pass 0.9 of the window. The messages that stay are the last
    /// `SAW_KEEP` from a request of the person on, as many as fit in a quarter of the window, and at least the last request.
    pub fn compact_if_over(&mut self, window: i64, system: i64) -> bool {
        use nexo_curator::sim::price::tokens_of;
        if (system + self.tokens()) * 10 <= window * 9 {
            return false;
        }
        let n = self.live.len();
        let mut cut = (0..n).rev().find(|&i| is_request(&self.live[i])).unwrap_or(n - 1);
        let mut tokens: i64 = self.live[cut..].iter().map(tokens_of).sum();
        // earlier requests join while they fit
        for i in (0..cut).rev().filter(|&i| is_request(&self.live[i])) {
            let more: i64 = self.live[i..cut].iter().map(tokens_of).sum();
            if n - i > SAW_KEEP || (tokens + more) * 4 > window {
                break;
            }
            (cut, tokens) = (i, tokens + more);
        }
        let dropped: Vec<Value> = self.live.drain(..cut).collect();
        for m in &dropped {
            self.read(m);
        }
        self.live_tokens = tokens;
        self.compactions += 1;
        let (up, ack) = (
            json!({"role": "user", "content": self.text()}),
            json!({"role": "assistant", "content": "Understood. Continuing from the summary."}),
        );
        self.summary_tokens = tokens_of(&up) + tokens_of(&ack);
        self.summary = Some((up, ack));
        true
    }

    /// Takes what the summary keeps from a message that leaves the view.
    fn read(&mut self, m: &Value) {
        let text = nexo_curator::sim::input::text_of(m);
        if is_request(m) {
            let cut: String = text.chars().take(SAW_CUT).collect();
            if self.first.is_empty() {
                self.first = cut.clone();
            }
            keep_last(&mut self.reqs, cut, SAW_REQS);
        }
        if let Some(blocks) = m["content"].as_array() {
            for input in blocks.iter().filter(|b| b["type"] == "tool_use").map(|b| &b["input"]) {
                let key = ["command", "path", "file_path", "url"].iter().find_map(|k| input[*k].as_str());
                if let Some(c) = key {
                    keep_last(&mut self.files, c.chars().take(SAW_CUT).collect(), SAW_FILES);
                }
            }
        }
        if m["role"] == "user" {
            for l in text.lines().map(str::trim).filter(|l| l.contains(" = ") && !l.starts_with('#') && l.len() <= 200)
            {
                self.kv.push_back(l.to_string());
                if self.kv.len() > SAW_KV {
                    self.kv.pop_front();
                }
            }
        }
    }

    /// The summary, at most `SAW_CHARS` characters.
    fn text(&self) -> String {
        let list = |q: &VecDeque<String>| q.iter().map(|x| format!("- {x}\n")).collect::<String>();
        let kv: String = self.kv.iter().map(|x| format!("{x}\n")).collect();
        let s = format!(
            "Summary of the conversation so far.\nFirst request: {}\nRecent requests:\n{}Commands run:\n{}Settings seen:\n{kv}",
            self.first,
            list(&self.reqs),
            list(&self.files)
        );
        s.chars().take(SAW_CHARS).collect()
    }
}

/// Runs the sierra arm on the cold session: the history until the view would pass 0.9 of the window, then a summary
/// that keeps the last 40 `key = value` lines and the last messages, and on. Same oracle, same columns, same prices as
/// `run_cold`; the plain fields stay empty (the curator row carries the reference).
pub fn run_saw(p: &ColdParams) -> ColdRow {
    use nexo_curator::sim::price::Prices;
    let window = if p.window > 0 { p.window } else { SAW_WINDOW };
    let system = system_tokens(window);
    let prices = Prices::LIVE;
    let mut g = Cold::new(p.clone());
    let mut saw = Saw::default();
    let mut sent = Sent::default();
    let mut row = ColdRow::blank(p);
    row.window = window;
    let (mut miss, mut hit, mut requests) = (0i64, 0i64, 0usize);
    let (mut times, mut shares) = (Vec::new(), Vec::new());
    let _ = std::fs::write("/proc/self/clear_refs", "5");
    let start = Instant::now();
    'session: while let Some(b) = g.next_block() {
        let mut ask = b.ask;
        let mut stand = b.stand;
        for m in b.msgs {
            let user = m["role"] == "user";
            row.tokens += nexo_curator::sim::price::tokens_of(&m);
            saw.push(m);
            if !user {
                continue;
            }
            requests += 1;
            if requests.is_multiple_of(512) {
                let (secs, rss) = (start.elapsed().as_secs_f64(), rss_mb());
                if (p.max_secs > 0.0 && secs > p.max_secs) || (p.max_rss_mb > 0.0 && rss > p.max_rss_mb) {
                    let what = if rss > p.max_rss_mb && p.max_rss_mb > 0.0 { "RSS" } else { "tiempo" };
                    row.stopped = Some(format!("{what} sobre el tope ({requests} peticiones), RSS {rss:.0} MB"));
                    break 'session;
                }
            }
            let t = Instant::now();
            // the compaction is checked on every request; only the calls the curator arm makes are priced
            saw.compact_if_over(window as i64, system);
            let question = ask.take();
            let task = stand.take();
            if question.is_none() && task.is_none() && !requests.is_multiple_of(p.stride.max(1)) {
                continue;
            }
            let view = saw.view();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            row.judge(&view, saw.tokens(), question, task);
            let (all, cached) = sent.send(view);
            let (all, cached) = (all + system, if row.calls == 1 { 0 } else { cached + system });
            miss += all - cached;
            hit += cached;
            if row.calls > 1 {
                shares.push(share(all, cached));
            }
        }
    }
    row.sent = miss + hit;
    row.usd = prices.dollars(miss, hit);
    row.p50_ms = pct(&times, 0.5);
    row.p99_ms = pct(&times, 0.99);
    row.prefix = pct(&shares, 0.5);
    row.compactions = saw.compactions;
    row.rss_mb = peak_rss_mb();
    row.secs[3] = start.elapsed().as_secs_f64();
    row
}
