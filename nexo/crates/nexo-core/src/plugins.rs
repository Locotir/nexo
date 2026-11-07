//! Plugins: a folder (or git repo) that bundles commands, sub-agents, skills, hooks and MCP servers,
//! so a team ships one install to everyone. The layout is the Claude Code plugin format, so the
//! plugins written for it work as they are:
//!
//! ```text
//! my-plugin/
//!   .nexo-plugin/plugin.json   (or .claude-plugin/plugin.json: name, version, description, author)
//!   commands/*.md              /my-plugin:command
//!   agents/*.md                sub-agent my-plugin:agent
//!   skills/<name>/SKILL.md     skill my-plugin:name
//!   hooks/hooks.json           hooks (their commands may use ${CLAUDE_PLUGIN_ROOT})
//!   .mcp.json                  MCP servers (command, args and env may use ${CLAUDE_PLUGIN_ROOT})
//! ```
//!
//! Installed plugins live in `<config dir>/plugins/<name>`, next to a small `.registry.json` that
//! says where each came from, which commit it is pinned to and whether it is enabled. This module
//! reads all of that and hands the pieces to the loaders nexo already has (skills, commands and
//! agents in `compat`, hooks, `.mcp.json`); fetching and installing is the `nexo plugin` command.
//! Everything takes the config folder as an argument, so tests use temporary ones.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{HookConfig, McpServerConfig};

/// The folders a plugin's manifest (`plugin.json`) is looked for in, nexo's own first.
pub const MANIFEST_DIRS: [&str; 2] = [".nexo-plugin", ".claude-plugin"];
/// The variables replaced by the plugin's folder in hook commands and MCP server settings.
pub const ROOT_VARS: [&str; 2] = ["${CLAUDE_PLUGIN_ROOT}", "${NEXO_PLUGIN_ROOT}"];

/// `<config dir>/plugins`.
pub fn plugins_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("plugins")
}

/// `text` with the plugin root variables replaced by `root`.
pub fn expand_root(text: &str, root: &Path) -> String {
    let r = root.display().to_string();
    ROOT_VARS.iter().fold(text.to_string(), |t, v| t.replace(v, &r))
}

/// A plugin or marketplace name: letters, digits, `.`, `_` and `-`, not starting with a dot.
pub fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && !n.starts_with('.')
        && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The manifest file of the plugin in `root`, if it has one.
pub fn manifest_file(root: &Path) -> Option<PathBuf> {
    MANIFEST_DIRS.iter().map(|d| root.join(d).join("plugin.json")).find(|f| f.is_file())
}

/// What `plugin.json` says. Everything but the paths is for display.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    /// Extra folders of commands, agents and skills (relative to the plugin), besides the usual ones.
    pub commands: Vec<String>,
    pub agents: Vec<String>,
    pub skills: Vec<String>,
    /// Hooks and MCP servers: a path to a file, or the settings written inline.
    pub hooks: Vec<Value>,
    pub mcp: Vec<Value>,
}

fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        _ => Vec::new(),
    }
}

fn values(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(a)) => a.clone(),
        Some(x) => vec![x.clone()],
        None => Vec::new(),
    }
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if !v.is_object() {
            return Err("plugin.json is not a JSON object".into());
        }
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
        let author = match v.get("author") {
            Some(Value::String(a)) => a.clone(),
            Some(a) => a.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            None => String::new(),
        };
        Ok(Manifest {
            name: s("name"),
            version: s("version"),
            description: s("description"),
            author,
            commands: strings(v.get("commands")),
            agents: strings(v.get("agents")),
            skills: strings(v.get("skills")),
            hooks: values(v.get("hooks")),
            mcp: values(v.get("mcpServers")),
        })
    }

    /// The manifest of the plugin in `root`. A folder with none is not a plugin.
    pub fn read(root: &Path) -> Result<Manifest, String> {
        let f = manifest_file(root).ok_or_else(|| {
            format!("{} has no .nexo-plugin/plugin.json or .claude-plugin/plugin.json", root.display())
        })?;
        let text = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        Manifest::parse(&text).map_err(|e| format!("{}: {e}", f.display()))
    }
}

/// `rel` below `root`, if it stays inside it (no `..`, not absolute).
fn inside(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = Path::new(rel.trim());
    let ok = rel.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    ok.then(|| root.join(rel))
}

/// What the registry knows of an installed plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    /// Where it came from: a git URL or a folder, as given.
    pub source: String,
    /// The branch, tag or commit asked for, if any.
    pub git_ref: Option<String>,
    /// The commit it is pinned to (`local` for a folder that is not a repository).
    pub commit: String,
    pub version: String,
    pub enabled: bool,
    /// The marketplace it was found in, if any.
    pub marketplace: Option<String>,
    /// `project` when it was installed for what a project asks for, else `user`. A managed policy can switch either off.
    pub scope: String,
}

impl Default for Entry {
    fn default() -> Entry {
        Entry {
            source: String::new(),
            git_ref: None,
            commit: String::new(),
            version: String::new(),
            enabled: true,
            marketplace: None,
            scope: "user".into(),
        }
    }
}

/// A marketplace: a folder or repo with a `marketplace.json` that lists plugins.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MarketEntry {
    pub source: String,
    pub git_ref: Option<String>,
    pub commit: String,
}

/// `<plugins>/.registry.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Registry {
    pub plugins: BTreeMap<String, Entry>,
    pub marketplaces: BTreeMap<String, MarketEntry>,
}

impl Registry {
    fn file(config_dir: &Path) -> PathBuf {
        plugins_dir(config_dir).join(".registry.json")
    }

    /// The registry; an absent or unreadable file is an empty registry.
    pub fn load(config_dir: &Path) -> Registry {
        std::fs::read_to_string(Registry::file(config_dir))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, config_dir: &Path) -> std::io::Result<()> {
        let f = Registry::file(config_dir);
        std::fs::create_dir_all(f.parent().unwrap_or(Path::new(".")))?;
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(f, text + "\n")
    }
}

/// An installed plugin, read from its folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Plugin {
    pub name: String,
    pub root: PathBuf,
    pub manifest: Manifest,
    pub entry: Entry,
}

/// What a plugin adds, as it would be loaded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Contents {
    /// Namespaced: `plugin:name`.
    pub commands: Vec<String>,
    pub agents: Vec<String>,
    pub skills: Vec<String>,
    pub hooks: Vec<HookConfig>,
    /// Under their final names (`plugin_<plugin>_<server>`).
    pub mcp: BTreeMap<String, McpServerConfig>,
}

impl Contents {
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
            && self.agents.is_empty()
            && self.skills.is_empty()
            && self.hooks.is_empty()
            && self.mcp.is_empty()
    }

    /// Lines for a person: what is added, with the commands that hooks and servers would run.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        let list = |label: &str, items: &[String], out: &mut Vec<String>| {
            if !items.is_empty() {
                out.push(format!("  {label}: {}", items.join(", ")));
            }
        };
        let slash: Vec<String> = self.commands.iter().map(|c| format!("/{c}")).collect();
        list("commands", &slash, &mut out);
        list("agents", &self.agents, &mut out);
        let skills: Vec<String> = self.skills.iter().map(|c| format!("/{c}")).collect();
        list("skills", &skills, &mut out);
        for h in &self.hooks {
            let on = h.matcher.as_deref().map(|m| format!(" [{m}]")).unwrap_or_default();
            out.push(format!("  hook {}{on} runs: {}", h.event, h.command));
        }
        for (name, m) in &self.mcp {
            let what = match &m.url {
                Some(u) => format!("connects to {u}"),
                None => format!("runs: {} {}", m.command, m.args.join(" ")).trim_end().to_string(),
            };
            out.push(format!("  MCP server {name} {what}"));
        }
        if out.is_empty() {
            out.push("  (nothing: no commands, agents, skills, hooks or MCP servers found)".into());
        }
        out
    }
}

fn plugin_server_name(plugin: &str, server: &str) -> String {
    let clean = |s: &str| {
        s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' }).collect::<String>()
    };
    format!("plugin_{}_{}", clean(plugin), clean(server))
}

impl Plugin {
    fn dirs(&self, usual: &str, extra: &[String]) -> Vec<PathBuf> {
        let mut out = vec![self.root.join(usual)];
        out.extend(extra.iter().filter_map(|r| inside(&self.root, r)));
        out.into_iter().filter(|d| d.is_dir()).collect()
    }

    pub fn command_dirs(&self) -> Vec<PathBuf> {
        self.dirs("commands", &self.manifest.commands)
    }

    pub fn agent_dirs(&self) -> Vec<PathBuf> {
        self.dirs("agents", &self.manifest.agents)
    }

    /// Folders that hold `<skill>/SKILL.md` folders.
    pub fn skill_roots(&self) -> Vec<PathBuf> {
        self.dirs("skills", &self.manifest.skills)
    }

    fn json_files(&self, usual: &str, inline: &[Value]) -> Vec<Value> {
        let read = |f: PathBuf| std::fs::read_to_string(f).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
        let mut out: Vec<Value> = read(self.root.join(usual)).into_iter().collect();
        for v in inline {
            match v {
                Value::String(p) => out.extend(inside(&self.root, p).and_then(read)),
                Value::Object(_) => out.push(v.clone()),
                _ => {}
            }
        }
        out
    }

    /// The plugin's hooks, with the root variables replaced; `source` says which plugin they are from.
    pub fn hooks(&self) -> Vec<HookConfig> {
        let mut out = Vec::new();
        let source = format!("plugin {}", self.name);
        for v in self.json_files("hooks/hooks.json", &self.manifest.hooks) {
            let events = match v.get("hooks").and_then(Value::as_object) {
                Some(e) => Some(e),
                None => v.as_object(),
            };
            if let Some(e) = events {
                out.extend(crate::compat::hooks_from_events(e, &source, Some(&self.root)));
            }
        }
        out
    }

    /// The plugin's MCP servers under their final names, the root variables replaced.
    pub fn mcp(&self) -> BTreeMap<String, McpServerConfig> {
        let mut out = BTreeMap::new();
        let mut add = |text: &str| {
            let mut found = crate::mcp_json::parse(text).unwrap_or_default();
            if found.is_empty() {
                // some plugins list the servers at the top level, without `mcpServers`
                found = crate::mcp_json::parse(&format!("{{\"mcpServers\": {text}}}")).unwrap_or_default();
            }
            for (name, mut cfg) in found {
                self.expand_server(&mut cfg);
                out.entry(plugin_server_name(&self.name, &name)).or_insert(cfg);
            }
        };
        for v in self.json_files(".mcp.json", &self.manifest.mcp) {
            let text = v.to_string();
            if v.get("mcpServers").is_none() && self.manifest.mcp.contains(&v) {
                // written inline in the manifest: `{ "name": { "command": ... } }`
                add(&format!("{{\"mcpServers\": {text}}}"));
            } else {
                add(&text);
            }
        }
        out
    }

    fn expand_server(&self, cfg: &mut McpServerConfig) {
        let e = |s: &str| expand_root(s, &self.root);
        cfg.command = e(&cfg.command);
        cfg.args = cfg.args.iter().map(|a| e(a)).collect();
        for v in cfg.env.values_mut().chain(cfg.headers.values_mut()) {
            *v = e(v);
        }
        cfg.url = cfg.url.as_deref().map(e);
        cfg.cwd = cfg.cwd.as_ref().map(|c| PathBuf::from(e(&c.display().to_string())));
    }

    /// Everything the plugin adds, under the names it is loaded with.
    pub fn contents(&self) -> Contents {
        let mut c = Contents::default();
        for d in self.command_dirs() {
            let mut files = Vec::new();
            crate::compat::collect_md(&d, &d, 0, &mut files);
            files.sort();
            c.commands.extend(files.into_iter().map(|(n, _)| format!("{}:{}", self.name, n.to_ascii_lowercase())));
        }
        for d in self.agent_dirs() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            let mut files: Vec<PathBuf> =
                rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "md")).collect();
            files.sort();
            for f in files {
                let stem = f.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                let named = std::fs::read_to_string(&f).ok().and_then(|t| {
                    crate::compat::front_matter(&t).0.into_iter().find(|(k, _)| k == "name").map(|(_, v)| v)
                });
                c.agents.push(format!("{}:{}", self.name, named.filter(|n| !n.is_empty()).unwrap_or(stem)));
            }
        }
        for r in self.skill_roots() {
            c.skills.extend(crate::skills::scan_root(&r, "", Some(&self.name)).into_iter().map(|s| s.name));
        }
        c.hooks = self.hooks();
        c.mcp = self.mcp();
        c
    }
}

/// Every installed plugin, enabled or not, and a line for each one that could not be read.
pub fn installed(config_dir: &Path) -> (Vec<Plugin>, Vec<String>) {
    let reg = Registry::load(config_dir);
    let (mut out, mut problems) = (Vec::new(), Vec::new());
    for (name, entry) in reg.plugins {
        let root = plugins_dir(config_dir).join(&name);
        match Manifest::read(&root) {
            Ok(manifest) => out.push(Plugin { name, root, manifest, entry }),
            Err(e) => problems.push(format!("plugin {name} cannot be loaded: {e}")),
        }
    }
    (out, problems)
}

/// The enabled plugins (and the problems of [`installed`]).
pub fn enabled(config_dir: &Path) -> (Vec<Plugin>, Vec<String>) {
    let (all, problems) = installed(config_dir);
    let policy = crate::managed::current();
    let allowed = |p: &Plugin| {
        policy
            .as_ref()
            .is_none_or(|m| m.extension_allowed(if p.entry.scope == "project" { "project" } else { "user" }, "plugins"))
    };
    (all.into_iter().filter(|p| p.entry.enabled && allowed(p)).collect(), problems)
}

/// Adds the enabled plugins' hooks and MCP servers to `cfg`. A server of the person's own with
/// the same name stays as it is. `cfg.plugins` (what the project asks for) that is not installed
/// becomes a notice with the command to install it.
pub fn apply(cfg: &mut crate::Config, config_dir: &Path) {
    let (plugins, problems) = enabled(config_dir);
    cfg.notices.extend(problems);
    for p in &plugins {
        cfg.hooks.extend(p.hooks());
        for (name, server) in p.mcp() {
            cfg.mcp.entry(name).or_insert(server);
        }
    }
    let reg = Registry::load(config_dir);
    for (name, source) in &cfg.plugins {
        if !reg.plugins.contains_key(name) {
            cfg.notices.push(format!(
                "this project asks for the plugin {name}, which is not installed. Look at what it adds and \
                 install it with: nexo plugin install {source}"
            ));
        }
    }
}

/// The text of `/plugins`: each installed plugin and what it adds, and the ones a project asks for.
pub fn describe(config_dir: &Path, requested: &BTreeMap<String, String>) -> String {
    let (all, problems) = installed(config_dir);
    let mut lines = Vec::new();
    if all.is_empty() {
        lines.push("no plugins installed · `nexo plugin install <git-url|path>` adds one (see docs/PLUGINS.md)".into());
    } else {
        lines.push(format!("{}:", crate::count(all.len(), "plugin")));
    }
    for p in &all {
        let state = if p.entry.enabled { "" } else { "  [disabled]" };
        let version = if p.entry.version.is_empty() { String::new() } else { format!(" {}", p.entry.version) };
        let about =
            if p.manifest.description.is_empty() { String::new() } else { format!("  {}", p.manifest.description) };
        lines.push(format!("{}{version}{state}{about}", p.name));
        lines.extend(p.contents().lines());
    }
    lines.extend(problems);
    for (name, source) in requested {
        if !all.iter().any(|p| &p.name == name) {
            lines.push(format!("{name}  [asked for by this project, not installed]: nexo plugin install {source}"));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(p: PathBuf, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// A plugin folder with a bit of everything, under `root`.
    pub(crate) fn sample(root: &Path, manifest_dir: &str) {
        w(
            root.join(manifest_dir).join("plugin.json"),
            r#"{"name":"acme","version":"1.2.0","description":"Acme tools","author":{"name":"Acme"}}"#,
        );
        w(root.join("commands/ship.md"), "---\ndescription: Ship it\n---\nShip $ARGUMENTS");
        w(root.join("commands/git/sync.md"), "Sync");
        w(root.join("agents/reviewer.md"), "---\ndescription: reviews\ntools: Read, Grep\n---\nReview.");
        w(root.join("skills/lint/SKILL.md"), "---\nname: lint\ndescription: Lint things\n---\nRun the linter.");
        w(
            root.join("hooks/hooks.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/check.sh","timeout":5}]}]}}"#,
        );
        w(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"db":{"command":"${NEXO_PLUGIN_ROOT}/bin/db","args":["--data","${CLAUDE_PLUGIN_ROOT}/data"],"env":{"HOME_DIR":"${CLAUDE_PLUGIN_ROOT}"}}}}"#,
        );
    }

    /// Registers `src` as installed `name` in `cfg` by copying nothing: the registry points at `plugins/name`.
    pub(crate) fn register(cfg: &Path, name: &str, enabled: bool) -> PathBuf {
        let mut reg = Registry::load(cfg);
        reg.plugins.insert(name.into(), Entry { enabled, ..Default::default() });
        reg.save(cfg).unwrap();
        plugins_dir(cfg).join(name)
    }

    #[test]
    fn both_manifest_folders_load_and_the_author_may_be_a_string() {
        for dir in MANIFEST_DIRS {
            let d = tempfile::tempdir().unwrap();
            sample(d.path(), dir);
            let m = Manifest::read(d.path()).unwrap();
            assert_eq!((m.name.as_str(), m.version.as_str(), m.author.as_str()), ("acme", "1.2.0", "Acme"));
        }
        let m =
            Manifest::parse(r#"{"name":"x","author":"Ann","commands":["./more","extra"],"hooks":"./h.json"}"#).unwrap();
        assert_eq!((m.author.as_str(), m.commands.len(), m.hooks.len()), ("Ann", 2, 1));
        assert!(Manifest::parse("[]").is_err());
        let empty = tempfile::tempdir().unwrap();
        assert!(Manifest::read(empty.path()).unwrap_err().contains("no .nexo-plugin"));
    }

    #[test]
    fn contents_are_namespaced_and_the_root_variable_expands() {
        let cfg = tempfile::tempdir().unwrap();
        let root = register(cfg.path(), "acme", true);
        sample(&root, ".claude-plugin");
        let (list, problems) = enabled(cfg.path());
        assert!(problems.is_empty(), "{problems:?}");
        let c = list[0].contents();
        assert_eq!(c.commands, ["acme:git:sync", "acme:ship"]);
        assert_eq!(c.agents, ["acme:reviewer"]);
        assert_eq!(c.skills, ["acme:lint"]);
        let r = root.display().to_string();
        assert_eq!(c.hooks[0].command, format!("{r}/check.sh"));
        assert_eq!((c.hooks[0].event.as_str(), c.hooks[0].matcher.as_deref()), ("pre_tool", Some("bash")));
        assert_eq!(c.hooks[0].source, "plugin acme");
        let db = &c.mcp["plugin_acme_db"];
        assert_eq!(db.command, format!("{r}/bin/db"));
        assert_eq!(db.args, ["--data".to_string(), format!("{r}/data")]);
        assert_eq!(db.env["HOME_DIR"], r);
        assert!(c.lines().iter().any(|l| l.contains("check.sh")) && c.lines().iter().any(|l| l.contains("bin/db")));
    }

    #[test]
    fn mcp_servers_may_be_listed_without_the_wrapper_or_inline_in_the_manifest() {
        let cfg = tempfile::tempdir().unwrap();
        let root = register(cfg.path(), "bare", true);
        w(root.join(".claude-plugin/plugin.json"), r#"{"name":"bare","mcpServers":{"inl":{"command":"inl-srv"}}}"#);
        w(root.join(".mcp.json"), r#"{"flat":{"command":"flat-srv"}}"#);
        let p = &enabled(cfg.path()).0[0];
        let names: Vec<String> = p.mcp().into_keys().collect();
        assert_eq!(names, ["plugin_bare_flat", "plugin_bare_inl"]);
    }

    #[test]
    fn disabled_plugins_are_hidden_and_paths_cannot_leave_the_plugin() {
        let cfg = tempfile::tempdir().unwrap();
        let root = register(cfg.path(), "acme", false);
        sample(&root, ".nexo-plugin");
        assert!(enabled(cfg.path()).0.is_empty());
        assert_eq!(installed(cfg.path()).0.len(), 1);
        assert!(inside(Path::new("/p"), "../x").is_none() && inside(Path::new("/p"), "/etc").is_none());
        assert_eq!(inside(Path::new("/p"), "./a/b"), Some(PathBuf::from("/p/./a/b")));
    }

    #[test]
    fn a_broken_plugin_is_reported_not_fatal() {
        let cfg = tempfile::tempdir().unwrap();
        register(cfg.path(), "gone", true);
        let (list, problems) = enabled(cfg.path());
        assert!(list.is_empty() && problems[0].contains("gone"), "{problems:?}");
    }

    #[test]
    fn apply_adds_hooks_and_servers_but_the_users_own_server_wins_and_missing_plugins_are_noticed() {
        let cfg = tempfile::tempdir().unwrap();
        let root = register(cfg.path(), "acme", true);
        sample(&root, ".nexo-plugin");
        let mut c = crate::Config::default();
        c.mcp.insert("plugin_acme_db".into(), McpServerConfig { command: "mine".into(), ..Default::default() });
        c.plugins.insert("other".into(), "https://example.test/other.git".into());
        apply(&mut c, cfg.path());
        assert_eq!(c.hooks.len(), 1);
        assert_eq!(c.mcp["plugin_acme_db"].command, "mine");
        assert!(
            c.notices.iter().any(|n| n.contains("nexo plugin install https://example.test/other.git")),
            "{:?}",
            c.notices
        );
        let text = describe(cfg.path(), &c.plugins);
        assert!(text.contains("acme") && text.contains("/acme:ship") && text.contains("not installed"), "{text}");
    }
}
