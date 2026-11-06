//! The Aider Polyglot benchmark (`--tasks polyglot`): Exercism practice exercises in Python, Rust, Go,
//! C++, JavaScript and Java, solved by nexo's agent and scored by the
//! benchmark's own protocol: the model edits the stubs, the hidden tests run, and when they fail the model
//! gets the test output once and tries again in the same conversation. `pass@1` is solved at the first
//! try, `pass@2` at either. See [`prompts`] for the ported texts and [`runner`] for the test commands.
//!
//! One difference from aider's runs is on purpose and stated in every report: the model here is an agent
//! with tools and may run code in the box while it works, aider's model only replies with edits.

pub mod exercise;
pub mod prompts;
pub mod report;
pub mod runner;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::arm::Arm;
use crate::client::Stop;
use crate::llmb::repoqa;
use crate::rng::Rng;
use crate::run::{self, Bench, Followup, Part, Plan, Record};
use crate::seal::Seal;
use exercise::{Exercise, Lang, LANGS};
use runner::Outcome;

/// The task name.
pub const NAME: &str = "polyglot";

/// Where the benchmark lives and the commit this bench was written against.
pub const REPO: &str = "https://github.com/Aider-AI/polyglot-benchmark";
pub const COMMIT: &str = "7e0611e77b54e2dea774cdc0aa00cf9f7ed6144f";

/// The tools of an exercise: the files are read and edited and programs run. `recall` reaches the model
/// only in an arm whose policy offers it (the curator, unless `recall=0`); the other arms never see it.
pub const TOOLS: [&str; 8] = ["read", "write", "edit", "bash", "grep", "glob", "ls", "recall"];

/// One exercise to run with every arm.
pub struct Job {
    pub ex: Exercise,
}

impl Job {
    pub fn name(&self) -> String {
        self.ex.id()
    }
}

/// What the jobs share.
pub struct Ctx {
    /// The benchmark's folder (a checkout of [`REPO`]).
    pub root: PathBuf,
    /// Where the installs the exercises share are kept: JavaScript's packages and Rust's vendored crates.
    pub cache: PathBuf,
}

/// The benchmark's folder: `given`, or [`COMMIT`] cloned once into `cache`.
pub fn root_of(given: Option<&Path>, cache: &Path) -> Result<PathBuf, String> {
    match given {
        Some(p) => p.canonicalize().map_err(|e| format!("{}: {e}", p.display())),
        None => repoqa::materialise(cache, REPO, COMMIT, &mut repoqa::system_git),
    }
}

/// The commit of the checkout at `root`, when it is a git checkout.
pub fn commit_of(root: &Path) -> Option<String> {
    repoqa::system_git(&["rev-parse", "HEAD"], Some(root)).ok().map(|s| s.trim().to_string())
}

/// The order a language's exercises go in: a fixed shuffle of their sorted names, so `--limit N` takes the
/// same N on every machine and the N of a smaller limit are among the N of a larger one.
fn shuffled(lang: Lang, mut all: Vec<Exercise>) -> Vec<Exercise> {
    all.sort_by(|a, b| a.name.cmp(&b.name));
    let seed = 0x706f_6c79_676c_6f74 ^ ((LANGS.iter().position(|l| *l == lang).unwrap_or(0) as u64 + 1) << 8);
    let mut rng = Rng::new(seed);
    for i in (1..all.len()).rev() {
        all.swap(i, rng.below(i + 1));
    }
    all
}

/// The jobs for `langs`: the exercises named in `wanted` (`name` or `lang/name`; all when empty), at most
/// `limit` per language, taken in the fixed shuffled order. The jobs go round the languages, so a run cut
/// short (the budget) has covered each of them about equally.
pub fn jobs(root: &Path, langs: &[Lang], wanted: &[String], limit: Option<usize>) -> Result<Vec<Job>, String> {
    let mut per_lang: Vec<Vec<Exercise>> = Vec::new();
    for &lang in langs {
        let mut ex = shuffled(lang, exercise::list(root, lang)?);
        if !wanted.is_empty() {
            ex.retain(|e| {
                wanted.iter().any(|w| *w == e.name || *w == format!("{}/{}", lang.name(), e.name) || *w == e.id())
            });
        }
        if let Some(n) = limit {
            ex.truncate(n);
        }
        per_lang.push(ex);
    }
    let mut out = Vec::new();
    let rounds = per_lang.iter().map(Vec::len).max().unwrap_or(0);
    for i in 0..rounds {
        for ex in &per_lang {
            if let Some(e) = ex.get(i) {
                out.push(Job { ex: e.clone() });
            }
        }
    }
    Ok(out)
}

/// Languages named on the command line (all five when none).
pub fn parse_langs(names: &[String]) -> Result<Vec<Lang>, String> {
    let names: Vec<&str> = names.iter().flat_map(|n| n.split(',')).map(str::trim).filter(|n| !n.is_empty()).collect();
    if names.is_empty() {
        return Ok(LANGS.to_vec());
    }
    let mut out = Vec::new();
    for n in names {
        let l = Lang::parse(n)?;
        if !out.contains(&l) {
            out.push(l);
        }
    }
    Ok(out)
}

fn sh(dir: &Path, cmd: &str) -> Result<(), String> {
    let out = std::process::Command::new("sh")
        .args(["-c", cmd])
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&[out.stdout, out.stderr].concat()).into_owned();
    let tail: String = text.lines().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    Err(format!("{cmd} in {}: {tail}", dir.display()))
}

/// The folder JavaScript's packages are installed into, once, for every exercise (they share one
/// `package.json`). The exercises link it as their `node_modules`, so nothing is fetched per exercise.
fn npm_dir(cache: &Path) -> PathBuf {
    cache.join("polyglot-npm")
}

fn install_npm(root: &Path, cache: &Path) -> Result<(), String> {
    let dir = npm_dir(cache);
    if dir.join("node_modules/.bin/jest").exists() {
        return Ok(());
    }
    let first = exercise::list(root, Lang::Javascript)?.into_iter().next().ok_or("no JavaScript exercise")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::copy(first.dir.join("package.json"), dir.join("package.json")).map_err(|e| e.to_string())?;
    eprintln!("bench: installing JavaScript packages once into {}", dir.display());
    sh(&dir, "npm install --no-audit --no-fund --no-package-lock --loglevel=error")
}

/// Where Java's build is fetched, once: gradle 8.7 (the version the exercises' wrapper names) and the
/// exercises' packages, in a gradle home kept read-only in the box. There gradle's own folder is the run's
/// `/tmp/.gradle` and the packages are read from this home's `caches` (`GRADLE_RO_DEP_CACHE`).
fn java_dir(cache: &Path) -> PathBuf {
    cache.join("polyglot-java")
}

/// A JDK gradle 8.7 runs on (17 to 21): `NEXO_BENCH_JAVA_HOME`, else one in the usual places.
fn java_home() -> Result<PathBuf, String> {
    if let Some(h) = std::env::var_os("NEXO_BENCH_JAVA_HOME") {
        return Ok(PathBuf::from(h));
    }
    let found = ["21", "17"]
        .iter()
        .flat_map(|v| [format!("/usr/lib/jvm/java-{v}-openjdk"), format!("/usr/lib/jvm/java-{v}-openjdk-amd64")])
        .map(PathBuf::from)
        .find(|p| p.join("bin/java").is_file());
    found.ok_or_else(|| "java: gradle 8.7 needs a JDK 17 or 21 (set NEXO_BENCH_JAVA_HOME)".into())
}

/// The `bin` folder of the gradle fetched into the cache.
fn gradle_bin(cache: &Path) -> Option<PathBuf> {
    let dists = java_dir(cache).join("home/wrapper/dists/gradle-8.7-bin");
    std::fs::read_dir(dists)
        .ok()?
        .flatten()
        .map(|e| e.path().join("gradle-8.7/bin"))
        .find(|b| b.join("gradle").is_file())
}

/// Fetches gradle and the packages of every distinct `build.gradle` once, by running each such exercise's
/// tests on its example solution with the wrapper, outside the box.
fn install_gradle(root: &Path, cache: &Path) -> Result<(), String> {
    let dir = java_dir(cache);
    if dir.join("ready").is_file() && gradle_bin(cache).is_some() {
        return Ok(());
    }
    let jdk = java_home()?;
    let home = dir.join("home");
    let mut seen: Vec<String> = Vec::new();
    for ex in exercise::list(root, Lang::Java)? {
        let build = std::fs::read_to_string(ex.dir.join("build.gradle")).unwrap_or_default();
        if seen.contains(&build) {
            continue;
        }
        seen.push(build);
        let work = dir.join("warm");
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
        ex.stage(&work)?;
        ex.install_tests(&work)?;
        for rel in &ex.files.example {
            let to = work.join("src/main").join(rel.trim_start_matches(".meta/src/reference/"));
            std::fs::copy(ex.dir.join(rel), &to).map_err(|e| format!("{}: {e}", to.display()))?;
        }
        eprintln!("bench: fetching gradle and the packages of {} once into {}", ex.id(), home.display());
        let cmd = format!(
            "GRADLE_USER_HOME={} JAVA_HOME={} ./gradlew test --no-daemon -q",
            crate::seal::quote(&home.to_string_lossy()),
            crate::seal::quote(&jdk.to_string_lossy())
        );
        sh(&work, &cmd)?;
        let _ = std::fs::remove_dir_all(&work);
    }
    std::fs::write(dir.join("ready"), "").map_err(|e| format!("{}: {e}", dir.display()))
}

/// `seal` with what `lang`'s tools need inside the box: JavaScript's packages, Java's gradle and JDK.
fn for_lang(cache: &Path, lang: Lang, seal: Seal) -> Seal {
    match lang {
        Lang::Javascript => seal.with_keep(&npm_dir(cache)),
        Lang::Java => {
            let dir = java_dir(cache);
            let ro = dir.join("home/caches");
            let mut seal = seal
                .with_keep(&dir)
                .with_env("GRADLE_USER_HOME", "/tmp/.gradle")
                .with_env("GRADLE_OPTS", "-Dorg.gradle.welcome=never")
                .with_env("GRADLE_RO_DEP_CACHE", &ro.to_string_lossy());
            if let Ok(jdk) = java_home() {
                seal = seal.with_env("JAVA_HOME", &jdk.to_string_lossy());
            }
            match gradle_bin(cache) {
                Some(bin) => seal.with_bin(&bin),
                None => seal,
            }
        }
        _ => seal,
    }
}

/// Whether a `Cargo.toml` lists dependencies.
fn has_dependencies(cargo_toml: &str) -> bool {
    let re = regex::Regex::new(r"(?m)^\[(?:dev-|build-)?dependencies\]\s*\n\s*[A-Za-z_]").expect("a fixed pattern");
    re.is_match(cargo_toml)
}

fn vendor_dir(cache: &Path, name: &str) -> PathBuf {
    cache.join("polyglot-rust-vendor").join(name)
}

/// Fetches the crates of a Rust exercise that has dependencies, once, into `cache` (the box has no network).
fn vendor_crates(cache: &Path, ex: &Exercise) -> Result<(), String> {
    let toml = std::fs::read_to_string(ex.dir.join("Cargo.toml")).unwrap_or_default();
    let dest = vendor_dir(cache, &ex.name);
    if !has_dependencies(&toml) || dest.join("Cargo.lock").is_file() {
        return Ok(());
    }
    let partial = dest.with_extension("partial");
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial).map_err(|e| format!("{}: {e}", partial.display()))?;
    ex.stage(&partial)?;
    eprintln!("bench: fetching the crates of {} once", ex.id());
    sh(&partial, "cargo vendor vendor >/dev/null")?;
    let _ = std::fs::remove_dir_all(partial.join("src"));
    std::fs::rename(&partial, &dest).map_err(|e| format!("{}: {e}", dest.display()))
}

/// What the jobs need that is fetched from the network, done once before the first run.
pub fn prepare(ctx: &Ctx, jobs: &[Job]) -> Result<(), String> {
    if jobs.iter().any(|j| j.ex.lang == Lang::Javascript) {
        install_npm(&ctx.root, &ctx.cache)?;
    }
    if jobs.iter().any(|j| j.ex.lang == Lang::Java) {
        install_gradle(&ctx.root, &ctx.cache)?;
    }
    for j in jobs.iter().filter(|j| j.ex.lang == Lang::Rust) {
        vendor_crates(&ctx.cache, &j.ex)?;
    }
    Ok(())
}

/// Puts into `wd` what the language's build needs besides the stubs: JavaScript's packages, the vendored
/// crates of a Rust exercise.
fn provision(ctx: &Ctx, ex: &Exercise, wd: &Path) -> Result<(), String> {
    match ex.lang {
        Lang::Javascript => link(&npm_dir(&ctx.cache).join("node_modules"), &wd.join("node_modules")),
        Lang::Rust => {
            let v = vendor_dir(&ctx.cache, &ex.name);
            if !v.join("Cargo.lock").is_file() {
                return Ok(());
            }
            repoqa::copy_tree(&v.join("vendor"), &wd.join("vendor"), false).map_err(|e| e.to_string())?;
            std::fs::copy(v.join("Cargo.lock"), wd.join("Cargo.lock")).map_err(|e| e.to_string())?;
            std::fs::create_dir_all(wd.join(".cargo")).map_err(|e| e.to_string())?;
            std::fs::write(
                wd.join(".cargo/config.toml"),
                "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n[source.vendored-sources]\ndirectory = \"vendor\"\n",
            )
            .map_err(|e| e.to_string())
        }
        _ => Ok(()),
    }
}

#[cfg(unix)]
fn link(target: &Path, at: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, at).map_err(|e| format!("{}: {e}", at.display()))
}

#[cfg(not(unix))]
fn link(_: &Path, _: &Path) -> Result<(), String> {
    Err("the benchmark's tests run in a sealed box, which needs Linux".into())
}

/// The scratch `/tmp` of the grading runs next to the agent's `tmp`.
fn grade_tmp(tmp: &Path) -> PathBuf {
    let name = tmp.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    tmp.with_file_name(format!("{name}.grade"))
}

/// Grades `wd` as the benchmark would: harness files restored and stray tests removed, the tests installed,
/// run in a fresh `/tmp` (`scratch`, deleted afterwards, so nothing the agent left in its own `/tmp` is
/// there), and taken out again so the model never sees them.
fn grade(ex: &Exercise, wd: &Path, seal: &Seal, scratch: &Path) -> Result<Outcome, String> {
    ex.restore_harness(wd)?;
    let _ = std::fs::remove_dir_all(scratch);
    std::fs::create_dir_all(scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let sealed = seal.clone().with_tmp(scratch);
    let out = ex.install_tests(wd).and_then(|_| runner::run_tests(ex.lang, wd, &sealed));
    ex.remove_tests(wd);
    let _ = std::fs::remove_dir_all(scratch);
    out
}

/// A failed record for an exercise that could not be set up or graded here: it counts as not solved (and
/// its cost, if any, is in `driven`'s record), never as left out of the score.
fn failed(bench: &Bench, arm: &Arm, rep: u32, name: &str, why: &str) -> Record {
    Record {
        task: name.to_string(),
        arm: arm.name.clone(),
        policy: arm.policy.clone(),
        spec: arm.spec.clone(),
        rep,
        model: bench.model.clone(),
        commit: run::COMMIT.into(),
        max_steps: bench.max_steps,
        max_tokens: bench.max_tokens,
        temperature: bench.temperature,
        detail: format!("pass@1 no, pass@2 no (not run: {why})"),
        parts: vec![Part { id: "pass@1".into(), ok: Some(false) }, Part { id: "pass@2".into(), ok: Some(false) }],
        ..Default::default()
    }
}

/// The attempts of an exercise as they are graded: the model gets the test output once.
#[derive(Debug, Default)]
pub struct Tries {
    pub attempts: Vec<Outcome>,
    /// The second message was sent.
    pub retry_sent: bool,
    /// Why grading failed, if it did.
    pub error: Option<String>,
}

impl Tries {
    /// Records an attempt; the message to send next: the test output with the benchmark's text after a
    /// failed first attempt, nothing after a pass or the second attempt.
    pub fn after(&mut self, outcome: Outcome, file_list: &str) -> Option<String> {
        let next =
            (!outcome.pass && self.attempts.is_empty()).then(|| prompts::retry_prompt(&outcome.output, file_list));
        self.attempts.push(outcome);
        self.retry_sent |= next.is_some();
        next
    }

    pub fn passed(&self, n: usize) -> bool {
        self.attempts.iter().take(n).any(|a| a.pass)
    }
}

/// Runs `job` once with `arm`; Err when it cannot run or grade here.
pub async fn run(bench: &Bench, ctx: &Ctx, job: &Job, arm: &Arm, rep: u32, budget_left: f64) -> Result<Record, String> {
    let ex = job.ex.clone();
    let name = job.name();
    let tag = format!("{}-{}-{}-{rep}", ex.lang.name(), ex.name, arm.name);
    let work = bench.out.join("work");
    let wd = work.join(&tag).join(&ex.name);
    let tmp = work.join(format!("{tag}.tmp"));
    for d in [&wd, &tmp] {
        let _ = std::fs::remove_dir_all(d);
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let wd = wd.canonicalize().map_err(|e| e.to_string())?;
    let tmp = tmp.canonicalize().map_err(|e| e.to_string())?;
    let result = attempt(bench, ctx, &ex, &name, arm, rep, budget_left, &wd, &tmp).await;
    let _ = std::fs::remove_dir_all(&wd);
    let _ = std::fs::remove_dir_all(work.join(&tag));
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(grade_tmp(&tmp));
    result
}

#[allow(clippy::too_many_arguments)]
async fn attempt(
    bench: &Bench,
    ctx: &Ctx,
    ex: &Exercise,
    name: &str,
    arm: &Arm,
    rep: u32,
    budget_left: f64,
    wd: &Path,
    tmp: &Path,
) -> Result<Record, String> {
    let setup = ex.stage(wd).and_then(|_| provision(ctx, ex, wd)).and_then(|_| ex.prompt());
    let prompt = match setup {
        Ok(p) => p,
        Err(e) => return Ok(failed(bench, arm, rep, name, &e)),
    };
    // the run's own folders and the benchmark's are hidden, the working directory and the packages put back
    let mut hide: Vec<PathBuf> = bench.hide.clone();
    hide.push(ctx.root.canonicalize().unwrap_or_else(|_| ctx.root.clone()));
    hide.push(ctx.cache.canonicalize().unwrap_or_else(|_| ctx.cache.clone()));
    let seal = for_lang(&ctx.cache, ex.lang, Seal::new(wd, hide, vec![])?.with_tmp(tmp));
    let tries = Arc::new(Mutex::new(Tries::default()));
    let file_list = ex.file_list();
    let followup: Followup = {
        let (tries, ex, wd, seal, file_list, gt) =
            (tries.clone(), ex.clone(), wd.to_path_buf(), seal.clone(), file_list.clone(), grade_tmp(tmp));
        Arc::new(move |turn, _replies| {
            let mut t = tries.lock().unwrap();
            if turn != 0 || t.attempts.len() >= 2 {
                return None;
            }
            match grade(&ex, &wd, &seal, &gt) {
                Ok(o) => t.after(o, &file_list),
                Err(e) => {
                    t.error = Some(e);
                    None
                }
            }
        })
    };
    let plan = Plan {
        name: name.to_string(),
        wd: wd.to_path_buf(),
        seal: Some(seal.clone()),
        tools: Some(TOOLS.to_vec()),
        confine: true,
        system_prompt: Some(crate::llmb::system_file(bench, "polyglot-system.md", prompts::SYSTEM)?),
        turns: vec![(0, prompt)],
        // --max-steps holds per attempt
        max_steps: bench.max_steps.saturating_mul(2),
        followup: Some(followup),
    };
    let driven = run::drive(bench, &plan, arm, rep, budget_left).await?;
    let mut grading_error = tries.lock().unwrap().error.clone();
    // an attempt the conversation ended without grading (the step limit, an early stop, the budget) is graded
    // as it stands: grading costs nothing
    let missing = {
        let t = tries.lock().unwrap();
        t.attempts.is_empty() || (t.attempts.len() == 1 && t.retry_sent)
    };
    if missing && grading_error.is_none() {
        let (ex2, wd2, seal2, fl, gt) = (ex.clone(), wd.to_path_buf(), seal.clone(), file_list.clone(), grade_tmp(tmp));
        match tokio::task::spawn_blocking(move || grade(&ex2, &wd2, &seal2, &gt)).await {
            Ok(Ok(o)) => {
                tries.lock().unwrap().after(o, &fl);
            }
            Ok(Err(e)) => grading_error = Some(e),
            Err(e) => grading_error = Some(e.to_string()),
        }
    }
    let t = tries.lock().unwrap();
    let (p1, p2) = (t.passed(1), t.passed(2));
    // a grading failure counts as not solved, never as left out
    let second_known = t.attempts.len() >= 2 || p1 || grading_error.is_some();
    let tests_s: f64 = t.attempts.iter().map(|a| a.seconds).sum();
    let summaries: Vec<String> = t.attempts.iter().map(|a| a.summary.clone()).collect();
    let cut = driven.tally.stop == Some(Stop::Budget);
    let detail = format!(
        "pass@1 {}, pass@2 {} ({}); tests took {:.0}s{}{}",
        if p1 { "yes" } else { "no" },
        if p2 {
            "yes"
        } else if second_known {
            "no"
        } else {
            "not graded"
        },
        summaries.join(" -> "),
        tests_s,
        if cut { "; the budget ended the run" } else { "" },
        grading_error.as_ref().map(|e| format!("; grading failed: {e}")).unwrap_or_default(),
    );
    let solved = solution_dir(bench, ex);
    for f in ex.solution() {
        let to = solved.join(&f);
        if let Some(p) = to.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let _ = std::fs::copy(wd.join(&f), to);
    }
    crate::llmb::write_score(
        bench,
        &driven.transcript,
        &json!({"task": name, "pass_at_1": p1, "pass_at_2": p2, "attempts": t.attempts.iter().map(|a| json!({
            "pass": a.pass, "timed_out": a.timed_out, "summary": a.summary, "seconds": a.seconds, "output": a.output,
        })).collect::<Vec<_>>()}),
    );
    let mut rec = run::record(bench, arm, rep, name, &driven, if p2 { 1.0 } else { 0.0 }, p2, detail);
    rec.parts =
        vec![Part { id: "pass@1".into(), ok: Some(p1) }, Part { id: "pass@2".into(), ok: second_known.then_some(p2) }];
    Ok(rec)
}

/// Where the final solution files of an exercise are kept, next to the results.
fn solution_dir(bench: &Bench, ex: &Exercise) -> PathBuf {
    bench.out.join("solutions").join(ex.lang.name()).join(&ex.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(pass: bool) -> Outcome {
        Outcome { pass, timed_out: false, output: "E  assert 1 == 2\n".into(), summary: "x".into(), seconds: 1.0 }
    }

    fn benchmark(langs: &[(&str, &[&str])]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for (lang, names) in langs {
            for n in *names {
                let d = root.path().join(lang).join("exercises/practice").join(n);
                std::fs::create_dir_all(d.join(".meta")).unwrap();
                std::fs::create_dir_all(d.join(".docs")).unwrap();
                std::fs::write(
                    d.join(".meta/config.json"),
                    r#"{"files":{"solution":["a.py"],"test":["a_test.py"],"example":[]}}"#,
                )
                .unwrap();
                std::fs::write(d.join(".docs/instructions.md"), "x").unwrap();
            }
        }
        root
    }

    // a small exercise of each language
    fn pick(lang: Lang) -> &'static str {
        match lang {
            Lang::Python => "proverb",
            Lang::Rust => "acronym",
            Lang::Go => "sublist",
            Lang::Cpp => "gigasecond",
            Lang::Javascript => "beer-song",
            Lang::Java => "resistor-color-trio",
        }
    }

    // Needs the toolchains and a checkout: `POLYGLOT_DIR=... cargo test -p nexo-bench -- --ignored runners_pass_the_examples`
    // (the cache in `POLYGLOT_CACHE`, default a temporary folder). No model is called.
    #[test]
    #[ignore]
    fn runners_pass_the_examples_and_fail_the_stubs() {
        let Some(root) = std::env::var_os("POLYGLOT_DIR").map(PathBuf::from) else { return };
        let cache = std::env::var_os("POLYGLOT_CACHE").map(PathBuf::from).unwrap_or_else(|| {
            let d = std::env::temp_dir().join("polyglot-test-cache");
            std::fs::create_dir_all(&d).unwrap();
            d
        });
        let ctx = Ctx { root: root.clone(), cache };
        let only: Vec<String> =
            std::env::var("POLYGLOT_LANGS").unwrap_or_default().split(',').map(String::from).collect();
        for lang in parse_langs(&only).unwrap() {
            let ex = exercise::list(&root, lang).unwrap().into_iter().find(|e| e.name == pick(lang)).unwrap();
            prepare(&ctx, &[Job { ex: ex.clone() }]).unwrap();
            let work = tempfile::tempdir().unwrap();
            let wd = work.path().join("wd").join(&ex.name);
            let tmp = work.path().join("tmp");
            std::fs::create_dir_all(&wd).unwrap();
            std::fs::create_dir_all(&tmp).unwrap();
            let (wd, tmp) = (wd.canonicalize().unwrap(), tmp.canonicalize().unwrap());
            ex.stage(&wd).unwrap();
            provision(&ctx, &ex, &wd).unwrap();
            let seal = for_lang(&ctx.cache, lang, Seal::new(&wd, vec![], vec![]).unwrap().with_tmp(&tmp));
            let gt = work.path().join("grade-tmp");
            let stub = grade(&ex, &wd, &seal, &gt).unwrap();
            eprintln!(
                "{} stub: pass={} {} | {}",
                ex.id(),
                stub.pass,
                stub.summary,
                stub.output.lines().take(3).collect::<Vec<_>>().join(" / ")
            );
            assert!(!stub.pass, "{}: the stub passed", ex.id());
            for (s, e) in ex.solution().iter().zip(&ex.files.example) {
                std::fs::copy(ex.dir.join(e), wd.join(s)).unwrap();
            }
            let done = grade(&ex, &wd, &seal, &gt).unwrap();
            eprintln!(
                "{} example: pass={} {} | {}",
                ex.id(),
                done.pass,
                done.summary,
                done.output.lines().take(5).collect::<Vec<_>>().join(" / ")
            );
            assert!(done.pass, "{}: the example failed:\n{}", ex.id(), done.output);
        }
    }
    #[test]
    fn grading_has_its_own_tmp_beside_the_agents() {
        let g = grade_tmp(Path::new("/x/work/go-bob-plain-0.tmp"));
        assert_eq!(g, Path::new("/x/work/go-bob-plain-0.tmp.grade"));
    }

    // Needs a sealed box: what the agent leaves in its /tmp is not there when the tests run.
    #[test]
    fn the_tests_run_in_a_fresh_tmp() {
        if crate::seal::available().is_err() {
            return;
        }
        let work = tempfile::tempdir().unwrap();
        let (wd, tmp, gt) = (work.path().join("wd"), work.path().join("tmp"), work.path().join("tmp.grade"));
        for d in [&wd, &tmp] {
            std::fs::create_dir_all(d).unwrap();
        }
        let (wd, tmp) = (wd.canonicalize().unwrap(), tmp.canonicalize().unwrap());
        std::fs::write(tmp.join("left-by-the-agent"), "x").unwrap();
        let seal = Seal::new(&wd, vec![], vec![]).unwrap().with_tmp(&tmp);
        let sealed = seal.clone().with_tmp({
            std::fs::create_dir_all(&gt).unwrap();
            &gt
        });
        let line = |s: &Seal| format!("{} 2>&1", s.wrap("ls /tmp"));
        let run = |s: &Seal| {
            String::from_utf8_lossy(&std::process::Command::new("sh").args(["-c", &line(s)]).output().unwrap().stdout)
                .into_owned()
        };
        assert!(run(&seal).contains("left-by-the-agent"));
        assert!(!run(&sealed).contains("left-by-the-agent"));
    }

    #[test]
    fn the_second_message_is_sent_once_and_only_after_a_failure() {
        let mut t = Tries::default();
        let next = t.after(outcome(false), "lib.rs").unwrap();
        assert!(next.starts_with("E  assert 1 == 2\n####\n\nSee the testing errors above."), "{next}");
        assert!(t.retry_sent && !t.passed(1));
        assert!(t.after(outcome(true), "lib.rs").is_none());
        assert!(!t.passed(1) && t.passed(2), "solved at the second try only");
        let mut t = Tries::default();
        assert!(t.after(outcome(true), "x").is_none() && t.passed(1) && !t.retry_sent);
    }

    #[test]
    fn the_sample_is_fixed_nested_and_balanced_across_languages() {
        let root =
            benchmark(&[("python", &["a", "b", "c", "d", "e"]), ("go", &["a", "b", "c"]), ("rust", &["a", "b"])]);
        let langs = [Lang::Python, Lang::Rust, Lang::Go];
        let two = jobs(root.path(), &langs, &[], Some(2)).unwrap();
        let names: Vec<String> = two.iter().map(Job::name).collect();
        assert_eq!(names.len(), 6);
        assert_eq!(names, jobs(root.path(), &langs, &[], Some(2)).unwrap().iter().map(Job::name).collect::<Vec<_>>());
        // two of each language, round by round
        assert!(names[..3].iter().all(|n| n != &names[3]));
        for l in ["python", "go", "rust"] {
            assert_eq!(names.iter().filter(|n| n.starts_with(&format!("polyglot/{l}/"))).count(), 2);
        }
        let three: Vec<String> = jobs(root.path(), &langs, &[], Some(3)).unwrap().iter().map(Job::name).collect();
        assert!(names.iter().all(|n| three.contains(n)), "a smaller limit is inside a larger one");
        assert_eq!(jobs(root.path(), &langs, &[], None).unwrap().len(), 10);
    }

    #[test]
    fn exercises_can_be_named_with_or_without_their_language() {
        let root = benchmark(&[("python", &["bob", "dot"]), ("go", &["bob"])]);
        let langs = [Lang::Python, Lang::Go];
        let both: Vec<String> =
            jobs(root.path(), &langs, &["bob".into()], None).unwrap().iter().map(Job::name).collect();
        assert_eq!(both.len(), 2);
        let one = jobs(root.path(), &langs, &["go/bob".into()], None).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].name(), "polyglot/go/bob");
    }

    #[test]
    fn languages_default_to_all_six() {
        assert_eq!(parse_langs(&[]).unwrap().len(), 6);
        assert_eq!(parse_langs(&["go,rust".into(), "go".into()]).unwrap(), [Lang::Go, Lang::Rust]);
        assert_eq!(parse_langs(&["java".into()]).unwrap(), [Lang::Java]);
    }

    #[test]
    fn dependencies_are_found_in_a_cargo_toml() {
        assert!(has_dependencies("[package]\nname = \"x\"\n\n[dependencies]\ntime = \"0.3\"\n"));
        assert!(has_dependencies("[dev-dependencies]\nanyhow = \"1.0\"\n"));
        assert!(!has_dependencies("[package]\nname = \"x\"\n\n[dependencies]\n"));
    }
}
