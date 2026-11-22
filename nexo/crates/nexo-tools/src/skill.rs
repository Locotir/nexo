//! `skill`: load an Agent Skill by name. The session catalogue is supplied to the system prompt;
//! the body is read on demand and comes back as an ordinary tool result.

use std::sync::Arc;

use async_trait::async_trait;
use nexo_core::skills::{self, Skill};
use nexo_core::{Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

pub struct SkillTool {
    skills: Arc<Vec<Skill>>,
    description: String,
    /// The same for a small window: the list shortened.
    compact: String,
}

/// The tool for `skills`; None when there are none, so the model is not offered an empty tool.
pub fn tool(skills: Vec<Skill>) -> Option<Arc<dyn Tool>> {
    if skills.is_empty() {
        return None;
    }
    let description = "Load a skill by name from the system prompt catalogue. Follow its instructions as applicable; files it mentions are relative to its folder (read them with `read`).".to_string();
    let compact = "Load a skill by name from the system prompt catalogue.".to_string();
    Some(Arc::new(SkillTool { skills: Arc::new(skills), description, compact }))
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

    fn compact(&self) -> Option<nexo_core::Compact<'_>> {
        Some(nexo_core::Compact { description: &self.compact, params: &[] })
    }

    fn skill_catalog(&self, compact: bool) -> Option<String> {
        Some(if compact { skills::compact_listing(&self.skills) } else { skills::listing(&self.skills) })
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
            post_check: false,
            check_secs: 45,
            claims: Default::default(),
            detach: None,
            tool_names: Default::default(),
            room: None,
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
    async fn loads_a_body_without_listing_skills_in_description() {
        let d = tempfile::tempdir().unwrap();
        let t = tool(make(d.path())).unwrap();
        assert!(!t.spec().description.contains("Work with PDFs"));
        assert!(!t.compact().unwrap().description.contains("Work with PDFs"));
        assert!(t.skill_catalog(false).unwrap().contains("- pdf: Work with PDFs"));
        let out = t.run(json!({"name": "pdf", "arguments": "merge a and b"}), &ctx(d.path())).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Use forms.md.") && out.content.contains("Arguments: merge a and b"));
        assert!(out.content.contains(&d.path().join("pdf").display().to_string()));
        let bad = t.run(json!({"name": "nope"}), &ctx(d.path())).await;
        assert!(bad.is_error && bad.content.contains("pdf"));
        assert!(t.run(json!({}), &ctx(d.path())).await.is_error);
    }

    #[tokio::test]
    async fn builtins_are_advertised_and_loaded_on_demand() {
        let d = tempfile::tempdir().unwrap();
        let t = tool(skills::discover(d.path(), false)).unwrap();
        assert!(t.skill_catalog(false).unwrap().contains("- research:"));
        assert!(!t.spec().description.contains("- research:"));
        assert!(!t.spec().description.contains("Prefer primary sources"));
        let out = t.run(json!({"name": "research", "arguments": "topic"}), &ctx(d.path())).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Prefer primary sources"));
        assert!(out.content.contains("Arguments: topic"));
    }

    #[test]
    fn no_skills_no_tool() {
        assert!(tool(Vec::new()).is_none());
    }
}
