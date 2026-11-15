//! Loading the policy and applying it: the checks, the clamps and the config it rewrites.

use std::path::Path;
use std::sync::Arc;

use crate::config::{Config, McpServerConfig};
use crate::permission::PermissionMode;
use crate::sandbox::SandboxMode;

use super::*;

/// Reads the managed file. `Ok(None)`: there is none. A file that is there and cannot be read,
/// parsed or understood is an error, never "no policy".
pub fn load() -> Result<Option<ManagedPolicy>, ManagedError> {
    load_from(&path())
}

pub fn load_from(file: &Path) -> Result<Option<ManagedPolicy>, ManagedError> {
    let err = |problem: String| ManagedError { file: file.to_path_buf(), problem };
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(err(format!("cannot be read ({e})"))),
    };
    let mut p = ManagedPolicy::parse(&text).map_err(|e| err(format!("is not valid: {e}")))?;
    p.file = file.to_path_buf();
    Ok(Some(p))
}

/// The policy in force for code that has no [`Config`] at hand (skills and custom commands). In
/// release builds it is read once; a file that cannot be read gives none here, because `Config::load`
/// has already refused to start on it.
pub fn current() -> Option<Arc<ManagedPolicy>> {
    #[cfg(any(test, debug_assertions))]
    if override_path().is_some() {
        return load().ok().flatten().map(Arc::new);
    }
    static CELL: std::sync::OnceLock<Option<Arc<ManagedPolicy>>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| load().ok().flatten().map(Arc::new)).clone()
}

impl ManagedPolicy {
    pub fn parse(text: &str) -> Result<ManagedPolicy, String> {
        let p: ManagedPolicy = toml::from_str(text).map_err(|e| e.to_string())?;
        for h in &p.forced_hooks {
            if h.event_name().is_none() {
                return Err(format!("forced_hooks: unknown event {:?}", h.event));
            }
            if h.command.trim().is_empty() {
                return Err("forced_hooks: a hook has no command".into());
            }
        }
        Ok(p)
    }

    pub(super) fn shown_file(&self) -> String {
        self.file.display().to_string()
    }

    /// The refusal for something the user asked for: `what` and the words every refusal ends with.
    pub fn blocked(&self, what: &str) -> String {
        format!("{what} is blocked by your organization's policy ({})", self.shown_file())
    }

    // ---- providers and models

    /// Whether `provider` may be used at all.
    pub fn provider_allowed(&self, provider: &str) -> bool {
        !any_match(&self.providers.deny, provider)
            && self.providers.allow.as_ref().is_none_or(|a| any_match(a, provider))
    }

    /// Whether `model` of `provider` may be used.
    pub fn model_allowed(&self, provider: &str, model: &str) -> bool {
        if !self.provider_allowed(provider) {
            return false;
        }
        let full = format!("{provider}/{model}");
        let hit = |g: &[String]| any_match(g, model) || any_match(g, &full);
        !hit(&self.models.deny) && self.models.allow.as_ref().is_none_or(|a| hit(a))
    }

    pub fn check_model(&self, provider: &str, model: &str) -> Result<(), String> {
        if self.model_allowed(provider, model) {
            Ok(())
        } else {
            Err(self.blocked(&format!("the model {provider}/{model}")))
        }
    }

    // ---- permissions and sandbox

    pub fn check_mode(&self, mode: PermissionMode) -> Result<(), String> {
        if self.disable_bypass_mode && mode == PermissionMode::Auto {
            Err(self.blocked("bypass mode (permission mode yolo)"))
        } else {
            Ok(())
        }
    }

    /// The sandbox mode a session may have, given what was asked for.
    pub fn clamp_sandbox(&self, asked: SandboxMode) -> SandboxMode {
        match self.permissions.sandbox {
            Some(m) if self.permissions.sandbox_forced => m,
            Some(m) if rank(asked) < rank(m) => m,
            _ => asked,
        }
    }

    pub fn check_sandbox(&self, asked: SandboxMode) -> Result<(), String> {
        if self.clamp_sandbox(asked) == asked {
            Ok(())
        } else {
            Err(self.blocked(&format!("sandbox mode {}", asked.as_str())))
        }
    }

    pub fn check_sandbox_network(&self, asked: bool) -> Result<(), String> {
        if asked && self.permissions.sandbox_network == Some(false) {
            Err(self.blocked("network access for sandboxed commands"))
        } else {
            Ok(())
        }
    }

    // ---- MCP

    /// Whether the server called `name` may start.
    pub fn check_mcp(&self, name: &str, server: &McpServerConfig) -> Result<(), String> {
        let target = match &server.url {
            Some(u) => u.clone(),
            None => std::iter::once(server.command.as_str())
                .chain(server.args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" "),
        };
        let m = &self.mcp;
        let denied = any_match(&m.deny, name) || any_match(&m.deny_commands, &target);
        let listed = m.allow.is_some() || m.allow_commands.is_some();
        let allowed = m.allow.as_ref().is_some_and(|a| any_match(a, name))
            || m.allow_commands.as_ref().is_some_and(|a| any_match(a, &target));
        if denied || (listed && !allowed) {
            Err(self.blocked(&format!("the MCP server {name:?}")))
        } else {
            Ok(())
        }
    }

    // ---- hooks, extensions

    pub fn user_hooks_allowed(&self) -> bool {
        self.allow_user_hooks.unwrap_or(true)
    }

    pub fn project_hooks_allowed(&self) -> bool {
        self.allow_project_hooks.unwrap_or(true)
    }

    /// Whether a scope (`"user"` or `"project"`) may bring in `what` (`skills`, `commands`, `plugins`).
    pub fn extension_allowed(&self, scope: &str, what: &str) -> bool {
        let e = &self.extensions;
        let v = match (scope, what) {
            ("user", "plugins") => e.user_plugins,
            ("project", "plugins") => e.project_plugins,
            ("user", "skills") => e.user_skills,
            ("project", "skills") => e.project_skills,
            ("user", "commands") => e.user_commands,
            ("project", "commands") => e.project_commands,
            _ => None,
        };
        v.unwrap_or(true)
    }

    // ---- applying it

    /// Puts the policy over `cfg`: what it forbids is taken out, what it forces is put in, and each
    /// change is said in `cfg.notices`. Running it twice changes nothing more.
    pub fn enforce(&self, cfg: &mut Config) {
        let mut said: Vec<String> = Vec::new();
        let file = self.shown_file();
        let mut say = |what: String| said.push(format!("{what} (blocked by your organization's policy ({file}))"));

        // providers and models
        let gone: Vec<String> = cfg.providers.keys().filter(|n| !self.provider_allowed(n)).cloned().collect();
        for n in &gone {
            cfg.providers.remove(n);
            say(format!("provider {n:?} removed"));
        }
        if cfg.provider.as_deref().is_some_and(|p| !self.provider_allowed(p)) {
            cfg.provider = None;
        }
        for (name, p) in cfg.providers.iter_mut() {
            let n = p.models.len();
            p.models.retain(|m| self.model_allowed(name, m));
            if p.models.len() != n {
                say(format!("models of {name:?} removed from the list"));
            }
            if p.default_model.as_deref().is_some_and(|m| !self.model_allowed(name, m)) {
                p.default_model = None;
                say(format!("default model of {name:?} ignored"));
            }
        }
        if let Some(m) = cfg.model.clone() {
            // a model without a provider is judged under every provider it could run on
            let ok = match cfg.provider.as_deref() {
                Some(p) => self.model_allowed(p, &m),
                None => cfg.providers.keys().any(|p| self.model_allowed(p, &m)),
            };
            if !ok {
                cfg.model = None;
                say(format!("model {m:?} ignored"));
            }
        }

        // permissions
        if self.disable_bypass_mode && cfg.permissions.mode == PermissionMode::Auto {
            cfg.permissions.mode = PermissionMode::Ask;
            say("permission mode yolo changed to ask".into());
        }
        for r in &self.permissions.deny {
            if !cfg.permissions.deny.contains(r) {
                cfg.permissions.deny.push(r.clone());
            }
        }
        for r in &self.permissions.allow {
            if !cfg.permissions.allow.contains(r) {
                cfg.permissions.allow.push(r.clone());
            }
        }
        let asked = cfg.permissions.sandbox;
        let clamped = self.clamp_sandbox(asked);
        if clamped != asked {
            cfg.permissions.sandbox = clamped;
            say(format!("sandbox {} changed to {}", asked.as_str(), clamped.as_str()));
        }
        if cfg.permissions.sandbox_network && self.permissions.sandbox_network == Some(false) {
            cfg.permissions.sandbox_network = false;
            say("sandbox network access removed".into());
        }
        if let Some(dirs) = &self.permissions.sandbox_write_dirs {
            if cfg.permissions.sandbox_write_dirs != *dirs {
                cfg.permissions.sandbox_write_dirs = dirs.clone();
                say("sandbox write directories replaced".into());
            }
        }

        // MCP servers
        let names: Vec<String> = cfg.mcp.keys().cloned().collect();
        for n in names {
            if self.check_mcp(&n, &cfg.mcp[&n]).is_err() {
                cfg.mcp.remove(&n);
                say(format!("MCP server {n:?} not started"));
            }
        }

        // hooks: the user's and the project's as allowed, then the forced ones
        let mut removed = 0;
        cfg.hooks.retain(|h| {
            if h.source == SOURCE {
                return false;
            }
            let keep =
                if h.source.starts_with("project") { self.project_hooks_allowed() } else { self.user_hooks_allowed() };
            removed += usize::from(!keep);
            keep
        });
        if removed > 0 {
            say(format!("{removed} hook(s) of the user or the project turned off"));
        }
        for h in &self.forced_hooks {
            let mut h = h.clone();
            h.source = SOURCE.into();
            cfg.hooks.push(h);
        }

        if self.disable_web && cfg.web.enabled {
            cfg.web.enabled = false;
            say("web tools turned off".into());
        }

        if self.audit.enabled {
            if !cfg.audit.enabled {
                say("audit log turned on".into());
            }
            cfg.audit.enabled = true;
            if let Some(p) = &self.audit.path {
                if cfg.audit.path.as_ref() != Some(p) {
                    say(format!("audit log path set to {p}"));
                }
                cfg.audit.path = Some(p.clone());
            }
        }

        for s in said {
            if !cfg.notices.contains(&s) {
                cfg.notices.push(s);
            }
        }
    }

    /// Every value the policy forces, as `(key, value)`, for `/status` and `nexo doctor`.
    pub fn summary(&self) -> Vec<(String, String)> {
        let list = |v: &[String]| v.join(", ");
        let mut out: Vec<(String, String)> = Vec::new();
        let mut add = |k: &str, v: String| out.push((k.to_string(), v));
        if !self.permissions.deny.is_empty() {
            add("permissions.deny", list(&self.permissions.deny));
        }
        if !self.permissions.allow.is_empty() {
            add("permissions.allow", list(&self.permissions.allow));
        }
        if let Some(m) = self.permissions.sandbox {
            let how = if self.permissions.sandbox_forced { "forced" } else { "at least" };
            add("permissions.sandbox", format!("{} ({how})", m.as_str()));
        }
        if let Some(n) = self.permissions.sandbox_network {
            add("permissions.sandbox_network", if n { "user decides".into() } else { "off".into() });
        }
        if let Some(d) = &self.permissions.sandbox_write_dirs {
            add(
                "permissions.sandbox_write_dirs",
                d.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "),
            );
        }
        if self.disable_bypass_mode {
            add("disable_bypass_mode", "true".into());
        }
        if self.disable_web {
            add("disable_web", "true".into());
        }
        if self.audit.enabled {
            add("audit.enabled", "true".into());
            if let Some(p) = &self.audit.path {
                add("audit.path", p.clone());
            }
        }
        for (k, v) in [("providers", &self.providers), ("models", &self.models)] {
            if let Some(a) = &v.allow {
                add(&format!("{k}.allow"), list(a));
            }
            if !v.deny.is_empty() {
                add(&format!("{k}.deny"), list(&v.deny));
            }
        }
        if let Some(a) = &self.mcp.allow {
            add("mcp.allow", list(a));
        }
        if !self.mcp.deny.is_empty() {
            add("mcp.deny", list(&self.mcp.deny));
        }
        if let Some(a) = &self.mcp.allow_commands {
            add("mcp.allow_commands", list(a));
        }
        if !self.mcp.deny_commands.is_empty() {
            add("mcp.deny_commands", list(&self.mcp.deny_commands));
        }
        if let Some(b) = self.allow_user_hooks {
            add("allow_user_hooks", b.to_string());
        }
        if let Some(b) = self.allow_project_hooks {
            add("allow_project_hooks", b.to_string());
        }
        if !self.forced_hooks.is_empty() {
            let h: Vec<String> = self.forced_hooks.iter().map(|h| format!("{}: {}", h.event, h.command)).collect();
            add("forced_hooks", h.join("; "));
        }
        let e = &self.extensions;
        for (k, v) in [
            ("user_plugins", e.user_plugins),
            ("project_plugins", e.project_plugins),
            ("user_skills", e.user_skills),
            ("project_skills", e.project_skills),
            ("user_commands", e.user_commands),
            ("project_commands", e.project_commands),
        ] {
            if let Some(b) = v {
                add(&format!("extensions.{k}"), b.to_string());
            }
        }
        out
    }

    /// The lines of `/status` and `nexo doctor`: the file, then each forced value with its source.
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![format!("managed policy {}", self.shown_file())];
        for (k, v) in self.summary() {
            out.push(format!("  {k} = {v}  (source: {SOURCE})"));
        }
        out
    }
}

impl Config {
    /// Puts the managed policy, when there is one, over this configuration again. Call it after
    /// anything that changes the configuration from the command line.
    pub fn enforce_managed(&mut self) {
        if let Some(p) = self.managed.clone() {
            p.enforce(self);
        }
    }

    /// `Err` with the refusal when the managed policy forbids this permission mode.
    pub fn check_mode(&self, mode: PermissionMode) -> Result<(), String> {
        self.managed.as_ref().map_or(Ok(()), |p| p.check_mode(mode))
    }

    /// `Err` with the refusal when the managed policy forbids this provider and model.
    pub fn check_model(&self, provider: &str, model: &str) -> Result<(), String> {
        self.managed.as_ref().map_or(Ok(()), |p| p.check_model(provider, model))
    }
}
