//! What the draft asks the palette for, and the values the lists offer.

use super::*;

/// The values the argument list offers for `command`: the models one has
/// set up, the effort levels the model takes, the themes, or the fixed ones.
pub fn arg_values(
    command: &str,
    models: &[(String, String)],
    current: &str,
    effort_levels: &[String],
) -> Vec<(String, String)> {
    match command {
        "model" => models
            .iter()
            .map(|(p, m)| {
                let value = format!("{p}/{m}");
                let hint = if value == current { "in use" } else { "" };
                (value, hint.to_string())
            })
            .collect(),
        "effort" => {
            let mut v = vec![("default".to_string(), "what the model does without being told".to_string())];
            if effort_levels.is_empty() {
                v.extend(["off", "low", "medium", "high"].iter().map(|l| (l.to_string(), String::new())));
            } else {
                v.extend(effort_levels.iter().map(|l| (l.clone(), String::new())));
            }
            v
        }
        "theme" => crate::theme::NAMES
            .iter()
            .map(|n| {
                let d = crate::theme::Theme::by_name(n).map(|t| t.description.to_string()).unwrap_or_default();
                (n.to_string(), d)
            })
            .collect(),
        other => commands::fixed_args(other).iter().map(|(v, d)| (v.to_string(), d.to_string())).collect(),
    }
}

/// Values taken from what is going on now, after the fixed ones: each running
/// background job for `/jobs kill <id>`, and the sessions deleted in this run
/// for `/undelete`, the last one first (the one a bare `/undelete` brings back),
/// and the earlier sessions here for `/resume`, with their titles.
pub fn live_values(
    command: &str,
    jobs: &[JobInfo],
    trashed: &[String],
    sessions: &[(String, String)],
    peers: &[(String, String)],
) -> Vec<(String, String)> {
    match command {
        "resume" => sessions.to_vec(),
        "msg" => peers.to_vec(),
        "jobs" => jobs
            .iter()
            .filter(|j| j.status == JobStatus::Running)
            .map(|j| {
                let what = match j.kind {
                    JobKind::Shell => "stop",
                    JobKind::Agent => "stop sub-agent",
                };
                (format!("kill {}", j.id), format!("{what}: {}", j.label.lines().next().unwrap_or("")))
            })
            .collect(),
        "undelete" => trashed
            .iter()
            .rev()
            .enumerate()
            .map(|(k, id)| {
                let hint = if k == 0 { "deleted last · the one /undelete brings back" } else { "deleted in this run" };
                (id.clone(), hint.to_string())
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The folders `/add-dir` offers for what is typed so far: the folders inside the part before the
/// last separator (from `cwd`, or home for `~/`), written the way the person started, each ending in
/// a separator so picking one lists its own folders next. Hidden folders only once a `.` is typed.
pub fn dir_values(cwd: &std::path::Path, typed: &str) -> Vec<(String, String)> {
    dir_values_in(cwd, typed, nexo_core::paths::user_home().as_deref())
}

/// The same with the home folder given (tests): `HOME` is not set on Windows, and tests must not change the environment.
pub(super) fn dir_values_in(
    cwd: &std::path::Path,
    typed: &str,
    home: Option<&std::path::Path>,
) -> Vec<(String, String)> {
    // Windows people type `\` as often as `/`; elsewhere a backslash is a letter of a name
    let is_sep = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    let (head, tail) = match typed.rfind(is_sep) {
        Some(i) => typed.split_at(i + 1),
        None => ("", typed),
    };
    // the separator the person is using goes on each name, so `..\` offers `..\lib\` and not `..\lib/`
    let sep = if cfg!(windows) && head.ends_with('\\') { '\\' } else { '/' };
    let base = if head.is_empty() {
        cwd.to_path_buf()
    } else if let Some(rest) = head.strip_prefix("~/").or_else(|| head.strip_prefix("~\\").filter(|_| cfg!(windows))) {
        match home {
            Some(h) => h.join(rest),
            None => return Vec::new(),
        }
    } else {
        cwd.join(head)
    };
    let Ok(entries) = std::fs::read_dir(&base) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.') || tail.starts_with('.'))
        .collect();
    names.sort();
    names.truncate(200);
    let mut out: Vec<(String, String)> =
        names.into_iter().map(|n| (format!("{head}{n}{sep}"), String::new())).collect();
    // the folder above is one step away when nothing narrows the list yet
    if tail.is_empty() && !head.starts_with('~') && head.split(is_sep).all(|p| p.is_empty() || p == "..") {
        out.push((format!("{head}..{sep}"), "the folder above".to_string()));
    }
    out
}

/// The commands of `history` (lines that start with `/`), most recent first,
/// each once, by primary name.
pub fn recent_commands(history: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in history.iter().rev() {
        let Some(rest) = line.trim().strip_prefix('/') else { continue };
        let word = rest.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        if let Some(s) = commands::lookup(&word) {
            if !out.iter().any(|n| n == s.name) {
                out.push(s.name.to_string());
            }
        }
        if out.len() >= MAX_RECENT {
            break;
        }
    }
    out
}

/// Folders that hold no project files the person would mention: version control data and tool caches.
const SKIPPED_DIRS: [&str; 6] = [".git", ".hg", ".svn", ".venv", ".cache", ".direnv"];

/// The files of a project, relative paths, gitignore-aware, at most `max`.
/// Dotfiles such as `.github` or `.env.example` are listed only inside a git
/// repository, where `.gitignore` keeps the secrets out; anywhere else (a home
/// folder, say) they would fill the cap with `.ssh` and `.config` names.
pub fn project_files(root: &Path, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let in_repo = root.join(".git").exists();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(!in_repo)
        .git_ignore(true)
        .git_global(true)
        // matched by name alone: in a git worktree or a submodule `.git` is a file that points at the real folder
        .filter_entry(|e| !SKIPPED_DIRS.iter().any(|d| e.file_name() == *d))
        .build();
    for entry in walker.flatten() {
        if out.len() >= max {
            break;
        }
        if entry.file_type().is_some_and(|t| t.is_file()) {
            if let Ok(rel) = entry.path().strip_prefix(root) {
                out.push(nexo_core::shell::slashed(rel));
            }
        }
    }
    out.sort_by(|a, b| a.matches('/').count().cmp(&b.matches('/').count()).then(a.cmp(b)));
    out
}

/// What the draft asks for at the cursor: a command palette when the first
/// line starts with `/` and holds no space yet, the values of the command's
/// argument while the cursor is in its first word, a file picker after a word-initial `@`.
pub fn wanted(draft: &str, cursor: usize) -> Option<(PaletteKind, usize, String)> {
    let cursor = cursor.min(draft.len());
    let head = &draft[..cursor];
    if let Some(rest) = draft.strip_prefix('/') {
        if !rest.contains(char::is_whitespace) && !head.is_empty() {
            return Some((PaletteKind::Commands, 1, rest.to_string()));
        }
        if let Some(sp) = rest.find(char::is_whitespace) {
            let after = &rest[sp..];
            let at = 1 + sp + (after.len() - after.trim_start().len());
            let arg = &draft[at..];
            if cursor >= at && !arg.contains(char::is_whitespace) && !after.contains('\n') {
                let typed = rest[..sp].to_ascii_lowercase();
                // the old `/undelete` keeps offering the sessions in the trash
                let name = match typed.as_str() {
                    "undelete" | "restore" => Some("undelete"),
                    _ => commands::lookup(&typed).map(|s| s.name).filter(|n| commands::takes_listed_args(n)),
                };
                if let Some(name) = name {
                    return Some((PaletteKind::Args(name.to_string()), at, arg.to_string()));
                }
            }
        }
    }
    let word_start = head.rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
    let word = &head[word_start..];
    if let Some(at) = word.rfind('@') {
        // only a word-initial `@` is a mention; `bob@m` and `git@host` are plain text
        let opens = word[..at].chars().next_back().is_none_or(|c| "([{<\"'`".contains(c));
        if opens {
            let q = &word[at + 1..];
            return Some((PaletteKind::Files, word_start + at + 1, q.to_string()));
        }
    }
    None
}
