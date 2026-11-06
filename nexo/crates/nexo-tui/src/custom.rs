//! Commands of one's own: a markdown file per command in `~/.nexo/commands/` or in
//! the project's `.nexo/commands/` (the project's win). The other tools' folders work too:
//! `.claude/commands` (a sub-folder `git/sync.md` is `/git:sync`) and `.codex/prompts`, at user
//! and project level; nexo's own folders win on a name clash (see `nexo_core::compat`).
//!
//! `/name args` sends the file's text as a request. `$ARGUMENTS` is replaced by what follows the
//! name, `$1` to `$9` by its words (quotes keep a phrase together); text with none of them gets
//! the arguments appended. The palette shows the `description:` of a front matter block, else the
//! file's first line, and its `argument-hint:`; `allowed-tools:` is kept for the listing only.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct CustomCommand {
    pub name: String,
    pub description: String,
    pub body: String,
    pub path: PathBuf,
    /// `argument-hint:` of the front matter.
    pub hint: String,
    /// `allowed-tools:` of the front matter, as written (not enforced).
    pub allowed_tools: String,
    /// Where it was found: `project .claude/commands`, `user .nexo/commands`.
    pub origin: String,
}

/// The words of `args`, a quoted phrase being one.
fn words(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut cur, mut quote, mut any) = (String::new(), None::<char>, false);
    for c in args.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

impl CustomCommand {
    /// The request the command sends with `args`.
    pub fn expand(&self, args: &str) -> String {
        let args = args.trim();
        let positional = (1..=9).any(|n| self.body.contains(&format!("${n}")));
        if !self.body.contains("$ARGUMENTS") && !positional {
            return if args.is_empty() { self.body.clone() } else { format!("{}\n\n{args}", self.body.trim_end()) };
        }
        let w = words(args);
        let mut out = self.body.replace("$ARGUMENTS", args);
        for n in (1..=9).rev() {
            out = out.replace(&format!("${n}"), w.get(n - 1).map_or("", String::as_str));
        }
        out
    }
}

/// The commands of the user and the project, in nexo's folders and the other tools'; names of
/// built-in commands are left out. A project nobody trusted yet (`nexo config trust`) has its
/// commands left out: their text comes from the repository.
pub fn load(cwd: &Path) -> Vec<CustomCommand> {
    let home = nexo_core::compat::foreign_home();
    load_from(cwd, &nexo_core::paths::config_dir(), &nexo_core::paths::trusted_projects_file(), home.as_deref())
}

fn load_from(cwd: &Path, config_dir: &Path, trust_file: &Path, home: Option<&Path>) -> Vec<CustomCommand> {
    let reserved = |n: &str| crate::commands::lookup(n).is_some();
    nexo_core::compat::commands(cwd, config_dir, trust_file, home, &reserved)
        .into_iter()
        .map(|c| CustomCommand {
            name: c.name,
            description: c.description,
            body: c.body,
            path: c.path,
            hint: c.hint,
            allowed_tools: c.allowed_tools,
            origin: c.origin,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_load_expand_and_the_project_wins() {
        let home = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(proj.path().join(".git")).unwrap();
        let mine = home.path().join("commands");
        let theirs = proj.path().join(".nexo/commands");
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::create_dir_all(&theirs).unwrap();
        std::fs::write(mine.join("triage.md"), "Review the diff for bugs.").unwrap();
        std::fs::write(mine.join("help.md"), "a built-in name: left out").unwrap();
        std::fs::write(
            theirs.join("triage.md"),
            "---\ndescription: review against our rules\n---\nReview $ARGUMENTS against CONTRIBUTING.md.",
        )
        .unwrap();
        std::fs::write(theirs.join("explain.md"), "# Explain the code\nExplain it simply.").unwrap();
        let trust = home.path().join("trusted-projects");
        let untrusted = load_from(proj.path(), home.path(), &trust, None);
        let names: Vec<&str> = untrusted.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["triage"], "an untrusted project's commands are left out");
        assert_eq!(untrusted[0].description, "Review the diff for bugs.");
        nexo_core::config::trust_project(proj.path(), &trust).unwrap();
        let cmds = load_from(proj.path(), home.path(), &trust, None);
        let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["explain", "triage"]);
        let review = &cmds[1];
        assert_eq!(review.description, "review against our rules");
        assert_eq!(review.origin, "project .nexo/commands");
        assert_eq!(review.expand("src/lib.rs"), "Review src/lib.rs against CONTRIBUTING.md.");
        assert_eq!(cmds[0].description, "Explain the code");
        assert_eq!(cmds[0].expand("the parser"), "# Explain the code\nExplain it simply.\n\nthe parser");
    }

    #[test]
    fn positional_arguments_fill_in_and_quotes_keep_a_phrase() {
        let c = |body: &str| CustomCommand {
            name: "x".into(),
            description: String::new(),
            body: body.into(),
            path: PathBuf::new(),
            hint: String::new(),
            allowed_tools: String::new(),
            origin: String::new(),
        };
        assert_eq!(
            c("Fix $1 in $2 (all: $ARGUMENTS)").expand("bug 'the parser' x"),
            "Fix bug in the parser (all: bug 'the parser' x)"
        );
        assert_eq!(c("Only $1, then $3.").expand("a"), "Only a, then .");
        assert_eq!(c("Plain.").expand(""), "Plain.");
    }

    #[test]
    fn the_other_tools_folders_are_read_and_nest_with_a_colon() {
        let (home, cfg, proj) =
            (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::create_dir_all(proj.path().join(".git")).unwrap();
        std::fs::create_dir_all(proj.path().join(".claude/commands/git")).unwrap();
        std::fs::write(proj.path().join(".claude/commands/git/sync.md"), "Sync $1").unwrap();
        std::fs::create_dir_all(home.path().join(".codex/prompts")).unwrap();
        std::fs::write(home.path().join(".codex/prompts/audit.md"), "Plan it").unwrap();
        let trust = cfg.path().join("trusted-projects");
        nexo_core::config::trust_project(proj.path(), &trust).unwrap();
        let cmds = load_from(proj.path(), cfg.path(), &trust, Some(home.path()));
        let names: Vec<(&str, &str)> = cmds.iter().map(|c| (c.name.as_str(), c.origin.as_str())).collect();
        assert_eq!(names, [("audit", "user .codex/prompts"), ("git:sync", "project .claude/commands")]);
    }
}
