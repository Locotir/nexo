//! Day-one compatibility with the files a team already keeps for other coding agents:
//! `CLAUDE.md`, `CLAUDE.local.md` and nested `AGENTS.md` instructions with `@path` imports,
//! commands in `.claude/commands` and `.codex/prompts`, and sub-agents in `.claude/agents`.
//!
//! Everything here takes its folders as arguments, so tests use temporary ones. nexo's own files
//! (`NEXO.md`, `.nexo/commands`, `.nexo/agents`) win over the foreign ones on a clash, and a
//! project's foreign commands and agents are left out until the project is trusted, like nexo's.

use std::path::{Path, PathBuf};

/// How many imports may be nested in one another.
const IMPORT_DEPTH: usize = 5;
/// The most nested instruction files listed.
const NESTED_MAX: usize = 20;

/// The nearest ancestor of `cwd` with a `.git` entry, else `cwd` itself.
pub fn project_root(cwd: &Path) -> PathBuf {
    cwd.ancestors().find(|d| d.join(".git").exists()).unwrap_or(cwd).to_path_buf()
}

/// Where `~/.claude` and `~/.codex` are looked for: `NEXO_COMPAT_HOME` when set (empty turns the
/// lookup off), else the person's home. A custom `NEXO_CONFIG_DIR` marks an isolated setup (tests,
/// containers): then the other tools' user folders are not read unless `NEXO_COMPAT_HOME` says so.
pub fn foreign_home() -> Option<PathBuf> {
    match std::env::var_os("NEXO_COMPAT_HOME") {
        Some(v) if v.is_empty() => None,
        Some(v) => Some(PathBuf::from(v)),
        None if std::env::var_os("NEXO_CONFIG_DIR").is_some_and(|v| !v.is_empty()) => None,
        None => dirs::home_dir(),
    }
}

/// One instructions file, its `@path` imports already placed after the line that names them.
#[derive(Debug, Clone, PartialEq)]
pub struct Instruction {
    pub path: PathBuf,
    pub text: String,
    /// Where it comes from, in a few words (`project`, `user`, `user fallback`).
    pub origin: &'static str,
}

fn read(p: &Path) -> Option<String> {
    let t = std::fs::read_to_string(p).ok()?;
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// The instruction files that apply in `cwd`: the user-level ones first, then the project's from the
/// nearest folder up to the repository root, as nexo always did. In one folder: `NEXO.md`, then `AGENTS.md`, then `CLAUDE.md` (also
/// `.claude/CLAUDE.md`) unless `AGENTS.md` is there, then `CLAUDE.local.md`.
///
/// User level: `NEXO.md` or `AGENTS.md` in `config_dir`; only when neither exists, the other
/// tools' `~/.claude/CLAUDE.md` and `~/.codex/AGENTS.md` (under `home`).
pub fn instructions(cwd: &Path, config_dir: &Path, home: Option<&Path>) -> Vec<Instruction> {
    let mut out = Vec::new();
    let own: Vec<PathBuf> =
        ["NEXO.md", "AGENTS.md"].iter().map(|n| config_dir.join(n)).filter(|p| p.is_file()).collect();
    if own.is_empty() {
        if let Some(h) = home {
            for rel in [".codex/AGENTS.md", ".claude/CLAUDE.md"] {
                push(&mut out, h.join(rel), "user fallback");
            }
        }
    } else {
        for p in own {
            push(&mut out, p, "user");
        }
    }
    let mut p = Some(cwd);
    while let Some(d) = p {
        push(&mut out, d.join("NEXO.md"), "project");
        let agents = d.join("AGENTS.md");
        let had_agents = agents.is_file();
        push(&mut out, agents, "project");
        if !had_agents {
            if d.join("CLAUDE.md").is_file() {
                push(&mut out, d.join("CLAUDE.md"), "project");
            } else {
                push(&mut out, d.join(".claude").join("CLAUDE.md"), "project");
            }
        }
        push(&mut out, d.join("CLAUDE.local.md"), "project (local)");
        if d.join(".git").exists() {
            break;
        }
        p = d.parent();
    }
    out
}

fn push(out: &mut Vec<Instruction>, path: PathBuf, origin: &'static str) {
    if !path.is_file() {
        return;
    }
    let mut stack = vec![path.canonicalize().unwrap_or_else(|_| path.clone())];
    if let Some(text) = read(&path).map(|t| expand_imports(&t, path.parent().unwrap_or(Path::new(".")), 0, &mut stack))
    {
        out.push(Instruction { path, text, origin });
    }
}

/// `text` with the files named by `@relative/path` words placed after the line that names them.
/// Fenced code and words that are not an existing file stay as they are. Depth is limited and a
/// file already being expanded is not entered again. Files that hold keys are never imported.
fn expand_imports(text: &str, dir: &Path, depth: usize, stack: &mut Vec<PathBuf>) -> String {
    let mut out = String::new();
    let mut fenced = false;
    for line in text.lines() {
        out.push_str(line);
        out.push('\n');
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced || depth >= IMPORT_DEPTH {
            continue;
        }
        let mut prev_space = true;
        let mut word = String::new();
        let mut words = Vec::new();
        for c in line.chars().chain(std::iter::once(' ')) {
            if c.is_whitespace() {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                prev_space = true;
            } else if prev_space && c == '@' && word.is_empty() {
                word.push('@');
                prev_space = false;
            } else if word.starts_with('@') {
                word.push(c);
            } else {
                prev_space = false;
            }
        }
        for w in words {
            let rel = w[1..].trim_end_matches(['.', ',', ';', ':', ')', '!', '?']);
            if rel.is_empty() || rel.starts_with('/') || rel.starts_with('~') || Path::new(rel).is_absolute() {
                continue;
            }
            let file = dir.join(rel);
            let Ok(real) = file.canonicalize() else { continue };
            if !real.is_file()
                || stack.contains(&real)
                || crate::paths::holds_keys(&file)
                || crate::paths::holds_keys(&real)
            {
                continue;
            }
            let Some(body) = read(&real) else { continue };
            stack.push(real.clone());
            let inner = expand_imports(&body, real.parent().unwrap_or(dir), depth + 1, stack);
            stack.pop();
            out.push_str(&format!("\n[imported from {rel}]\n{}\n[end of {rel}]\n\n", inner.trim()));
        }
    }
    out.trim_end().to_string()
}

/// Instruction files below the project root that are not loaded (those in a folder the agent
/// is not working in), as paths relative to the root; the agent reads one when it works there.
pub fn nested_instructions(cwd: &Path) -> Vec<PathBuf> {
    let root = project_root(cwd);
    let loaded: Vec<&Path> = cwd.ancestors().take_while(|d| d.starts_with(&root)).collect();
    let mut found = Vec::new();
    let walk = ignore::WalkBuilder::new(&root).max_depth(Some(5)).build();
    for (seen, e) in walk.flatten().enumerate() {
        if seen > 20_000 || found.len() >= NESTED_MAX {
            break;
        }
        let name = e.file_name().to_string_lossy();
        if !matches!(name.as_ref(), "AGENTS.md" | "CLAUDE.md" | "CLAUDE.local.md") || !e.path().is_file() {
            continue;
        }
        let Some(parent) = e.path().parent() else { continue };
        if loaded.contains(&parent) {
            continue;
        }
        found.push(e.path().strip_prefix(&root).unwrap_or(e.path()).to_path_buf());
    }
    found.sort();
    found
}

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
    let mut out = Vec::new();
    let mut add = |dir: PathBuf, scope: &str, shown: String, foreign: bool| {
        out.push(Source { dir, origin: format!("{scope} {shown}"), foreign });
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
    if trusted {
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
    for s in sources(cwd, config_dir, trust_file, home, "commands", Some("commands"), Some("prompts")) {
        let mut files = Vec::new();
        collect_md(&s.dir, &s.dir, 0, &mut files);
        files.sort();
        for (name, f) in files {
            let name = name.to_ascii_lowercase();
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
fn collect_md(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(String, PathBuf)>) {
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

fn first_line(text: &str) -> String {
    let l = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().trim_start_matches('#').trim();
    l.chars().take(70).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(p: PathBuf, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".git")).unwrap();
        d
    }

    #[test]
    fn agents_md_wins_over_claude_md_and_the_local_file_adds() {
        let (p, cfg, home) = (repo(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        w(p.path().join("AGENTS.md"), "agents");
        w(p.path().join("CLAUDE.md"), "claude");
        w(p.path().join("CLAUDE.local.md"), "mine");
        let got: Vec<String> =
            instructions(p.path(), cfg.path(), Some(home.path())).into_iter().map(|i| i.text).collect();
        assert_eq!(got, ["agents", "mine"]);
        std::fs::remove_file(p.path().join("AGENTS.md")).unwrap();
        let got: Vec<String> =
            instructions(p.path(), cfg.path(), Some(home.path())).into_iter().map(|i| i.text).collect();
        assert_eq!(got, ["claude", "mine"]);
    }

    #[test]
    fn user_level_fallbacks_only_when_nexo_has_none() {
        let (p, cfg, home) = (repo(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        w(home.path().join(".claude/CLAUDE.md"), "c-user");
        w(home.path().join(".codex/AGENTS.md"), "x-user");
        let got: Vec<String> =
            instructions(p.path(), cfg.path(), Some(home.path())).into_iter().map(|i| i.text).collect();
        assert_eq!(got, ["x-user", "c-user"]);
        w(cfg.path().join("AGENTS.md"), "own");
        let got = instructions(p.path(), cfg.path(), Some(home.path()));
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].text.as_str(), got[0].origin), ("own", "user"));
    }

    #[test]
    fn imports_are_relative_limited_and_never_cycle() {
        let (p, cfg) = (repo(), tempfile::tempdir().unwrap());
        w(
            p.path().join("AGENTS.md"),
            "Top\nSee @docs/a.md for more.\n```\n@docs/a.md\n```\nmail me@docs/a.md and @types/node",
        );
        w(p.path().join("docs/a.md"), "A text\n@b.md\n@../AGENTS.md");
        w(p.path().join("docs/b.md"), "B text");
        let t = &instructions(p.path(), cfg.path(), None)[0].text;
        assert!(t.contains("A text") && t.contains("B text"), "{t}");
        assert_eq!(t.matches("A text").count(), 1, "the fenced mention and the mid-word one do not import: {t}");
        assert_eq!(t.matches("Top").count(), 1, "the cycle back to the top file is skipped: {t}");
        assert!(t.contains("@types/node"));
        // depth: a chain of files stops after 5 hops
        for i in 0..8 {
            w(p.path().join(format!("c{i}.md")), &format!("level{i}\n@c{}.md", i + 1));
        }
        w(p.path().join("AGENTS.md"), "@c0.md");
        let t = &instructions(p.path(), cfg.path(), None)[0].text;
        assert!(t.contains("level4") && !t.contains("level5"), "{t}");
    }

    #[test]
    fn a_key_file_is_never_imported() {
        let (p, cfg) = (repo(), tempfile::tempdir().unwrap());
        w(p.path().join(".env"), "SECRET=1");
        w(p.path().join("AGENTS.md"), "x\n@.env");
        assert!(!instructions(p.path(), cfg.path(), None)[0].text.contains("SECRET"));
    }

    #[test]
    fn nested_files_are_listed_but_those_on_the_way_are_loaded() {
        let (p, cfg) = (repo(), tempfile::tempdir().unwrap());
        w(p.path().join("AGENTS.md"), "root");
        w(p.path().join("web/CLAUDE.md"), "web");
        w(p.path().join("api/AGENTS.md"), "api");
        w(p.path().join("api/src/CLAUDE.md"), "deep");
        let cwd = p.path().join("api");
        let got: Vec<String> = instructions(&cwd, cfg.path(), None).into_iter().map(|i| i.text).collect();
        assert_eq!(got, ["api", "root"]);
        let nested = nested_instructions(&cwd);
        assert_eq!(nested, [PathBuf::from("api/src/CLAUDE.md"), PathBuf::from("web/CLAUDE.md")]);
    }

    #[test]
    fn commands_nest_with_colons_and_nexo_wins() {
        let (p, cfg, home) = (repo(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let trust = cfg.path().join("trusted-projects");
        w(home.path().join(".codex/prompts/ship.md"), "codex ship");
        w(
            p.path().join(".claude/commands/ship.md"),
            "---\ndescription: Ship it\nargument-hint: [env]\nallowed-tools: Bash(git:*)\n---\nclaude ship",
        );
        w(p.path().join(".claude/commands/git/sync.md"), "# Sync\nsync");
        w(cfg.path().join("commands/ship.md"), "nexo ship");
        let none = |_: &str| false;
        let before = commands(p.path(), cfg.path(), &trust, Some(home.path()), &none);
        let names: Vec<&str> = before.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["ship"], "an untrusted project's commands are left out");
        assert_eq!(before[0].body, "nexo ship");
        crate::config::trust_project(p.path(), &trust).unwrap();
        let all = commands(p.path(), cfg.path(), &trust, Some(home.path()), &none);
        let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["git:sync", "ship"]);
        assert_eq!(all[1].origin, "user .nexo/commands");
        assert_eq!(all[0].origin, "project .claude/commands");
        assert_eq!(all[0].description, "Sync");
        std::fs::remove_file(cfg.path().join("commands/ship.md")).unwrap();
        let all = commands(p.path(), cfg.path(), &trust, Some(home.path()), &none);
        let ship = all.iter().find(|c| c.name == "ship").unwrap();
        assert_eq!(
            (ship.description.as_str(), ship.hint.as_str(), ship.allowed_tools.as_str()),
            ("Ship it", "[env]", "Bash(git:*)")
        );
        assert_eq!(ship.body, "claude ship", "the project's own folder beats the user's");
    }
}
