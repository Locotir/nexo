use super::*;

/// The least input cost (miss, hit tokens) any view can have that forgets nothing a reply copies: at each
/// request it sends the lines of the latest request and, for each distinctive word a reply from this
/// request on copies, the smallest segment holding it. Every token sent costs at least a cache hit, and a
/// segment is a miss the first time it is sent, whatever the order and wherever it sits. A view that cuts
/// inside a message still keeps whole segments around what it keeps, so none goes under this.
///
/// With `once`, each new message is also sent whole in the request that first holds it: the model sees every
/// result it asked for at least once. Replies that reason over what they read (a number worked out from a
/// listing) copy few of its words, so without this the floor leaves the material out.
pub(super) fn bound(requests: &[Request], once: bool) -> (i64, i64) {
    let said: Vec<HashSet<String>> = (0..requests.len())
        .map(|i| reply_to(requests, i).map(|m| said_words(m).into_iter().collect()).unwrap_or_default())
        .collect();
    let mut ahead: Vec<HashSet<String>> = vec![HashSet::new(); requests.len() + 1];
    for i in (0..requests.len()).rev() {
        ahead[i] = ahead[i + 1].union(&said[i]).cloned().collect();
    }
    // per message: its segments, with their distinctive words and tokens
    let mut pieces: HashMap<String, Vec<(String, Vec<String>, i64)>> = HashMap::new();
    let (mut sent, mut miss, mut hit) = (HashSet::<String>::new(), 0i64, 0i64);
    for (i, r) in requests.iter().enumerate() {
        let keys: Vec<String> = r.messages.iter().map(Value::to_string).collect();
        for (k, m) in keys.iter().zip(&r.messages) {
            pieces.entry(k.clone()).or_insert_with(|| {
                segments(m)
                    .into_iter()
                    .map(|s| {
                        let (w, t) = (distinctive_words(&s), (s.len() as i64 + 3) / 4);
                        (s, w, t)
                    })
                    .collect()
            });
        }
        let mut need: HashMap<&str, i64> = HashMap::new();
        if let Some(q) = (0..keys.len()).rev().find(|&j| r.messages[j]["role"] == "user" && !has_result(&r.messages[j]))
        {
            need.extend(pieces[&keys[q]].iter().map(|(s, _, t)| (s.as_str(), *t)));
        }
        if once {
            let new = i.checked_sub(1).map_or(0, |p| requests[p].messages.len());
            for k in keys.iter().skip(new) {
                need.extend(pieces[k].iter().map(|(s, _, t)| (s.as_str(), *t)));
            }
        }
        let mut smallest: HashMap<&str, (&str, i64)> = HashMap::new();
        for k in &keys {
            for (s, ws, t) in &pieces[k] {
                for w in ws.iter().filter(|w| ahead[i].contains(*w)) {
                    let e = smallest.entry(w).or_insert((s, *t));
                    if *t < e.1 {
                        *e = (s, *t);
                    }
                }
            }
        }
        need.extend(smallest.into_values());
        hit += r.system_tokens as i64;
        for (s, t) in need {
            if sent.insert(s.to_string()) {
                miss += t;
            } else {
                hit += t;
            }
        }
        if i == 0 {
            (miss, hit) = (miss + r.system_tokens as i64, hit - r.system_tokens as i64);
        }
    }
    (miss, hit)
}

/// How an oracle tells that the agent has digested a result: it may cut only what the agent has dealt with.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Digest {
    /// Cuts any result seen before.
    No,
    /// Only after a later reply with text or a `note` call (the engine's own definition).
    Text,
    /// Also after a call whose arguments draw on a word of the result.
    Evidence,
}

/// What an oracle's views lose, scored as the curator's views are: `blind` and `denied` (missing, all
/// tokens) and forgotten words.
#[derive(Default)]
pub(super) struct Loss {
    pub(super) blind: (i64, i64),
    pub(super) denied: (i64, i64),
    pub(super) forgotten: usize,
    /// The tokens of the messages it sends, over all requests (the basis of the `view` total).
    pub(super) sent: i64,
}

/// The input cost (miss, hit tokens) of an oracle that knows the replies to come and cuts whole results, as
/// a curator does: a result message seen before (not new in this request) and holding no distinctive word a
/// reply from this request on copies may go, as a short stub. It is priced with the same prefix cache as the
/// views, so each cut breaks the cache behind it. With `batched` it cuts the results that may go only when
/// what they cost over the requests left pays for the cache it breaks (`X · R > 9 · S`: `X` tokens cut, `R`
/// requests left, `S` tokens sent from the first cut on, a miss costing ten hits); else it cuts each as soon
/// as it may. A rule that drops whole results and forgets nothing goes much under the better of the two only
/// by knowing the future better: it is the reachable mark, where `bound` is the unreachable one. With
/// `chunk` 1 it keeps, of each result it cuts, the lines that hold a word a reply from this request on copies
/// (any seen result may then go), so only what is never used again leaves: the finest cut a curator could
/// make, still paying the cache it breaks. With a larger `chunk` it keeps or drops runs of that many lines
/// whole (a run stays when one of its lines is used later); 0 keeps nothing of a result it cuts.
pub(super) fn oracle(
    requests: &[Request],
    batched: bool,
    chunk: usize,
    digest: Digest,
) -> (i64, i64, Vec<(i64, i64)>, Loss) {
    let said: Vec<HashSet<String>> = (0..requests.len())
        .map(|i| reply_to(requests, i).map(|m| said_words(m).into_iter().collect()).unwrap_or_default())
        .collect();
    let mut ahead: Vec<HashSet<String>> = vec![HashSet::new(); requests.len() + 1];
    for i in (0..requests.len()).rev() {
        ahead[i] = ahead[i + 1].union(&said[i]).cloned().collect();
    }
    let mut words: HashMap<String, Vec<String>> = HashMap::new();
    let mut cut: HashMap<String, Value> = HashMap::new();
    let (mut prev, mut miss, mut hit, mut per) = (Vec::<Value>::new(), 0i64, 0i64, Vec::new());
    let mut loss = Loss::default();
    for (i, r) in requests.iter().enumerate() {
        let new = i.checked_sub(1).map_or(0, |p| requests[p].messages.len());
        let keys: Vec<String> = r.messages.iter().map(Value::to_string).collect();
        let start = latest_request(&r.messages);
        let may: Vec<usize> = (0..new.min(keys.len()))
            .filter(|&j| has_result(&r.messages[j]) && !cut.contains_key(&keys[j]))
            .filter(|&j| {
                digest == Digest::No
                    || j < start
                    || match digest {
                        Digest::Evidence => {
                            let ws: HashSet<String> = distinctive_words(&text_of(&r.messages[j])).into_iter().collect();
                            wrote_about(&r.messages[j + 1..], &ws)
                        }
                        _ => r.messages[j + 1..].iter().any(wrote),
                    }
                    || repeats(&r.messages[..j], &r.messages[j])
            })
            .filter(|&j| {
                chunk > 0 || {
                    let ws =
                        words.entry(keys[j].clone()).or_insert_with(|| distinctive_words(&text_of(&r.messages[j])));
                    !ws.iter().any(|w| ahead[i].contains(w))
                }
            })
            .collect();
        if let Some(&first) = may.first() {
            // each result block keeps its call id, as the API asks of any view; only its text shrinks
            let stub = |j: usize| {
                let m = &r.messages[j];
                // a result whose lines an earlier one holds is left out whole: the earlier copy stays
                let again = digest != Digest::No && repeats(&r.messages[..j], m);
                let shrink = |text: String| {
                    let kept: Vec<&str> = if chunk > 0 && !again {
                        // runs of `chunk` lines stay or go whole: a run stays when one of its lines is used later
                        let all: Vec<&str> = text.lines().collect();
                        all.chunks(chunk)
                            .filter(|c| c.iter().any(|l| distinctive_words(l).iter().any(|w| ahead[i].contains(w))))
                            .flatten()
                            .copied()
                            .collect()
                    } else {
                        Vec::new()
                    };
                    if kept.is_empty() {
                        "[out]".to_string()
                    } else {
                        kept.join("\n")
                    }
                };
                let mut m = m.clone();
                if let Some(Value::Array(blocks)) = m.get_mut("content") {
                    for b in blocks.iter_mut().filter(|b| b["type"] == "tool_result") {
                        let text = match &b["content"] {
                            Value::String(s) => s.clone(),
                            Value::Array(inner) => {
                                inner.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n")
                            }
                            _ => String::new(),
                        };
                        b["content"] = Value::String(shrink(text));
                    }
                }
                m
            };
            let stubs: Vec<(usize, Value)> = may.iter().map(|&j| (j, stub(j))).collect();
            let x: i64 = stubs.iter().map(|(j, v)| tokens_of(&r.messages[*j]) - tokens_of(v)).sum();
            let s: i64 = (first..keys.len())
                .map(|j| cut.get(&keys[j]).map_or_else(|| tokens_of(&r.messages[j]), tokens_of))
                .sum();
            if !batched || (x * (requests.len() - i) as i64) as f64 > cache_ratio() * s as f64 {
                cut.extend(stubs.into_iter().map(|(j, v)| (keys[j].clone(), v)));
            }
        }
        let view: Vec<Value> =
            r.messages.iter().zip(&keys).map(|(m, k)| cut.get(k).cloned().unwrap_or_else(|| m.clone())).collect();
        for (acc, (a, b)) in
            [(&mut loss.blind, blind(&r.messages, &view)), (&mut loss.denied, denied_values(&r.messages, &view))]
        {
            acc.0 += a;
            acc.1 += b;
        }
        if let Some(said) = reply_to(requests, i) {
            loss.forgotten += forgotten(said, &r.messages, &view, &r.system).0.len();
        }
        let h = cached(&prev, &view) + if i > 0 { r.system_tokens as i64 } else { 0 };
        let sent: i64 = view.iter().map(tokens_of).sum();
        loss.sent += sent;
        let all = sent + r.system_tokens as i64;
        hit += h;
        miss += all - h;
        per.push((all - h, h));
        prev = view;
    }
    (miss, hit, per, loss)
}
