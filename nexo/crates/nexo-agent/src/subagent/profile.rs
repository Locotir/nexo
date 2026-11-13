use super::*;

pub const DEFAULT: &str = "explore";

pub(super) const BASE: &str =
    "You are a sub-agent of a coding assistant. You get one task; do it with the tools you have and \
answer with a report for the assistant that sent you: what you found or did, with file paths and line numbers, \
in at most 300 words. Do not ask questions back: decide and report. A briefing may come before the task: \
passages of the conversation that sent you, picked for it. Trust it, do not read again what it holds when it \
says current, and do not quote it back in the report: cite path:line instead.
Search narrow first: the working directory and the paths the task names, with the search tools; widen one step \
at a time only when that finds nothing. Never sweep the whole home folder, / or /tmp in one command: it is slow \
and the person waits for it. Ask for independent reads and searches together in one round.";

/// Said to a sub-agent whose rounds are over, so its work comes back as a report instead of being lost.
pub(super) const LAST_ROUND: &str =
    "[nexo] No rounds left for tools. Answer now with your report: what you found, with \
path:line, and what you could not settle.";

pub(super) const EXPLORE: &str =
    "You can only read: read files, search, list, search the web. You cannot change anything.";

pub(super) const GENERAL: &str =
    "You can read, edit and run commands. Keep to the task: change only what it needs, check \
your work (build, tests) when you can, and say in the report exactly which files you changed.";

/// What `task` is for, in the text every request carries.
pub(super) const SHORT: &str =
    "Hand self-contained work to a sub-agent that does not see this conversation and returns a short report.";

/// The most characters of a kind's description in the short text of `task`.
pub(super) const KIND_CHARS: usize = 60;

/// The most characters the kinds of the person's own take in the short text of `task`.
pub(super) const KINDS_CHARS: usize = 1_400;

/// The start of a description, for the short text of `task`: its first clause (up to a full stop, a
/// semicolon, a colon, a dash, a bracket or a new line) and at most `max` characters of it. A longer clause
/// ends at its last comma within them, or on a whole word when that would leave under a third.
pub(super) fn clause(description: &str, max: usize) -> &str {
    let text = description.trim();
    let ends = [". ", "; ", ": ", " - ", " \u{2013} ", " \u{2014} ", " (", "\n", "\\n"];
    let mut cut = &text[..ends.iter().filter_map(|e| text.find(e)).min().unwrap_or(text.len())];
    if let Some((at, next)) = cut.char_indices().nth(max) {
        cut = &cut[..at];
        match cut.rfind(", ").filter(|comma| *comma >= at / 3) {
            Some(comma) => cut = &cut[..comma],
            None if !next.is_whitespace() => cut = &cut[..cut.rfind(' ').unwrap_or(at)],
            None => {}
        }
    }
    cut.trim_end_matches(|c: char| c.is_whitespace() || ".,;:-".contains(c))
}

/// The short text of `task`, which every request carries: what it is for, the two built-in kinds in a few
/// words, and each kind of the person's own by name with the start of its description ([`clause`]). That
/// list is held to [`KINDS_CHARS`] however many kinds there are: a kind is described while the names of
/// those after it still fit, then named alone, then only counted.
pub(super) fn short_text(profiles: &[Profile]) -> String {
    let stock = builtin();
    let kept = |name: &str| profiles.iter().any(|p| p.name == name && stock.contains(p));
    let mut text = String::from(SHORT);
    // a built-in kind the person replaced is listed with theirs, by what their file says of it
    match (kept("explore"), kept("general")) {
        (true, true) => text.push_str(" `explore` (default) is read-only; `general` can edit."),
        (true, false) => text.push_str(" `explore` (default) is read-only."),
        (false, true) => text.push_str(" `general` can edit."),
        (false, false) => {}
    }
    let own: Vec<&Profile> = profiles.iter().filter(|p| !stock.contains(p)).collect();
    if own.is_empty() {
        return text;
    }
    let label =
        |p: &Profile| if p.name == DEFAULT { format!("`{}` (default)", p.name) } else { format!("`{}`", p.name) };
    // what the names not yet written take, each with the `; ` after it
    let mut names_left: usize = own.iter().map(|p| label(p).len() + 2).sum();
    let (mut list, mut left_out) = (String::new(), 0);
    for p in own {
        let name = label(p);
        names_left -= name.len() + 2;
        let said = clause(&p.description, KIND_CHARS);
        let fits = list.len() + name.len() + 2 + said.len() + 2 + names_left <= KINDS_CHARS;
        let entry = if fits && !said.is_empty() { format!("{name}: {said}") } else { name };
        if list.len() + entry.len() + 2 > KINDS_CHARS {
            left_out += 1;
            continue;
        }
        list.push_str(&entry);
        list.push_str("; ");
    }
    text.push_str(" More kinds for `agent`: ");
    text.push_str(&list);
    match left_out {
        0 => text.truncate(text.len() - 2),
        n => text.push_str(&format!(
            "and {n} more: an `agent` that is none of these is answered with every kind and its description"
        )),
    }
    text.push('.');
    text
}

/// One kind of sub-agent.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub name: String,
    pub description: String,
    /// The tools it may use; `None` is every tool but `task`.
    pub tools: Option<Vec<String>>,
    /// Only reading and searching, inside the project, with nothing to ask.
    pub read_only: bool,
    /// Another model of the same provider.
    pub model: Option<String>,
    /// Its instructions, after the common ones.
    pub prompt: String,
    /// Rounds of tool calls before it must answer.
    pub rounds: usize,
    /// The file it came from (none for the built-in ones).
    pub source: Option<PathBuf>,
    /// The model comes from another tool's file: kept only when the provider offers it.
    pub soft_model: bool,
    /// Runs in a fresh git worktree of its own (`isolation: worktree` in the front matter).
    pub isolation: bool,
}

pub fn builtin() -> Vec<Profile> {
    vec![
        Profile {
            name: "explore".into(),
            description: "read-only: wide searches (where is X handled, what calls Y, how does Z work)".into(),
            tools: None,
            read_only: true,
            model: None,
            prompt: EXPLORE.into(),
            rounds: 16,
            source: None,
            soft_model: false,
            isolation: false,
        },
        Profile {
            name: "general".into(),
            description: "can edit files and run commands: a self-contained change or check (fix this test, \
                          port that module); its calls are asked for like yours"
                .into(),
            tools: None,
            read_only: false,
            model: None,
            prompt: GENERAL.into(),
            rounds: 40,
            source: None,
            soft_model: false,
            isolation: false,
        },
    ]
}

/// The built-in kinds, then the person's, then the project's; a later one
/// with the same name replaces the earlier. A project nobody trusted yet (`nexo config trust`) has
/// its agents left out: their instructions come from the repository.
pub fn load(cwd: &Path) -> Vec<Profile> {
    let home = nexo_core::compat::foreign_home();
    load_from(cwd, &nexo_core::paths::config_dir(), &nexo_core::paths::trusted_projects_file(), home.as_deref())
}

pub(super) fn load_from(cwd: &Path, config_dir: &Path, trust_file: &Path, foreign_home: Option<&Path>) -> Vec<Profile> {
    let mut out = builtin();
    // lowest priority first: the other tools' `.claude/agents`, then nexo's own folders
    let sources = nexo_core::compat::sources(cwd, config_dir, trust_file, foreign_home, "agents", Some("agents"), None);
    for s in sources {
        let Ok(rd) = std::fs::read_dir(&s.dir) else { continue };
        let mut files: Vec<PathBuf> =
            rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "md")).collect();
        files.sort();
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("agent").to_string();
            if let Some(mut p) = parse(&text, &stem) {
                if s.foreign {
                    adopt_foreign(&mut p);
                }
                p.source = Some(f.clone());
                if let Some(plugin) = &s.plugin {
                    p.name = format!("{plugin}:{}", p.name);
                }
                out.retain(|o| o.name != p.name);
                out.push(p);
            }
        }
    }
    out
}

/// Maps what a `.claude/agents` file says onto nexo: its tool names (a list that names none nexo
/// has stays read-only, never all tools) and its model (checked when the sub-agent runs).
pub(super) fn adopt_foreign(p: &mut Profile) {
    if let Some(list) = p.tools.take() {
        let mut mapped: Vec<String> = Vec::new();
        for t in list.iter().filter_map(|t| map_tool(t)) {
            if !mapped.contains(&t) {
                mapped.push(t);
            }
        }
        if mapped.is_empty() {
            mapped = ["read", "grep", "glob", "ls"].map(String::from).to_vec();
        }
        p.tools = Some(mapped);
    }
    if p.model.as_deref().is_some_and(|m| m.eq_ignore_ascii_case("inherit")) {
        p.model = None;
    }
    p.soft_model = p.model.is_some();
}

/// A custom sub-agent: an optional front matter between `---` lines
/// (`name`, `description`, `tools` as `a, b` or `[a, b]`, `model`, `rounds`),
/// then its instructions.
pub fn parse(text: &str, fallback_name: &str) -> Option<Profile> {
    let text = text.trim_start_matches('\u{feff}');
    let (head, body) = match text.strip_prefix("---") {
        Some(rest) => match rest.find("\n---") {
            Some(i) => (&rest[..i], rest[i + 4..].trim_start_matches(|c| c != '\n').trim()),
            None => ("", text.trim()),
        },
        None => ("", text.trim()),
    };
    let mut p = Profile {
        name: fallback_name.to_string(),
        description: String::new(),
        tools: None,
        read_only: false,
        model: None,
        prompt: body.to_string(),
        rounds: 30,
        source: None,
        soft_model: false,
        isolation: false,
    };
    for line in head.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
        match k.trim() {
            "name" if !v.is_empty() => p.name = v.to_string(),
            "description" => p.description = v.to_string(),
            "model" if !v.is_empty() => p.model = Some(v.to_string()),
            "rounds" => p.rounds = v.parse().unwrap_or(p.rounds).clamp(1, 200),
            "isolation" => p.isolation = v.eq_ignore_ascii_case("worktree"),
            "tools" => {
                let list: Vec<String> = v
                    .trim_matches(|c| c == '[' || c == ']')
                    .split(',')
                    .map(|t| t.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
                // a list that names only `task` and `workflow` leaves the sub-agent no tool, not every tool
                if !list.is_empty() {
                    p.tools = Some(list.into_iter().filter(|t| t != "task" && t != "workflow").collect());
                }
            }
            _ => {}
        }
    }
    let ok = !p.name.is_empty() && p.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then_some(p)
}
