//! The Polyglot section of a summary: pass@1 and pass@2 per language and in total, what they cost, how this
//! run differs from aider's protocol, and the command that repeats it.

use crate::run::Record;

/// Exercises per language in the benchmark at the pinned commit (Java: 47, not run).
pub const FULL_SET: [(&str, usize); 5] = [("python", 34), ("rust", 30), ("go", 39), ("cpp", 26), ("javascript", 49)];

/// How this bench differs from the protocol of aider's `benchmark/benchmark.py`.
pub const DEVIATIONS: [&str; 7] = [
    "The model is an agent with tools (read, write, edit, bash, grep, glob, ls) and may run code in a sealed box (no network) while it works; aider's model replies with edits and cannot run anything. Scores are therefore not comparable with aider's leaderboard.",
    "The working directory holds the stubs and the files a build needs, never the tests, examples, `.meta`, `.docs` or other exercises; the tests are copied in after each attempt and removed again before the second one (aider leaves them in the directory, outside the chat). Before grading, the files the agent was given that are not solution files (`Cargo.toml`, `go.mod`, `package.json`...) are restored and test files it left behind are removed, and the tests run in a fresh `/tmp`. The instructions are in the first message, as aider's.",
    "The second message is the benchmark's, with the cleaned test output (timing and the directory's full path removed), sent in the same conversation. Output beyond 200000 bytes is cut in the middle; aider cuts nothing.",
    "Java runs `gradle test --offline` with gradle 8.7 (the version the exercises' `./gradlew` fetches) on a JDK 17 or 21, its packages fetched beforehand; aider runs `./gradlew test`. The `@Disabled(...)` lines are taken out of the tests, as aider does.",
    "Tests and the agent's commands run in a sealed box without network; Rust exercises with dependencies (gigasecond, grep, simple-cipher) get their crates vendored beforehand and JavaScript exercises a shared `node_modules`.",
    "Each attempt may take --max-steps model requests. The solution files are listed in the config's order (aider's order is arbitrary).",
    "The tests run on this machine's toolchains (not aider's docker image); an exercise that fails to build here for a reason of the toolchain counts as not solved.",
];

fn pct(n: usize, of: usize) -> String {
    if of == 0 {
        "-".into()
    } else {
        format!("{:.1}% ({n}/{of})", 100.0 * n as f64 / of as f64)
    }
}

fn lang_of(task: &str) -> Option<&str> {
    task.strip_prefix("polyglot/")?.split('/').next()
}

fn solved(r: &Record, id: &str) -> Option<bool> {
    r.parts.iter().find(|p| p.id == id).and_then(|p| p.ok)
}

/// The Polyglot section for `records` (those of other tasks are ignored); None when there are none.
pub fn section(repro: Option<&str>, records: &[Record]) -> Option<String> {
    let mine: Vec<&Record> = records.iter().filter(|r| lang_of(&r.task).is_some()).collect();
    if mine.is_empty() {
        return None;
    }
    let mut arms: Vec<&str> = Vec::new();
    for r in &mine {
        if !arms.contains(&r.arm.as_str()) {
            arms.push(&r.arm);
        }
    }
    let mut s = String::from("\n## Polyglot (Aider)\n\npass@1: solved by the first attempt. pass@2: solved by the first or the second (after seeing the test output once).\n");
    for arm in arms {
        let rows: Vec<&Record> = mine.iter().copied().filter(|r| r.arm == arm).collect();
        s.push_str(&format!("\n### Arm `{arm}`\n\n| language | exercises | pass@1 | pass@2 | cost | per exercise | steps | input | cached | output | wall |\n|---|---|---|---|---|---|---|---|---|---|---|\n"));
        let mut langs: Vec<&str> = Vec::new();
        for r in &rows {
            let l = lang_of(&r.task).unwrap_or_default();
            if !langs.contains(&l) {
                langs.push(l);
            }
        }
        langs.sort();
        let line = |label: &str, set: &[&Record]| -> String {
            let n = set.len();
            let p1 = set.iter().filter(|r| solved(r, "pass@1") == Some(true)).count();
            let p2 = set.iter().filter(|r| r.ok).count();
            let cost: f64 = set.iter().map(|r| r.cost).sum();
            let est = set.iter().any(|r| r.estimated);
            let sum = |f: &dyn Fn(&Record) -> u64| set.iter().map(|r| f(r)).sum::<u64>();
            let k = |x: u64| format!("{:.0}k", x as f64 / 1000.0);
            format!(
                "| {label} | {n} | {} | {} | {} | {} | {:.1} | {} | {} | {} | {:.0}s |\n",
                pct(p1, n),
                pct(p2, n),
                if est { "unknown".into() } else { format!("${cost:.4}") },
                if est { "unknown".into() } else { format!("${:.4}", cost / n as f64) },
                set.iter().map(|r| f64::from(r.steps)).sum::<f64>() / n as f64,
                k(sum(&|r| r.input)),
                k(sum(&|r| r.cache_read)),
                k(sum(&|r| r.output)),
                set.iter().map(|r| r.wall_s).sum::<f64>() / n as f64,
            )
        };
        for l in &langs {
            let set: Vec<&Record> = rows.iter().copied().filter(|r| lang_of(&r.task) == Some(l)).collect();
            s.push_str(&line(l, &set));
        }
        s.push_str(&line("**total**", &rows));
        let cut = rows.iter().filter(|r| solved(r, "pass@2").is_none()).count();
        if cut > 0 {
            s.push_str(&format!("\n{cut} run(s) were ended by the budget during the second attempt; they count as not solved at pass@2.\n"));
        }
        // the full set at this run's cost per exercise
        let est = rows.iter().any(|r| r.estimated);
        if !est {
            let mut parts = Vec::new();
            let mut total = 0.0;
            for (l, count) in FULL_SET {
                let set: Vec<&Record> = rows.iter().copied().filter(|r| lang_of(&r.task) == Some(l)).collect();
                if set.is_empty() {
                    continue;
                }
                let per = set.iter().map(|r| r.cost).sum::<f64>() / set.len() as f64;
                total += per * count as f64;
                parts.push(format!("{l} {count} x ${per:.4} = ${:.2}", per * count as f64));
            }
            if !parts.is_empty() {
                s.push_str(&format!(
                    "\nFull set at this run's cost per exercise: {}; **${total:.2}** for the languages run.\n",
                    parts.join(", ")
                ));
            }
        }
    }
    s.push_str("\n### Differences from aider's protocol\n\n");
    for d in DEVIATIONS {
        s.push_str(&format!("- {d}\n"));
    }
    if let Some(cmd) = repro {
        s.push_str(&format!("\n### Reproduce\n\n```\n{cmd}\n```\n"));
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::{sample, Part};

    fn rec(task: &str, p1: bool, p2: bool, cost: f64) -> Record {
        let mut r = sample(task, "plain", if p2 { 1.0 } else { 0.0 }, cost);
        r.parts = vec![Part { id: "pass@1".into(), ok: Some(p1) }, Part { id: "pass@2".into(), ok: Some(p2) }];
        r
    }

    #[test]
    fn the_section_has_one_row_per_language_and_a_total() {
        let records = vec![
            rec("polyglot/python/a", true, true, 0.01),
            rec("polyglot/python/b", false, true, 0.03),
            rec("polyglot/rust/c", false, false, 0.02),
            sample("other/task", "plain", 1.0, 5.0),
        ];
        let s = section(Some("nexo bench --tasks polyglot"), &records).unwrap();
        assert!(s.contains("| python | 2 | 50.0% (1/2) | 100.0% (2/2) | $0.0400 | $0.0200 |"), "{s}");
        assert!(s.contains("| rust | 1 | 0.0% (0/1) | 0.0% (0/1) | $0.0200 |"), "{s}");
        assert!(s.contains("| **total** | 3 | 33.3% (1/3) | 66.7% (2/3) | $0.0600 |"), "{s}");
        assert!(s.contains("python 34 x $0.0200 = $0.68") && s.contains("rust 30 x $0.0200 = $0.60"), "{s}");
        assert!(s.contains("```\nnexo bench --tasks polyglot\n```") && s.contains("gradle 8.7"));
        assert!(!s.contains("$5.0000"), "other tasks are not counted");
        assert!(section(None, &[sample("other/task", "plain", 1.0, 1.0)]).is_none());
    }
}
