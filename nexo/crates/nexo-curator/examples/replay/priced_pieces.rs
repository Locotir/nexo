use super::*;

/// The price sheet in $ per million tokens, (missed, cached): `REPLAY_PRICE="miss,hit"`, by default 0.28,0.028.
pub(super) fn prices() -> (f64, f64) {
    static SHEET: std::sync::OnceLock<(f64, f64)> = std::sync::OnceLock::new();
    *SHEET.get_or_init(|| match std::env::var("REPLAY_PRICE") {
        Err(_) => (0.28, 0.028),
        Ok(v) => v
            .split_once(',')
            .and_then(|(m, h)| Some((m.trim().parse::<f64>().ok()?, h.trim().parse::<f64>().ok()?)))
            .filter(|(m, h)| m.is_finite() && h.is_finite() && *m >= *h && *h >= 0.0)
            .unwrap_or_else(|| {
                eprintln!("REPLAY_PRICE wants \"miss,hit\" in $ per million tokens, with miss >= hit >= 0");
                std::process::exit(2);
            }),
    })
}

/// The input cost of a view sequence at the price sheet (see `prices`).
pub(super) fn dollars(miss: i64, hit: i64) -> f64 {
    let (m, h) = prices();
    (miss as f64 * m + hit as f64 * h) / 1e6
}

/// How many cached tokens one rewritten token costs: `(miss - hit) / hit`, 9 at the default price sheet.
pub(super) fn cache_ratio() -> f64 {
    let (m, h) = prices();
    if h <= 0.0 {
        return f64::INFINITY;
    }
    let r = (m - h) / h;
    if (r - r.round()).abs() < 1e-9 {
        r.round()
    } else {
        r
    }
}

/// The text of a tool result block, as the oracle reads it.
fn result_text(b: &Value) -> String {
    match &b["content"] {
        Value::String(s) => s.clone(),
        Value::Array(inner) => inner.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// A result message: its first request, its framing tokens and its runs (tokens, dead).
pub(super) type Resultant = (usize, i64, Vec<(i64, usize)>);

/// What the clairvoyant optimum prices of a transcript: the pieces, the tokens of the oracle's `[out]`
/// marker, and whether a result message is one piece (`coarse`) because one piece per run made the table too big.
pub(super) struct Pieces {
    pub(super) pieces: Vec<Piece>,
    pub(super) stub: i64,
    pub(super) coarse: bool,
}

/// The history of `requests` as pieces for `optimum` (none when the history is rewritten between requests), cut as `oracle(requests, _, chunk, digest)` decides what
/// may go. A result message is a framing piece that stays and one piece per run of `chunk` lines (the whole
/// result for chunk 0), dead at the first request at which the oracle lets it go: the message was seen before,
/// the agent has digested it, and no word of the run is said by a reply from then on (a message whose lines
/// an earlier result holds goes whole). Anything else stays for good. The oracle freezes what it keeps of a
/// message when it first cuts it; this lets each run go on its own, which can only lower the optimum.
/// Messages are taken to be the same at every request that holds them, and the digest tests to hold from the
/// first request that meets them on.
pub(super) fn optimum_pieces(requests: &[Request], chunk: usize, digest: Digest) -> Option<Pieces> {
    let n = requests.len();
    // the model needs a history that only grows: each request starts with the messages of the one before
    let grows = |w: &[Request]| {
        w[1].messages.len() >= w[0].messages.len() && w[1].messages[..w[0].messages.len()] == w[0].messages[..]
    };
    if !requests.windows(2).all(grows) {
        return None;
    }
    let said: Vec<HashSet<String>> = (0..n)
        .map(|i| reply_to(requests, i).map(|m| said_words(m).into_iter().collect()).unwrap_or_default())
        .collect();
    // ahead[i] holds w exactly while i <= last_say[w]
    let mut last_say: HashMap<&str, usize> = HashMap::new();
    for (i, s) in said.iter().enumerate() {
        for w in s {
            last_say.insert(w, i);
        }
    }
    let free_from = |words: &mut dyn Iterator<Item = String>| {
        words.filter_map(|w| last_say.get(w.as_str()).map(|i| i + 1)).max().unwrap_or(0)
    };
    let starts: Vec<usize> = requests.iter().map(|r| latest_request(&r.messages)).collect();
    let mut born: Vec<usize> = Vec::new();
    for (i, r) in requests.iter().enumerate() {
        born.resize(born.len().max(r.messages.len()), i);
    }
    let mut pieces = vec![Piece { tokens: requests[0].system_tokens as i64, born: 0, dead: n }];
    // per result message: framing tokens and the runs (tokens, dead), in order
    let mut results: Vec<Resultant> = Vec::new();
    let mut order: Vec<Result<Piece, usize>> = Vec::new();
    for (j, &b) in born.iter().enumerate() {
        let m = &requests[b].messages[j];
        if !has_result(m) {
            order.push(Ok(Piece { tokens: tokens_of(m), born: b, dead: n }));
            continue;
        }
        let blank = {
            let mut m = m.clone();
            if let Some(Value::Array(blocks)) = m.get_mut("content") {
                for x in blocks.iter_mut().filter(|x| x["type"] == "tool_result") {
                    x["content"] = Value::String(String::new());
                }
            }
            m
        };
        let total = tokens_of(m);
        let framing = tokens_of(&blank).min(total);
        // the runs as lists of lines
        let mut runs: Vec<Vec<String>> = Vec::new();
        let blocks = m["content"].as_array().cloned().unwrap_or_default();
        let texts: Vec<String> = blocks.iter().filter(|x| x["type"] == "tool_result").map(result_text).collect();
        if chunk == 0 {
            runs.push(texts.iter().flat_map(|t| t.lines().map(str::to_string)).collect());
        } else {
            for t in &texts {
                let lines: Vec<&str> = t.lines().collect();
                runs.extend(lines.chunks(chunk).map(|c| c.iter().map(|l| l.to_string()).collect()));
            }
        }
        let weight = |run: &Vec<String>| run.iter().map(|l| l.len() as i64 + 2).sum::<i64>();
        let all: i64 = runs.iter().map(weight).sum();
        let text_tokens = total - framing;
        // the lines' tokens are shared out by size, so that the message sums to `tokens_of`
        let (mut cum, mut given) = (0i64, 0i64);
        let tokens: Vec<i64> = runs
            .iter()
            .map(|r| {
                cum += weight(r);
                let upto = if all > 0 { text_tokens * cum / all } else { 0 };
                let t = upto - given;
                given = upto;
                t
            })
            .collect();
        let rest = text_tokens - given;
        let words_of = |run: &Vec<String>| -> Vec<String> {
            if chunk == 0 {
                distinctive_words(&text_of(m))
            } else {
                run.iter().flat_map(|l| distinctive_words(l)).collect()
            }
        };
        let cands: Vec<usize> = runs.iter().map(|r| (b + 1).max(free_from(&mut words_of(r).into_iter()))).collect();
        let again = chunk > 0 && digest != Digest::No && repeats(&requests[b].messages[..j], m);
        let mut ok_from = None;
        if !again && cands.iter().any(|&c| c < n) {
            let ws: HashSet<String> = distinctive_words(&text_of(m)).into_iter().collect();
            let mut rep = None;
            let first = cands.iter().copied().min().unwrap_or(n);
            ok_from = (first..n).find(|&i| {
                let msgs = &requests[i].messages;
                digest == Digest::No
                    || j < starts[i]
                    || match digest {
                        Digest::Evidence => wrote_about(&msgs[j + 1..], &ws),
                        _ => msgs[j + 1..].iter().any(wrote),
                    }
                    || *rep.get_or_insert_with(|| repeats(&msgs[..j], m))
            });
        }
        let dead: Vec<usize> = cands
            .iter()
            .map(|&c| match (again, ok_from) {
                (true, _) => b + 1,
                (_, Some(f)) if c < n => c.max(f),
                _ => n,
            })
            .map(|d| if d >= n { n } else { d })
            .collect();
        let mut run_list: Vec<(i64, usize)> = tokens.into_iter().zip(dead).collect();
        if rest > 0 {
            run_list.push((rest, n));
        }
        results.push((b, framing, run_list));
        order.push(Err(results.len() - 1));
    }
    let removable: usize = results.iter().flat_map(|r| &r.2).filter(|r| r.1 < n).count();
    let coarse = (n as f64).powi(3) * removable as f64 / 6.0 > 2e10;
    for o in order {
        match o {
            Ok(p) => pieces.push(p),
            Err(k) => {
                let (b, framing, runs) = &results[k];
                pieces.push(Piece { tokens: *framing, born: *b, dead: n });
                if coarse {
                    let stays: i64 = runs.iter().filter(|r| r.1 >= n).map(|r| r.0).sum();
                    let goes: i64 = runs.iter().filter(|r| r.1 < n).map(|r| r.0).sum();
                    let dead = runs.iter().filter(|r| r.1 < n).map(|r| r.1).max();
                    pieces.push(Piece { tokens: stays, born: *b, dead: n });
                    if let Some(dead) = dead {
                        pieces.push(Piece { tokens: goes, born: *b, dead });
                    }
                } else {
                    pieces.extend(runs.iter().map(|&(tokens, dead)| Piece { tokens, born: *b, dead }));
                }
            }
        }
    }
    pieces.retain(|p| p.tokens > 0);
    Some(Pieces { pieces, stub: ("[out]".len() as i64 + 3) / 4, coarse })
}

/// The optimum of `pieces` at the price sheet, in $: without stubs (the least) and with them (an upper bound).
pub(super) fn priced_optimum(p: &Pieces, n: usize) -> (Cost, Cost) {
    let (m, h) = prices();
    let started = std::time::Instant::now();
    let both = (optimum(&p.pieces, n, h, m, 0), optimum(&p.pieces, n, h, m, p.stub));
    if std::env::var("REPLAY_TIME").is_ok_and(|v| v == "1") {
        eprintln!(
            "optimum: {} pieces, {n} requests, {:.2} s{}",
            p.pieces.len(),
            started.elapsed().as_secs_f64(),
            if p.coarse { " coarse" } else { "" }
        );
    }
    both
}
