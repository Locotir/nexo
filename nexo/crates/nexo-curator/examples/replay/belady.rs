use super::*;

/// `--oracle`: the floor an offline policy sets under the view. It knows the replies to come (the words
/// each reply copies from the history) and at each request keeps, within the tokens the curator's view
/// spent on that request, the messages of the request itself, the smallest message holding each word
/// the next reply copies (small ones first when they do not all fit), then the messages with the nearest
/// use ahead, the small ones first (Belady's rule weighed by size); a message never copied from again
/// goes. A reply loses a word only when what it needs does not fit. The views are priced with the same
/// prefix cache, so each change behind the start breaks it: the cost says what the floor would pay, the
/// floor is on what is lost. A history the host rewound is indexed by its final shape.
pub(super) fn belady(
    records: &[(usize, Vec<Value>)],
    said: &[Option<Vec<String>>],
    budgets: &[i64],
    system: &HashSet<String>,
    system_tokens: i64,
) -> Way {
    let mut fin = Sending::default();
    let mut len_at = Vec::with_capacity(records.len());
    for (base, new) in records {
        fin.extend(*base, new.clone());
        len_at.push(fin.msgs.len());
    }
    let mut holders: HashMap<&str, Vec<usize>> = HashMap::new();
    for (j, ws) in fin.words.iter().enumerate() {
        for w in ws {
            holders.entry(w).or_default().push(j);
        }
    }
    // per message, the requests whose reply copies one of its words, in order
    let mut uses: Vec<Vec<usize>> = vec![Vec::new(); fin.msgs.len()];
    for (r, ws) in said.iter().enumerate() {
        for w in ws.iter().flatten() {
            for &j in holders.get(w.as_str()).map_or(&[][..], Vec::as_slice) {
                if j < len_at[r] && uses[j].last() != Some(&r) {
                    uses[j].push(r);
                }
            }
        }
    }
    let (mut whole, mut view, mut way) = (Sending::default(), Sending::default(), Way::default());
    for (r, (base, new)) in records.iter().enumerate() {
        if let Some(ws) = r.checked_sub(1).and_then(|p| said[p].as_ref()) {
            let lost = ws.iter().filter(|w| !view.bag.has(w) && !system.contains(*w)).count();
            way.lost += lost;
            way.lost_replies += usize::from(lost > 0);
        }
        whole.extend(*base, new.clone());
        let n = whole.msgs.len();
        let base = (*base).min(n);
        let mut keep: Vec<bool> = (0..n).map(|j| j >= base).collect();
        let mut total: i64 = whole.tokens[base..].iter().sum();
        let budget = budgets.get(r).copied().unwrap_or(0).max(total);
        let mut needed: Vec<usize> = said
            .get(r)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|w| {
                holders.get(w.as_str())?.iter().filter(|&&j| j < n).min_by_key(|&&j| (fin.tokens[j], j)).copied()
            })
            .collect();
        needed.sort_unstable();
        needed.dedup();
        needed.sort_by_key(|&j| fin.tokens[j]);
        for j in needed {
            if !keep[j] && total + whole.tokens[j] <= budget {
                keep[j] = true;
                total += whole.tokens[j];
            }
        }
        let mut ahead: Vec<(i64, usize)> = (0..base)
            .filter(|&j| !keep[j])
            .filter_map(|j| {
                let next = *uses[j].iter().find(|&&u| u > r)?;
                Some(((next - r) as i64 * whole.tokens[j].max(1), j))
            })
            .collect();
        ahead.sort_unstable();
        for (_, j) in ahead {
            if total + whole.tokens[j] <= budget {
                keep[j] = true;
                total += whole.tokens[j];
            }
        }
        let msgs: Vec<Value> = (0..n).filter(|&j| keep[j]).map(|j| whole.msgs[j].clone()).collect();
        let (hit, rewrote) = view.send(msgs);
        way.rewrites += usize::from(rewrote);
        let head = if r > 0 { system_tokens } else { 0 };
        way.count(view.total() + system_tokens, hit + head);
    }
    way
}
