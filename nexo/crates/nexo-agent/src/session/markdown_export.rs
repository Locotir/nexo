use super::*;

impl Session {
    /// Where `export` writes when no path is given: next to the session file.
    pub fn export_path(&self) -> PathBuf {
        let d = if self.path.as_os_str().is_empty() {
            dir()
        } else {
            self.path.parent().map(Path::to_path_buf).unwrap_or_else(dir)
        };
        d.join(format!("{}.md", self.meta.id))
    }

    /// The conversation as markdown. `summary` names what a call does
    /// (`read src/a.rs`) from the tool's name and arguments.
    pub fn export_markdown(&self, summary: impl Fn(&str, &serde_json::Value) -> String) -> String {
        self.markdown(summary, false)
    }

    /// [`Session::export_markdown`] for sharing: the output of a call that names a `.env*` file is replaced
    /// whole by `[redacted: .env file]`.
    pub fn export_markdown_redacted(&self, summary: impl Fn(&str, &serde_json::Value) -> String) -> String {
        self.markdown(summary, true)
    }

    fn markdown(&self, summary: impl Fn(&str, &serde_json::Value) -> String, hide_env: bool) -> String {
        const FOLD_LINES: usize = 60;
        let mut out = String::new();
        let title = if self.meta.title.is_empty() { "nexo session".to_string() } else { self.meta.title.clone() };
        out.push_str(&format!("# {title}\n\n"));
        out.push_str(&format!(
            "- session: `{}`\n- started: {}\n- model: {}/{}\n- directory: `{}`\n\n",
            self.meta.id,
            self.meta.created,
            self.meta.provider,
            self.meta.model,
            self.meta.cwd.display()
        ));
        for m in &self.messages {
            if m.is_tool_results() {
                continue; // shown under the calls that made them
            }
            out.push_str(if m.role == Role::User { "## You\n\n" } else { "## nexo\n\n" });
            for p in &m.content {
                match p {
                    Part::Text { text } => {
                        out.push_str(text.trim_end());
                        out.push_str("\n\n");
                    }
                    Part::Reasoning { text, .. } if !text.trim().is_empty() => {
                        out.push_str("<details><summary>reasoning</summary>\n\n");
                        out.push_str(text.trim_end());
                        out.push_str("\n\n</details>\n\n");
                    }
                    Part::ToolCall { id, name, args } => {
                        out.push_str(&format!("**{}** {}\n\n", name, summary(name, args)));
                        let body = if hide_env && crate::scrub::touches_env_file(args) {
                            crate::scrub::ENV_BODY.to_string()
                        } else {
                            match self.outputs.get(id) {
                                Some(o) => o.content.clone(),
                                None => self
                                    .messages
                                    .iter()
                                    .flat_map(|m| m.content.iter())
                                    .find_map(|p| match p {
                                        Part::ToolResult { call_id, content, .. } if call_id == id => {
                                            Some(Session::result_text(content))
                                        }
                                        _ => None,
                                    })
                                    .unwrap_or_default(),
                            }
                        };
                        let lines: Vec<&str> = body.lines().collect();
                        let fence = if body.contains("```") { "````" } else { "```" };
                        out.push_str(fence);
                        out.push('\n');
                        for l in lines.iter().take(FOLD_LINES) {
                            out.push_str(l);
                            out.push('\n');
                        }
                        if lines.len() > FOLD_LINES {
                            out.push_str(&format!("… {} more lines\n", lines.len() - FOLD_LINES));
                        }
                        out.push_str(fence);
                        out.push_str("\n\n");
                    }
                    _ => {}
                }
            }
        }
        let u = self.usage;
        out.push_str(&format!(
            "---\n\ntokens: {} in, {} out, {} cached read, {} cached write · cost: ${:.4}\n",
            u.input, u.output, u.cache_read, u.cache_write, self.cost
        ));
        out
    }
}
