//! Commands of one's own: a markdown file per command in `~/.nexo/commands/` or in
//! the project's `.nexo/commands/` (the project's win). `/name args` sends the file's text
//! as a request, with `$ARGUMENTS` replaced by what follows the name (appended when the
//! text has no `$ARGUMENTS`). The description the palette shows is the `description:` of a
//! front matter block, else the file's first line.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct CustomCommand {
    pub name: String,
    pub description: String,
    pub body: String,
    pub path: PathBuf,
}

impl CustomCommand {
    /// The request the command sends with `args`.
    pub fn expand(&self, args: &str) -> String {
        let args = args.trim();
        if self.body.contains("$ARGUMENTS") {
            self.body.replace("$ARGUMENTS", args)
        } else if args.is_empty() {
            self.body.clone()
        } else {
            format!("{}\n\n{args}", self.body.trim_end())
        }
    }
}

/// The commands in the user's directory and the project's, the project's winning on a
/// name both have; names of built-in commands are left out. A project nobody trusted yet
/// (`nexo config trust`) has its commands left out: their text comes from the repository.
pub fn load(cwd: &Path) -> Vec<CustomCommand> {
    load_from(cwd, &nexo_core::paths::config_dir(), &nexo_core::paths::trusted_projects_file())
}

fn load_from(cwd: &Path, config_dir: &Path, trust_file: &Path) -> Vec<CustomCommand> {
    let mut dirs = vec![config_dir.join("commands")];
    if let Some(p) = nexo_core::paths::project_dir(cwd) {
        if p.parent().is_some_and(|root| nexo_core::config::is_trusted(root, trust_file)) {
            dirs.push(p.join("commands"));
        }
    }
    let mut out: Vec<CustomCommand> = Vec::new();
    for d in dirs {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        files.sort();
        for f in files {
            if f.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Some(name) = f.file_stem().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()) else { continue };
            if name.is_empty() || name.contains(char::is_whitespace) || crate::commands::lookup(&name).is_some() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            let (description, body) = parse(&text);
            out.retain(|c| c.name != name);
            out.push(CustomCommand { name, description, body, path: f });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `(description, body)` of a command file.
fn parse(text: &str) -> (String, String) {
    if let Some(rest) = text.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            let front = &rest[..end];
            let body = rest[end + 4..].trim_start_matches('\n').to_string();
            let description = front
                .lines()
                .find_map(|l| l.strip_prefix("description:"))
                .map(|d| d.trim().trim_matches('"').to_string())
                .unwrap_or_else(|| first_line(&body));
            return (description, body);
        }
    }
    (first_line(text), text.to_string())
}

fn first_line(text: &str) -> String {
    let l = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().trim_start_matches('#').trim();
    l.chars().take(70).collect()
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
        std::fs::write(mine.join("review.md"), "Review the diff for bugs.").unwrap();
        std::fs::write(mine.join("help.md"), "a built-in name: left out").unwrap();
        std::fs::write(
            theirs.join("review.md"),
            "---\ndescription: review against our rules\n---\nReview $ARGUMENTS against CONTRIBUTING.md.",
        )
        .unwrap();
        std::fs::write(theirs.join("explain.md"), "# Explain the code\nExplain it simply.").unwrap();
        let trust = home.path().join("trusted-projects");
        let untrusted = load_from(proj.path(), home.path(), &trust);
        let names: Vec<&str> = untrusted.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["review"], "an untrusted project's commands are left out");
        assert_eq!(untrusted[0].description, "Review the diff for bugs.");
        nexo_core::config::trust_project(proj.path(), &trust).unwrap();
        let cmds = load_from(proj.path(), home.path(), &trust);
        let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["explain", "review"]);
        let review = &cmds[1];
        assert_eq!(review.description, "review against our rules");
        assert_eq!(review.expand("src/lib.rs"), "Review src/lib.rs against CONTRIBUTING.md.");
        assert_eq!(cmds[0].description, "Explain the code");
        assert_eq!(cmds[0].expand("the parser"), "# Explain the code\nExplain it simply.\n\nthe parser");
    }
}
