//! Command and agent files of nexo and the other tools, and their front matter.

use std::path::{Path, PathBuf};

use super::*;

/// A front matter (`key: value` lines between `---` lines) and the text after it.
pub fn front_matter(text: &str) -> (Vec<(String, String)>, String) {
    let text = text.trim_start_matches('\u{feff}');
    let norm;
    let text = if text.contains('\r') {
        norm = text.replace("\r\n", "\n");
        norm.as_str()
    } else {
        text
    };
    if let Some(rest) = text.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            let keys = rest[..end]
                .lines()
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| {
                    (k.trim().to_ascii_lowercase(), v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
                })
                .collect();
            let body = rest[end + 4..].trim_start_matches(|c| c != '\n').trim_start_matches('\n').to_string();
            return (keys, body);
        }
    }
    (Vec::new(), text.to_string())
}

/// One folder a kind of file is read from, lowest priority first in [`sources`].
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub dir: PathBuf,
    /// Short and plain: `project .claude/commands`, `user .nexo/agents`.
    pub origin: String,
    pub foreign: bool,
    /// The plugin this folder belongs to: its items are named `plugin:item`.
    pub plugin: Option<String>,
}

/// The folders to read `nexo_sub` (`commands`) from, and the other tools' equivalents
/// (`claude_sub`, `codex_sub`), lowest priority first: the other tools' user folders, their
/// project folders, then nexo's own user folder and nexo's project folder. A project's folders
/// are left out until the project is trusted.
pub fn sources(
    cwd: &Path,
    config_dir: &Path,
    trust_file: &Path,
    home: Option<&Path>,
    nexo_sub: &str,
    claude_sub: Option<&str>,
    codex_sub: Option<&str>,
) -> Vec<Source> {
    let claude_sub = claude_sub.filter(|_| claude_code());
    sources_with(cwd, config_dir, trust_file, home, nexo_sub, claude_sub, codex_sub)
}

pub(super) fn sources_with(
    cwd: &Path,
    config_dir: &Path,
    trust_file: &Path,
    home: Option<&Path>,
    nexo_sub: &str,
    claude_sub: Option<&str>,
    codex_sub: Option<&str>,
) -> Vec<Source> {
    let mut out = Vec::new();
    // installed plugins come first: everything of the person's own overrides them
    for p in crate::plugins::enabled(config_dir).0 {
        let dirs = match nexo_sub {
            "commands" => p.command_dirs(),
            "agents" => p.agent_dirs(),
            _ => Vec::new(),
        };
        for dir in dirs {
            let origin = format!("plugin {}", p.name);
            out.push(Source { dir, origin, foreign: true, plugin: Some(p.name.clone()) });
        }
    }
    let mut add = |dir: PathBuf, scope: &str, shown: String, foreign: bool| {
        out.push(Source { dir, origin: format!("{scope} {shown}"), foreign, plugin: None });
    };
    let root = project_root(cwd);
    let trusted = crate::config::is_trusted(&root, trust_file);
    if let Some(h) = home {
        if let Some(s) = codex_sub {
            add(h.join(".codex").join(s), "user", format!(".codex/{s}"), true);
        }
        if let Some(s) = claude_sub {
            add(h.join(".claude").join(s), "user", format!(".claude/{s}"), true);
        }
    }
    // a home kept in git: its `.claude` and `.codex` are the user folders above, not a project's
    if trusted && !crate::paths::is_home(&root, home) {
        if let Some(s) = codex_sub {
            add(root.join(".codex").join(s), "project", format!(".codex/{s}"), true);
        }
        if let Some(s) = claude_sub {
            add(root.join(".claude").join(s), "project", format!(".claude/{s}"), true);
        }
    }
    add(config_dir.join(nexo_sub), "user", format!(".nexo/{nexo_sub}"), false);
    if let Some(p) = crate::paths::project_dir(cwd) {
        if p.parent().is_some_and(|r| crate::config::is_trusted(r, trust_file)) {
            add(p.join(nexo_sub), "project", format!(".nexo/{nexo_sub}"), false);
        }
    }
    out
}

/// A command file: `name` is the path below the folder without `.md`, folders joined with `:`.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandFile {
    pub name: String,
    pub description: String,
    pub hint: String,
    pub allowed_tools: String,
    pub body: String,
    pub path: PathBuf,
    pub origin: String,
}

/// All command files, a later source replacing an earlier one with the same name. Names for
/// which `reserved` says true (the built-in commands) are left out.
pub fn commands(
    cwd: &Path,
    config_dir: &Path,
    trust_file: &Path,
    home: Option<&Path>,
    reserved: &dyn Fn(&str) -> bool,
) -> Vec<CommandFile> {
    let mut out: Vec<CommandFile> = Vec::new();
    let policy = crate::managed::current();
    for s in sources(cwd, config_dir, trust_file, home, "commands", Some("commands"), Some("prompts")) {
        // a managed policy can switch off the person's commands and the project's (origin: `user ...`, `project ...`)
        let scope = s.origin.split(' ').next().unwrap_or("");
        if policy.as_ref().is_some_and(|p| !p.extension_allowed(scope, "commands")) {
            continue;
        }
        let mut files = Vec::new();
        collect_md(&s.dir, &s.dir, 0, &mut files);
        files.sort();
        for (name, f) in files {
            let name = match &s.plugin {
                Some(p) => format!("{}:{}", p.to_ascii_lowercase(), name.to_ascii_lowercase()),
                None => name.to_ascii_lowercase(),
            };
            if name.is_empty() || name.contains(char::is_whitespace) || reserved(&name) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            let (keys, body) = front_matter(&text);
            let get = |k: &str| keys.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
            let description = get("description").filter(|d| !d.is_empty()).unwrap_or_else(|| first_line(&body));
            out.retain(|c| c.name != name);
            out.push(CommandFile {
                name,
                description,
                hint: get("argument-hint").unwrap_or_default(),
                allowed_tools: get("allowed-tools").unwrap_or_default(),
                body,
                path: f,
                origin: s.origin.clone(),
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `(name, path)` of the `.md` files below `dir`, up to three folders deep, hidden ones skipped.
pub(crate) fn collect_md(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if p.is_dir() {
            if depth < 3 {
                collect_md(root, &p, depth + 1, out);
            }
        } else if p.extension().and_then(|x| x.to_str()) == Some("md") {
            let rel = p.strip_prefix(root).unwrap_or(&p).with_extension("");
            let name: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            out.push((name.join(":"), p));
        }
    }
}

pub(super) fn first_line(text: &str) -> String {
    let l = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().trim_start_matches('#').trim();
    l.chars().take(70).collect()
}
