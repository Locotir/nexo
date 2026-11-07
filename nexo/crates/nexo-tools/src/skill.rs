//! `skill`: load the instructions of an Agent Skill by name. The description lists the skills
//! (name and short description) found when the session started, so the text the model sees is the
//! same on every request; the body is read from disk when the tool is called and comes back as an
//! ordinary tool result.

use std::sync::Arc;

use async_trait::async_trait;
use nexo_core::skills::{self, Skill};
use nexo_core::{Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

pub struct SkillTool {
    skills: Arc<Vec<Skill>>,
    description: String,
}

/// The tool for `skills`; None when there are none, so the model is not offered an empty tool.
pub fn tool(skills: Vec<Skill>) -> Option<Arc<dyn Tool>> {
    if skills.is_empty() {
        return None;
    }
    let description = format!(
        "Load a skill: step-by-step instructions for a kind of task, kept in a folder. When a task matches one \
         of the skills below, call this first with its name, then follow what comes back. The files a skill \
         mentions are relative to its folder (read them with `read`). Skills:\n{}",
        skills::listing(&skills)
    );
    Some(Arc::new(SkillTool { skills: Arc::new(skills), description }))
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "skill".into(),
            description: self.description.clone(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "The skill's name, as listed."},
                    "arguments": {"type": "string", "description": "What the person asked, if the skill takes input."}
                },
                "required": ["name"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn summary(&self, args: &Value) -> String {
        format!("skill {}", args.get("name").and_then(Value::as_str).unwrap_or("?"))
    }

    async fn run(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
        let Some(name) = args.get("name").and_then(Value::as_str) else {
            return ToolOutput::error("`name` is required: the skill's name from the list");
        };
        let Some(skill) = skills::find(&self.skills, name) else {
            let names: Vec<&str> = self.skills.iter().map(|s| s.name.as_str()).collect();
            return ToolOutput::error(format!("no skill called `{name}`; the skills are: {}", names.join(", ")));
        };
        let extra = args.get("arguments").and_then(Value::as_str).unwrap_or("");
        match skill.instructions(extra) {
            Ok(t) => ToolOutput::ok(t),
            Err(e) => ToolOutput::error(format!("cannot read {}: {e}", skill.file().display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(cwd: &std::path::Path) -> ToolContext {
        ToolContext {
            cwd: cwd.to_path_buf(),
            session_id: "s".into(),
            call_id: "c".into(),
            cancel: Default::default(),
            progress: None,
            env: Default::default(),
            jobs: Default::default(),
            seen: Default::default(),
            check: None,
            sandbox: Default::default(),
            vision: true,
        }
    }

    fn make(d: &std::path::Path) -> Vec<Skill> {
        let dir = d.join("pdf");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "---\nname: pdf\ndescription: Work with PDFs\n---\nUse forms.md.\n")
            .unwrap();
        skills::discover_in(d, d, &d.join(".nexo"), false)
            .into_iter()
            .chain(std::iter::once(Skill {
                name: "pdf".into(),
                description: "Work with PDFs".into(),
                dir,
                allowed_tools: vec![],
                source: ".claude/skills".into(),
            }))
            .collect()
    }

    #[tokio::test]
    async fn loads_a_body_and_lists_the_skills_in_its_description() {
        let d = tempfile::tempdir().unwrap();
        let t = tool(make(d.path())).unwrap();
        assert!(t.spec().description.contains("- pdf: Work with PDFs"));
        let out = t.run(json!({"name": "pdf", "arguments": "merge a and b"}), &ctx(d.path())).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Use forms.md.") && out.content.contains("Arguments: merge a and b"));
        assert!(out.content.contains(&d.path().join("pdf").display().to_string()));
        let bad = t.run(json!({"name": "nope"}), &ctx(d.path())).await;
        assert!(bad.is_error && bad.content.contains("pdf"));
        assert!(t.run(json!({}), &ctx(d.path())).await.is_error);
    }

    #[test]
    fn no_skills_no_tool() {
        assert!(tool(Vec::new()).is_none());
    }
}
