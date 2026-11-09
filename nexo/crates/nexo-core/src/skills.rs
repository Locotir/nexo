//! Agent Skills: a folder with a `SKILL.md` (front matter `name`, `description`, optional
//! `allowed-tools`) and any files it refers to. The format is shared with Claude Code and Codex, so a
//! team's skills work here as they are. Only the name and description go to the model, as a short
//! list; the body is loaded when a skill is used (the `skill` tool, or `/name` in the TUI).
//!
//! Skills are looked for in `.nexo/skills`, `.agents/skills` and `.claude/skills`, first in the
//! project (the working directory and its parents up to the repository root), then in the person's
//! home. The first skill of a name wins, nexo's own folders first.

use std::path::{Path, PathBuf};

/// The most skills the model is told about.
pub const MAX_LISTED: usize = 100;
/// The most characters of one description in the list.
pub const MAX_DESCRIPTION: usize = 200;
/// The most characters of a skill body handed to the model.
pub const MAX_BODY: usize = 100_000;

/// The folders skills live in, inside a project or a home, in the order they win.
const FOLDERS: [&str; 3] = [".nexo/skills", ".agents/skills", ".claude/skills"];

#[derive(Debug, Clone, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// The skill's folder: the files it mentions are relative to it.
    pub dir: PathBuf,
    /// Tools the skill asks to use freely. Kept for display; nexo's permission rules still decide.
    pub allowed_tools: Vec<String>,
    /// Where it was found, as shown to the person (`.claude/skills`, `~/.agents/skills`).
    pub source: String,
}

impl Skill {
    pub fn file(&self) -> PathBuf {
        self.dir.join("SKILL.md")
    }

    /// The instructions: the file without its front matter, cut at [`MAX_BODY`] characters.
    pub fn body(&self) -> std::io::Result<String> {
        let text = std::fs::read_to_string(self.file())?;
        let (_, body) = split_front(&text);
        let body = body.trim();
        Ok(match body.char_indices().nth(MAX_BODY) {
            Some((i, _)) => format!("{}\n\n[cut: the skill is longer than {MAX_BODY} characters]", &body[..i]),
            None => body.to_string(),
        })
    }

    /// What `/name args` or the `skill` tool hands over: the folder, the instructions and the arguments.
    pub fn instructions(&self, args: &str) -> std::io::Result<String> {
        let body = self.body()?;
        let mut out = format!(
            "Skill `{}` (folder {}; files it mentions are relative to it):\n\n{body}",
            self.name,
            self.dir.display()
        );
        let args = args.trim();
        if !args.is_empty() {
            out.push_str(&format!("\n\nArguments: {args}"));
        }
        Ok(out)
    }
}

/// `(front matter, body)` of a file; the front matter is empty when there is none.
fn split_front(text: &str) -> (&str, &str) {
    let text = text.trim_start_matches('\u{feff}');
    let Some(rest) = text.strip_prefix("---") else { return ("", text) };
    let Some(rest) = rest.strip_prefix("\r\n").or_else(|| rest.strip_prefix('\n')) else { return ("", text) };
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return (&rest[..at], &rest[at + line.len()..]);
        }
        at += line.len();
    }
    ("", text)
}

/// A value of the front matter: plain, quoted, or a block (`>`, `|`) / list on the lines under the key.
fn field(front: &str, key: &str) -> Option<String> {
    let lines: Vec<&str> = front.lines().collect();
    let i = lines.iter().position(|l| l.strip_prefix(key).is_some_and(|r| r.starts_with(':')))?;
    let first = lines[i][key.len() + 1..].trim();
    let more: Vec<&str> = lines[i + 1..]
        .iter()
        .take_while(|l| l.starts_with([' ', '\t']) || l.trim().is_empty())
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let unquote = |s: &str| {
        let s = s.trim();
        for q in ['"', '\''] {
            if let Some(inner) = s.strip_prefix(q).and_then(|r| r.strip_suffix(q)) {
                return inner.to_string();
            }
        }
        s.to_string()
    };
    let block = first.starts_with(['>', '|']);
    let value = if block || first.is_empty() { more.join(" ") } else { unquote(first) };
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!value.is_empty()).then_some(value)
}

/// The tool names of `allowed-tools`: a comma or space separated line, `[a, b]`, or a `- a` list.
fn tool_list(front: &str) -> Vec<String> {
    let Some(raw) = field(front, "allowed-tools") else { return Vec::new() };
    raw.trim_matches(['[', ']'])
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(|t| t.trim_matches(['-', '"', '\'']).to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// A skill name the model can pass back and a person can type after `/`.
fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 64 && !n.contains(|c: char| c.is_whitespace() || c == '/' || c == '\\')
}

/// The skill in `dir`, if it has a readable `SKILL.md`.
fn load(dir: &Path, source: &str) -> Option<Skill> {
    let text = std::fs::read_to_string(dir.join("SKILL.md")).ok()?;
    let (front, body) = split_front(&text);
    let folder = dir.file_name()?.to_string_lossy().to_string();
    let name = field(front, "name").unwrap_or(folder);
    if !valid_name(&name) {
        return None;
    }
    let description = field(front, "description").or_else(|| {
        body.lines().map(|l| l.trim().trim_start_matches('#').trim()).find(|l| !l.is_empty()).map(String::from)
    })?;
    Some(Skill {
        name,
        description,
        dir: dir.to_path_buf(),
        allowed_tools: tool_list(front),
        source: source.to_string(),
    })
}

/// Every skill reachable from `cwd` and the person's folders. `home` holds `.agents` and `.claude`;
/// `nexo_dir` is nexo's own config folder (`~/.nexo`). With `user` false the person's folders are skipped.
pub fn discover_in(cwd: &Path, home: &Path, nexo_dir: &Path, user: bool) -> Vec<Skill> {
    discover_with(cwd, home, nexo_dir, user, crate::compat::claude_code())
}

/// [`discover_in`] with the `.claude/skills` folders read (`claude`) or left out.
fn discover_with(cwd: &Path, home: &Path, nexo_dir: &Path, user: bool, claude: bool) -> Vec<Skill> {
    let mut roots: Vec<(PathBuf, String)> = Vec::new();
    // the project: the working directory and its parents up to the repository root
    let mut p = Some(cwd);
    while let Some(d) = p {
        // a home kept in git: its skill folders are the person's own (listed below), not a project's
        if crate::paths::is_home(d, Some(home)) {
            break;
        }
        for f in FOLDERS.iter().filter(|f| claude || !f.starts_with(".claude")) {
            roots.push((d.join(f), f.to_string()));
        }
        if d.join(".git").exists() {
            break;
        }
        // no repository: only the working directory counts
        if !cwd.ancestors().any(|a| a.join(".git").exists()) {
            break;
        }
        p = d.parent();
    }
    if user {
        roots.push((nexo_dir.join("skills"), "~/.nexo/skills".into()));
        roots.push((home.join(".agents/skills"), "~/.agents/skills".into()));
        if claude {
            roots.push((home.join(".claude/skills"), "~/.claude/skills".into()));
        }
    }
    let mut out: Vec<Skill> = Vec::new();
    for (root, source) in roots {
        for s in scan_root(&root, &source, None) {
            if !out.iter().any(|o| o.name.eq_ignore_ascii_case(&s.name)) {
                out.push(s);
            }
        }
    }
    // installed plugins last: a skill of the person's own with the same name wins, and a plugin's
    // skill is called `plugin:name` anyway
    if user {
        for p in crate::plugins::enabled(nexo_dir).0 {
            for root in p.skill_roots() {
                for s in scan_root(&root, &format!("plugin {}", p.name), Some(&p.name)) {
                    if !out.iter().any(|o| o.name.eq_ignore_ascii_case(&s.name)) {
                        out.push(s);
                    }
                }
            }
        }
    }
    out
}

/// The skills in the folders directly below `root`, sorted by folder; with a `plugin`, each one's
/// name is `plugin:name`.
pub fn scan_root(root: &Path, source: &str, plugin: Option<&str>) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    let mut out = Vec::new();
    for d in dirs {
        if let Some(mut s) = load(&d, source) {
            if let Some(p) = plugin {
                s.name = format!("{p}:{}", s.name);
            }
            out.push(s);
        }
    }
    out
}

/// [`discover_in`] for this machine: the real home and nexo's config folder.
pub fn discover(cwd: &Path, user: bool) -> Vec<Skill> {
    let home = dirs::home_dir().unwrap_or_default();
    // a managed policy can switch off the person's skills and the project's
    let policy = crate::managed::current();
    let allowed = |scope: &str| policy.as_ref().is_none_or(|p| p.extension_allowed(scope, "skills"));
    let mut found = discover_in(cwd, &home, &crate::paths::config_dir(), user && allowed("user"));
    if !allowed("project") {
        // the person's folders are listed as `~/...`, the project's as plain folder names
        found.retain(|s| s.source.starts_with('~'));
    }
    found
}

/// The skill called `name` (any case; a leading `/` is ignored).
pub fn find<'a>(skills: &'a [Skill], name: &str) -> Option<&'a Skill> {
    let name = name.trim().trim_start_matches('/');
    skills.iter().find(|s| s.name.eq_ignore_ascii_case(name))
}

/// The list the model reads: one line per skill, at most [`MAX_LISTED`], descriptions cut to
/// [`MAX_DESCRIPTION`]. Sorted by name, so it is the same text every request (the prompt cache holds).
pub fn listing(skills: &[Skill]) -> String {
    let mut sorted: Vec<&Skill> = skills.iter().collect();
    sorted.sort_by_key(|s| s.name.to_ascii_lowercase());
    let mut out = String::new();
    for s in sorted.iter().take(MAX_LISTED) {
        let d: String = if s.description.chars().count() > MAX_DESCRIPTION {
            let cut: String = s.description.chars().take(MAX_DESCRIPTION - 3).collect();
            format!("{}...", cut.trim_end())
        } else {
            s.description.clone()
        };
        out.push_str(&format!("- {}: {d}\n", s.name));
    }
    if sorted.len() > MAX_LISTED {
        out.push_str(&format!("- ({} more skills are not listed)\n", sorted.len() - MAX_LISTED));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let f = root.join(rel);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, text).unwrap();
    }

    #[test]
    fn reads_front_matter_in_the_common_shapes() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "a/SKILL.md",
            "---\nname: alpha\ndescription: \"Does A\"\nallowed-tools: Bash, Read\n---\n# Body\nSteps\n",
        );
        write(d.path(), "b/SKILL.md", "---\nname: beta\ndescription: >\n  Folded text\n  over lines\nallowed-tools:\n  - Grep\n  - Glob\n---\nGo\n");
        write(d.path(), "c/SKILL.md", "Just text, no front matter\nmore");
        let a = load(&d.path().join("a"), "x").unwrap();
        assert_eq!((a.name.as_str(), a.description.as_str()), ("alpha", "Does A"));
        assert_eq!(a.allowed_tools, ["Bash", "Read"]);
        assert_eq!(a.body().unwrap(), "# Body\nSteps");
        let b = load(&d.path().join("b"), "x").unwrap();
        assert_eq!(b.description, "Folded text over lines");
        assert_eq!(b.allowed_tools, ["Grep", "Glob"]);
        let c = load(&d.path().join("c"), "x").unwrap();
        assert_eq!((c.name.as_str(), c.description.as_str()), ("c", "Just text, no front matter"));
        write(d.path(), "bad name/SKILL.md", "---\ndescription: x\n---\n");
        assert!(load(&d.path().join("bad name"), "x").is_none());
        assert!(load(&d.path().join("missing"), "x").is_none());
    }

    #[test]
    fn project_beats_user_and_nexo_beats_the_others() {
        let proj = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(proj.path().join(".git")).unwrap();
        write(proj.path(), ".claude/skills/dup/SKILL.md", "---\nname: dup\ndescription: claude project\n---\n");
        write(proj.path(), ".nexo/skills/dup/SKILL.md", "---\nname: dup\ndescription: nexo project\n---\n");
        write(proj.path(), ".agents/skills/mid/SKILL.md", "---\nname: mid\ndescription: agents project\n---\n");
        write(home.path(), ".nexo/skills/dup/SKILL.md", "---\nname: dup\ndescription: nexo user\n---\n");
        write(home.path(), ".claude/skills/mid/SKILL.md", "---\nname: MID\ndescription: claude user\n---\n");
        write(home.path(), ".claude/skills/only/SKILL.md", "---\nname: only\ndescription: user only\n---\n");
        let sub = proj.path().join("src/deep");
        std::fs::create_dir_all(&sub).unwrap();
        let found = discover_in(&sub, home.path(), &home.path().join(".nexo"), true);
        let by = |n: &str| found.iter().find(|s| s.name == n).map(|s| (s.description.clone(), s.source.clone()));
        assert_eq!(by("dup"), Some(("nexo project".into(), ".nexo/skills".into())));
        assert_eq!(by("mid"), Some(("agents project".into(), ".agents/skills".into())));
        assert_eq!(by("only"), Some(("user only".into(), "~/.claude/skills".into())));
        assert_eq!(found.len(), 3, "{found:?}");
        let project_only = discover_in(&sub, home.path(), &home.path().join(".nexo"), false);
        assert_eq!(project_only.len(), 2);
    }

    #[test]
    fn a_home_kept_in_git_lists_its_skills_once_as_the_persons_own() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".git")).unwrap();
        write(home.path(), ".claude/skills/x/SKILL.md", "---\nname: x\ndescription: home skill\n---\n");
        let sub = home.path().join("notes");
        std::fs::create_dir(&sub).unwrap();
        let nexo = home.path().join(".nexo");
        for cwd in [home.path(), sub.as_path()] {
            let found = discover_in(cwd, home.path(), &nexo, true);
            let got: Vec<_> = found.iter().map(|s| (s.name.as_str(), s.source.as_str())).collect();
            assert_eq!(got, [("x", "~/.claude/skills")], "from {cwd:?}");
        }
        // with the person's folders off, the home's skills are not smuggled back in as a project's
        assert!(discover_in(home.path(), home.path(), &nexo, false).is_empty());
    }

    #[test]
    fn a_project_under_a_home_kept_in_git_keeps_its_own_skills() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".git")).unwrap();
        write(home.path(), ".claude/skills/x/SKILL.md", "---\nname: x\ndescription: home skill\n---\n");
        let proj = home.path().join("code/app");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        write(&proj, ".claude/skills/y/SKILL.md", "---\nname: y\ndescription: project skill\n---\n");
        let found = discover_in(&proj.join("src"), home.path(), &home.path().join(".nexo"), true);
        let mut got: Vec<_> = found.iter().map(|s| (s.name.as_str(), s.source.as_str())).collect();
        got.sort();
        assert_eq!(got, [("x", "~/.claude/skills"), ("y", ".claude/skills")]);
    }

    #[test]
    fn a_folder_outside_the_home_is_unchanged() {
        let (home, proj) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::create_dir(proj.path().join(".git")).unwrap();
        write(proj.path(), ".claude/skills/y/SKILL.md", "---\nname: y\ndescription: project skill\n---\n");
        write(home.path(), ".claude/skills/x/SKILL.md", "---\nname: x\ndescription: home skill\n---\n");
        let found = discover_in(proj.path(), home.path(), &home.path().join(".nexo"), true);
        let mut got: Vec<_> = found.iter().map(|s| (s.name.as_str(), s.source.as_str())).collect();
        got.sort();
        assert_eq!(got, [("x", "~/.claude/skills"), ("y", ".claude/skills")]);
    }

    #[test]
    fn without_a_repository_only_the_working_directory_counts() {
        let outer = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(outer.path(), ".claude/skills/up/SKILL.md", "---\nname: up\ndescription: parent\n---\n");
        let inner = outer.path().join("inner");
        std::fs::create_dir(&inner).unwrap();
        assert!(discover_in(&inner, home.path(), &home.path().join(".nexo"), true).is_empty());
    }

    #[test]
    fn the_list_is_sorted_capped_and_stable() {
        let skills: Vec<Skill> = (0..105)
            .rev()
            .map(|i| Skill {
                name: format!("s{i:03}"),
                description: "d ".repeat(300),
                dir: PathBuf::from("/x"),
                allowed_tools: vec![],
                source: String::new(),
            })
            .collect();
        let l = listing(&skills);
        let lines: Vec<&str> = l.lines().collect();
        assert_eq!(lines.len(), MAX_LISTED + 1);
        assert!(lines[0].starts_with("- s000: d d"));
        assert!(lines[0].ends_with("..."));
        assert!(lines[0].chars().count() < MAX_DESCRIPTION + 12);
        assert_eq!(lines[MAX_LISTED], "- (5 more skills are not listed)");
        let mut shuffled = skills.clone();
        shuffled.reverse();
        assert_eq!(listing(&shuffled), l);
    }

    #[test]
    fn instructions_carry_the_folder_and_arguments() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "s/SKILL.md", "---\nname: s\ndescription: x\n---\nDo it.\n");
        let s = load(&d.path().join("s"), "x").unwrap();
        let t = s.instructions(" now ").unwrap();
        assert!(
            t.contains(&d.path().join("s").display().to_string())
                && t.contains("Do it.")
                && t.ends_with("Arguments: now")
        );
        assert_eq!(find(&[s], "/S").map(|s| s.name.as_str()), Some("s"));
    }

    #[test]
    fn with_claude_code_off_its_skill_folders_are_not_read() {
        let (home, proj) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::create_dir_all(proj.path().join(".git")).unwrap();
        write(proj.path(), ".claude/skills/p/SKILL.md", "---\nname: p\ndescription: project\n---\n");
        write(proj.path(), ".agents/skills/a/SKILL.md", "---\nname: a\ndescription: agents\n---\n");
        write(home.path(), ".claude/skills/h/SKILL.md", "---\nname: h\ndescription: home\n---\n");
        let names = |claude| -> Vec<String> {
            discover_with(proj.path(), home.path(), &home.path().join(".nexo"), true, claude)
                .into_iter()
                .map(|s| s.name)
                .collect()
        };
        assert_eq!(names(true), ["a", "p", "h"]);
        assert_eq!(names(false), ["a"]);
    }
}
