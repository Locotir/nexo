use super::*;

// ---- update, remove, enable ---------------------------------------------------------------------

pub(super) fn update(config_dir: &Path, cwd: &Path, name: Option<&str>, ask: Ask) -> anyhow::Result<i32> {
    let reg = Registry::load(config_dir);
    let names: Vec<String> = match name {
        Some(n) if reg.plugins.contains_key(n) => vec![n.to_string()],
        Some(n) => bail!("no plugin {n} is installed"),
        None => reg.plugins.keys().cloned().collect(),
    };
    if names.is_empty() {
        // Same wording as `plugin list`; a silent success looks like a hang.
        println!("no plugins installed");
        return Ok(0);
    }
    let mut code = 0;
    for n in names {
        match update_one(config_dir, cwd, &n, ask) {
            Ok(Some(msg)) => println!("{n}: {msg}"),
            Ok(None) => {
                println!("{n}: kept as it is");
                code = 1;
            }
            Err(e) => {
                eprintln!("{n}: {e:#}");
                code = 1;
            }
        }
    }
    Ok(code)
}

/// Some(what happened), or None when the person declined.
pub(super) fn update_one(config_dir: &Path, _cwd: &Path, name: &str, ask: Ask) -> anyhow::Result<Option<String>> {
    let mut reg = Registry::load(config_dir);
    let old = reg.plugins.get(name).cloned().ok_or_else(|| anyhow!("not installed"))?;
    let w = Where { loc: old.source.clone(), git_ref: old.git_ref.clone(), marketplace: old.marketplace.clone() };
    let (st, src, commit) = stage(config_dir, &w, false)?;
    if commit == old.commit && commit != "local" {
        return Ok(Some(format!("up to date ({})", short(&commit))));
    }
    let mut plugin = read_plugin(config_dir, name, &src, &w, &commit)?;
    if plugin.name != name {
        bail!("the source now calls the plugin {}, not {name}; remove it and install again", plugin.name);
    }
    let real_root = plugin.root.clone();
    plugin.root = src.clone();
    let text =
        summary(&plugin, &w, &commit, &src).replace(&src.display().to_string(), &real_root.display().to_string());
    if !ask(&format!("{name} changed ({} -> {}).\n{text}", short(&old.commit), short(&commit)))? {
        return Ok(None);
    }
    swap_in(&src, &real_root)?;
    drop(st);
    reg.plugins.insert(name.to_string(), Entry { enabled: old.enabled, scope: old.scope.clone(), ..plugin.entry });
    reg.save(config_dir)?;
    Ok(Some(format!("updated to {}", short(&commit))))
}

pub(super) fn remove(config_dir: &Path, name: &str) -> anyhow::Result<()> {
    let mut reg = Registry::load(config_dir);
    if reg.plugins.remove(name).is_none() {
        bail!("no plugin {name} is installed");
    }
    // the name came from the registry, which only holds valid names, but never delete a path built from anything else
    if plugins::valid_name(name) {
        let dir = plugins::plugins_dir(config_dir).join(name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("cannot delete {}", dir.display()))?;
        }
    }
    reg.save(config_dir)?;
    Ok(())
}

pub(super) fn set_enabled(config_dir: &Path, name: &str, on: bool) -> anyhow::Result<()> {
    let mut reg = Registry::load(config_dir);
    let e = reg.plugins.get_mut(name).ok_or_else(|| anyhow!("no plugin {name} is installed"))?;
    e.enabled = on;
    reg.save(config_dir)?;
    println!("{name} {}", if on { "enabled" } else { "disabled" });
    Ok(())
}

pub(super) fn list_text(config_dir: &Path, requested: &BTreeMap<String, String>) -> String {
    let (all, problems) = plugins::installed(config_dir);
    let mut out = String::new();
    for p in &all {
        let state = if p.entry.enabled { "enabled" } else { "disabled" };
        let market = p.entry.marketplace.as_deref().map(|m| format!(" via {m}")).unwrap_or_default();
        out.push_str(&format!(
            "{} {} [{state}] {}{market} @ {}\n",
            p.name,
            p.manifest.version,
            p.entry.source,
            short(&p.entry.commit)
        ));
        for l in p.contents().lines() {
            out.push_str(&format!("{l}\n"));
        }
    }
    for p in problems {
        out.push_str(&format!("{p}\n"));
    }
    for (n, s) in requested {
        if !all.iter().any(|p| &p.name == n) {
            out.push_str(&format!("{n}: asked for by this project, not installed (nexo plugin install {s})\n"));
        }
    }
    if out.is_empty() {
        out.push_str("no plugins installed\n");
    }
    out
}

// ---- the project's list -------------------------------------------------------------------------

pub(super) fn project_file(cwd: &Path) -> PathBuf {
    nexo_core::paths::project_config_file(cwd).unwrap_or_else(|| cwd.join(".nexo").join("config.toml"))
}

/// Writes `[plugins] name = "source"` into the project's config file.
pub(super) fn record_in_project(file: &Path, name: &str, source: &str) -> anyhow::Result<()> {
    let mut doc = match std::fs::read_to_string(file) {
        Ok(t) => t.parse::<DocumentMut>().with_context(|| format!("cannot parse {}", file.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DocumentMut::new(),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", file.display())),
    };
    let t = doc.entry("plugins").or_insert_with(|| Item::Table(Table::new()));
    let t = t.as_table_mut().ok_or_else(|| anyhow!("`plugins` in {} is not a table", file.display()))?;
    t.insert(name, value(source));
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(file, doc.to_string()).with_context(|| format!("cannot write {}", file.display()))
}
