use super::*;

// ---- sources ------------------------------------------------------------------------------------

/// Where a plugin or marketplace is fetched from.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Where {
    /// A git URL or an absolute folder.
    pub(super) loc: String,
    pub(super) git_ref: Option<String>,
    pub(super) marketplace: Option<String>,
}

impl Where {
    /// What a teammate can use to get the same thing: `loc` and `@ref`.
    pub(super) fn portable(&self) -> String {
        match &self.git_ref {
            Some(r) => format!("{}@{r}", self.loc),
            None => self.loc.clone(),
        }
    }
}

pub(super) fn is_url(s: &str) -> bool {
    if s.contains("://") {
        return true;
    }
    // `git@github.com:owner/repo.git`
    match (s.find('@'), s.find(':')) {
        (Some(at), Some(colon)) => at < colon && !s[..at].contains('/') && !s[at..colon].contains('/'),
        _ => false,
    }
}

/// `<source>[@ref]`: the text before the last `@` and the ref, when what follows `@` can be a ref.
pub(super) fn split_ref(spec: &str) -> (&str, Option<&str>) {
    match spec.rsplit_once('@') {
        Some((l, r))
            if !l.is_empty()
                && !r.is_empty()
                && !r.contains([':', '/', '\\'])
                && !(is_url(spec)
                    && !l.contains("://")
                    && !spec.contains("://")
                    && l.matches('@').count() == 0
                    && false) =>
        {
            // `git@host:path` has a colon after the `@`, so it never gets here; `https://u@host/x` has a slash
            (l, Some(r))
        }
        _ => (spec, None),
    }
}

/// A location: a URL as it is, a folder made absolute.
pub(super) fn locate(cwd: &Path, text: &str) -> anyhow::Result<String> {
    if is_url(text) {
        return Ok(text.to_string());
    }
    let p = cwd.join(text);
    let p = nexo_core::paths::canonical(&p).with_context(|| format!("{} is not a folder or a git URL", p.display()))?;
    Ok(p.display().to_string())
}

/// What `spec` means: `name@marketplace`, a bare `name` a marketplace lists, or `<url|folder>[@ref]`.
pub(super) fn resolve(config_dir: &Path, cwd: &Path, spec: &str) -> anyhow::Result<Where> {
    let spec = spec.trim();
    let exists = cwd.join(spec).exists();
    if let Some((name, market)) = spec.rsplit_once('@') {
        if plugins::valid_name(name) && plugins::valid_name(market) && !exists && !is_url(spec) {
            return from_marketplace(config_dir, name, Some(market));
        }
    }
    if plugins::valid_name(spec) && !exists {
        return from_marketplace(config_dir, spec, None);
    }
    let (text, git_ref) = split_ref(spec);
    Ok(Where { loc: locate(cwd, text)?, git_ref: git_ref.map(str::to_string), marketplace: None })
}

pub(super) fn market_root(config_dir: &Path, name: &str) -> PathBuf {
    plugins::plugins_dir(config_dir).join(".marketplaces").join(name)
}

pub(super) fn market_file(root: &Path) -> Option<PathBuf> {
    MANIFEST_DIRS.iter().map(|d| root.join(d).join("marketplace.json")).find(|f| f.is_file())
}

/// `(marketplace name, its plugin entries)` of the marketplace in `root`.
pub(super) fn read_market(root: &Path) -> anyhow::Result<(String, Vec<Value>)> {
    let f = market_file(root).ok_or_else(|| {
        anyhow!("{} has no .nexo-plugin/marketplace.json or .claude-plugin/marketplace.json", root.display())
    })?;
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&f)?).with_context(|| f.display().to_string())?;
    let name = v.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    if !plugins::valid_name(&name) {
        bail!("{}: the marketplace needs a \"name\" of letters, digits, . _ -", f.display());
    }
    let list = v.get("plugins").and_then(Value::as_array).cloned().unwrap_or_default();
    Ok((name, list))
}

pub(super) fn from_marketplace(config_dir: &Path, plugin: &str, market: Option<&str>) -> anyhow::Result<Where> {
    let reg = Registry::load(config_dir);
    let names: Vec<&String> = match market {
        Some(m) if reg.marketplaces.contains_key(m) => vec![reg.marketplaces.get_key_value(m).unwrap().0],
        Some(m) => bail!("no marketplace {m}; add it with `nexo plugin marketplace add <git-url | folder>`"),
        None => reg.marketplaces.keys().collect(),
    };
    let mut found = Vec::new();
    for m in names {
        let root = market_root(config_dir, m);
        let Ok((_, list)) = read_market(&root) else { continue };
        for p in list {
            if p.get("name").and_then(Value::as_str) == Some(plugin) {
                found.push((m.clone(), root.clone(), p));
            }
        }
    }
    match found.len() {
        0 => bail!(
            "no plugin {plugin} in {}; `nexo plugin marketplace list` shows what there is",
            market.unwrap_or("the marketplaces")
        ),
        1 => {
            let (m, root, p) = found.remove(0);
            let mut w = market_source(&root, &p)?;
            w.marketplace = Some(m);
            Ok(w)
        }
        _ => bail!(
            "{plugin} is in several marketplaces ({}); say which: {plugin}@<marketplace>",
            found.iter().map(|f| f.0.as_str()).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Where a marketplace entry says its plugin is: `"./path"` inside the marketplace, a git URL, a
/// GitHub `owner/repo`, or an object (`{"source": "github", "repo": ...}`, `{"source": "url", "url": ...}`).
pub(super) fn market_source(root: &Path, entry: &Value) -> anyhow::Result<Where> {
    let src = entry.get("source");
    let github = |repo: &str| format!("https://github.com/{repo}.git");
    let (loc, git_ref) = match src {
        Some(Value::String(s)) if s.starts_with("./") || s == "." => {
            if s.split('/').any(|c| c == "..") {
                bail!("the marketplace entry points outside its folder: {s}");
            }
            (
                nexo_core::paths::canonical(&root.join(s))
                    .with_context(|| format!("{s} is not in the marketplace"))?
                    .display()
                    .to_string(),
                None,
            )
        }
        Some(Value::String(s)) if is_url(s) => (s.clone(), None),
        Some(Value::String(s)) if s.matches('/').count() == 1 && !s.starts_with('/') => (github(s), None),
        Some(Value::Object(o)) => {
            let get = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_string);
            let loc = match get("source").as_deref() {
                Some("github") => github(&get("repo").ok_or_else(|| anyhow!("the entry has no repo"))?),
                Some("url" | "git") => get("url").ok_or_else(|| anyhow!("the entry has no url"))?,
                other => bail!("the marketplace entry has a source nexo does not read: {other:?}"),
            };
            (loc, get("ref").or_else(|| get("sha")))
        }
        _ => bail!("the marketplace entry has no usable source"),
    };
    Ok(Where { loc, git_ref, marketplace: None })
}
