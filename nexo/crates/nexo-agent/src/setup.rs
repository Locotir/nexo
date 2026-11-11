//! The model setup behind `/model`: what a model can do, gathered from the
//! server, the models.dev catalogue and the presets and then tried with one real
//! request; and the saving of the result in the user's configuration.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use nexo_core::config::ProviderConfig;
use nexo_core::provider::{Effort, ModelCheck, ModelRequest, ReasoningConfig, ServerProbe, ToolSpec};
use nexo_core::stream::StreamEvent;
use nexo_core::Message;
use nexo_providers::{catalogue, presets, probe};
use serde_json::json;
use toml_edit::{Array, Value};

use crate::client::ModelClient;
use crate::config_edit;

/// How long the test request may take (a large local model loads first).
const TRY: Duration = Duration::from_secs(120);

/// Everything known of `model` at the probed server, and how a real request went.
pub async fn check(
    client: Arc<dyn ModelClient>,
    name: &str,
    probe: &ServerProbe,
    cfg: &ProviderConfig,
    model: &str,
) -> ModelCheck {
    let (mut info, mut sources) = probe::model_details(probe, model).await;
    // the catalogue and the preset fill what the server did not say
    if let Some(e) = catalogue::cached().and_then(|c| c.lookup(name, model).cloned()) {
        let mut took = Vec::new();
        if info.context_window.is_none() && e.context.is_some() {
            info.context_window = e.context;
            took.push("context");
        }
        if info.max_output.is_none() && e.output.is_some() {
            info.max_output = e.output;
            took.push("largest reply");
        }
        if info.pricing.is_none() && e.pricing.is_some() {
            info.pricing = e.pricing;
            took.push("prices");
        }
        info.reasoning |= e.reasoning == Some(true);
        info.tools |= e.tools == Some(true);
        info.vision |= e.vision == Some(true);
        if !took.is_empty() {
            sources.push(format!("{} from the models.dev catalogue", took.join(", ")));
        }
    }
    if let Some(p) = presets().get(name) {
        if info.context_window.is_none() {
            if let Some(n) = p.context_window.get(model) {
                info.context_window = Some(*n);
                sources.push(format!("context {n} from the {name} preset"));
            }
        }
        if info.pricing.is_none() {
            if let Some(pr) = p.pricing_for(model) {
                info.pricing = Some(pr);
                sources.push(format!("prices from the {name} preset"));
            }
        }
    }
    // a server on one's own hardware costs nothing per token
    if info.pricing.is_none() && matches!(probe.kind.as_str(), "ollama" | "llama.cpp" | "lmstudio" | "vllm") {
        info.pricing = Some(nexo_core::Pricing { input: 0.0, output: 0.0, cache_read: 0.0, cache_write: 0.0 });
        sources.push(format!("prices 0 from {}: it runs on your own hardware", probe::kind_label(&probe.kind)));
    }
    info.efforts = probe::effort_levels(&probe.protocol, &probe.kind, &probe.base_url, model, info.reasoning);

    let mut check = ModelCheck { info, sources, ..Default::default() };
    let level = check.info.efforts.iter().find(|l| *l != "off").cloned();
    let mut outcome = try_once(&client, cfg, model, level.as_deref()).await;
    if let (Err(e), Some(l)) = (&outcome, &level) {
        if e.to_ascii_lowercase().contains("reason") || e.to_ascii_lowercase().contains("think") {
            check.sources.push(format!("the server refused the reasoning level `{l}`: its reasoning cannot be set"));
            check.info.efforts.clear();
            outcome = try_once(&client, cfg, model, None).await;
        }
    }
    match outcome {
        Ok(t) => {
            check.latency_ms = Some(t.ms);
            check.reply = Some(t.text.trim().chars().take(200).collect());
            check.called_tool = t.called_tool;
            check.reasoned = t.reasoned;
            if t.called_tool {
                check.info.tools = true;
            }
            if t.reasoned && !check.info.reasoning {
                check.info.reasoning = true;
                check.sources.push("it reasons: the test reply carried reasoning".into());
            }
        }
        Err(e) => check.error = Some(e),
    }
    check
}

struct Tried {
    ms: u64,
    text: String,
    called_tool: bool,
    reasoned: bool,
}

/// One short request with a test tool the model is asked to call.
async fn try_once(
    client: &Arc<dyn ModelClient>,
    cfg: &ProviderConfig,
    model: &str,
    level: Option<&str>,
) -> Result<Tried, String> {
    let mut req = ModelRequest::new(
        model,
        "You are being checked by a setup program. Call the `ping` tool once, with no arguments. Do not write anything else.",
        vec![Message::user("ping")],
    );
    req.max_tokens = 1024;
    req.cache = false;
    req.tools = vec![ToolSpec {
        name: "ping".into(),
        description: "Answers pong. Call it once.".into(),
        parameters: json!({"type": "object", "properties": {}}),
    }];
    req.reasoning = level.map(|l| match Effort::parse(l) {
        Some(e) => ReasoningConfig::Effort(e),
        None => ReasoningConfig::Effort(Effort::Medium),
    });
    let start = Instant::now();
    let run = async {
        let mut s = client.stream(cfg, &req, 0).await.map_err(|e| e.human())?;
        let mut t = Tried { ms: 0, text: String::new(), called_tool: false, reasoned: false };
        while let Some(ev) = s.next().await {
            match ev {
                StreamEvent::TextDelta(d) => t.text.push_str(&d),
                StreamEvent::ReasoningDelta(_) | StreamEvent::RedactedReasoning { .. } => t.reasoned = true,
                StreamEvent::ToolCallStart { .. } => t.called_tool = true,
                StreamEvent::Error(e) => return Err(e.human()),
                StreamEvent::Done => break,
                _ => {}
            }
        }
        Ok(t)
    };
    let mut t = tokio::time::timeout(TRY, run).await.map_err(|_| format!("no reply in {} s", TRY.as_secs()))??;
    t.ms = start.elapsed().as_millis() as u64;
    Ok(t)
}

/// Where a key typed in the setup is kept: a file of its own, readable only by the user.
pub fn key_file(name: &str) -> PathBuf {
    let safe: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    nexo_core::paths::data_dir().join("keys").join(safe)
}

/// Writes a typed key to its file (mode 600) and points `cfg` at it.
pub fn keep_key(name: &str, key: &str, cfg: &mut ProviderConfig) -> anyhow::Result<PathBuf> {
    keep_key_at(key_file(name), key, cfg)
}

/// A key written for one test request only. The file has a name of its own (never shared by two
/// checks) and is removed when this guard is dropped, so a check that is cancelled, aborted or
/// panics leaves nothing behind. The key's own file waits for the save.
#[derive(Debug)]
pub struct TrialKey {
    path: PathBuf,
}

impl TrialKey {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TrialKey {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        live().lock().unwrap_or_else(|e| e.into_inner()).remove(&self.path);
    }
}

/// The trial files this process has open right now: the sweep leaves them alone.
fn live() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static LIVE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> = std::sync::OnceLock::new();
    LIVE.get_or_init(Default::default)
}

const TRIAL: &str = ".trying-";
/// A trial file of another process is old enough to be a leftover after this long.
const STALE: Duration = Duration::from_secs(300);
/// A file this young is never taken for a leftover just because its writer is not in the process list
/// (that list may hide other users' or other containers' processes).
const YOUNG: Duration = Duration::from_secs(10);
/// A whole check (the model's facts, then one or two test requests) ends within this, well below `STALE`,
/// so the file of a check that is still running never looks abandoned.
pub const CHECK_LIMIT: Duration = Duration::from_secs(200);
const _: () = assert!(CHECK_LIMIT.as_secs() < STALE.as_secs());

/// Writes a typed key to a file that lasts only for the test request: `cfg` points at it until the
/// guard is dropped. The key's own file is not touched, so a key that fails leaves nothing behind.
pub fn trial_key(name: &str, key: &str, cfg: &mut ProviderConfig) -> anyhow::Result<TrialKey> {
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let own = key_file(name);
    let own = own.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let file = key_file(name).with_file_name(format!("{TRIAL}{own}-{}-{n}-{nanos:x}", std::process::id()));
    // the lock is held from before the file is written until it is listed: a sweep never sees the file
    // of a check that is starting as a leftover
    let mut held = live().lock().unwrap_or_else(|e| e.into_inner());
    let path = keep_key_at(file, key, cfg)?;
    held.insert(path.clone());
    Ok(TrialKey { path })
}

/// A key written beside the saved one, to replace it only when the rest of the save went through. It is
/// a trial file (the sweep clears it if the run dies) and the sweep leaves it alone while this guard
/// lives. `cfg` is pointed at the key's own file, which is where the key will be.
pub fn stage_key(name: &str, key: &str, cfg: &mut ProviderConfig) -> anyhow::Result<TrialKey> {
    let mut scratch = cfg.clone();
    let staged = trial_key(name, key, &mut scratch)?;
    cfg.api_key_file = Some(key_file(name));
    cfg.api_key_cmd = None;
    cfg.api_key_env = None;
    Ok(staged)
}

impl TrialKey {
    /// Moves the staged key over the key's own file: the old key goes in one step, never half written.
    pub fn put_in_place(&self, name: &str) -> std::io::Result<PathBuf> {
        let to = key_file(name);
        std::fs::rename(&self.path, &to)?;
        Ok(to)
    }
}

/// Whether the process that wrote this trial file is surely gone. Known only where the system
/// lists its processes as folders (Linux); elsewhere the file's age decides.
fn writer_gone(name: &str, age: Option<Duration>) -> bool {
    if !age.is_some_and(|a| a > YOUNG) {
        return false;
    }
    let pid = name.rsplit('-').nth(2).and_then(|p| p.parse::<u32>().ok());
    match pid {
        Some(pid) if cfg!(target_os = "linux") && Path::new("/proc/self").exists() => {
            !Path::new(&format!("/proc/{pid}")).exists()
        }
        _ => false,
    }
}

/// Removes the trial files a killed run left in the keys folder: those of this process that no check
/// holds, those whose process is gone, and those of other processes untouched for a while. Called
/// when nexo starts and when the setup opens.
pub fn sweep_trial_keys() -> usize {
    let Ok(dir) = std::fs::read_dir(key_file("x").parent().map(Path::to_path_buf).unwrap_or_default()) else {
        return 0;
    };
    let me = format!("-{}-", std::process::id());
    // held while the folder is scanned: a check that starts meanwhile waits for the scan to end
    let held = live().lock().unwrap_or_else(|e| e.into_inner());
    let mut n = 0;
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with(TRIAL) || held.contains(&e.path()) {
            continue;
        }
        let age = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
        let old = age.is_some_and(|a| a > STALE);
        if (name.contains(&me) || old || writer_gone(&name, age)) && std::fs::remove_file(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

fn keep_key_at(f: PathBuf, key: &str, cfg: &mut ProviderConfig) -> anyhow::Result<PathBuf> {
    if let Some(d) = f.parent() {
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            b.mode(0o700);
            b.create(d)?;
            // a folder that was there already, wider than 700, is narrowed
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
        }
        #[cfg(not(unix))]
        b.create(d)?;
    }
    // created readable only by the user, never for a moment by others; a file that was there
    // already is narrowed before the key goes in
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut file = o.open(&f)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    std::io::Write::write_all(&mut file, key.trim().as_bytes())?;
    cfg.api_key_file = Some(f.clone());
    cfg.api_key_cmd = None;
    cfg.api_key_env = None;
    Ok(f)
}

/// Saves the provider and what is known of `model` in `file`, and makes them the default.
pub fn save(file: &Path, name: &str, cfg: &ProviderConfig, model: &str) -> anyhow::Result<()> {
    // one read, every edit in memory, one write: a failure leaves the file as it was, never half done
    config_edit::edit(file, |doc| {
        let p = ["providers", name];
        let set = config_edit::set_in;
        set(doc, &p, "protocol", Value::from(cfg.protocol.as_str()));
        set(doc, &p, "base_url", Value::from(cfg.base_url.as_str()));
        if let Some(v) = &cfg.api_key_env {
            set(doc, &p, "api_key_env", Value::from(v.as_str()));
        }
        if let Some(c) = &cfg.api_key_cmd {
            set(doc, &p, "api_key_cmd", Value::from(c.as_str()));
        }
        if let Some(f) = &cfg.api_key_file {
            set(doc, &p, "api_key_file", Value::from(f.to_string_lossy().as_ref()));
        }
        config_edit::push_in(doc, &p, "models", model);
        set(doc, &p, "default_model", Value::from(model));
        if let Some(n) = cfg.context_window.get(model) {
            config_edit::set_limit_in(doc, name, model, "context_window", *n);
        }
        if let Some(n) = cfg.max_output.get(model) {
            config_edit::set_limit_in(doc, name, model, "max_output", *n);
        }
        if let Some(pr) = cfg.pricing.get(model) {
            config_edit::set_pricing_in(doc, name, model, *pr);
        }
        if let Some(levels) = cfg.efforts.get(model) {
            let arr: Array = levels.iter().map(String::as_str).collect();
            set(doc, &["providers", name, "efforts"], model, Value::Array(arr));
        }
        if let Some(e) = cfg.effort.get(model) {
            set(doc, &["providers", name, "effort"], model, Value::from(e.as_str()));
        }
        set(doc, &[], "provider", Value::from(name));
        set(doc, &[], "model", Value::from(model));
        Ok(())
    })
}

/// The reasoning level in force for `model`: its own, else the agent's.
pub fn effort_of(cfg: &ProviderConfig, model: &str, fallback: &Option<String>) -> Option<String> {
    cfg.effort.get(model).filter(|e| !e.is_empty()).cloned().or_else(|| fallback.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::Pricing;

    #[cfg(unix)]
    #[test]
    fn the_keys_folder_is_private_and_a_wider_one_is_narrowed() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let d = tempfile::tempdir().unwrap();
        let mut cfg = ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None);
        let fresh = d.path().join("a/keys");
        let f = keep_key_at(fresh.join("k"), " sk-1 ", &mut cfg).unwrap();
        assert_eq!((mode(&fresh), mode(&f)), (0o700, 0o600));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "sk-1");
        let old = d.path().join("old");
        std::fs::create_dir(&old).unwrap();
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(old.join("k"), "x").unwrap();
        std::fs::set_permissions(old.join("k"), std::fs::Permissions::from_mode(0o644)).unwrap();
        keep_key_at(old.join("k"), "sk-2", &mut cfg).unwrap();
        assert_eq!((mode(&old), mode(&old.join("k"))), (0o700, 0o600));
    }

    static DATA: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Tests that point the data folder at a temp dir take turns.
    fn in_temp_data<R>(f: impl FnOnce(&Path) -> R) -> R {
        let _g = DATA.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("NEXO_DATA_DIR", d.path());
        f(d.path())
    }

    #[test]
    fn a_key_tried_goes_to_a_file_of_its_own_per_check_and_leaves_with_its_guard() {
        in_temp_data(|_| {
            let mut cfg = ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None);
            let mut other = cfg.clone();
            let a = trial_key("deepseek", "sk-one", &mut cfg).unwrap();
            let b = trial_key("deepseek", "sk-two", &mut other).unwrap();
            assert_ne!(a.path(), b.path(), "two checks never share a file");
            assert_ne!(a.path(), key_file("deepseek"), "the key's own file waits for the save");
            assert_eq!(std::fs::read_to_string(a.path()).unwrap(), "sk-one");
            assert_eq!(cfg.resolve_key().as_deref(), Some("sk-one"));
            let (pa, pb) = (a.path().to_path_buf(), b.path().to_path_buf());
            drop(a);
            assert!(!pa.exists() && pb.exists(), "dropping one guard removes only its file");
            drop(b);
            assert!(!pb.exists());
            assert!(!key_file("deepseek").exists());
        });
    }

    #[test]
    fn a_check_stopped_midway_removes_its_key_file() {
        let _g = DATA.lock().unwrap_or_else(|e| e.into_inner());
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(stopped_midway());
    }

    async fn stopped_midway() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("NEXO_DATA_DIR", d.path());
        let mut cfg = ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None);
        let guard = trial_key("deepseek", "sk-one", &mut cfg).unwrap();
        let path = guard.path().to_path_buf();
        let task = tokio::spawn(async move {
            let _held = guard;
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        assert!(path.exists(), "the key is on disk while the check runs");
        task.abort();
        let _ = task.await;
        assert!(!path.exists(), "an aborted check leaves no key behind");
    }

    #[test]
    fn leftover_trial_files_are_swept_when_the_setup_opens_but_a_live_one_stays() {
        in_temp_data(|_| {
            let mut cfg = ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None);
            let live = trial_key("deepseek", "sk-live", &mut cfg).unwrap();
            let dir = key_file("x").parent().unwrap().to_path_buf();
            // what a run killed in the middle of a check left, from this pid and from another
            let mine = dir.join(format!(".trying-deepseek-{}-99-1", std::process::id()));
            let theirs = dir.join(".trying-deepseek-1-0-1");
            // a young file of a process this system does not list (another user's, another container's)
            let fresh_theirs = dir.join(".trying-deepseek-4194998-0-1");
            // a process number above any the system gives out: its writer is gone for sure
            let killed = dir.join(".trying-deepseek-4194999-0-1");
            let own = key_file("deepseek");
            for f in [&mine, &theirs, &fresh_theirs, &killed, &own] {
                std::fs::write(f, "sk-old").unwrap();
            }
            let old = std::time::SystemTime::now() - Duration::from_secs(3600);
            std::fs::File::options().write(true).open(&theirs).unwrap().set_modified(old).unwrap();
            // a killed run's file is old enough to go at once, on Linux, where its process can be looked up
            std::fs::File::options().write(true).open(&killed).unwrap().set_modified(old_enough()).unwrap();
            assert_eq!(sweep_trial_keys(), if cfg!(target_os = "linux") { 3 } else { 2 });
            assert!(!mine.exists() && !theirs.exists());
            assert_eq!(killed.exists(), !cfg!(target_os = "linux"), "a killed run's key goes at the next start");
            assert!(fresh_theirs.exists(), "a young file of another process may be a check running there");
            assert!(live.path().exists() && own.exists(), "a live check and a saved key stay");
        });
    }

    /// Older than `YOUNG`, younger than `STALE`.
    fn old_enough() -> std::time::SystemTime {
        std::time::SystemTime::now() - Duration::from_secs(60)
    }

    #[test]
    fn a_writer_missing_from_the_process_list_costs_a_young_file_nothing_and_an_alive_one_never() {
        let mine = format!(".trying-deepseek-{}-1-1", std::process::id());
        let gone = ".trying-deepseek-4194999-1-1";
        assert!(!writer_gone(&mine, Some(Duration::from_secs(60))), "this very process is alive");
        assert!(!writer_gone(&mine, None));
        assert!(!writer_gone(gone, Some(Duration::from_secs(2))), "young: the list may hide a live writer");
        assert!(!writer_gone(gone, None), "age unknown: not removed on a guess");
        assert_eq!(writer_gone(gone, Some(Duration::from_secs(60))), cfg!(target_os = "linux"));
    }

    #[test]
    fn a_check_that_is_starting_cannot_be_swept_before_it_is_listed() {
        in_temp_data(|_| {
            let dir = key_file("x").parent().unwrap().to_path_buf();
            // the sweep is scanning (it holds the list of live files)
            let scanning = live().lock().unwrap();
            let starting = std::thread::spawn(|| {
                let mut cfg = ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None);
                trial_key("deepseek", "sk-starting", &mut cfg).unwrap()
            });
            std::thread::sleep(Duration::from_millis(300));
            let written = |d: &Path| {
                std::fs::read_dir(d)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .any(|e| e.file_name().to_string_lossy().starts_with(TRIAL))
            };
            assert!(!written(&dir), "no file exists before the check is listed");
            drop(scanning);
            let guard = starting.join().unwrap();
            assert!(guard.path().exists());
            assert_eq!(sweep_trial_keys(), 0, "and once listed it stays");
            assert!(guard.path().exists());
        });
    }

    #[test]
    fn a_staged_key_takes_the_saved_ones_place_only_when_asked_and_is_cleared_when_dropped() {
        in_temp_data(|_| {
            let own = key_file("deepseek");
            std::fs::create_dir_all(own.parent().unwrap()).unwrap();
            std::fs::write(&own, "sk-old").unwrap();
            let mut cfg = ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None);
            let staged = stage_key("deepseek", "sk-new", &mut cfg).unwrap();
            assert_eq!(cfg.api_key_file.as_deref(), Some(own.as_path()), "the config names the key's own file");
            assert_eq!(std::fs::read_to_string(&own).unwrap(), "sk-old", "the old key is not touched yet");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(std::fs::metadata(staged.path()).unwrap().permissions().mode() & 0o777, 0o600);
            }
            assert_eq!(sweep_trial_keys(), 0, "the sweep leaves a staged key of this run alone");
            let gone = staged.path().to_path_buf();
            drop(staged);
            assert!(!gone.exists() && std::fs::read_to_string(&own).unwrap() == "sk-old");
            let staged = stage_key("deepseek", "sk-new", &mut cfg).unwrap();
            staged.put_in_place("deepseek").unwrap();
            assert_eq!(std::fs::read_to_string(&own).unwrap(), "sk-new");
        });
    }

    #[test]
    fn a_save_is_one_write_and_an_edit_that_fails_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("config.toml");
        std::fs::write(&f, "# mine\nmodel = \"old\"\n").unwrap();
        let err = config_edit::edit(&f, |doc| {
            config_edit::set_in(doc, &[], "model", Value::from("new"));
            anyhow::bail!("fails midway")
        });
        assert!(err.is_err());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "# mine\nmodel = \"old\"\n");
        let cfg = ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None);
        save(&f, "ollama", &cfg, "qwen3:8b").unwrap();
        let t = std::fs::read_to_string(&f).unwrap();
        assert!(t.starts_with("# mine\n") && t.contains("qwen3:8b") && t.contains("ollama"), "{t}");
        config_edit::restore(&f, Some("# mine\nmodel = \"old\"\n")).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "# mine\nmodel = \"old\"\n");
        config_edit::restore(&f, None).unwrap();
        assert!(!f.exists());
    }

    #[test]
    fn a_saved_model_reads_back_as_the_default_with_its_facts() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("config.toml");
        std::fs::write(&f, "# mine\n[ui]\ntheme = \"nexo\"\n").unwrap();
        let mut cfg = ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None);
        cfg.context_window.insert("qwen3:8b".into(), 8192);
        cfg.pricing.insert("qwen3:8b".into(), Pricing { input: 0.0, output: 0.0, cache_read: 0.0, cache_write: 0.0 });
        cfg.efforts.insert("qwen3:8b".into(), vec!["off".into(), "on".into()]);
        cfg.effort.insert("qwen3:8b".into(), "on".into());
        save(&f, "ollama", &cfg, "qwen3:8b").unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.contains("# mine\n[ui]\ntheme = \"nexo\""), "the rest of the file stays: {text}");
        let back: nexo_core::config::Config = toml::from_str(&text).unwrap();
        assert_eq!(back.provider.as_deref(), Some("ollama"));
        assert_eq!(back.model.as_deref(), Some("qwen3:8b"));
        let p = &back.providers["ollama"];
        assert_eq!(p.base_url, "http://localhost:11434/v1");
        assert_eq!(p.context_window["qwen3:8b"], 8192);
        assert_eq!(p.efforts["qwen3:8b"], ["off", "on"]);
        assert_eq!(effort_of(p, "qwen3:8b", &None).as_deref(), Some("on"));
        assert_eq!(effort_of(p, "other", &Some("high".into())).as_deref(), Some("high"));
    }
}
