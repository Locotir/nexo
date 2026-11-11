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

/// Writes a typed key to a file that lasts only for the test request (`drop_trial_key` removes it): the
/// key's own file is not touched until the model is saved, so a key that fails leaves nothing behind.
pub fn trial_key(name: &str, key: &str, cfg: &mut ProviderConfig) -> anyhow::Result<PathBuf> {
    keep_key_at(trial_file(name), key, cfg)
}

fn trial_file(name: &str) -> PathBuf {
    let f = key_file(name);
    let own = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    f.with_file_name(format!(".trying-{own}"))
}

/// Removes the file `trial_key` wrote.
pub fn drop_trial_key(name: &str) {
    let _ = std::fs::remove_file(trial_file(name));
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
    let p = ["providers", name];
    config_edit::set(file, &p, "protocol", Value::from(cfg.protocol.as_str()))?;
    config_edit::set(file, &p, "base_url", Value::from(cfg.base_url.as_str()))?;
    if let Some(v) = &cfg.api_key_env {
        config_edit::set(file, &p, "api_key_env", Value::from(v.as_str()))?;
    }
    if let Some(c) = &cfg.api_key_cmd {
        config_edit::set(file, &p, "api_key_cmd", Value::from(c.as_str()))?;
    }
    if let Some(f) = &cfg.api_key_file {
        config_edit::set(file, &p, "api_key_file", Value::from(f.to_string_lossy().as_ref()))?;
    }
    config_edit::push(file, &p, "models", model)?;
    config_edit::set(file, &p, "default_model", Value::from(model))?;
    if let Some(n) = cfg.context_window.get(model) {
        config_edit::set_limit(file, name, model, "context_window", *n)?;
    }
    if let Some(n) = cfg.max_output.get(model) {
        config_edit::set_limit(file, name, model, "max_output", *n)?;
    }
    if let Some(pr) = cfg.pricing.get(model) {
        config_edit::set_pricing(file, name, model, *pr)?;
    }
    if let Some(levels) = cfg.efforts.get(model) {
        let arr: Array = levels.iter().map(String::as_str).collect();
        config_edit::set(file, &["providers", name, "efforts"], model, Value::Array(arr))?;
    }
    if let Some(e) = cfg.effort.get(model) {
        config_edit::set(file, &["providers", name, "effort"], model, Value::from(e.as_str()))?;
    }
    config_edit::set(file, &[], "provider", Value::from(name))?;
    config_edit::set(file, &[], "model", Value::from(model))?;
    Ok(())
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

    #[test]
    fn a_key_tried_goes_to_a_file_beside_its_own_never_to_its_own() {
        let (own, trial) = (key_file("deepseek"), trial_file("deepseek"));
        assert_ne!(own, trial, "the key's own file waits for the save");
        assert_eq!(own.parent(), trial.parent());
        assert!(trial.file_name().unwrap().to_string_lossy().starts_with(".trying-"));
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
