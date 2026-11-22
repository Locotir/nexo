use super::*;
use nexo_core::skills::Skill;

impl Run {
    /// Return metadata, not bytes. Only the caller may read the checked file, through
    /// the no-follow opener; a read-hook rewrite cannot redirect a catalogue entry.
    pub(super) async fn worker_skill(
        &self,
        id: &str,
        tool: &dyn Tool,
        args: &Value,
        ctx: &ToolContext,
    ) -> Result<Skill, (String, Why)> {
        let deny = |s: &str| (s.to_owned(), Why::denied("rule"));
        let mut skill =
            args["name"].as_str().and_then(|n| tool.named_skill(n)).cloned().ok_or_else(|| deny("unknown skill"))?;
        if skill.source == "builtin" {
            return Ok(skill);
        }
        let read = self
            .tools
            .get("read")
            .filter(|r| r.kind() == ToolKind::Read)
            .ok_or_else(|| deny("filesystem skill requires the worker's read tool"))?;
        let original = skill.file();
        let mut paths = vec![original];
        if let Some(checkout) = &self.checkout {
            if matches!(skill.source.as_str(), ".nexo/skills" | ".agents/skills" | ".claude/skills") {
                let root = nexo_core::paths::canonical(checkout).map_err(|_| deny("cannot resolve worker checkout"))?;
                let rel =
                    skill.dir.strip_prefix(root).map_err(|_| deny("project skill is not in the launch checkout"))?;
                let worker_dir = ctx.cwd.join(rel);
                let worker_file = worker_dir.join("SKILL.md");
                match std::fs::symlink_metadata(&worker_file) {
                    Ok(_) => {
                        skill.dir = worker_dir;
                        paths.push(worker_file);
                    }
                    // Never hand an isolated worker the original checkout's skill folder.
                    // Untracked or ignored skills are absent from fresh worktrees; copying
                    // their resources safely would require separately authorizing each one.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        return Err(deny("project skill is unavailable in the worker worktree; commit it or run without worktree isolation"));
                    }
                    Err(_) => return Err(deny("cannot inspect worker skill file")),
                }
            }
        }
        for (i, path) in paths.iter().enumerate() {
            let path = path
                .to_str()
                .filter(|s| path.is_absolute() && s.trim() == *s)
                .ok_or_else(|| deny("invalid skill path"))?;
            let a = json!({"path": path});
            if let Some(h) = &self.host {
                let o = h.hook("pre_tool", "read", self.fields("read", &a)).await;
                if o.blocked.is_some() || o.updated_input.is_some_and(|u| u != a) {
                    return Err(("skill read blocked or redirected by hook".into(), Why::denied("hook")));
                }
                let verdict = h
                    .permit(&format!("{id}.read{i}"), &self.profile.name, read.as_ref(), &a, &ctx.cancel, o.decision)
                    .await;
                let (status, why) = match &verdict {
                    Ok(w) => ("allowed", *w),
                    Err((_, w)) => ("denied", *w),
                };
                h.audit(AuditCall {
                    tool: "read",
                    args: &a,
                    summary: "authorize skill file",
                    why,
                    status,
                    duration_ms: 0,
                })
                .await;
                verdict.map_err(|(_, w)| ("skill read permission denied".into(), w))?;
            } else if !self.profile.read_only
                || {
                    let base = if i == 0 { self.checkout.as_ref().unwrap_or(&ctx.cwd) } else { &ctx.cwd };
                    !Path::new(path).starts_with(
                        nexo_core::paths::canonical(base).map_err(|_| deny("cannot resolve worker directory"))?,
                    ) || outside(&a, base).is_some()
                }
                || ctx.check.as_ref().is_some_and(|c| c("read", ToolKind::Read, &a) != nexo_core::tool::Check::Allow)
            {
                return Err(deny("skill read is not permitted inside this worker"));
            }
        }
        Ok(skill)
    }
}
