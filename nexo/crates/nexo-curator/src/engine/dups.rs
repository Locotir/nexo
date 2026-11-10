//! Runs of lines that an earlier output holds too, and the pointers that stand in for them.

use serde_json::Value;

use crate::hindsight::UKind;
use crate::json::est_str;
use crate::mechanical::MARK;
use crate::msg::{get_str, key_of};
use crate::textops::{self};

use super::files::command_of;
use super::Engine;

/// Places a window of output lines is remembered at, the latest ones: a line every output repeats points
/// to the nearest.
const RUN_SOURCES: usize = 32;

/// Lines of a tool output that an earlier output holds too (`Params.dedup`): `stub`, a line that points to
/// them there, goes in their place while that output is whole in the view.
#[derive(Clone, Debug)]
pub(super) struct Dup {
    /// The tool result and its lines `a..b`.
    pub(super) unit: usize,
    pub(super) lines: (usize, usize),
    /// The earlier tool result that holds them, from its line `at`.
    pub(super) src: usize,
    pub(super) at: usize,
    pub(super) stub: String,
}

/// How many lines run `d` stands for.
pub(super) fn len_of(d: &Dup) -> usize {
    d.lines.1 - d.lines.0
}

/// The line ids a pointer stands for.
fn shown_ids<'a>(ids: &'a [u32], d: &Dup) -> impl Iterator<Item = u32> + 'a {
    ids[d.lines.0..d.lines.1].iter().copied()
}

/// The line a run of `n` lines goes as: the same as lines `j + 1..=j + n` of the output of call `label`.
pub(super) fn pointer(n: usize, j: usize, label: &str, first: &str, last: &str) -> String {
    format!(
        "{MARK} {n} lines left out here: the same as lines {}-{} of the output of {label} above, from \"{}\" to \"{}\"",
        j + 1,
        j + n,
        clipped(first, 32),
        clipped(last, 32)
    )
}

/// Lines `a..b` of `lines` onto `out`, a run of `runs` among them as its stub, once (`put` holds the runs
/// whose stub is out already).
pub(super) fn put_lines(
    lines: &[&str],
    (a, b): (usize, usize),
    runs: &[&Dup],
    put: &mut Vec<usize>,
    out: &mut Vec<String>,
) {
    let mut k = a;
    while k < b {
        match runs.iter().position(|d| d.lines.0 <= k && k < d.lines.1) {
            Some(r) => {
                if !put.contains(&r) {
                    put.push(r);
                    out.push(runs[r].stub.clone());
                }
                k = runs[r].lines.1;
            }
            None => {
                out.push(lines[k].to_string());
                k += 1;
            }
        }
    }
}

/// Whether a tool result holds only text: a pointer stands for lines of text, never for an image.
fn text_only(b: &Value) -> bool {
    match b.get("content") {
        Some(Value::Array(parts)) => parts.iter().all(|p| p.get("type").and_then(Value::as_str) == Some("text")),
        _ => true,
    }
}

pub(super) fn clipped(s: &str, n: usize) -> String {
    if textops::len(s) > n {
        format!("{}…", textops::head(s, n))
    } else {
        s.to_string()
    }
}

/// The key of a window of lines (its first three; unused places are `u32::MAX`).
fn window(ids: &[u32]) -> [u32; 3] {
    let mut w = [u32::MAX; 3];
    for (x, &id) in w.iter_mut().zip(ids) {
        *x = id;
    }
    w
}

impl Engine {
    /// The runs of the tool results that just arrived (units `new`) that an earlier result, whole in the
    /// view, holds too: each goes as a pointer to it. Decided once, as the message is born, so the message
    /// is sent the same way on every later request (the cache keeps it) for as long as that result stays.
    /// Then the lines the new results send become places later runs can point to.
    pub(super) fn find_dups(&mut self, new: std::ops::Range<usize>) {
        let least = self.p.dedup as usize;
        let w = least.min(3);
        for i in new {
            let u = &self.tr.units[i];
            if u.kind != UKind::Result || !text_only(self.tr.block(i)) {
                continue;
            }
            let mi = u.msg;
            let text = self.tr.unit_text(i).into_owned();
            let lines: Vec<&str> = text.split('\n').collect();
            let ids: Vec<u32> = lines.iter().map(|l| self.line_id(l.trim_end())).collect();
            let mut found: Vec<Dup> = Vec::new();
            let mut saved = 0;
            let mut a = 0;
            while a + w <= ids.len() {
                // the longest run from here that an earlier result came with too, the latest: whether it still sends
                // the lines is decided at each request (`dups_on`)
                let mut best = (0, 0, 0);
                for &(s, j) in self.runs_at.get(&window(&ids[a..a + w])).into_iter().flatten().rev() {
                    if self.tr.units[s].msg >= mi {
                        continue;
                    }
                    let src = &self.shown[&s];
                    let n =
                        (0..).take_while(|&n| a + n < ids.len() && src.get(j + n) == Some(&Some(ids[a + n]))).count();
                    if n > best.2 {
                        best = (s, j, n);
                    }
                }
                let (s, j, n) = best;
                if n >= least {
                    let first = lines[a..a + n].iter().find(|l| !l.trim().is_empty()).map_or("", |l| l.trim());
                    let last = lines[a..a + n].iter().rev().find(|l| !l.trim().is_empty()).map_or("", |l| l.trim());
                    let stub = pointer(n, j, &self.call_label(s), first, last);
                    let (run, cost) = (est_str(&lines[a..a + n].join("\n")), est_str(&stub));
                    // a pointer is worth it when it is half the lines it stands for or less: below that it saves little and
                    // sends the reader up for them
                    if run >= 2 * cost {
                        saved += run - cost;
                        found.push(Dup { unit: i, lines: (a, a + n), src: s, at: j, stub });
                        a += n;
                        continue;
                    }
                }
                a += 1;
            }
            // an output that is mostly lines already in the view is one the agent asked for again: it did not find or
            // trust the copy above, and a pointer back to it would only send it round once more
            // (with `asked=2` only once a pointer was sent for most of those lines: the first copy asked again
            // goes as a pointer, and the agent asking past it says the pointer did not serve)
            let again = |d: &Dup| shown_ids(&ids, d).filter(|x| self.pointed_lines.contains(x)).count();
            let mostly = 2 * found.iter().map(len_of).sum::<usize>() >= lines.len();
            // (with `asked=3` an output an earlier call gave, line for line, goes as one line that says so, whichever call
            // gave it: the copy above is whole, and when the same call gave it, running it again changed nothing; once
            // that copy leaves the view this one goes whole again, as every pointer does)
            let same = self.p.asked == 3
                && found.len() == 1
                && found[0].lines == (0, lines.len())
                && found[0].at == 0
                && self.shown.get(&found[0].src).map(Vec::len) == Some(lines.len());
            let same_call = same && self.call_label(found[0].src) == self.call_label(i);
            if same {
                let (n, src) = (lines.len(), self.call_label(found[0].src));
                found[0].stub = if same_call {
                    format!("{MARK} the same {n} lines as {src} gave above: nothing changed")
                } else {
                    format!("{MARK} the same {n} lines as {src} gave above")
                };
            }
            let refused = !same
                && (self.p.asked != 2 || 2 * found.iter().map(again).sum::<usize>() >= found.iter().map(len_of).sum());
            if self.p.asked != 0 && mostly && refused {
                if !found.is_empty() {
                    self.bump("dedup_asked", 1);
                }
                found.clear();
            } else if !found.is_empty() {
                self.bump("dedup_runs", found.len() as i64);
                self.bump("dedup_saved", saved);
                for d in &found {
                    self.pointed_lines.extend(shown_ids(&ids, d));
                }
            }
            let mut shown: Vec<Option<u32>> = ids.into_iter().map(Some).collect();
            for d in &found {
                shown[d.lines.0..d.lines.1].iter_mut().for_each(|x| *x = None);
            }
            for k in 0..(shown.len() + 1).saturating_sub(w) {
                let win: Option<Vec<u32>> = shown[k..k + w].iter().copied().collect();
                if let Some(win) = win {
                    let at = self.runs_at.entry(window(&win)).or_default();
                    if at.len() == RUN_SOURCES {
                        at.remove(0);
                    }
                    at.push((i, k));
                }
            }
            self.shown.insert(i, shown);
            if !found.is_empty() {
                let ds = self.dups.entry(mi).or_default();
                for d in found {
                    self.copies.entry(d.src).or_default().push((mi, ds.len()));
                    ds.push(d);
                }
            }
        }
    }

    fn line_id(&mut self, line: &str) -> u32 {
        if let Some(&id) = self.line_ids.get(line) {
            return id;
        }
        let id = self.line_ids.len() as u32;
        self.line_ids.insert(line.to_string(), id);
        id
    }

    /// The call whose output is tool result `i`, as a pointer to that output names it: its command, or its
    /// tool and arguments, on one line.
    fn call_label(&self, i: usize) -> String {
        let u = &self.tr.units[i];
        let id = key_of(self.tr.block(i).get("tool_use_id"));
        let call = (u.msg.saturating_sub(3)..u.msg)
            .rev()
            .flat_map(|m| self.fz.by_msg(m).iter().copied())
            .find(|&c| self.tr.units[c].kind == UKind::Call && key_of(self.tr.block(c).get("id")) == id);
        let Some(c) = call else { return format!("the {} call", u.tool) };
        let b = self.tr.block(c);
        let command = command_of(b);
        let text = if command.is_empty() {
            format!("{} {}", get_str(b, "name"), b.get("input").map_or(String::new(), Value::to_string))
        } else {
            command
        };
        format!("`{}`", clipped(&textops::squash_ws(&text), 100))
    }

    /// The lines of tool result `i` the view sends: none when its message is not visible, else all but those
    /// of its cut parts.
    fn sent_lines(&self, i: usize, vis: &[usize]) -> Vec<bool> {
        let n = self.shown.get(&i).map_or(0, Vec::len);
        if vis.binary_search(&self.tr.units[i].msg).is_err() {
            return vec![false; n];
        }
        let mut sent = vec![true; n];
        if let Some(drop) = self.dropped.get(&i) {
            let at = |x: i64| (if x < 0 { (n as i64 + x).max(0) } else { x.min(n as i64) }) as usize;
            for (_, c) in self.fz.chunks[i].iter().enumerate().filter(|(j, _)| drop.contains(j)) {
                sent[at(c.start)..at(c.end).max(at(c.start))].iter_mut().for_each(|s| *s = false);
            }
        }
        sent
    }

    /// Whether tool result `u`, from its line `x`, sends every line of run `d` that the run's own message
    /// sends (`mine`): then the run can point there and the person's view loses nothing.
    fn covers(&self, d: &Dup, mine: &[bool], (u, x): (usize, usize), vis: &[usize]) -> bool {
        let theirs = self.sent_lines(u, vis);
        (0..len_of(d)).all(|k| !mine[d.lines.0 + k] || theirs.get(x + k) == Some(&true))
    }

    /// Whether the pointers of message `mi`, the output that went whole as the latest one (`Params.fresh`), pay for
    /// the cache they break: keeping the tokens they save costs `ratio` of each on every call still to come
    /// (`calls_ahead`, not `horizon`: the message a swap rewrites is the newest one, the last the window
    /// takes, so the room left before the cap does not bound the calls it is sent for); sending the message again, as
    /// it will be, without the cache costs the rest (the messages after it are new either way). Always with `ahead=0`, the fixed rule the price replaces.
    pub(super) fn swap_pays(&mut self, mi: usize) -> bool {
        if self.p.ahead <= 0.0 {
            return true;
        }
        let after = self.message(mi).1;
        self.fresh_wait.insert(mi);
        let before = self.message(mi).1;
        self.fresh_wait.remove(&mi);
        let r = self.p.ratio;
        r * self.calls_ahead(self.tr.steps.len()) * (before - after) as f64 > self.p.margin * (1.0 - r) * after as f64
    }

    /// Where each run of message `mi` points now, as (tool result, line): to its earlier output while that
    /// output sends its lines, else to the first copy of them that does (`carrier`). `None`: the run is sent
    /// as it came.
    pub(super) fn dups_on(&mut self, mi: usize) -> Vec<Option<(usize, usize)>> {
        let Some(n) = self.dups.get(&mi).map(Vec::len) else { return Vec::new() };
        if self.p.fresh != 0 && (mi + 1 == self.tr.msgs.len() || self.fresh_wait.contains(&mi)) {
            return vec![None; n];
        }
        let vis = self.visible();
        let ds = &self.dups[&mi];
        ds.iter()
            .enumerate()
            .map(|(k, d)| {
                let mine = self.sent_lines(d.unit, &vis);
                if self.covers(d, &mine, (d.src, d.at), &vis) {
                    Some((d.src, d.at))
                } else {
                    self.holder(d, &mine, &vis).or_else(|| self.carrier(d, &mine, (mi, k), &vis))
                }
            })
            .collect()
    }

    /// The first earlier tool result that came with the lines of run `d` itself, in the same order, and
    /// still sends every one of them its message sends (`mine`): a read made again after its first output
    /// lost some chunks, by the same or another command, then holds them. Found by content, not by command.
    fn holder(&self, d: &Dup, mine: &[bool], vis: &[usize]) -> Option<(usize, usize)> {
        let w = (self.p.dedup as usize).min(3);
        let ids: Vec<u32> = self.shown[&d.src][d.at..d.at + len_of(d)].iter().map(|x| x.unwrap_or(u32::MAX)).collect();
        let me = self.tr.units[d.unit].msg;
        let mut found: Vec<(usize, usize)> = self
            .runs_at
            .get(&window(&ids[..w.min(ids.len())]))
            .into_iter()
            .flatten()
            .copied()
            .filter(|&(s, j)| {
                self.tr.units[s].msg < me
                    && self.shown[&s]
                        .get(j..j + ids.len())
                        .is_some_and(|l| l.iter().zip(&ids).all(|(a, b)| *a == Some(*b)))
                    && self.covers(d, mine, (s, j), vis)
            })
            .collect();
        found.sort_unstable();
        found.first().copied()
    }

    /// Where run `d` (`at` in `dups`) points when the output it points to no longer sends its lines: the
    /// copies of that output before it go in full in order, each one but those an earlier one in full or the
    /// output itself covers, and the first of them that covers it carries its lines. Lines read many times
    /// then go once after their first output leaves, not once per copy.
    fn carrier(&self, d: &Dup, mine: &[bool], at: (usize, usize), vis: &[usize]) -> Option<(usize, usize)> {
        let within = |f: &Dup, e: &Dup| f.unit != e.unit && f.at <= e.at && e.at + len_of(e) <= f.at + len_of(f);
        let line = |f: &Dup, e: &Dup| (f.unit, f.lines.0 + e.at - f.at);
        let mut full: Vec<&Dup> = Vec::new();
        for &(m, k) in &self.copies[&d.src] {
            if (m, k) == at {
                break;
            }
            // a copy out of the view sends nothing, so it carries nothing: no need to weigh its lines
            if vis.binary_search(&m).is_err() {
                continue;
            }
            let e = &self.dups[&m][k];
            let theirs = self.sent_lines(e.unit, vis);
            let carried = !theirs[e.lines.0..e.lines.1].contains(&true)
                || self.covers(e, &theirs, (e.src, e.at), vis)
                || full.iter().any(|f| within(f, e) && self.covers(e, &theirs, line(f, e), vis));
            if !carried {
                full.push(e);
            }
        }
        full.iter().find(|f| within(f, d) && self.covers(d, mine, line(f, d), vis)).map(|f| line(f, d))
    }

    /// The pointer run `d` goes as when it points to line `x` of tool result `u`.
    pub(super) fn stub_at(&self, d: &Dup, (u, x): (usize, usize)) -> String {
        if (u, x) == (d.src, d.at) {
            return d.stub.clone();
        }
        let text = self.tr.unit_text(d.unit);
        let run: Vec<&str> = text.split('\n').skip(d.lines.0).take(len_of(d)).collect();
        let first = run.iter().find(|l| !l.trim().is_empty()).map_or("", |l| l.trim());
        let last = run.iter().rev().find(|l| !l.trim().is_empty()).map_or("", |l| l.trim());
        pointer(len_of(d), x, &self.call_label(u), first, last)
    }
}
