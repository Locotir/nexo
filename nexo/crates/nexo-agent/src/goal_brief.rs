//! `goal_brief`: an auxiliary, unverified interpretation of the person's authoritative goal. The tool only says how the call
//! reads: the agent loop answers it, since the goal lives in the loop's state.
//!
//! The brief itself is made by [`ask`]: one short request, whenever a goal is set, that judges the person's words and
//! answers `CLEAR` (the words stand as they are, no brief) or `REWRITE` with the four fields of [`FIELDS`].

use async_trait::async_trait;
use futures::StreamExt;
use nexo_core::message::Message;
use nexo_core::provider::ModelRequest;
use nexo_core::stream::{Collector, StreamEvent, Usage};
use nexo_core::{Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Map, Value};

use crate::client::{ModelClient, Target};

pub struct GoalBrief;

/// The fields of a call, in the order the brief shows them: (key, label).
pub const FIELDS: [(&str, &str); 4] = [("want", "Want"), ("why", "Why"), ("how", "How"), ("done", "Done when")];

/// The brief a call gives, or the first field it lacks.
pub fn brief(args: &Value) -> Result<String, &'static str> {
    let mut lines = Vec::new();
    for (key, label) in FIELDS {
        let v = args.get(key).and_then(Value::as_str).map(str::trim).unwrap_or_default();
        if v.is_empty() {
            return Err(key);
        }
        lines.push(format!("{label}: {v}"));
    }
    Ok(lines.join("\n"))
}

/// What the check asks of the model, whose reply [`parse_check`] reads.
const CHECK_SYSTEM: &str = "You judge a goal that a person gave to a coding agent. Question: is this goal clear, \
ordered and well-defined enough to work from as written, or does it need restructuring (ordering, defining done, \
separating sub-tasks)? If it can be worked from as written, reply with the single word CLEAR. Otherwise reply with the \
word REWRITE and then exactly these four lines, each on one line: `Want:` what they want, concrete, in one or two \
sentences, with the sub-tasks in order; `Why:` the need behind it; `How:` how they expect it done: the quality bar, \
conventions and limits they would take for granted; `Done when:` checks anyone could run or see. Use only what the \
goal says or plainly implies, and invent no requirements. Write in the goal's language. When in doubt, reply CLEAR. \
No other text.";

/// The most characters of the person's words the check reads.
const CHECK_CHARS: usize = 6000;

/// The reply tokens the check may take: a rewrite is four short lines, the rest is room for a model that reasons first.
const CHECK_TOKENS: u32 = 1024;

/// The brief a reply to the check holds: None for `CLEAR`, and for any reply that is not a whole `REWRITE`.
pub fn parse_check(reply: &str) -> Option<String> {
    let reply = reply.trim();
    let head = reply.get(..7).filter(|h| h.eq_ignore_ascii_case("REWRITE"))?;
    let mut fields = Map::new();
    let mut at: Option<&str> = None;
    for line in reply[head.len()..].lines() {
        let line = line.trim().trim_start_matches(['*', '-', '`', '>', ':', ' ']);
        let labelled = FIELDS.iter().find_map(|(key, label)| {
            let (l, rest) = (line.get(..label.len())?, line.get(label.len()..)?);
            let rest = rest.trim_start_matches(['*', '`']).strip_prefix(':')?;
            l.eq_ignore_ascii_case(label).then_some((*key, rest))
        });
        let (key, text) = match (labelled, at) {
            (Some((key, rest)), _) => (key, rest),
            (None, Some(key)) => (key, line),
            (None, None) => continue,
        };
        at = Some(key);
        let old = fields.get(key).and_then(Value::as_str).unwrap_or_default();
        fields.insert(key.into(), json!(format!("{old} {}", text.trim_matches(['*', '`', ' '])).trim()));
    }
    brief(&Value::Object(fields)).ok()
}

/// The check for `goal`: the brief that restates it when it needs one (None when it stands as written), what the
/// request spent and the model.
pub async fn ask(
    client: &dyn ModelClient,
    target: &Target,
    retries: u32,
    goal: &str,
) -> anyhow::Result<(Option<String>, Usage, String)> {
    let (_, cfg, model) = target.get();
    let words: String = goal.chars().take(CHECK_CHARS).collect();
    let mut req = ModelRequest::new(model.clone(), CHECK_SYSTEM, vec![Message::user(format!("Goal:\n{words}"))]);
    req.max_tokens = CHECK_TOKENS;
    req.temperature = Some(0.0);
    // a verdict and four lines: the least thinking the model takes, not the session's level
    req.reasoning = nexo_providers::probe::side_reasoning(&cfg, &model);
    let mut stream = client.stream(&cfg, &req, retries).await?;
    let mut c = Collector::new();
    while let Some(ev) = stream.next().await {
        if let StreamEvent::Error(e) = &ev {
            anyhow::bail!("the goal check failed: {e}");
        }
        c.push(ev);
    }
    Ok((parse_check(&c.message().text()), c.usage, model))
}

#[async_trait]
impl Tool for GoalBrief {
    fn name(&self) -> &str {
        "goal_brief"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "goal_brief".into(),
            description: "Retired; only the person's /goal sets the goal.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "want": {"type": "string", "description": "What they want, concrete, in one or two sentences."},
                    "why": {"type": "string", "description": "Why they want it: the need behind it."},
                    "how": {"type": "string", "description": "How they expect it done: the quality bar, conventions and limits they would take for granted."},
                    "done": {"type": "string", "description": "When it counts as done: checks anyone could run or see."}
                },
                "required": ["want", "why", "how", "done"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn summary(&self, args: &Value) -> String {
        let want = args.get("want").and_then(Value::as_str).unwrap_or("?");
        format!("goal_brief: restating the goal · {}", want.chars().take(60).collect::<String>())
    }

    async fn run(&self, _args: Value, _ctx: &ToolContext) -> ToolOutput {
        ToolOutput::error("goal_brief is retired; only the person's /goal sets the goal")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_brief_needs_every_field() {
        let full = json!({"want": "a", "why": "b", "how": "c", "done": " d "});
        assert_eq!(brief(&full).unwrap(), "Want: a\nWhy: b\nHow: c\nDone when: d");
        assert!(brief(&full).unwrap().starts_with(nexo_core::agent::GOAL_BRIEF_HEAD));
        assert_eq!(brief(&json!({"want": "a", "why": "b", "how": " "})), Err("how"));
    }

    #[test]
    fn the_check_reads_clear_and_a_whole_rewrite_only() {
        assert_eq!(parse_check(" CLEAR\n"), None);
        assert_eq!(parse_check("clear."), None);
        let whole = "REWRITE\nWant: 1. fix a\n2. fix b\nWhy: **b**\nHow: c\n- Done when: tests pass";
        assert_eq!(parse_check(whole).unwrap(), "Want: 1. fix a 2. fix b\nWhy: b\nHow: c\nDone when: tests pass");
        assert_eq!(parse_check("REWRITE: Want: a\nWhy: b\nHow: c\nDone when: d").unwrap().lines().count(), 4);
        assert_eq!(parse_check("REWRITE\nWant: a\nWhy: b"), None, "a field is missing");
        assert_eq!(parse_check("Want: a\nWhy: b\nHow: c\nDone when: d"), None, "no verdict");
        assert_eq!(parse_check(""), None);
    }
}
