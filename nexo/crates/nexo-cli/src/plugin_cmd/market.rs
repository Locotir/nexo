use super::*;

// ---- marketplaces -------------------------------------------------------------------------------

pub(super) fn market_add(config_dir: &Path, cwd: &Path, spec: &str) -> anyhow::Result<String> {
    let (text, git_ref) = split_ref(spec.trim());
    let w = Where { loc: locate(cwd, text)?, git_ref: git_ref.map(str::to_string), marketplace: None };
    let (st, src, commit) = stage(config_dir, &w, true)?;
    let (name, _) = read_market(&src)?;
    let mut reg = Registry::load(config_dir);
    if reg.marketplaces.contains_key(&name) {
        bail!("the marketplace {name} is already added; `nexo plugin marketplace update {name}` fetches the newest");
    }
    swap_in(&src, &market_root(config_dir, &name))?;
    drop(st);
    reg.marketplaces.insert(name.clone(), MarketEntry { source: w.loc, git_ref: w.git_ref, commit });
    reg.save(config_dir)?;
    Ok(name)
}

pub(super) fn market_remove(config_dir: &Path, name: &str) -> anyhow::Result<()> {
    let mut reg = Registry::load(config_dir);
    if reg.marketplaces.remove(name).is_none() {
        bail!("no marketplace {name}");
    }
    if plugins::valid_name(name) {
        let _ = std::fs::remove_dir_all(market_root(config_dir, name));
    }
    reg.save(config_dir)?;
    Ok(())
}

pub(super) fn market_update(config_dir: &Path, _cwd: &Path, name: Option<&str>) -> anyhow::Result<()> {
    let mut reg = Registry::load(config_dir);
    let names: Vec<String> = match name {
        Some(n) if reg.marketplaces.contains_key(n) => vec![n.to_string()],
        Some(n) => bail!("no marketplace {n}"),
        None => reg.marketplaces.keys().cloned().collect(),
    };
    for n in names {
        let old = reg.marketplaces[&n].clone();
        let w = Where { loc: old.source.clone(), git_ref: old.git_ref.clone(), marketplace: None };
        let (st, src, commit) = stage(config_dir, &w, true)?;
        if read_market(&src)?.0 != n {
            bail!("{n}: the source now calls the marketplace something else");
        }
        swap_in(&src, &market_root(config_dir, &n))?;
        drop(st);
        println!("{n}: {}", if commit == old.commit { "up to date" } else { "updated" });
        reg.marketplaces.insert(n, MarketEntry { commit, ..old });
    }
    reg.save(config_dir)?;
    Ok(())
}

pub(super) fn market_list_text(config_dir: &Path) -> String {
    let reg = Registry::load(config_dir);
    let mut out = String::new();
    for (n, m) in &reg.marketplaces {
        out.push_str(&format!("{n}  {} @ {}\n", m.source, short(&m.commit)));
        match read_market(&market_root(config_dir, n)) {
            Ok((_, list)) => {
                for p in list {
                    let get = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("");
                    let about =
                        if get("description").is_empty() { String::new() } else { format!(": {}", get("description")) };
                    out.push_str(&format!("  {}@{n}{about}\n", get("name")));
                }
            }
            Err(e) => out.push_str(&format!("  cannot be read: {e}\n")),
        }
    }
    if out.is_empty() {
        out.push_str("no marketplaces; `nexo plugin marketplace add <git-url | folder>` adds one\n");
    }
    out
}
