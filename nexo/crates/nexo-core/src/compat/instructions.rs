//! Instruction files (`CLAUDE.md`, `AGENTS.md`, `NEXO.md`) and their `@path` imports.

use std::path::{Path, PathBuf};

use super::*;

/// How many imports may be nested in one another.
pub(super) const IMPORT_DEPTH: usize = 5;
/// The most nested instruction files listed.
pub(super) const NESTED_MAX: usize = 20;

/// One instructions file, its `@path` imports already placed after the line that names them.
#[derive(Debug, Clone, PartialEq)]
pub struct Instruction {
    pub path: PathBuf,
    pub text: String,
    /// Where it comes from, in a few words (`project`, `user`, `user fallback`).
    pub origin: &'static str,
}

pub(super) fn read(p: &Path) -> Option<String> {
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
    instructions_with(cwd, config_dir, home, claude_code(), dirs::home_dir().as_deref())
}

/// The folders whose instruction files are the project's, nearest first: from `cwd` up to the repository
/// root. The person's home (`own_home`) is none (see [`home_stop`]): a file there speaks to every tool run
/// in any folder below it (another tool's state notes, say), and nexo's own user file lives in its config
/// folder.
pub(super) fn project_dirs<'a>(cwd: &'a Path, own_home: Option<&Path>) -> Vec<&'a Path> {
    let mut out = Vec::new();
    for d in cwd.ancestors() {
        if home_stop(d, cwd, own_home) {
            break;
        }
        out.push(d);
        if d.join(".git").exists() {
            break;
        }
    }
    out
}

/// Whether `cwd` is in no project: the person's home (see [`home_stop`]). nexo maps no tree there and
/// reads no instruction file of it, so the notes and folders of everything kept under it stay out.
pub fn no_project(cwd: &Path) -> bool {
    project_dirs(cwd, dirs::home_dir().as_deref()).is_empty()
}

/// [`instructions`] with Claude Code's files read (`claude`) or left out, below the person's home `own_home`.
pub(super) fn instructions_with(
    cwd: &Path,
    config_dir: &Path,
    home: Option<&Path>,
    claude: bool,
    own_home: Option<&Path>,
) -> Vec<Instruction> {
    let mut out = Vec::new();
    let own: Vec<PathBuf> =
        ["NEXO.md", "AGENTS.md"].iter().map(|n| config_dir.join(n)).filter(|p| p.is_file()).collect();
    if own.is_empty() {
        if let Some(h) = home {
            for rel in fallbacks(claude) {
                push(&mut out, h.join(rel), "user fallback");
            }
        }
    } else {
        for p in own {
            push(&mut out, p, "user");
        }
    }
    for d in project_dirs(cwd, own_home) {
        push(&mut out, d.join("NEXO.md"), "project");
        let agents = d.join("AGENTS.md");
        let had_agents = agents.is_file();
        push(&mut out, agents, "project");
        if !had_agents && claude {
            if d.join("CLAUDE.md").is_file() {
                push(&mut out, d.join("CLAUDE.md"), "project");
            } else {
                push(&mut out, d.join(".claude").join("CLAUDE.md"), "project");
            }
        }
        if claude {
            push(&mut out, d.join("CLAUDE.local.md"), "project (local)");
        }
    }
    // a home kept in git: `~/.claude/CLAUDE.md` read as the user's must not come again as the project's
    let mut seen: Vec<PathBuf> = Vec::new();
    out.retain(|i| {
        let real = i.path.canonicalize().unwrap_or_else(|_| i.path.clone());
        !seen.contains(&real) && {
            seen.push(real);
            true
        }
    });
    out
}

/// The project file `/memory` opens in `cwd`: the nearest instructions file nexo reads, from `cwd` up
/// to the repository root, or `AGENTS.md` in `cwd` when there is none yet. In the person's home, which is
/// no project, it is nexo's own user file.
pub fn project_file(cwd: &Path) -> PathBuf {
    project_file_with(cwd, claude_code(), dirs::home_dir().as_deref(), &crate::paths::config_dir())
}

pub(super) fn project_file_with(cwd: &Path, claude: bool, own_home: Option<&Path>, config_dir: &Path) -> PathBuf {
    let dirs = project_dirs(cwd, own_home);
    for d in &dirs {
        for name in ["NEXO.md", "AGENTS.md", "CLAUDE.md", ".claude/CLAUDE.md"] {
            if (claude || !name.contains("CLAUDE")) && d.join(name).is_file() {
                return d.join(name);
            }
        }
    }
    if dirs.is_empty() {
        return user_file_with(config_dir, None, claude).0;
    }
    cwd.join("AGENTS.md")
}

/// The user file `/memory user` opens: nexo's own in `config_dir`, and when it does not exist yet,
/// the text to start it with: what the other tools' files under `home` say, which nexo reads in its
/// place until it exists and never after.
pub fn user_file(config_dir: &Path, home: Option<&Path>) -> (PathBuf, Option<String>) {
    user_file_with(config_dir, home, claude_code())
}

pub(super) fn user_file_with(config_dir: &Path, home: Option<&Path>, claude: bool) -> (PathBuf, Option<String>) {
    if let Some(p) = ["NEXO.md", "AGENTS.md"].iter().map(|n| config_dir.join(n)).find(|p| p.is_file()) {
        return (p, None);
    }
    let mut fallback = Vec::new();
    if let Some(h) = home {
        for rel in fallbacks(claude) {
            push(&mut fallback, h.join(rel), "user fallback");
        }
    }
    let seed = fallback.iter().map(|i| i.text.trim_end()).collect::<Vec<_>>().join("\n\n");
    (config_dir.join("AGENTS.md"), (!seed.is_empty()).then(|| format!("{seed}\n")))
}

/// The other tools' user files read in place of nexo's own, under the home folder.
pub(super) fn fallbacks(claude: bool) -> &'static [&'static str] {
    if claude {
        &[".codex/AGENTS.md", ".claude/CLAUDE.md"]
    } else {
        &[".codex/AGENTS.md"]
    }
}

pub(super) fn push(out: &mut Vec<Instruction>, path: PathBuf, origin: &'static str) {
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
pub(super) fn expand_imports(text: &str, dir: &Path, depth: usize, stack: &mut Vec<PathBuf>) -> String {
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
    nested_instructions_with(cwd, claude_code(), dirs::home_dir().as_deref())
}

pub(super) fn nested_instructions_with(cwd: &Path, claude: bool, own_home: Option<&Path>) -> Vec<PathBuf> {
    // the person's home is no project: the other folders' files under it are theirs, not a part of this one
    if project_dirs(cwd, own_home).is_empty() {
        return Vec::new();
    }
    let root = project_root_with(cwd, own_home);
    let loaded: Vec<&Path> = cwd.ancestors().take_while(|d| d.starts_with(&root)).collect();
    let mut found = Vec::new();
    let walk = ignore::WalkBuilder::new(&root).max_depth(Some(5)).build();
    for (seen, e) in walk.flatten().enumerate() {
        if seen > 20_000 || found.len() >= NESTED_MAX {
            break;
        }
        let name = e.file_name().to_string_lossy();
        let wanted = match name.as_ref() {
            "AGENTS.md" => true,
            "CLAUDE.md" | "CLAUDE.local.md" => claude,
            _ => false,
        };
        if !wanted || !e.path().is_file() {
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
