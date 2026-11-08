//! The word index on a session of 2M tokens: linear to build, microseconds to look up.

use std::time::Instant;

use nexo_tui::complete::{Src, WordIndex, CAP, PUMP, SCAN_CAP};

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

fn pseudo(r: &mut Lcg, len: usize) -> String {
    const SYL: [&str; 12] = ["con", "fig", "ura", "ción", "dep", "loy", "men", "ta", "ser", "ver", "ing", "pro"];
    let mut s = String::new();
    while s.chars().count() < len {
        s.push_str(SYL[(r.next() % 12) as usize]);
    }
    s
}

/// ~`bytes` of text: sentences from a vocabulary, random letter words, hashes and paths.
fn text(r: &mut Lcg, bytes: usize, vocab: &[String]) -> String {
    let mut s = String::with_capacity(bytes + 256);
    while s.len() < bytes {
        match r.next() % 10 {
            0 => {
                for _ in 0..8 {
                    s.push_str(&format!("{:016x}", r.next() ^ (r.next() << 24)));
                }
                s.push('\n');
            }
            1 => s.push_str(&format!(
                "crates/{}/src/{}.rs:{} ",
                vocab[(r.next() % vocab.len() as u64) as usize],
                vocab[(r.next() % vocab.len() as u64) as usize],
                r.next() % 900
            )),
            2 | 3 => {
                let n = 5 + (r.next() % 6) as usize;
                s.push_str(&(0..n).map(|_| (b'a' + (r.next() % 26) as u8) as char).collect::<String>());
                s.push(' ');
            }
            _ => {
                s.push_str(&vocab[(r.next() % vocab.len() as u64) as usize]);
                s.push(if matches!(r.next() % 12, 0) { '\n' } else { ' ' });
            }
        }
    }
    s
}

/// Feeds `slices` blocks of ~1 MB into one index and times each slice. Linear work costs about the
/// same per slice; work that re-reads what it already has costs more with every slice.
fn build(r: &mut Lcg, vocab: &[String], slices: usize) -> (WordIndex, u64, Vec<f64>) {
    let mut w = WordIndex::new(true);
    let mut fed = 0u64;
    let mut times = Vec::new();
    for _ in 0..slices {
        let t = Instant::now();
        let mut slice = 0usize;
        while slice < 1 << 20 {
            let block = text(r, 48 * 1024, vocab);
            slice += block.len();
            fed += block.len() as u64;
            w.feed(&block, if matches!(fed % 20, 0) { Src::User } else { Src::Other });
        }
        w.flush();
        times.push(t.elapsed().as_secs_f64());
    }
    (w, fed, times)
}

fn best(times: &[f64]) -> f64 {
    times.iter().copied().fold(f64::MAX, f64::min)
}

/// How much slower the late slices are than the early ones, by their fastest: a busy machine only
/// makes samples slower, so the minimum of each half is the part of the time that is the work.
fn growth(times: &[f64]) -> f64 {
    let (early, late) = times.split_at(times.len() / 2);
    best(late) / best(early)
}

#[test]
fn eight_megabytes_index_in_linear_time_and_look_up_fast() {
    let mut r = Lcg(42);
    let vocab: Vec<String> = (0..30_000).map(|i| format!("{}{}", pseudo(&mut r, 5 + i % 6), i % 97)).collect();

    // No wall-clock bound: the cost of a slice is compared with the cost of the first ones, and the
    // best of up to three runs counts (load can only add time; quadratic work fails every run).
    let (mut w, mut fed, mut times, mut ratio) = (WordIndex::new(true), 0, Vec::new(), f64::MAX);
    for _ in 0..3 {
        let (w1, fed1, t1) = build(&mut r, &vocab, 8);
        let g = growth(&t1);
        eprintln!(
            "build: slices {:?} s, late/early {g:.2}",
            t1.iter().map(|t| (t * 100.0).round() / 100.0).collect::<Vec<_>>()
        );
        if g < ratio {
            (w, fed, times, ratio) = (w1, fed1, t1, g);
        }
        if ratio <= 3.0 {
            break;
        }
    }
    assert_eq!(w.stats.bytes_scanned, fed, "every byte is scanned exactly once");
    assert!(ratio <= 3.0, "build is linear: the late megabytes cost {ratio:.1}x the early ones");
    assert!(w.len() <= CAP + CAP / 10, "the cap holds: {}", w.len());
    assert!(w.len() > CAP / 2, "and the index is big: {}", w.len());
    // a megabyte of text costs this much; a lookup is measured against it, not against the clock
    let unit = best(&times);

    // lookups: random 3-letter prefixes of words, hashes and letters
    let mut prefixes: Vec<String> = Vec::new();
    for _ in 0..2000 {
        let p = match r.next() % 3 {
            0 => vocab[(r.next() % vocab.len() as u64) as usize].chars().take(3).collect(),
            1 => format!("{:03x}", r.next() % 4096),
            _ => (0..3).map(|_| (b'a' + (r.next() % 26) as u8) as char).collect(),
        };
        prefixes.push(p);
    }
    let (mut worst, mut hits) = (0.0f64, 0);
    let mut times: Vec<f64> = Vec::new();
    let all = Instant::now();
    let looks0 = w.stats.lookups.get();
    for p in &prefixes {
        // the best of three: a busy machine must not fail a bound on the work itself
        let mut best = f64::MAX;
        for _ in 0..3 {
            let t = Instant::now();
            let got = w.suggest(p).is_some();
            best = best.min(t.elapsed().as_secs_f64());
            let _ = got;
        }
        if w.suggest(p).is_some() {
            hits += 1;
        }
        worst = worst.max(best);
        times.push(best);
    }
    let all = all.elapsed().as_secs_f64() * 1000.0;
    let (looks, scanned) = (w.stats.lookups.get(), w.stats.lookups_scanned.get());
    eprintln!(
        "lookups: {} done, {hits} hits, worst {:.3} ms, mean {:.4} ms, scanned/lookup {:.0}, words {}",
        prefixes.len(),
        worst * 1000.0,
        all / (w.stats.lookups.get() - looks0).max(1) as f64,
        scanned as f64 / looks.max(1) as f64,
        w.len()
    );
    times.sort_by(|a, b| a.total_cmp(b));
    let p99 = times[times.len() * 99 / 100];
    eprintln!("p99 {:.3} ms, indexing a megabyte {:.3} s", p99 * 1000.0, unit);
    // against the same machine's cost of indexing one megabyte: a lookup is 1/50 of it at p99
    // and the single worst (a busy machine spikes it) a fifth
    assert!(p99 < unit / 50.0, "p99 lookup took {:.3} ms against {unit:.3} s per megabyte", p99 * 1000.0);
    assert!(worst < unit / 5.0, "a lookup took {:.3} ms against {unit:.3} s per megabyte", worst * 1000.0);
    assert!(scanned <= looks * 3 * SCAN_CAP as u64, "bounded scans");
}

#[test]
fn pump_never_does_more_than_its_budget() {
    let mut r = Lcg(7);
    let vocab: Vec<String> = (0..2000).map(|_| pseudo(&mut r, 6)).collect();
    let mut w = WordIndex::new(true);
    let mut total = 0u64;
    for _ in 0..6 {
        let t = text(&mut r, 900 * 1024, &vocab);
        total += t.len() as u64;
        w.feed(&t, Src::Other);
    }
    assert!(w.pending() > 0, "big blocks wait for the frames");
    let mut calls = 0;
    loop {
        let before = w.stats.bytes_scanned;
        let done = w.pump(PUMP);
        assert!(done <= PUMP, "{done} > {PUMP}");
        assert_eq!(w.stats.bytes_scanned - before, done as u64);
        if done == 0 {
            break;
        }
        calls += 1;
    }
    assert_eq!(w.stats.bytes_scanned, total);
    assert!(calls >= (total as usize / PUMP), "{calls} frames");
}
