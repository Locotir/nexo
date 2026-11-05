//! The external benchmark (`--llmb DIR`, the frozen distribution): its `repo_qa` family as task
//! `repoqa` and its canonical-step track as task `canonstep`, run by nexo's agent and scored with
//! the distribution's own rules, ported here. Only the instances' frozen files are read; the model
//! sees prompts and material only, never an oracle, a score or `instance.json`.

pub mod answer;
pub mod canonstep;
pub mod flux;
pub mod py;
pub mod repoqa;
pub mod score;
pub mod sha;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::arm::Arm;
use crate::run::{self, Bench, Plan, Record};
use crate::seal::Seal;
use canonstep::Adapted;
use score::Oracle;

/// The task names this module answers to.
pub const NAMES: [&str; 2] = ["repoqa", "canonstep"];

/// The tools of a canonical-step conversation: the material is read, programs written and run.
pub const CANONSTEP_TOOLS: [&str; 8] = ["read", "write", "edit", "bash", "grep", "glob", "ls", "note"];

#[derive(Debug, Clone, Deserialize)]
pub struct Hashes {
    pub prompt: String,
    pub oracle: String,
    #[serde(default)]
    pub files: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Workspace {
    pub repository: String,
    pub commit: String,
    pub files_dir: String,
}

/// The parts of `instance.json` the bench uses.
#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    pub schema_version: u32,
    pub id: String,
    pub scoring: String,
    pub prompt_file: String,
    pub oracle_file: String,
    pub sha256: Hashes,
    #[serde(default)]
    pub workspace: Option<Workspace>,
}

/// An instance with its prompt read and checked.
#[derive(Debug, Clone)]
pub struct Instance {
    pub dir: PathBuf,
    pub meta: Meta,
    pub prompt: String,
    pub prompt_sha256: String,
}

/// A member file of an instance: a plain relative path that stays in its folder.
fn member(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let p = Path::new(name);
    if name.is_empty() || p.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return Err(format!("{}: {name:?} is not a file of the instance", dir.display()));
    }
    Ok(dir.join(p))
}

/// Reads the instance in `dir`, checking its prompt against the frozen digest.
pub fn read_instance(dir: &Path) -> Result<Instance, String> {
    let at = |e: &dyn std::fmt::Display| format!("{}: {e}", dir.display());
    let text = std::fs::read_to_string(dir.join("instance.json")).map_err(|e| at(&e))?;
    let meta: Meta = serde_json::from_str(&text).map_err(|e| at(&e))?;
    if meta.schema_version != 1 {
        return Err(at(&"unsupported instance schema_version"));
    }
    if !matches!(meta.scoring.as_str(), "exact_fields" | "swe_flux_fields") {
        return Err(at(&"instance scoring must be exact_fields or swe_flux_fields"));
    }
    let bytes = std::fs::read(member(dir, &meta.prompt_file)?).map_err(|e| at(&e))?;
    let prompt_sha256 = sha::hex(&bytes);
    if prompt_sha256 != meta.sha256.prompt {
        return Err(at(&"prompt SHA-256 does not match instance.json"));
    }
    let prompt = String::from_utf8(bytes).map_err(|e| at(&e))?;
    Ok(Instance { dir: dir.to_path_buf(), meta, prompt, prompt_sha256 })
}

impl Instance {
    /// The oracle, checked against the frozen digest. Read by the bench only, after the run.
    pub fn oracle(&self) -> Result<Oracle, String> {
        let bytes = std::fs::read(member(&self.dir, &self.meta.oracle_file)?)
            .map_err(|e| format!("{}: {e}", self.dir.display()))?;
        if sha::hex(&bytes) != self.meta.sha256.oracle {
            return Err(format!("{}: oracle SHA-256 does not match instance.json", self.dir.display()));
        }
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: oracle: {e}", self.dir.display()))
    }
}

/// The ids to run out of `all` (sorted): those of `wanted` that are in `all` (every one when none
/// is), the first `limit` of them.
pub fn choose(all: &[String], wanted: &[String], limit: Option<usize>) -> Vec<String> {
    let mut all = all.to_vec();
    all.sort();
    let mine: Vec<String> = all.iter().filter(|id| wanted.contains(id)).cloned().collect();
    let mut chosen = if mine.is_empty() { all } else { mine };
    if let Some(n) = limit {
        chosen.truncate(n);
    }
    chosen
}

/// One unit of work: an instance to run with every arm.
pub enum Job {
    RepoQa(Instance),
    CanonStep(Adapted, Instance),
}

impl Job {
    /// The task name of its records.
    pub fn name(&self) -> String {
        match self {
            Job::RepoQa(i) => format!("repoqa/{}", i.meta.id),
            Job::CanonStep(a, _) => format!("canonstep/{}", a.id),
        }
    }
}

/// The ids of the repo_qa instances under `root`.
pub fn repoqa_ids(root: &Path) -> Result<Vec<String>, String> {
    let dir = root.join(repoqa::DIR);
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut ids: Vec<String> = rd
        .filter_map(Result::ok)
        .filter(|e| e.path().join("instance.json").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    Ok(ids)
}

/// The jobs of the tasks `names` (of `NAMES`) under `root`, instances chosen as `choose` says.
pub fn jobs(root: &Path, names: &[&str], wanted: &[String], limit: Option<usize>) -> Result<Vec<Job>, String> {
    let mut out = Vec::new();
    if names.contains(&"repoqa") {
        for id in choose(&repoqa_ids(root)?, wanted, limit) {
            let inst = read_instance(&root.join(repoqa::DIR).join(&id))?;
            let ws = inst.meta.workspace.as_ref().ok_or_else(|| format!("{id}: no workspace"))?;
            let files = member(&inst.dir, &ws.files_dir)?;
            let digest = repoqa::tree_digest(&files).map_err(|e| format!("{}: {e}", files.display()))?;
            if Some(&digest) != inst.meta.sha256.files.as_ref() {
                return Err(format!("{id}: files digest mismatch"));
            }
            out.push(Job::RepoQa(inst));
        }
    }
    if names.contains(&"canonstep") {
        let battery = canonstep::battery(root)?;
        let ids: Vec<String> = battery.iter().map(|a| a.id.clone()).collect();
        let chosen = choose(&ids, wanted, limit);
        for a in battery.into_iter().filter(|a| chosen.contains(&a.id)) {
            let inst = read_instance(&root.join(&a.path))?;
            out.push(Job::CanonStep(a, inst));
        }
    }
    Ok(out)
}

/// What the jobs of a benchmark share.
pub struct Ctx {
    /// The distribution's root.
    pub root: PathBuf,
    /// Where repositories are cloned once.
    pub cache: PathBuf,
}

/// Writes `text` as the system prompt file `name` under the bench's own nexo home.
pub(crate) fn system_file(bench: &Bench, name: &str, text: &str) -> Result<PathBuf, String> {
    let dir = bench.out.join(".nexo").join("llmb");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(name);
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Writes the scoring of a run next to its transcript.
pub(crate) fn write_score(bench: &Bench, transcript: &str, value: &Value) {
    let name = transcript.strip_suffix(".jsonl").unwrap_or(transcript);
    let path = bench.out.join(format!("{name}.score.json"));
    let _ = std::fs::write(path, serde_json::to_string_pretty(value).unwrap_or_default());
}

/// Runs `job` once with `arm`. Err when it cannot run or grade here (what it spent is in
/// `bench.spent` all the same).
pub async fn run(bench: &Bench, ctx: &Ctx, job: &Job, arm: &Arm, rep: u32, budget_left: f64) -> Result<Record, String> {
    match job {
        Job::RepoQa(inst) => run_repoqa(bench, ctx, inst, &job.name(), arm, rep, budget_left).await,
        Job::CanonStep(a, inst) => run_canonstep(bench, ctx, a, inst, &job.name(), arm, rep, budget_left).await,
    }
}

async fn run_repoqa(
    bench: &Bench,
    ctx: &Ctx,
    inst: &Instance,
    name: &str,
    arm: &Arm,
    rep: u32,
    budget_left: f64,
) -> Result<Record, String> {
    let ws = inst.meta.workspace.clone().ok_or("no workspace")?;
    let cache = ctx.cache.clone();
    let source = tokio::task::spawn_blocking(move || {
        repoqa::materialise(&cache, &ws.repository, &ws.commit, &mut repoqa::system_git)
    })
    .await
    .map_err(|e| e.to_string())??;
    let dir = tempfile::Builder::new().prefix("nexo-bench-repoqa-").tempdir().map_err(|e| e.to_string())?;
    let wd = dir.path().canonicalize().map_err(|e| e.to_string())?;
    let files = member(&inst.dir, &inst.meta.workspace.as_ref().map(|w| w.files_dir.clone()).unwrap_or_default())?;
    let (s, w, f) = (source.clone(), wd.clone(), files.clone());
    tokio::task::spawn_blocking(move || repoqa::copy_tree(&s, &w, true).and_then(|_| repoqa::copy_tree(&f, &w, false)))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("workspace: {e}"))?;
    let plan = Plan {
        name: name.to_string(),
        wd,
        seal: None,
        tools: Some(repoqa::TOOLS.to_vec()),
        confine: true,
        system_prompt: Some(system_file(bench, "repoqa-system.md", repoqa::SYSTEM)?),
        turns: vec![(0, inst.prompt.clone())],
        max_steps: bench.max_steps,
        followup: None,
    };
    // a bad oracle stops the run before it spends anything
    let oracle = inst.oracle()?;
    let driven = run::drive(bench, &plan, arm, rep, budget_left).await?;
    drop(dir);
    let text = driven.answers.last().cloned().unwrap_or_default();
    let parsed = answer::parse_answer(&text, &oracle.scoreable_fields);
    let scored = oracle.score(parsed.as_ref().ok().map(|(m, _)| m));
    let how = match &parsed {
        Ok((_, found)) => found.as_str().to_string(),
        Err(why) => format!("no answer: {why}"),
    };
    let detail = format!("{}/{} fields ({how})", scored.correct, scored.total);
    write_score(bench, &driven.transcript, &json!({"task": name, "parse": how, "answer": text, "score": scored}));
    Ok(run::record(bench, arm, rep, name, &driven, scored.fraction(), scored.all_correct(), detail))
}

#[allow(clippy::too_many_arguments)]
async fn run_canonstep(
    bench: &Bench,
    ctx: &Ctx,
    a: &Adapted,
    inst: &Instance,
    name: &str,
    arm: &Arm,
    rep: u32,
    budget_left: f64,
) -> Result<Record, String> {
    let dir = tempfile::Builder::new().prefix("nexo-bench-canonstep-").tempdir().map_err(|e| e.to_string())?;
    let wd = dir.path().canonicalize().map_err(|e| e.to_string())?;
    let mut hide: Vec<PathBuf> = bench.hide.iter().filter(|h| !wd.starts_with(h)).cloned().collect();
    hide.push(ctx.root.canonicalize().unwrap_or_else(|_| ctx.root.clone()));
    let seal = Seal::new(&wd, hide, vec![])?;
    for d in ["material", "tmp"] {
        std::fs::create_dir_all(wd.join(d)).map_err(|e| e.to_string())?;
    }
    for (file, content) in &a.material {
        std::fs::write(wd.join("material").join(file), content).map_err(|e| e.to_string())?;
    }
    let turns = a.steps.iter().enumerate().map(|(i, s)| (0, canonstep::worker_request(a, s, i == 0))).collect();
    let plan = Plan {
        name: name.to_string(),
        wd: wd.clone(),
        seal: Some(seal),
        tools: Some(CANONSTEP_TOOLS.to_vec()),
        confine: true,
        system_prompt: Some(system_file(bench, "canonstep-system.md", canonstep::WORKER_INSTRUCTIONS)?),
        turns,
        // --max-steps holds per objective
        max_steps: bench.max_steps.saturating_mul(a.steps.len() as u32),
        followup: None,
    };
    let oracle = inst.oracle()?;
    let driven = run::drive(bench, &plan, arm, rep, budget_left).await?;
    let modified: Vec<&String> = a
        .material
        .iter()
        .filter(|(f, c)| std::fs::read(wd.join("material").join(f)).ok().as_deref() != Some(c.as_bytes()))
        .map(|(f, _)| f)
        .collect();
    drop(dir);
    // the final statement of each component counts; a reply that cannot be read states nothing
    let mut stated: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    let mut unread = Vec::new();
    for (step, text) in a.steps.iter().zip(&driven.answers) {
        match canonstep::statements(text, &step.id, &a.steps) {
            Some(st) => {
                for (key, comps) in st {
                    stated.entry(key).or_default().extend(comps);
                }
            }
            None => unread.push(step.id.clone()),
        }
    }
    let snap = canonstep::score_snapshot(a, &stated, &oracle.expected);
    let mut detail = format!(
        "{}/{} objectives, {:.1}% of components ({} answers unread)",
        snap.cases_correct,
        snap.total,
        snap.partial_percent,
        unread.len()
    );
    if !modified.is_empty() {
        detail.push_str(&format!("; material modified: {modified:?}"));
    }
    write_score(
        bench,
        &driven.transcript,
        &json!({"task": name, "answers": driven.answers, "unread": unread, "statements": stated, "score": snap}),
    );
    let ok = snap.total > 0 && snap.cases_correct == snap.total;
    let mut rec = run::record(bench, arm, rep, name, &driven, snap.partial_percent / 100.0, ok, detail);
    rec.parts = snap
        .cases
        .iter()
        .map(|(id, s)| run::Part {
            id: id.clone(),
            ok: stated.get(id).is_some_and(|c| !c.is_empty()).then_some(s.correct == s.total),
        })
        .collect();
    Ok(rec)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The distribution's root, when the environment names one (`NEXO_LLMB_DIR`).
    pub fn dist() -> Option<PathBuf> {
        std::env::var_os("NEXO_LLMB_DIR").map(PathBuf::from).filter(|p| p.join(canonstep::MANIFEST).is_file())
    }

    #[test]
    fn instances_are_chosen_in_sorted_order() {
        let all: Vec<String> = ["c", "a", "b"].iter().map(|s| s.to_string()).collect();
        assert_eq!(choose(&all, &[], Some(2)), ["a", "b"]);
        assert_eq!(choose(&all, &["c".into(), "zz".into()], None), ["c"]);
        assert_eq!(choose(&all, &["zz".into()], None), ["a", "b", "c"]);
    }

    #[test]
    fn member_files_stay_in_the_instance() {
        assert!(member(Path::new("/d"), "../oracle.json").is_err());
        assert!(member(Path::new("/d"), "/etc/passwd").is_err());
        assert_eq!(member(Path::new("/d"), "prompt.txt").unwrap(), Path::new("/d/prompt.txt"));
    }

    #[test]
    fn the_real_battery_splits_into_6_6_and_13_objectives() {
        let Some(root) = dist() else {
            eprintln!("NEXO_LLMB_DIR unset: skipped");
            return;
        };
        let b = canonstep::battery(&root).expect("the battery matches its frozen digests");
        let counts: Vec<usize> = b.iter().map(|a| a.steps.len()).collect();
        assert_eq!(counts, [6, 6, 13]);
        let asm = &b[2];
        let first = canonstep::worker_request(asm, &asm.steps[0], true);
        assert!(first.starts_with("Frozen material is in material/: assembly-A.txt, assembly-B.txt, assembly-C.txt."));
        assert!(!canonstep::worker_request(asm, &asm.steps[1], false).contains("Frozen material"));
    }

    #[test]
    fn the_real_oracles_score_themselves_perfectly() {
        let Some(root) = dist() else {
            eprintln!("NEXO_LLMB_DIR unset: skipped");
            return;
        };
        let jobs = jobs(&root, &NAMES, &[], None).unwrap();
        assert_eq!(jobs.len(), 63);
        for job in &jobs {
            match job {
                Job::RepoQa(i) => {
                    let o = i.oracle().unwrap();
                    let fenced = format!("```json\n{}\n```", Value::Object(o.expected.clone()));
                    let (m, _) = answer::parse_answer(&fenced, &o.scoreable_fields).unwrap();
                    assert!(o.score(Some(&m)).all_correct(), "{}", i.meta.id);
                    assert_eq!(o.score(None).correct, 0);
                }
                Job::CanonStep(a, i) => {
                    let o = i.oracle().unwrap();
                    let mut stated = BTreeMap::new();
                    for s in &a.steps {
                        let text = o.expected[&s.id].to_string();
                        for (k, c) in canonstep::statements(&text, &s.id, &a.steps).unwrap() {
                            stated.insert(k, c);
                        }
                    }
                    let snap = canonstep::score_snapshot(a, &stated, &o.expected);
                    assert_eq!(snap.cases_correct, snap.total, "{}", a.id);
                }
            }
        }
    }
}
