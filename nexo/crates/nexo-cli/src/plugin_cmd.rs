//! `nexo plugin install | list | remove | update | enable | disable | marketplace`: manage the plugins
//! in `<config dir>/plugins` (see `docs/PLUGINS.md` and `nexo_core::plugins`). Plugins come from a git
//! repository (a shallow clone through the `git` program) or from a folder, directly or by name from
//! a marketplace. Before anything is installed the person is shown what it adds, hooks and MCP
//! servers with the commands they would run, and agrees (`--yes` when there is no one to ask).

use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context};
use clap::Subcommand;
use nexo_core::plugins::{self, Entry, MarketEntry, Plugin, Registry, MANIFEST_DIRS};
use nexo_core::Config;
use serde_json::Value;
use toml_edit::{value, DocumentMut, Item, Table};

#[derive(Subcommand, Debug)]
pub enum PluginCmd {
    /// Install a plugin: `<git-url | folder>[@ref]`, or `name@marketplace`. Shows what it adds and asks first.
    /// With no argument: install what this project asks for (`[plugins]` in .nexo/config.toml).
    Install {
        source: Option<String>,
        /// Do not ask (needed when nothing can be asked, as in a script).
        #[arg(long, short = 'y')]
        yes: bool,
        /// `user` (the default) or `project`: also record it in .nexo/config.toml so teammates are told to install it.
        #[arg(long, value_parser = ["user", "project"], default_value = "user")]
        scope: String,
    },
    /// The installed plugins and what each adds.
    List,
    /// Delete an installed plugin.
    Remove { name: String },
    /// Fetch the newest version of a plugin (all of them with no name); asks again when the code changed.
    Update {
        name: Option<String>,
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Turn an installed plugin on.
    Enable { name: String },
    /// Turn an installed plugin off without deleting it.
    Disable { name: String },
    /// Lists of plugins a team keeps in a repository or folder (a `marketplace.json`).
    Marketplace {
        #[command(subcommand)]
        what: MarketCmd,
    },
}

#[derive(Subcommand, Debug)]
pub enum MarketCmd {
    /// Add a marketplace: `<git-url | folder>[@ref]`.
    Add { source: String },
    /// The marketplaces and the plugins each lists.
    List,
    /// Forget a marketplace (plugins installed from it stay).
    Remove { name: String },
    /// Fetch the newest version of a marketplace (all of them with no name).
    Update { name: Option<String> },
}

pub fn run(cwd: &Path, config: &Config, what: &PluginCmd) -> anyhow::Result<i32> {
    let dir = nexo_core::paths::config_dir();
    match what {
        PluginCmd::Install { source, yes, scope } => {
            // installing what the project asks for (no source) is project scope too
            let level = if source.is_none() { "project" } else { scope.as_str() };
            if let Some(p) = &config.managed {
                if !p.extension_allowed(level, "plugins") {
                    bail!("{}", p.blocked(&format!("installing {level} plugins")));
                }
            }
            let mut ask = |summary: &str| ask_person(summary, *yes);
            let sources: Vec<String> = match source {
                Some(s) => vec![s.clone()],
                None => config.plugins.values().cloned().collect(),
            };
            if sources.is_empty() {
                bail!("give a source: nexo plugin install <git-url | folder>[@ref] (this project asks for no plugins)");
            }
            let mut code = 0;
            for s in sources {
                if source.is_none()
                    && config.plugins.iter().any(|(n, v)| *v == s && Registry::load(&dir).plugins.contains_key(n))
                {
                    continue;
                }
                match install(&dir, cwd, &s, &mut ask)? {
                    Some(done) => {
                        println!("installed {} (pinned to {})", done.name, short(&done.commit));
                        if level == "project" {
                            set_scope(&dir, &done.name, "project")?;
                        }
                        if scope == "project" {
                            let file = project_file(cwd);
                            record_in_project(&file, &done.name, &done.portable_source)?;
                            println!("recorded in {}: teammates are told to install it", file.display());
                        }
                    }
                    None => code = 1,
                }
            }
            Ok(code)
        }
        PluginCmd::List => {
            print!("{}", list_text(&dir, &config.plugins));
            Ok(0)
        }
        PluginCmd::Remove { name } => {
            remove(&dir, name)?;
            println!("removed {name}");
            Ok(0)
        }
        PluginCmd::Update { name, yes } => {
            let mut ask = |summary: &str| ask_person(summary, *yes);
            update(&dir, cwd, name.as_deref(), &mut ask)
        }
        PluginCmd::Enable { name } => set_enabled(&dir, name, true).map(|_| 0),
        PluginCmd::Disable { name } => set_enabled(&dir, name, false).map(|_| 0),
        PluginCmd::Marketplace { what } => match what {
            MarketCmd::Add { source } => {
                let name = market_add(&dir, cwd, source)?;
                println!("added marketplace {name}; install from it with `nexo plugin install <plugin>@{name}`");
                Ok(0)
            }
            MarketCmd::List => {
                print!("{}", market_list_text(&dir));
                Ok(0)
            }
            MarketCmd::Remove { name } => {
                market_remove(&dir, name)?;
                println!("removed marketplace {name}");
                Ok(0)
            }
            MarketCmd::Update { name } => market_update(&dir, cwd, name.as_deref()).map(|_| 0),
        },
    }
}

// ---- asking -------------------------------------------------------------------------------------

/// Shows `summary` and asks. With `yes` it only shows it.
fn ask_person(summary: &str, yes: bool) -> anyhow::Result<bool> {
    println!("{summary}");
    let stdin = std::io::stdin();
    confirm(yes, stdin.is_terminal(), &mut stdin.lock())
}

/// The answer: yes when `yes`; an error when nobody can be asked; else a line from `input`.
fn confirm(yes: bool, interactive: bool, input: &mut dyn BufRead) -> anyhow::Result<bool> {
    if yes {
        return Ok(true);
    }
    if !interactive {
        bail!("nothing can be asked here, so pass --yes to go ahead (read what it adds above first)");
    }
    print!("Go ahead? [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

type Ask<'a> = &'a mut dyn FnMut(&str) -> anyhow::Result<bool>;

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

// ---- sources ------------------------------------------------------------------------------------

/// Where a plugin or marketplace is fetched from.
#[derive(Debug, Clone, PartialEq)]
struct Where {
    /// A git URL or an absolute folder.
    loc: String,
    git_ref: Option<String>,
    marketplace: Option<String>,
}

impl Where {
    /// What a teammate can use to get the same thing: `loc` and `@ref`.
    fn portable(&self) -> String {
        match &self.git_ref {
            Some(r) => format!("{}@{r}", self.loc),
            None => self.loc.clone(),
        }
    }
}

fn is_url(s: &str) -> bool {
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
fn split_ref(spec: &str) -> (&str, Option<&str>) {
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
fn locate(cwd: &Path, text: &str) -> anyhow::Result<String> {
    if is_url(text) {
        return Ok(text.to_string());
    }
    let p = cwd.join(text);
    let p = nexo_core::paths::canonical(&p).with_context(|| format!("{} is not a folder or a git URL", p.display()))?;
    Ok(p.display().to_string())
}

/// What `spec` means: `name@marketplace`, a bare `name` a marketplace lists, or `<url|folder>[@ref]`.
fn resolve(config_dir: &Path, cwd: &Path, spec: &str) -> anyhow::Result<Where> {
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

fn market_root(config_dir: &Path, name: &str) -> PathBuf {
    plugins::plugins_dir(config_dir).join(".marketplaces").join(name)
}

fn market_file(root: &Path) -> Option<PathBuf> {
    MANIFEST_DIRS.iter().map(|d| root.join(d).join("marketplace.json")).find(|f| f.is_file())
}

/// `(marketplace name, its plugin entries)` of the marketplace in `root`.
fn read_market(root: &Path) -> anyhow::Result<(String, Vec<Value>)> {
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

fn from_marketplace(config_dir: &Path, plugin: &str, market: Option<&str>) -> anyhow::Result<Where> {
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
fn market_source(root: &Path, entry: &Value) -> anyhow::Result<Where> {
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

// ---- fetching -----------------------------------------------------------------------------------

/// A folder under the plugins folder that is removed when it goes away.
struct Staging(PathBuf);

impl Staging {
    fn new(config_dir: &Path) -> anyhow::Result<Staging> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let p = plugins::plugins_dir(config_dir).join(format!(".staging-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).with_context(|| format!("cannot create {}", p.display()))?;
        Ok(Staging(p))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(args: &[&str], dir: Option<&Path>) -> anyhow::Result<String> {
    let mut c = Command::new("git");
    if let Some(d) = dir {
        c.current_dir(d);
    }
    // never ask for a password, and only the usual transports (no `ext::` command URLs)
    let out = c
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ALLOW_PROTOCOL", "file:git:http:https:ssh")
        .stdin(Stdio::null())
        .output()
        .context("cannot run git (is it installed?)")?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn head_of(dir: &Path) -> Option<String> {
    git(&["rev-parse", "HEAD"], Some(dir)).ok().filter(|h| !h.is_empty())
}

fn is_repo(p: &Path) -> bool {
    p.join(".git").exists() || (p.join("HEAD").is_file() && p.join("objects").is_dir())
}

/// A shallow clone of `url` into `dest` (which must not exist), at `git_ref` when given.
fn clone(url: &str, git_ref: Option<&str>, dest: &Path) -> anyhow::Result<String> {
    let d = dest.display().to_string();
    let shallow = ["clone", "--quiet", "--depth", "1"];
    match git_ref {
        None => {
            git(&[&shallow[..], &["--", url, &d]].concat(), None)?;
        }
        Some(r) => {
            // a branch or tag clones shallow; a commit needs the history to check it out
            if git(&[&shallow[..], &["--branch", r, "--", url, &d]].concat(), None).is_err() {
                let _ = std::fs::remove_dir_all(dest);
                git(&["clone", "--quiet", "--", url, &d], None)?;
                git(&["checkout", "--quiet", r], Some(dest))?;
            }
        }
    }
    head_of(dest).ok_or_else(|| anyhow!("the clone has no commit"))
}

/// Copies a folder without `.git` and without links.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let name = e.file_name();
        let t = e.file_type()?;
        if name == ".git" || t.is_symlink() {
            continue;
        }
        if t.is_dir() {
            copy_tree(&e.path(), &to.join(&name))?;
        } else {
            std::fs::copy(e.path(), to.join(&name))?;
        }
    }
    Ok(())
}

/// Gets `w` into the empty folder `dest`; returns the commit it is pinned to. A folder that is a
/// plugin or marketplace is copied as it is (with no `ref`); a git repository or URL is cloned.
fn fetch(w: &Where, dest: &Path, is_market: bool) -> anyhow::Result<String> {
    if is_url(&w.loc) {
        return clone(&w.loc, w.git_ref.as_deref(), dest);
    }
    let p = Path::new(&w.loc);
    if !p.is_dir() {
        bail!("{} is not a folder", w.loc);
    }
    let has_manifest = if is_market { market_file(p).is_some() } else { plugins::manifest_file(p).is_some() };
    if has_manifest && w.git_ref.is_none() {
        copy_tree(p, dest)?;
        return Ok(head_of(p).unwrap_or_else(|| "local".into()));
    }
    if is_repo(p) {
        return clone(&w.loc, w.git_ref.as_deref(), dest);
    }
    bail!("{} is neither a plugin folder (no plugin.json) nor a git repository", w.loc)
}

fn stage(config_dir: &Path, w: &Where, is_market: bool) -> anyhow::Result<(Staging, PathBuf, String)> {
    let st = Staging::new(config_dir)?;
    let dest = st.0.join("src");
    let commit = fetch(w, &dest, is_market)?;
    Ok((st, dest, commit))
}

/// Puts `from` at `to`; an old `to` is moved aside first and removed once the new one is in place.
fn swap_in(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to.parent().unwrap_or(Path::new(".")))?;
    let old = to.with_extension("old-replaced");
    let had = to.exists();
    if had {
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(to, &old)?;
    }
    if let Err(e) = std::fs::rename(from, to) {
        if had {
            let _ = std::fs::rename(&old, to);
        }
        return Err(e).context("cannot move the new files into place");
    }
    if had {
        let _ = std::fs::remove_dir_all(&old);
    }
    Ok(())
}

// ---- install ------------------------------------------------------------------------------------

#[derive(Debug)]
struct Installed {
    name: String,
    commit: String,
    portable_source: String,
}

/// The plugin's name: its manifest's, else the last part of where it came from.
fn plugin_name(manifest: &plugins::Manifest, w: &Where) -> anyhow::Result<String> {
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
fn summary(p: &Plugin, w: &Where, commit: &str, staged_at: &Path) -> String {
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

fn read_plugin(config_dir: &Path, name_hint: &str, root: &Path, w: &Where, commit: &str) -> anyhow::Result<Plugin> {
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
fn install(config_dir: &Path, cwd: &Path, spec: &str, ask: Ask) -> anyhow::Result<Option<Installed>> {
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
fn set_scope(config_dir: &Path, name: &str, scope: &str) -> anyhow::Result<()> {
    let mut reg = Registry::load(config_dir);
    if let Some(e) = reg.plugins.get_mut(name) {
        e.scope = scope.into();
    }
    reg.save(config_dir)?;
    Ok(())
}

// ---- update, remove, enable ---------------------------------------------------------------------

fn update(config_dir: &Path, cwd: &Path, name: Option<&str>, ask: Ask) -> anyhow::Result<i32> {
    let reg = Registry::load(config_dir);
    let names: Vec<String> = match name {
        Some(n) if reg.plugins.contains_key(n) => vec![n.to_string()],
        Some(n) => bail!("no plugin {n} is installed"),
        None => reg.plugins.keys().cloned().collect(),
    };
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
fn update_one(config_dir: &Path, _cwd: &Path, name: &str, ask: Ask) -> anyhow::Result<Option<String>> {
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

fn remove(config_dir: &Path, name: &str) -> anyhow::Result<()> {
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

fn set_enabled(config_dir: &Path, name: &str, on: bool) -> anyhow::Result<()> {
    let mut reg = Registry::load(config_dir);
    let e = reg.plugins.get_mut(name).ok_or_else(|| anyhow!("no plugin {name} is installed"))?;
    e.enabled = on;
    reg.save(config_dir)?;
    println!("{name} {}", if on { "enabled" } else { "disabled" });
    Ok(())
}

fn list_text(config_dir: &Path, requested: &BTreeMap<String, String>) -> String {
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

fn project_file(cwd: &Path) -> PathBuf {
    nexo_core::paths::project_config_file(cwd).unwrap_or_else(|| cwd.join(".nexo").join("config.toml"))
}

/// Writes `[plugins] name = "source"` into the project's config file.
fn record_in_project(file: &Path, name: &str, source: &str) -> anyhow::Result<()> {
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

// ---- marketplaces -------------------------------------------------------------------------------

fn market_add(config_dir: &Path, cwd: &Path, spec: &str) -> anyhow::Result<String> {
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

fn market_remove(config_dir: &Path, name: &str) -> anyhow::Result<()> {
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

fn market_update(config_dir: &Path, _cwd: &Path, name: Option<&str>) -> anyhow::Result<()> {
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

fn market_list_text(config_dir: &Path) -> String {
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

#[cfg(test)]
mod tests;
