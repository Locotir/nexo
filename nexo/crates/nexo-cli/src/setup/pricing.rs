//! `nexo pricing`.

use super::*;

/// `nexo pricing [provider/model] [--set in=..,out=..,cache=..,write=..]`.
pub async fn pricing_cmd(
    cfg: &Config,
    which: Option<&str>,
    provider: Option<&str>,
    model: Option<&str>,
    set: Option<&str>,
) -> anyhow::Result<i32> {
    let (provider, model) = match which {
        Some(w) => match w.split_once('/') {
            Some((p, m)) => (p.to_string(), m.to_string()),
            None => {
                let (p, _) = pick_with(cfg, provider, None, false).map(|p| (p.provider, p.model))?;
                (p, w.to_string())
            }
        },
        None => {
            let picked = pick_with(cfg, provider, model, false)?;
            (picked.provider, picked.model)
        }
    };
    let pc = cfg.providers.get(&provider).ok_or_else(|| anyhow::anyhow!("unknown provider {provider:?}"))?;
    let l = nexo_agent::limits::resolve(&provider, pc, &model, None);
    let Some(set) = set else {
        println!("{provider}/{model}");
        match l.pricing {
            Some(p) => println!(
                "  prices per million: in {} · out {} · cache read {} · cache write {}  ({})",
                p.input, p.output, p.cache_read, p.cache_write, l.source
            ),
            None => println!("  prices: unknown (`nexo pricing {provider}/{model} --set in=..,out=..,cache=..`)"),
        }
        println!("  context window: {}", l.context_window.map(|n| n.to_string()).unwrap_or_else(|| "unknown".into()));
        println!("  largest reply:  {}", l.max_output.map(|n| n.to_string()).unwrap_or_else(|| "unknown".into()));
        return Ok(0);
    };
    let mut p = l.pricing.unwrap_or_default();
    for kv in set.split([',', ';', ' ']).map(str::trim).filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').ok_or_else(|| anyhow::anyhow!("expected key=value, got {kv:?}"))?;
        let v: f64 = v.trim().parse().map_err(|_| anyhow::anyhow!("{k}: not a number: {v:?}"))?;
        match k.trim().to_ascii_lowercase().as_str() {
            "in" | "input" => p.input = v,
            "out" | "output" => p.output = v,
            "cache" | "cache_read" | "cached" => p.cache_read = v,
            "write" | "cache_write" => p.cache_write = v,
            other => anyhow::bail!("unknown key {other:?} (in, out, cache, write)"),
        }
    }
    let file = nexo_core::paths::user_config_file();
    nexo_agent::config_edit::set_pricing(&file, &provider, &model, p)?;
    println!(
        "saved to {}: {provider}/{model} in {} · out {} · cache read {} · cache write {} per million",
        file.display(),
        p.input,
        p.output,
        p.cache_read,
        p.cache_write
    );
    Ok(0)
}
