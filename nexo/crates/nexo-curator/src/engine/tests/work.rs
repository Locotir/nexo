//! How the curator's work grows with the history, counted (`crate::work`), not timed; and the indexes that keep
//! it from growing faster, checked against the scans they replace.

use super::*;
use crate::work::{take, Work, K};
use crate::Curator;

#[path = "../../../tests/support/perf_gen.rs"]
mod perf_gen;

use perf_gen::{history, Lcg, Shape};

/// A history of `n` messages: tool outputs of 2 to 16 KB, edits of several files under other spellings.
fn session(n: usize) -> Vec<Value> {
    history(&Shape { files: 24, ..Shape::default() }, n)
}

/// The settings of the two runs: no window, and a window of 32,000 tokens with epochs on (it takes `window`).
fn curator(window: u64) -> Curator {
    let spec = if window == 0 { BASE.to_string() } else { format!("{BASE};epoch=1") };
    let mut c = Curator::new(&spec).unwrap();
    c.window(window);
    c
}

/// How much more work 4n messages may take than n: linear is 4, and the log factor and a margin are allowed;
/// quadratic is 16.
fn bound(n: usize) -> f64 {
    4.0 * ((4 * n) as f64).log2() / (n as f64).log2() * 1.25
}

/// Every work counter at about 4n messages (`big`) is at most `bound(n)` times what it was at n (`small`), plus 64
/// for the constants of small counts; one with an event counter (`Work::per`) is compared per event. The ones of
/// `must` have to be counted at n; the ones of `pinned` are quadratic and kept so: at most 16 · 1.25 times.
/// A counter that is zero at n is skipped, and said so.
fn assert_grows_at_most(what: &str, n: usize, small: &[u64; K], big: &[u64; K], must: &[Work], pinned: &[Work]) {
    for w in Work::ALL {
        if w.is_event() {
            continue;
        }
        if small[w as usize] == 0 {
            assert!(!must.contains(&w), "{what}: {w:?} is not counted at {n} messages");
            eprintln!("{what}: {w:?} is not counted at {n} messages ({} at 4n), skipped", big[w as usize]);
            continue;
        }
        let at = |c: &[u64; K]| {
            let x = c[w as usize] as f64;
            w.per().map_or(x, |e| x / c[e as usize].max(1) as f64)
        };
        let (s, b) = (at(small), at(big));
        let limit = if pinned.contains(&w) { 16.0 * 1.25 } else { bound(n) };
        assert!(
            b <= limit * s + 64.0,
            "{what}: {w:?} grows from {s:.1} at {n} messages to {b:.1} at 4n, more than {limit:.2} times"
        );
    }
}

/// The history up to `len` messages as a curator is asked for views, one more view every 16 messages.
struct Feed<'a> {
    cur: Curator,
    msgs: &'a [Value],
    at: usize,
}

impl Feed<'_> {
    fn view(&mut self, len: usize) {
        self.cur.view(0, &self.msgs[..len], len as f64 * 10.0).unwrap();
        // every edit is indexed after a view: were the index left behind, each lookup would fall back to a scan
        assert_eq!(
            self.cur.eng.edit_index.at(),
            self.cur.eng.edits.len(),
            "the edit index is behind at {len} messages"
        );
        self.at = len;
    }

    /// Views up to `len` messages (as the agent makes one per model call, sampled), then the work of the next
    /// `VIEWS` steps of two messages, one view each: a single view may or may not fall on an epoch try or a
    /// page, so many of them are summed.
    fn measure(&mut self, len: usize) -> [u64; K] {
        const VIEWS: usize = 32;
        while self.at + 16 <= len {
            self.view(self.at + 16);
        }
        self.view(len);
        take();
        for k in 1..=VIEWS {
            self.view(len + 2 * k);
        }
        take()
    }
}

#[test]
fn work_per_view_grows_at_most_n_log_n() {
    // 401 and 1601 are 1 modulo 16: the views fall on the checkpoints
    let (n, len) = (401, 1601 + 2 * 32);
    let msgs = session(len + 1);
    for window in [0, 32_000] {
        let mut feed = Feed { cur: curator(window), msgs: &msgs, at: 1 };
        let small = feed.measure(n);
        let big = feed.measure(4 * n - 3);
        // a turn is scored against every turn before it (kept quadratic: perf SPEC 4.3), and one opens in each of the
        // sampled stretches: pinned
        // a window of 32,000 tokens makes lookups of stale reads on its own (the cuts), the other config only through `drop_one`
        let must: &[Work] = if window == 0 {
            &[Work::EditCmp, Work::VisibleScan]
        } else {
            &[Work::EditCmp, Work::VisibleScan, Work::StaleCmp]
        };
        assert_grows_at_most(&format!("window {window}, per view"), n, &small, &big, must, &[Work::TurnScored]);
    }
}

#[test]
fn work_whole_ingest_grows_at_most_n_log_n() {
    // one view over the whole history: what a rewrite, a compaction or a failed restore costs
    let n = 400;
    let msgs = session(4 * n + 1);
    for window in [0, 32_000] {
        let ingest = |len: usize| {
            take();
            let mut c = curator(window);
            c.view(0, &msgs[..len], 0.0).unwrap();
            assert_eq!(c.eng.edit_index.at(), c.eng.edits.len(), "the edit index is behind");
            take()
        };
        let (small, big) = (ingest(n + 1), ingest(4 * n + 1));
        // the score of a turn looks at every turn (`Threads::open`): quadratic, and kept so (perf SPEC 4.3);
        // pinned, so that it cannot get worse
        assert_grows_at_most(
            &format!("window {window}, whole ingest"),
            n,
            &small,
            &big,
            &[Work::EditCmp, Work::VisibleScan],
            &[Work::TurnScored],
        );
    }
}

/// The reference for `edit_ok`: every result whose call made an edit, with whether it came back without error.
fn edit_ok_by_scan(e: &Engine) -> FxHashMap<String, bool> {
    let mut out = FxHashMap::default();
    for (i, u) in e.tr.units.iter().enumerate() {
        if u.kind != UKind::Result {
            continue;
        }
        let id = crate::msg::key_of(e.tr.block(i).get("tool_use_id"));
        if e.edits.iter().any(|x| x.2 == id) {
            out.insert(id, !e.fz.info.get(&i).is_some_and(|x| x.error));
        }
    }
    out
}

fn assert_edit_caches(e: &Engine, what: &str) {
    let ids: FxHashSet<String> = e.edits.iter().map(|x| x.2.clone()).collect();
    assert_eq!((*e.edit_ids).0, e.edits.len(), "{what}: edits covered");
    assert!(e.edit_ids.1 == ids, "{what}: ids");
    assert_eq!(e.edit_index.at(), e.edits.len(), "{what}: indexed");
    assert!(e.edit_ok == edit_ok_by_scan(e), "{what}: edit_ok");
}

#[test]
fn edit_ids_match_the_scan() {
    let msgs = session(401);
    let mut e = Engine::new(params("stale=1;eoq=0;threshold=0").unwrap());
    let mut saved = false;
    for len in (1..=msgs.len()).step_by(20) {
        e.sync(&msgs[..len]);
        assert_edit_caches(&e, &format!("sync {len}"));
        if len > 200 && !saved {
            // a state read back from a snapshot has edits and no caches until `take_history`
            saved = true;
            let bytes = crate::snapshot::options().serialize(&e).unwrap();
            let born = e.born_messages();
            let mut back: Engine = crate::snapshot::options().deserialize(&bytes).unwrap();
            assert_eq!((*back.edit_ids).0, 0);
            back.take_history(&msgs[..len], &born).unwrap();
            assert_edit_caches(&back, "restored");
            assert_eq!(back.edits, e.edits);
            e = back;
        }
    }
    assert!(!e.edits.is_empty() && e.edit_ok.values().any(|ok| !ok), "the session has edits, some failed");
    // a rewritten history starts the engine over
    let mut other = msgs[..100].to_vec();
    other.push(json!({"role": "assistant", "content": [{"type": "text", "text": "a different answer"}]}));
    assert!(e.sync(&other));
    assert_edit_caches(&e, "rewritten");
}

/// A session of reads of every spelling in `PATHS` (by a read tool and by `cat`), and debugger looks.
fn reads_of_every_spelling() -> Vec<Value> {
    let mut msgs = vec![json!({"role": "user", "content": "Look at the files."})];
    let mut k = 0;
    let mut step = |name: &str, input: Value, error: bool| {
        k += 1;
        msgs.extend(tool(k, name, input, &format!("output {k}"), error));
    };
    for p in PATHS.iter().filter(|p| !p.is_empty() && !p.starts_with("debug:")) {
        step("read_file", json!({"path": p}), false);
        step("bash", json!({"command": format!("cat {p}")}), false);
    }
    step("read_file", json!({"path": "a/b.rs"}), true);
    for command in ["regs", "-s S1 regs", "-s S2 regs"] {
        step("debug", json!({"command": command}), false);
    }
    step("debug", json!({"command": "bt"}), false);
    msgs
}

/// The paths of edits and reads: the spellings that name `a/b.rs`, a shorter name, a longer one, and the odd ones.
const PATHS: [&str; 15] = [
    "a/b.rs",
    "./a/b.rs",
    "a\\b.rs",
    "C:\\a\\b.rs",
    "b.rs",
    "/work/a/b.rs",
    "x/a/b.rs",
    "a/",
    "/",
    "",
    "a/c.rs",
    "debug:",
    "debug:S1",
    "debug:S2",
    "c/a/",
];

#[test]
fn stale_index_matches_the_scan() {
    let msgs = reads_of_every_spelling();
    let mut e = Engine::new(params("stale=1;eoq=0;threshold=0").unwrap());
    e.sync(&msgs);
    assert!(e.edits.is_empty());
    let mut rng = Lcg::new(7);
    let (mut found, mut none) = (0, 0);
    for case in 0..500 {
        let count = rng.below(301);
        let mut mis: Vec<usize> = (0..count).map(|_| rng.below(msgs.len())).collect();
        if case % 2 == 0 {
            // the order a real session pushes them in
            mis.sort_unstable();
        }
        e.edits = mis
            .into_iter()
            .enumerate()
            .map(|(k, mi)| (PATHS[rng.below(PATHS.len())].to_string(), mi, format!("c{}", k % 60)))
            .collect();
        e.edit_ok = FxHashMap::default();
        for k in 0..60 {
            match rng.below(4) {
                0 => {}
                v => {
                    e.edit_ok.insert(format!("c{k}"), v != 1);
                }
            }
        }
        e.edit_ids = Default::default();
        e.edit_index = Default::default();
        // not covered yet: the search goes over every edit, and says what the index must say
        for i in 0..e.tr.units.len() {
            for shell in [false, true] {
                assert_eq!(e.read_superseded(i, shell), e.read_superseded_linear(i, shell), "unindexed, case {case}");
            }
        }
        super::super::edit_index::sync_edit_caches(&mut e.edit_ids, &mut e.edit_index, &e.edits);
        assert_eq!(e.edit_index.at(), e.edits.len());
        for i in 0..e.tr.units.len() {
            for shell in [false, true] {
                let (fast, scan) = (e.read_superseded(i, shell), e.read_superseded_linear(i, shell));
                assert_eq!(fast, scan, "unit {i}, shell {shell}, case {case}, {} edits", e.edits.len());
                if fast.is_some() {
                    found += 1;
                } else {
                    none += 1;
                }
            }
        }
    }
    assert!(found > 1000 && none > 1000, "the cases tell both answers apart: {found} found, {none} none");
}

#[test]
fn stale_reads_match_the_scan_on_a_generated_session() {
    let msgs = session(601);
    let mut e = Engine::new(params("stale=1;eoq=0;threshold=0").unwrap());
    e.sync(&msgs);
    assert!(e.edits.len() > 20);
    assert_eq!(e.edit_index.at(), e.edits.len());
    let vis: Vec<usize> = (0..msgs.len()).collect();
    let by_scan: FxHashSet<usize> = vis
        .iter()
        .flat_map(|&mi| e.fz.by_msg(mi))
        .copied()
        .filter(|&i| e.read_superseded_linear(i, false).is_some())
        .collect();
    assert!(!by_scan.is_empty());
    assert!(e.stale_reads(&vis) == by_scan);
}

/// Wall clock, only when asked: `cargo test --release -p nexo-curator work_wall_clock -- --ignored --nocapture`.
/// Its only assertion is a loose slope between 10k and 50k messages.
#[test]
#[ignore]
fn work_wall_clock_1k_10k_50k() {
    let mut ms = Vec::new();
    for n in [1_000, 10_000, 50_000] {
        let msgs = session(n + 1);
        let t = std::time::Instant::now();
        curator(128_000).view(0, &msgs, 0.0).unwrap();
        ms.push(t.elapsed().as_secs_f64() * 1e3);
        eprintln!("whole ingest of {n} messages: {:.0} ms", ms[ms.len() - 1]);
    }
    assert!(ms[2] / ms[1] <= 5.0 * 2.0, "{:.0} ms at 50k against {:.0} ms at 10k", ms[2], ms[1]);
}
