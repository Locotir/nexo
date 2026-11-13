use super::*;

/// What the older runs were costed with: `deepseek-flash` at the chat prices.
pub const PRICES_OLD: Prices = Prices { miss: 0.28, hit: 0.028, out: 0.42 };

/// The spec a pair is run with when its curator row carries none.
pub const DEFAULT_SPEC: &str = "directives=0;nudge=0";

/// Resamples of every bootstrap.
pub const BOOT: usize = 2000;

// ---- rows ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ArmKind {
    Plain,
    Curator,
    Other,
}

/// One row of a `results.jsonl`.
#[derive(Debug, Clone)]
pub struct Row {
    /// Directory of the `results.jsonl` the row came from.
    pub dir: PathBuf,
    pub task: String,
    pub arm: String,
    pub kind: ArmKind,
    pub rep: i64,
    pub commit: String,
    pub model: String,
    pub spec: String,
    pub steps: usize,
    pub cost: f64,
    pub score: f64,
    pub stop: Option<String>,
    pub max_steps: usize,
    pub input: u64,
    pub cache_read: u64,
    pub output: u64,
    pub recall_calls: u64,
    pub rewrites: u64,
    pub transcript: Option<PathBuf>,
}

impl Row {
    fn parse(dir: &Path, v: &Value) -> Option<Row> {
        let arm = v["arm"].as_str()?.to_string();
        let policy = v["policy"].as_str().unwrap_or("");
        let spec = v["spec"].as_str().unwrap_or("").to_string();
        let kind = if arm == "plain" {
            ArmKind::Plain
        } else if (arm == "curator" || arm == "cur" || policy == "curator") && !spec.contains("at=") {
            ArmKind::Curator
        } else {
            ArmKind::Other
        };
        let transcript = v["transcript"].as_str().map(|t| dir.join(t)).filter(|p| p.is_file());
        Some(Row {
            dir: dir.to_path_buf(),
            task: v["task"].as_str()?.to_string(),
            arm,
            kind,
            rep: v["rep"].as_i64().unwrap_or(0),
            commit: v["commit"].as_str().unwrap_or("").chars().take(12).collect(),
            model: v["model"].as_str().unwrap_or("").to_string(),
            spec,
            steps: v["steps"].as_u64()? as usize,
            cost: v["cost"].as_f64().unwrap_or(0.0),
            score: v["score"].as_f64().unwrap_or(0.0),
            stop: v["stop"].as_str().map(str::to_string),
            max_steps: v["max_steps"].as_u64().unwrap_or(40) as usize,
            input: v["input"].as_u64().unwrap_or(0),
            cache_read: v["cache_read"].as_u64().unwrap_or(0),
            output: v["output"].as_u64().unwrap_or(0),
            recall_calls: v["counters"]["recall_calls"].as_u64().unwrap_or(0),
            rewrites: v["rewrites"].as_u64().unwrap_or(0),
            transcript,
        })
    }

    /// Both arms ended on their own: no step cap, no budget stop.
    pub fn clean(&self) -> bool {
        self.stop.is_none()
    }
}

fn find_results(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if dir.is_file() {
        out.push(dir.to_path_buf());
        return;
    }
    let f = dir.join("results.jsonl");
    if f.is_file() {
        out.push(f);
    }
    if depth == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut subs: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .collect();
    subs.sort();
    for s in subs {
        find_results(&s, depth - 1, out);
    }
}

/// The rows of every `results.jsonl` under the directories (or the files named), in path order.
pub fn load_rows(dirs: &[String]) -> Result<Vec<Row>, String> {
    let mut files = Vec::new();
    for d in dirs {
        super::super::synth::refuse(d)?;
        let p = Path::new(d);
        if !p.exists() {
            return Err(format!("{d}: no such run directory"));
        }
        find_results(p, 3, &mut files);
    }
    files.sort();
    files.dedup();
    let mut rows = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
        let dir = f.parent().unwrap_or(Path::new("."));
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            if let Some(r) = serde_json::from_str::<Value>(line).ok().and_then(|v| Row::parse(dir, &v)) {
                if let Some(t) = &r.transcript {
                    super::super::synth::refuse(&t.to_string_lossy())?;
                }
                rows.push(r);
            }
        }
    }
    Ok(rows)
}

/// The family of a task by its name (the truth set of the tagger, `SIMULATOR-SPEC.md` 7.2), if the name says.
pub fn family_of_task(task: &str) -> Option<&'static str> {
    let t = task.to_ascii_lowercase();
    if t.starts_with("dbg") || t.contains("realm") {
        Some("binary")
    } else if t.contains("asm") {
        Some("asm")
    } else if t.contains("http") {
        Some("http")
    } else if t.ends_with("-chat") || t == "chat" {
        Some("chat")
    } else if t.contains("marathon") || t.contains("long-haul") || t.contains("noisy") {
        Some("logs")
    } else if t.contains("vuln-hunt")
        || t.contains("revert")
        || t.contains("three-sessions")
        || t.contains("stale-doc")
        || t.contains("polyglot")
    {
        Some("code")
    } else {
        None
    }
}

// ---- pairs ----
