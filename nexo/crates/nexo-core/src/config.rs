//! Configuration: a TOML file per user (`~/.nexo/config.toml`), one
//! per project (`.nexo/config.toml`), environment variables on top. Keys are
//! never in these files: a provider names the variable (or the command, or
//! the file) that holds its key, or `nexo auth login` keeps it in the
//! credential store (`crate::auth`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;
use crate::permission::PermissionMode;
use crate::pricing::Pricing;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The provider used when none is asked for (a key of `providers`).
    pub provider: Option<String>,
    /// The model used when none is asked for.
    pub model: Option<String>,
    pub providers: BTreeMap<String, ProviderConfig>,
    pub agent: AgentConfig,
    pub permissions: PermissionsConfig,
    pub context: ContextConfig,
    pub mcp: BTreeMap<String, McpServerConfig>,
    pub ui: UiConfig,
    pub web: WebConfig,
    /// Behaviour of the built-in tools (`[tools]`).
    pub tools: ToolsConfig,
    /// What nexo reads of other coding agents' files (`[compat]`).
    pub compat: CompatConfig,
    /// Mail between sessions (`[mail]`).
    pub mail: crate::mail::MailConfig,
    /// The local audit log of tool calls and model requests (`[audit]`, off by default).
    pub audit: AuditConfig,
    /// Shell commands run at points of the loop (`[[hooks]]` blocks).
    pub hooks: Vec<HookConfig>,
    /// Plugins this project asks for, `name = "git-url or path"` (`[plugins]`; `nexo plugin install --scope project`
    /// writes it). Teammates are told to install the ones they lack; nothing is installed on its own.
    pub plugins: BTreeMap<String, String>,
    /// What loading had to tell the person (settings of an untrusted project left out); not read from files.
    #[serde(skip)]
    pub notices: Vec<String>,
    /// The managed policy in force (`/etc/nexo/managed.toml` and its kin), if any; not read from files.
    #[serde(skip)]
    pub managed: Option<std::sync::Arc<crate::managed::ManagedPolicy>>,
}

/// `[audit]`: an append-only JSON-lines log of every tool call and model request, kept on this machine only.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuditConfig {
    pub enabled: bool,
    /// The file to append to (none: `~/.nexo/audit/YYYY-MM.jsonl`, one file per month).
    pub path: Option<String>,
}

impl Config {
    /// What the model picker offers: each provider that is usable now, with its listed models (and its
    /// default one). A session's remembered model comes back only when it is still in this list.
    pub fn model_choices(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (name, p) in &self.providers {
            if !p.ready(name) {
                continue;
            }
            for m in &p.models {
                out.push((name.clone(), m.clone()));
            }
            if let Some(d) = &p.default_model {
                if !p.models.contains(d) {
                    out.push((name.clone(), d.clone()));
                }
            }
        }
        out
    }

    pub fn parse(text: &str) -> Result<Config, toml::de::Error> {
        toml::from_str(text)
    }

    /// User file, then the project's, then the environment (`NEXO_PROVIDER`,
    /// `NEXO_MODEL`, `NEXO_PERMISSION_MODE`, `NEXO_CONTEXT`).
    ///
    /// A project nobody trusted yet (`nexo config trust`) cannot run commands or send keys and files
    /// elsewhere: what could (see [`Config::strip_untrusted`]) is left out, and `notices` says so.
    pub fn load(cwd: &Path) -> Result<Config, ConfigError> {
        // first, so a policy that cannot be read stops everything before any other file is trusted
        let managed = crate::managed::load()?.map(std::sync::Arc::new);
        let project = paths::project_config_file(cwd);
        let mut cfg =
            Config::load_files(&paths::user_config_file(), project.as_deref(), &paths::trusted_projects_file())?;
        // the `.mcp.json` of the project root, under the same trust gate
        if let Some(f) = &project {
            let root = project_root(f);
            let trusted = is_trusted(&root, &paths::trusted_projects_file());
            if let Some(note) = crate::mcp_json::apply(&mut cfg.mcp, &root, trusted) {
                cfg.notices.push(note);
            }
        }
        // before anything reads a Claude Code file: the hooks below, the skills, the instructions
        crate::compat::set_claude_code(cfg.compat.claude_code);
        // the project's `.claude/settings.json` hooks, when the project is trusted
        let (claude, note) = crate::compat::claude_hooks(cwd, &paths::trusted_projects_file());
        cfg.hooks.extend(claude);
        cfg.notices.extend(note);
        crate::plugins::apply(&mut cfg, &paths::config_dir());
        cfg.apply_env();
        cfg.name_auth_entries();
        // last: the managed policy wins over the files, the project and the environment
        cfg.managed = managed;
        cfg.enforce_managed();
        Ok(cfg)
    }

    /// The user file, then the project file (`<root>/.nexo/config.toml`), trimmed unless `<root>`
    /// is listed in `trust_file`.
    pub fn load_files(user: &Path, project: Option<&Path>, trust_file: &Path) -> Result<Config, ConfigError> {
        let mut unread = Vec::new();
        let mut read = |f: &Path| -> Result<Option<(Config, toml::Table)>, ConfigError> {
            if !f.is_file() {
                return Ok(None);
            }
            let text = std::fs::read_to_string(f).map_err(|e| ConfigError::Io(f.to_path_buf(), e.to_string()))?;
            let parse = |e: toml::de::Error| ConfigError::Parse(f.to_path_buf(), one_line(&text, &e));
            let cfg = Config::parse(&text).map_err(parse)?;
            let said: toml::Table = toml::from_str(&text).map_err(parse)?;
            if let Some(n) = unread_keys(&text, &cfg) {
                unread.push(format!("{}: {n}", f.display()));
            }
            Ok(Some((cfg, said)))
        };
        let mut cfg = Config::default();
        if let Some((part, said)) = read(user)? {
            cfg.merge(part, &said);
        }
        if let Some(f) = project {
            if let Some((mut part, said)) = read(f)? {
                let root = project_root(f);
                for h in &mut part.hooks {
                    h.source = "project .nexo/config.toml".into();
                }
                if !is_trusted(&root, trust_file) {
                    let left_out = part.strip_untrusted(&cfg);
                    if !left_out.is_empty() {
                        cfg.notices.push(format!(
                            "{} is not trusted, so these settings of it are ignored: {}. If you trust this \
                             project, run `nexo config trust \"{}\"` and start again.",
                            f.display(),
                            left_out.join(", "),
                            root.display()
                        ));
                    }
                }
                cfg.merge(part, &said);
            }
        }
        cfg.notices.extend(unread);
        Ok(cfg)
    }

    /// Takes out of a project's file what could run a command or send a key, a file or the
    /// conversation somewhere else, or lift a spend limit: a provider's address, key sources, headers,
    /// protocol, `extra` settings and prices (the user's own stay), a default provider the user's file
    /// does not define, MCP servers, hooks, allow rules, the `yolo` and `accept-edits` modes, the
    /// status command, the system prompt file, the web backend's address and key, and a `max_cost` or
    /// `max_cost_day` that is not a positive finite number at most the user's, and a `max_checkpoints` above the
    /// user's (5 if unset); a user's `on_limit = "ask"` stays. Returns their names.
    pub fn strip_untrusted(&mut self, user: &Config) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(n) = self.provider.take() {
            if user.providers.contains_key(&n) {
                self.provider = Some(n);
            } else {
                out.push(format!("provider = \"{n}\""));
            }
        }
        for (field, mine, own) in [
            ("max_cost", &mut self.agent.max_cost, user.agent.max_cost),
            ("max_cost_day", &mut self.agent.max_cost_day, user.agent.max_cost_day),
        ] {
            // Only a lower, positive and finite limit: 0, a negative number or nan reads as "no limit".
            if let Some(m) = *mine {
                if !(m.is_finite() && m > 0.0 && own.is_none_or(|o| m <= o)) {
                    out.push(format!("agent.{field}"));
                    *mine = own;
                }
            }
        }
        // Only fewer checkpoints than the user allows (5 where they set none); a user who asked to be asked stays so.
        if let Some(m) = self.agent.max_checkpoints {
            if m > user.agent.max_checkpoints.unwrap_or(5) {
                out.push("agent.max_checkpoints".into());
                self.agent.max_checkpoints = user.agent.max_checkpoints;
            }
        }
        if user.agent.on_limit == OnLimit::Ask {
            self.agent.on_limit = OnLimit::Ask;
        }
        for (name, p) in self.providers.iter_mut() {
            let own = user.providers.get(name).cloned().unwrap_or_default();
            let mut take = |set: bool, field: &str| {
                if set {
                    out.push(format!("providers.{name}.{field}"));
                }
            };
            take(!p.base_url.is_empty() && p.base_url != own.base_url, "base_url");
            take(p.api_key_env.is_some() && p.api_key_env != own.api_key_env, "api_key_env");
            take(p.api_key_cmd.is_some() && p.api_key_cmd != own.api_key_cmd, "api_key_cmd");
            take(p.api_key_file.is_some() && p.api_key_file != own.api_key_file, "api_key_file");
            take(p.auth.is_some() && p.auth != own.auth, "auth");
            take(p.login.is_some() && p.login != own.login, "login");
            take(!p.headers.is_empty() && p.headers != own.headers, "headers");
            take(!p.protocol.is_empty() && p.protocol != own.protocol, "protocol");
            let n = p.extra.len();
            p.extra.retain(|k, v| own.extra.get(k) == Some(v));
            take(p.extra.len() != n, "extra");
            let n = p.pricing.len();
            p.pricing.retain(|k, v| own.pricing.get(k) == Some(v));
            take(p.pricing.len() != n, "pricing");
            p.protocol = own.protocol;
            p.base_url = own.base_url;
            p.api_key_env = own.api_key_env;
            p.api_key_cmd = own.api_key_cmd;
            p.api_key_file = own.api_key_file;
            p.auth = own.auth;
            p.login = own.login;
            p.headers = own.headers;
        }
        for name in std::mem::take(&mut self.mcp).into_keys() {
            out.push(format!("mcp.{name}"));
        }
        if !std::mem::take(&mut self.agent.worktree_setup).is_empty() {
            out.push("agent.worktree_setup".into());
        }
        if !std::mem::take(&mut self.hooks).is_empty() {
            out.push("hooks".into());
        }
        if !std::mem::take(&mut self.permissions.allow).is_empty() {
            out.push("permissions.allow".into());
        }
        if matches!(self.permissions.mode, PermissionMode::Auto | PermissionMode::AcceptEdits)
            && self.permissions.mode != user.permissions.mode
        {
            out.push(format!("permissions.mode = \"{}\"", self.permissions.mode.as_str()));
            self.permissions.mode = user.permissions.mode;
        }
        // a project may tighten the sandbox, never loosen it
        if self.permissions.sandbox_network && !user.permissions.sandbox_network {
            out.push("permissions.sandbox_network".into());
            self.permissions.sandbox_network = false;
        }
        if !std::mem::take(&mut self.permissions.sandbox_write_dirs).is_empty() {
            out.push("permissions.sandbox_write_dirs".into());
        }
        {
            let rank = |m: crate::sandbox::SandboxMode| match m {
                crate::sandbox::SandboxMode::Off => 0,
                crate::sandbox::SandboxMode::WorkspaceWrite => 1,
                crate::sandbox::SandboxMode::ReadOnly => 2,
            };
            if rank(self.permissions.sandbox) < rank(user.permissions.sandbox) {
                // `off` cannot be told from a key left out, so it is put back without a word
                if self.permissions.sandbox != crate::sandbox::SandboxMode::Off {
                    out.push(format!("permissions.sandbox = \"{}\"", self.permissions.sandbox.as_str()));
                }
                self.permissions.sandbox = user.permissions.sandbox;
            }
        }
        if !self.ui.status_command.is_empty() && self.ui.status_command != user.ui.status_command {
            out.push("ui.status_command".into());
        }
        self.ui.status_command = user.ui.status_command.clone();
        if self.agent.system_prompt_file.take().is_some() {
            out.push("agent.system_prompt_file".into());
        }
        if self.audit != user.audit {
            out.push("audit".into());
            self.audit = user.audit.clone();
        }
        if self.web.url.take().is_some() {
            out.push("web.url".into());
        }
        if self.web.api_key_env.take().is_some() {
            out.push("web.api_key_env".into());
        }
        out
    }

    /// Every provider reads the credential store under its own name unless it says otherwise.
    pub fn name_auth_entries(&mut self) {
        for (name, p) in self.providers.iter_mut() {
            if p.auth.is_none() {
                p.auth = Some(name.clone());
            }
        }
    }

    /// `other` wins for every key its file says (`said`, the file's own table), even one set to its
    /// default value. Sections and maps count key by key: a project's `[context] reader_at` keeps the
    /// user's `curator_spec`, a project's price for one model keeps the user's other prices, a provider
    /// that sets only `default_model` keeps the user's protocol, address and key source. An MCP server
    /// counts whole; rules and hooks add up.
    pub fn merge(&mut self, other: Config, said: &toml::Table) {
        let table = |c: &Config| match toml::Value::try_from(c) {
            Ok(toml::Value::Table(t)) => Some(t),
            _ => None,
        };
        if let (Some(mut base), Some(over)) = (table(self), table(&other)) {
            // Only the servers `other` still holds replace the user's: one a strip took out leaves it.
            if let (Some(toml::Value::Table(names)), Some(toml::Value::Table(b))) =
                (over.get("mcp"), base.get_mut("mcp"))
            {
                for n in names.keys() {
                    b.remove(n);
                }
            }
            overlay(&mut base, &over, said);
            if let Ok(mut merged) = toml::Value::Table(base).try_into::<Config>() {
                merged.permissions.allow = std::mem::take(&mut self.permissions.allow);
                merged.permissions.deny = std::mem::take(&mut self.permissions.deny);
                merged.hooks = std::mem::take(&mut self.hooks);
                merged.notices = std::mem::take(&mut self.notices);
                *self = merged;
            }
        }
        self.permissions.allow.extend(other.permissions.allow);
        self.permissions.deny.extend(other.permissions.deny);
        self.hooks.extend(other.hooks);
        self.notices.extend(other.notices);
    }

    pub fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("NEXO_PROVIDER") {
            self.provider = Some(v);
        }
        if let Ok(v) = std::env::var("NEXO_MODEL") {
            self.model = Some(v);
        }
        if let Some(m) = std::env::var("NEXO_PERMISSION_MODE").ok().and_then(|v| PermissionMode::parse(&v)) {
            self.permissions.mode = m;
        }
        if let Ok(v) = std::env::var("NEXO_CONTEXT") {
            self.context.policy = v;
        }
        if let Ok(v) = std::env::var("NEXO_CURATOR_SPEC") {
            self.context.curator_spec = v;
        }
    }

    /// The provider and model to use: what is configured, else the first
    /// provider whose key is present, else the first provider.
    pub fn pick(&self) -> Option<(String, String)> {
        let name = self
            .provider
            .clone()
            .filter(|p| self.providers.contains_key(p))
            .or_else(|| self.providers.iter().find(|(n, p)| p.needs_key() && p.ready(n)).map(|(k, _)| k.clone()))
            .or_else(|| self.providers.keys().next().cloned())?;
        let p = &self.providers[&name];
        let model = self.model.clone().or_else(|| p.default_model.clone()).or_else(|| p.models.first().cloned())?;
        Some((name, model))
    }
}

/// A parse error on one line: where (line and column) and what, without the quoted source the toml crate adds.
fn one_line(text: &str, e: &toml::de::Error) -> String {
    let Some(span) = e.span() else { return e.message().to_string() };
    let before = &text[..span.start.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    // the line itself names the key
    let said = text.lines().nth(line - 1).map(str::trim).filter(|l| !l.is_empty() && l.chars().count() <= 80);
    match said {
        Some(l) => format!("line {line}, column {col}: {} (`{l}`)", e.message()),
        None => format!("line {line}, column {col}: {}", e.message()),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {0}: {1}")]
    Io(PathBuf, String),
    #[error("cannot parse {0}: {1}")]
    Parse(PathBuf, String),
    #[error("{0}")]
    Managed(#[from] crate::managed::ManagedError),
}

impl Config {
    /// Lets the file tools read and write in `dir` (relative to `cwd`) besides the project, and a
    /// sandboxed command write there unless a managed policy fixes those folders; the folder as
    /// resolved and the allow rules added, or why it is not a folder.
    pub fn add_dir(&mut self, cwd: &Path, dir: &Path) -> Result<(PathBuf, Vec<String>), String> {
        let joined = if dir.is_absolute() { dir.to_path_buf() } else { cwd.join(dir) };
        let real = joined
            .canonicalize()
            .ok()
            .filter(|r| r.is_dir())
            .ok_or_else(|| format!("{}: not a folder", dir.display()))?;
        // the spelling a model writes has no `.` or `..` in it (`proj/../lib` is `lib`)
        let mut abs = PathBuf::new();
        for c in joined.components() {
            match c {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    abs.pop();
                }
                c => abs.push(c),
            }
        }
        // the folder as given and as resolved: on Windows the two can differ (8.3 short names, `\\?\`)
        let mut spellings = vec![rule_path(&abs), rule_path(&real)];
        spellings.dedup();
        let mut added = Vec::new();
        for p in &spellings {
            for tool in ["read", "grep", "glob", "ls", "write", "edit"] {
                let rule = format!("{tool}({p}/**)");
                if !self.permissions.allow.contains(&rule) {
                    self.permissions.allow.push(rule.clone());
                }
                added.push(rule);
            }
        }
        let fixed = self.managed.as_ref().is_some_and(|m| m.permissions.sandbox_write_dirs.is_some());
        if !fixed && !self.permissions.sandbox_write_dirs.contains(&real) {
            self.permissions.sandbox_write_dirs.push(real.clone());
        }
        Ok((real, added))
    }
}

/// Where the rules of a folder start: the path with forward slashes, which the rule globs read on every system.
pub fn rule_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    s.strip_prefix(r"\\?\").unwrap_or(&s).replace('\\', "/").trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ui_tests;

#[cfg(test)]
mod theme_default_tests;

mod hook_settings;
pub use hook_settings::*;
mod provider_settings;
pub use provider_settings::*;
mod agent_settings;
pub use agent_settings::*;
mod ui_settings;
pub use ui_settings::*;
mod tool_settings;
pub use tool_settings::*;
mod unknown_keys;
pub use unknown_keys::*;
mod trust;
pub use trust::*;
