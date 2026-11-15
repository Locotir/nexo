use super::*;

// ---- install ------------------------------------------------------------------------------------

#[derive(Debug)]
pub(super) struct Installed {
    pub(super) name: String,
    pub(super) commit: String,
    pub(super) portable_source: String,
}

/// The plugin's name: its manifest's, else the last part of where it came from.
pub(super) fn plugin_name(manifest: &plugins::Manifest, w: &Where) -> anyhow::Result<String> {
    let name = if manifest.name.is_empty() {
        let base = w.loc.trim_end_matches(['/', '\\']).rsplit(['/', '\\', ':']).next().unwrap_or("");
        base.trim_end_matches(".git").to_string()
    } else {
        manifest.name.clone()
    };
    if !plugins::valid_name(&name) {
        bail!("the plugin name {name:?} may only hold letters, digits, . _ - (set \"name\" in plugin.json)");
    }
    Ok(name)
}

/// What the person reads before agreeing: the plugin, where it is from, and everything it adds.
pub(super) fn summary(p: &Plugin, w: &Where, commit: &str, staged_at: &Path) -> String {
    let mut out = vec![format!(
        "Plugin {} {} {}",
        p.name,
        p.manifest.version,
        if p.manifest.author.is_empty() { String::new() } else { format!("by {}", p.manifest.author) }
    )
    .trim_end()
    .to_string()];
    if !p.manifest.description.is_empty() {
        out.push(format!("  {}", p.manifest.description));
    }
    out.push(format!("  from {} (commit {})", w.portable(), short(commit)));
    out.push("It adds:".into());
    // the lines were made with the files still in the staging folder: show where they will live
    let final_root = p.root.display().to_string();
    let staged = staged_at.display().to_string();
    out.extend(p.contents().lines().into_iter().map(|l| l.replace(&staged, &final_root)));
    out.push(
        "Hooks and MCP servers run programs on this machine with your permissions. Install plugins you trust.".into(),
    );
    out.join("\n")
}

pub(super) fn read_plugin(
    config_dir: &Path,
    name_hint: &str,
    root: &Path,
    w: &Where,
    commit: &str,
) -> anyhow::Result<Plugin> {
    let manifest = plugins::Manifest::read(root).map_err(|e| anyhow!(e))?;
    let name =
        plugin_name(&manifest, w).or_else(|e| if name_hint.is_empty() { Err(e) } else { Ok(name_hint.to_string()) })?;
    let entry = Entry {
        source: w.loc.clone(),
        git_ref: w.git_ref.clone(),
        commit: commit.to_string(),
        version: manifest.version.clone(),
        enabled: true,
        marketplace: w.marketplace.clone(),
        scope: "user".into(),
    };
    Ok(Plugin { root: plugins::plugins_dir(config_dir).join(&name), name, manifest, entry })
}

/// Installs `spec`. None when the person said no.
pub(super) fn install(config_dir: &Path, cwd: &Path, spec: &str, ask: Ask) -> anyhow::Result<Option<Installed>> {
    let w = resolve(config_dir, cwd, spec)?;
    let (st, src, commit) = stage(config_dir, &w, false)?;
    // read the plugin as if it already sat in its place, so the paths it shows are the final ones
    let mut plugin = read_plugin(config_dir, "", &src, &w, &commit)?;
    let mut reg = Registry::load(config_dir);
    if reg.plugins.contains_key(&plugin.name) || plugin.root.exists() {
        bail!(
            "{} is already installed; `nexo plugin update {}` fetches the newest, or `nexo plugin remove {}` first",
            plugin.name,
            plugin.name,
            plugin.name
        );
    }
    let real_root = plugin.root.clone();
    plugin.root = src.clone();
    let mut text = summary(&plugin, &w, &commit, &src);
    text = text.replace(&src.display().to_string(), &real_root.display().to_string());
    if !ask(&text)? {
        println!("not installed");
        return Ok(None);
    }
    swap_in(&src, &real_root)?;
    drop(st);
    let mut entry = plugin.entry.clone();
    entry.enabled = true;
    reg.plugins.insert(plugin.name.clone(), entry);
    reg.save(config_dir)?;
    Ok(Some(Installed { name: plugin.name, commit, portable_source: w.portable() }))
}

/// Marks an installed plugin as installed for a project.
pub(super) fn set_scope(config_dir: &Path, name: &str, scope: &str) -> anyhow::Result<()> {
    let mut reg = Registry::load(config_dir);
    if let Some(e) = reg.plugins.get_mut(name) {
        e.scope = scope.into();
    }
    reg.save(config_dir)?;
    Ok(())
}
