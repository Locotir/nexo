//! `diff A B FUNCTION`: one function's instructions side by side, aligned on what they do.

use iced_x86::{Formatter, IntelFormatter};
use serde_json::json;

use super::{same_data, same_target, status_of, text_row, Body, By, Item, Pairs, Refs, Side, Status};
use super::{CONTEXT, MAX_ALIGN, MAX_EDITS, MAX_FUNC};
use crate::disasm::{comment, formatter};
use crate::funcs::Func;
use crate::ops::{numeric, resolve};
use crate::page::{cut, shell_word, Output, Row};
use crate::{Error, Result};

/// The function a target names: in A (a name or an address), else in B; `b:` asks B first.
fn pick(sa: &Side, sb: &Side, p: &Pairs, target: &str) -> Result<(Option<usize>, Option<usize>)> {
    let index = |s: &Side, f: &Func| s.funcs.partition_point(|g| g.start < f.start);
    let t = target.trim();
    if let Some(rest) = t.strip_prefix("b:") {
        let j = index(sb, resolve(sb.img, rest)?);
        return Ok((p.ba[j], Some(j)));
    }
    let t = t.strip_prefix("a:").unwrap_or(t);
    match resolve(sa.img, t) {
        Ok(f) => {
            let i = index(sa, f);
            Ok((Some(i), p.ab[i]))
        }
        Err(e) if numeric(t) => Err(Error::Bad(format!("in a: {e} (an address in b is written b:{t})"))),
        Err(e) => match resolve(sb.img, t) {
            Ok(f) => {
                let j = index(sb, f);
                Ok((p.ba[j], Some(j)))
            }
            Err(_) => Err(Error::Bad(format!("{e}; b has no such function either"))),
        },
    }
}

/// One instruction's key, the instruction it branches to inside the function, and what it points at.
struct Key {
    key: u64,
    local: Option<usize>,
    refs: Vec<usize>,
    data: Vec<Vec<u64>>,
}

fn keys(s: &Side, f: &Func, body: &Body) -> Vec<Key> {
    body.items
        .iter()
        .map(|it| {
            let mut r = Refs::default();
            let (key, local) = s.key(f, body, it, &mut r);
            Key { key, local, refs: r.funcs, data: r.data }
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Same,
    Elsewhere,
    Gone,
    New,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn show(
    sa: &Side,
    sb: &Side,
    p: &Pairs,
    target: &str,
    all: bool,
    base: &str,
    from: Option<u64>,
    budget: usize,
) -> Result<Output> {
    let (i, j) = pick(sa, sb, p, target)?;
    let command = format!("{base} {}{}", shell_word(target.trim()), if all { " --all" } else { "" });
    let fn_cmd = |s: &Side, k: usize| crate::ops::command(s.img, "fn", &format!("{:#x}", s.funcs[k].start));
    let (i, j) = match (i, j) {
        (Some(i), Some(j)) => (i, j),
        (i, j) => {
            let (s, k, side, other, word) = match (i, j) {
                (Some(i), _) => (sa, i, "a", "b", "removed"),
                (_, Some(j)) => (sb, j, "b", "a", "added"),
                _ => return Err(Error::Bad(format!("no function is named {target:?}"))),
            };
            let f = &s.funcs[k];
            let lines = vec![
                format!(
                    "{} at {:#x} in {side} ({} bytes, {} instructions) is only in {side}: no function of {other} pairs with it ({word})",
                    s.name(k),
                    f.start,
                    s.sums[k].size,
                    s.sums[k].insns
                ),
                format!("see it: {}", fn_cmd(s, k)),
            ];
            let rows = lines.into_iter().enumerate().map(|(n, t)| Row { key: n as u64, text: t, json: None }).collect();
            let meta = json!({"status": word, "function": s.name(k), "side": side, "addr": f.start});
            return Ok(Output { op: "diff", meta, rows, tail: 0, command });
        }
    };
    let (fa, fb) = (&sa.funcs[i], &sb.funcs[j]);
    let (ba, bb) = (Body::read(sa.img, fa), Body::read(sb.img, fb));
    let (ka, kb) = (keys(sa, fa, &ba), keys(sb, fb, &bb));
    let (pairs, aligned) =
        align(&ka.iter().map(|k| k.key).collect::<Vec<_>>(), &kb.iter().map(|k| k.key).collect::<Vec<_>>());
    let mut amap = vec![None; ka.len()];
    for &(x, y) in &pairs {
        amap[x] = Some(y);
    }
    let goes_same = |x: &Key, y: &Key| {
        let local = match (x.local, y.local) {
            (None, None) => true,
            (Some(u), Some(v)) => (u == ka.len() && v == kb.len()) || amap.get(u) == Some(&Some(v)),
            _ => false,
        };
        local
            && x.refs.len() == y.refs.len()
            && x.refs.iter().zip(&y.refs).all(|(&u, &v)| same_target(p, sa, sb, u, v))
            && same_data(&x.data, &y.data)
    };
    // the lines in order: (mark, index in a, index in b)
    let mut lines: Vec<(Mark, Option<usize>, Option<usize>)> = Vec::new();
    let (mut x, mut y) = (0, 0);
    for &(px, py) in &pairs {
        lines.extend((x..px).map(|k| (Mark::Gone, Some(k), None)));
        lines.extend((y..py).map(|k| (Mark::New, None, Some(k))));
        let m = if goes_same(&ka[px], &kb[py]) { Mark::Same } else { Mark::Elsewhere };
        lines.push((m, Some(px), Some(py)));
        (x, y) = (px + 1, py + 1);
    }
    lines.extend((x..ka.len()).map(|k| (Mark::Gone, Some(k), None)));
    lines.extend((y..kb.len()).map(|k| (Mark::New, None, Some(k))));
    let n = |m: Mark| lines.iter().filter(|l| l.0 == m).count();
    let (gone, new, elsewhere, same) = (n(Mark::Gone), n(Mark::New), n(Mark::Elsewhere), n(Mark::Same));
    let (status, _) = status_of(p, sa, sb, i, j);
    let name = match (sa.name(i), sb.name(j)) {
        (x, y) if x == y => x.to_string(),
        (x, y) => format!("{x} -> {y}"),
    };
    let by = p.by[i].map_or("?", By::word);
    let word = if status == Status::Same { "same once addresses are set aside" } else { "changed" };
    let mut head = vec![format!(
        "diff {name}: a {:#010x} ({} bytes, {} instructions) -> b {:#010x} ({} bytes, {} instructions); matched by {by}; {word}",
        fa.start,
        ba.size,
        ka.len(),
        fb.start,
        bb.size,
        kb.len()
    )];
    if !aligned {
        head.push(format!(
            "note: too far apart to align (more than {MAX_EDITS} edits): past the common start and end, a is shown removed and b added"
        ));
    }
    if ba.big || bb.big {
        head.push(format!("note: only the first {} MiB of each side compared", MAX_FUNC >> 20));
    }
    // which lines to show: the changes and CONTEXT lines around each, unless all
    let mut near = vec![usize::MAX; lines.len()];
    let mut last = None;
    for (k, l) in lines.iter().enumerate() {
        if l.0 != Mark::Same {
            last = Some(k);
        }
        near[k] = last.map_or(usize::MAX, |c| k - c);
    }
    last = None;
    for (k, l) in lines.iter().enumerate().rev() {
        if l.0 != Mark::Same {
            last = Some(k);
        }
        near[k] = near[k].min(last.map_or(usize::MAX, |c| c - k));
    }
    let show = |k: usize| all || near[k] <= CONTEXT;
    enum L {
        Text(String),
        Line(usize),
        Fold(usize),
    }
    let mut items: Vec<L> = head.into_iter().map(L::Text).collect();
    if status == Status::Same && !all {
        items.push(L::Text(format!(
            "same: {} instructions on each side (--all lists them side by side)",
            ka.len().max(kb.len())
        )));
    } else {
        items.push(L::Text(format!("  {:<10}  {:<10}  instruction", "a", "b")));
        let mut k = 0;
        while k < lines.len() {
            if show(k) {
                items.push(L::Line(k));
                k += 1;
                continue;
            }
            let start = k;
            while k < lines.len() && !show(k) {
                k += 1;
            }
            if k - start == 1 {
                items.push(L::Line(start));
            } else {
                items.push(L::Fold(k - start));
            }
        }
        items.push(L::Text(format!(
            "{gone} only in a (-), {new} only in b (+), {elsewhere} going to another place (~), {same} the same (=)"
        )));
    }
    items.push(L::Text(format!("whole functions: {} | {}", fn_cmd(sa, i), fn_cmd(sb, j))));
    let mut fmt = formatter();
    let (rows, tail) = cut(&items, from, budget, |it| match it {
        L::Text(s) => text_row(s.clone()),
        L::Fold(n) => text_row(format!("  ... {n} same instructions")),
        L::Line(k) => {
            let (m, x, y) = lines[*k];
            let ta = x.map(|x| ins_text(sa, &mut fmt, &ba.items[x]));
            let tb = y.map(|y| ins_text(sb, &mut fmt, &bb.items[y]));
            let ip = |b: &Body, k: Option<usize>| k.map_or(String::new(), |k| format!("{:#010x}", b.items[k].ip));
            let (ia, ib) = (ip(&ba, x), ip(&bb, y));
            let (mark, text) = match m {
                Mark::Same => ("=", tb.clone().unwrap_or_default()),
                Mark::Elsewhere => {
                    ("~", format!("{}  =>  {}", ta.clone().unwrap_or_default(), tb.clone().unwrap_or_default()))
                }
                Mark::Gone => ("-", ta.clone().unwrap_or_default()),
                Mark::New => ("+", tb.clone().unwrap_or_default()),
            };
            Row {
                key: 0,
                text: format!("{mark} {ia:<10}  {ib:<10}  {text}"),
                json: Some(json!({
                    "mark": mark, "a_addr": x.map(|x| ba.items[x].ip), "b_addr": y.map(|y| bb.items[y].ip),
                    "a_text": ta, "b_text": tb,
                })),
            }
        }
    });
    let meta = json!({
        "function": name, "status": status.word(), "by": p.by[i].map(By::word),
        "a_addr": fa.start, "b_addr": fb.start, "a_size": ba.size, "b_size": bb.size,
        "a_insns": ka.len(), "b_insns": kb.len(),
        "removed": gone, "added": new, "elsewhere": elsewhere, "same": same, "aligned": aligned,
    });
    Ok(Output { op: "diff", meta, rows, tail, command })
}

fn ins_text(s: &Side, fmt: &mut IntelFormatter, it: &Item) -> String {
    if it.bad {
        return format!("(bad) {:02x}", it.byte);
    }
    let mut t = String::new();
    fmt.format(&it.ins, &mut t);
    if let Some(c) = comment(s.img, &it.ins) {
        t.push_str("  ; ");
        t.push_str(&c);
    }
    t
}

/// The equal keys of `a` and `b` paired in order (a longest common subsequence), and whether the middle
/// was aligned: past [`MAX_EDITS`] only the common start and end are paired.
fn align(a: &[u64], b: &[u64]) -> (Vec<(usize, usize)>, bool) {
    let pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..].iter().rev().zip(b[pre..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (ma, mb) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let mid = if ma.len() + mb.len() <= MAX_ALIGN { myers(ma, mb, MAX_EDITS) } else { None };
    let aligned = mid.is_some();
    let mut pairs: Vec<(usize, usize)> = (0..pre).map(|k| (k, k)).collect();
    pairs.extend(mid.unwrap_or_default().into_iter().map(|(x, y)| (x + pre, y + pre)));
    pairs.extend((0..suf).map(|k| (a.len() - suf + k, b.len() - suf + k)));
    (pairs, aligned)
}

/// Myers' shortest edit script: the pairs of equal items in order, or `None` past `max` edits.
fn myers(a: &[u64], b: &[u64], max: usize) -> Option<Vec<(usize, usize)>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let limit = (a.len() + b.len()).min(max) as isize;
    let off = limit + 1;
    let mut v = vec![0isize; (2 * limit + 3) as usize];
    // the furthest x on each diagonal after each round, for the walk back
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut end = None;
    'rounds: for d in 0..=limit {
        for k in (-d..=d).step_by(2) {
            let at = |k: isize| v[(off + k) as usize];
            let down = k == -d || (k != d && at(k - 1) < at(k + 1));
            let mut x = if down { at(k + 1) } else { at(k - 1) + 1 };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[(off + k) as usize] = x;
            if x >= n && y >= m {
                end = Some(d);
                trace.push(v[(off - d) as usize..=(off + d) as usize].to_vec());
                break 'rounds;
            }
        }
        trace.push(v[(off - d) as usize..=(off + d) as usize].to_vec());
    }
    let last = end?;
    let (mut x, mut y) = (n, m);
    let mut pairs = Vec::new();
    for d in (1..=last).rev() {
        let prev = &trace[(d - 1) as usize];
        let at = |k: isize| prev[(k + d - 1) as usize];
        let k = x - y;
        let down = k == -d || (k != d && at(k - 1) < at(k + 1));
        let pk = if down { k + 1 } else { k - 1 };
        let px = at(pk);
        let py = px - pk;
        let (sx, sy) = if down { (px, py + 1) } else { (px + 1, py) };
        while x > sx && y > sy {
            x -= 1;
            y -= 1;
            pairs.push((x as usize, y as usize));
        }
        (x, y) = (px, py);
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        pairs.push((x as usize, y as usize));
    }
    pairs.reverse();
    Some(pairs)
}

#[cfg(test)]
mod tests {
    use super::myers;

    #[test]
    fn myers_pairs_the_longest_common_run_of_keys() {
        let a = [1, 2, 3, 4, 5, 6];
        let b = [1, 9, 3, 4, 7, 5, 6];
        let p = myers(&a, &b, 100).unwrap();
        assert_eq!(p, vec![(0, 0), (2, 2), (3, 3), (4, 5), (5, 6)]);
        assert_eq!(myers(&[], &[], 10).unwrap(), vec![]);
        assert_eq!(myers(&[1, 2], &[], 10).unwrap(), vec![]);
        // more edits than allowed: no answer rather than a wrong one
        assert!(myers(&[1, 2, 3, 4], &[5, 6, 7, 8], 3).is_none());
    }
}
