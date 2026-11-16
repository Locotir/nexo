//! `nexo providers` and `nexo models`.

use super::*;

/// `nexo providers [--all] [--json]`: the providers in use (presets,
/// configured, and directory entries with a key), or with `--all` every one
/// nexo knows.
pub fn print_providers(cfg: &Config, all: bool, json: bool) -> i32 {
    let presets = nexo_providers::presets();
    let mut rows: Vec<(String, ProviderConfig, &str)> = cfg
        .providers
        .iter()
        .map(|(n, p)| {
            let origin = if presets.contains_key(n) {
                "preset"
            } else if nexo_providers::directory::get(n).is_some() {
                "directory"
            } else {
                "configured"
            };
            (n.clone(), p.clone(), origin)
        })
        .collect();
    if all {
        for e in nexo_providers::directory::all() {
            if !cfg.providers.contains_key(e.id) && nexo_providers::directory::alias(e.id).is_none() {
                let mut p = e.to_config();
                p.auth = Some(e.id.to_string());
                rows.push((e.id.to_string(), p, "directory"));
            }
        }
    }
    if json {
        let list: Vec<serde_json::Value> = rows
            .iter()
            .map(|(n, p, origin)| {
                serde_json::json!({
                    "name": n,
                    "origin": origin,
                    "protocol": p.protocol,
                    "base_url": p.base_url,
                    "key_env": p.api_key_env,
                    "login": p.login,
                    "key": crate::account::key_source(n, p),
                    "ready": p.ready(n),
                    "default_model": p.default_model.clone().or_else(|| p.models.first().cloned()),
                    "models": p.models,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&list).unwrap_or_default());
        return 0;
    }
    println!(
        "{:<16} {:<17} {:<20} {:<22} {:<24} base url",
        "provider", "protocol", "key", "key variable / login", "default model"
    );
    for (name, p, origin) in &rows {
        let var = match (&p.api_key_env, &p.login, &p.api_key_cmd) {
            (Some(v), Some(l), _) => format!("{v} | login {l}"),
            (Some(v), None, _) => v.clone(),
            (None, Some(l), _) => format!("login {l}"),
            (None, None, Some(c)) => format!("$({c})"),
            _ => "-".into(),
        };
        let model = p.default_model.clone().or_else(|| p.models.first().cloned()).unwrap_or_default();
        println!(
            "{:<16} {:<17} {:<20} {:<22} {:<24} {}{}",
            shorten(name, 16),
            p.protocol,
            shorten(&crate::account::key_source(name, p), 20),
            shorten(&var, 22),
            shorten(&model, 24),
            p.base_url,
            if *origin == "configured" { " (configured)" } else { "" }
        );
    }
    if !all {
        println!(
            "\n`nexo providers --all` lists every provider nexo knows ({} more); `nexo guide` explains the setup",
            { nexo_providers::directory::all().iter().filter(|e| !cfg.providers.contains_key(e.id)).count() }
        );
    }
    0
}

pub(super) fn shorten(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

/// The limits of every model known under `name`: the configured ones, the
/// catalogue's and the provider's, with prices and limits resolved the way the
/// agent resolves them.
pub async fn print_models(cfg: &Config, provider: Option<&str>, refresh: bool, json: bool) -> anyhow::Result<i32> {
    // with --json the notes go to stderr, so stdout holds only the list
    macro_rules! say {
        ($($t:tt)*) => {
            if json { eprintln!($($t)*) } else { println!($($t)*) }
        };
    }
    use nexo_providers::catalogue;
    let name = match provider {
        Some(p) => p.to_string(),
        None => cfg
            .pick()
            .map(|(p, _)| p)
            .ok_or_else(|| anyhow::anyhow!("no provider can be used; `nexo doctor` says why"))?,
    };
    let p: &ProviderConfig = cfg.providers.get(&name).ok_or_else(|| anyhow::anyhow!("unknown provider {name:?}"))?;
    if refresh {
        match catalogue::refresh().await {
            Ok(c) => say!(
                "catalogue refreshed: {} providers, cached at {}",
                c.providers.len(),
                catalogue::cache_file().display()
            ),
            Err(e) => say!("catalogue not refreshed: {}", e.message),
        }
    } else if let Some(age) = catalogue::cache_age() {
        say!("catalogue: cached {} day(s) ago ({})", age.as_secs() / 86_400, catalogue::cache_file().display());
    } else {
        say!("catalogue: not fetched yet (`nexo models --refresh`)");
    }
    let mut names: Vec<String> = p.models.clone();
    if let Some(c) = catalogue::cached() {
        for n in c.model_names(&name) {
            if !names.contains(&n) {
                names.push(n);
            }
        }
    }
    let vendor = if p.ready(&name) {
        let client = nexo_providers::Client::new(nexo_providers::registry());
        match tokio::time::timeout(std::time::Duration::from_secs(20), client.list_models(p)).await {
            Ok(Ok(list)) => {
                for m in &list {
                    if !names.contains(&m.id) {
                        names.push(m.id.clone());
                    }
                }
                Some(list)
            }
            Ok(Err(e)) => {
                say!("(the provider's list is not available: {})", e.message);
                None
            }
            Err(_) => {
                say!("(the provider's list did not answer in time)");
                None
            }
        }
    } else {
        say!("(no key for {name}: the provider's list is not asked for; {})", crate::account::how_to_unlock(&name, p));
        None
    };
    if json {
        let list: Vec<serde_json::Value> = names
            .iter()
            .map(|m| {
                let l = nexo_agent::limits::resolve(&name, p, m, vendor.as_deref());
                serde_json::json!({
                    "provider": name,
                    "model": m,
                    "context_window": l.context_window,
                    "max_output": l.max_output,
                    "pricing": l.pricing.map(|pr| serde_json::json!({
                        "input": pr.input, "output": pr.output,
                        "cache_read": pr.cache_read, "cache_write": pr.cache_write,
                    })),
                    "source": l.source,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&list).unwrap_or_default());
        return Ok(0);
    }
    println!("{:<40} {:>9} {:>8} {:>8} {:>8} {:>8}  source", "model", "context", "output", "in", "out", "cache");
    for m in &names {
        let l = nexo_agent::limits::resolve(&name, p, m, vendor.as_deref());
        let k = |v: Option<u32>| v.map(|n| format!("{}k", n / 1000)).unwrap_or_else(|| "-".into());
        let (i, o, c) = match l.pricing {
            Some(pr) => (format!("{:.3}", pr.input), format!("{:.3}", pr.output), format!("{:.3}", pr.cache_read)),
            None => ("-".into(), "-".into(), "-".into()),
        };
        println!(
            "{:<40} {:>9} {:>8} {:>8} {:>8} {:>8}  {}",
            shorten(m, 40),
            k(l.context_window),
            k(l.max_output),
            i,
            o,
            c,
            l.source
        );
    }
    Ok(0)
}
