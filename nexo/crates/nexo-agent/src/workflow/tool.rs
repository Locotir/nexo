//! The `workflow` tool: asks leave once, runs the workflow, answers once.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use nexo_core::provider::ToolSpec;
use nexo_core::tool::{Compact, Tool, ToolContext, ToolKind, ToolOutput};
use serde_json::{json, Value};

use super::file::{self, parse, problem_lines, saved, subject, valid_id, Workflow as Wf, MAX_PROMPT};
use super::run::{self, share, Outcome, State, StepOutcome};
use crate::subagent::SubAgent;

/// Names listed in the tool's description, and the characters of each line.
const LISTED: usize = 20;
const LINE: usize = 80;
/// Phase lines the question shows.
const PHASES_SHOWN: usize = 12;
/// What the turn puts in the call before it asks: the subject (`name@<32 hex>`) of the saved workflow the question is
/// about. A rule is matched against it, and `run` refuses when the file is no longer that one.
pub const PIN: &str = "pinned";

pub struct Workflow {
    sub: Arc<SubAgent>,
    cwd: PathBuf,
}

impl Workflow {
    pub fn new(sub: Arc<SubAgent>, cwd: PathBuf) -> Workflow {
        Workflow { sub, cwd }
    }

    /// Whether `cwd` has a saved workflow: the tool is offered only then.
    pub fn any_saved(cwd: &Path) -> bool {
        !saved(cwd).is_empty()
    }
}

/// The workflow a call names, and whether it was written inline. Checks of the file only; the kinds of
/// sub-agent are checked where the sub-agents are known.
fn load(args: &Value, cwd: &Path) -> Result<(Wf, bool), String> {
    let text = |k: &str| match args.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(format!("`{k}` must be text")),
    };
    let (name, inline) = (text("name")?, text("inline")?);
    // before anything else: a name that cannot be a file name is not looked up
    if let Some(n) = name.filter(|n| !valid_id(n)) {
        return Err(format!(
            "`{}` is not a workflow name; names use 1 to 32 characters of a-z, 0-9, - and _",
            cut_line(&n.escape_debug().to_string(), 40)
        ));
    }
    let list = || saved(cwd).iter().map(|s| s.name.clone()).collect::<Vec<_>>().join(", ");
    match (name, inline) {
        (Some(_), Some(_)) => Err("give `name` or `inline`, not both".into()),
        (None, None) => Err(format!("give `name` (a saved workflow: {}) or `inline` (the TOML text)", list())),
        (None, Some(t)) => parse("inline", t).map(|w| (w, true)).map_err(|p| refused("inline", &p)),
        (Some(n), None) => {
            let all = saved(cwd);
            let Some(s) = all.iter().find(|s| s.name == n) else {
                return Err(format!("no saved workflow `{n}`; there are: {}", list()));
            };
            if args.get(PIN).and_then(Value::as_str).is_some_and(|p| p != subject(&s.name, &s.text)) {
                return Err(format!(
                    "workflow `{n}` changed after you were asked about it, so nothing was started; ask again to run the new file."
                ));
            }
            parse(&s.name, &s.text).map(|w| (w, false)).map_err(|p| refused(n, &p))
        }
    }
}

/// Sets (or clears) [`PIN`] in the arguments of a `workflow` call, before the person is asked: the subject of the
/// saved workflow the name points to now, or nothing when it does not load, is inline text or is no valid name.
/// Whatever the model put there is dropped.
pub fn pin(args: &mut Value, cwd: &Path) {
    let Some(obj) = args.as_object_mut() else { return };
    obj.remove(PIN);
    let on = |k: &str| obj.get(k).and_then(Value::as_str).filter(|s| !s.trim().is_empty()).map(str::to_string);
    let (Some(name), None) = (on("name"), on("inline")) else { return };
    if !valid_id(&name) {
        return;
    }
    if let Some(s) = saved(cwd).into_iter().find(|s| s.name == name).filter(|s| parse(&s.name, &s.text).is_ok()) {
        obj.insert(PIN.into(), Value::String(subject(&s.name, &s.text)));
    }
}

fn refused(name: &str, problems: &[file::Problem]) -> String {
    format!("workflow `{name}` has problems, so nothing was started:\n{}", problem_lines(problems))
}

fn cut_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() > max {
        format!("{} ...", line.chars().take(max).collect::<String>())
    } else {
        line.to_string()
    }
}

/// What the question shows: one line, and for an inline workflow its phases and step ids.
pub fn question(args: &Value, cwd: &Path) -> (String, Vec<String>) {
    match load(args, cwd) {
        Err(e) => (format!("workflow: {}", cut_line(&e, 120)), Vec::new()),
        Ok((wf, inline)) => {
            let plan = wf.plan();
            let what = if inline { "inline workflow".to_string() } else { format!("workflow `{}`", wf.name) };
            let summary = format!(
                "{what}: up to {} sub-agents (at least {}), {} tokens at most",
                plan.at_most, plan.certain, wf.max_tokens
            );
            let mut detail = Vec::new();
            if !wf.description.is_empty() {
                detail.push(cut_line(&wf.description, 160));
            }
            if inline {
                for p in &wf.phases {
                    let ids: Vec<String> = p
                        .steps
                        .iter()
                        .map(|s| {
                            let mut t = s.id.clone();
                            if let Some(e) = &s.each {
                                t.push_str(&format!(" (each of {e}"));
                                if s.votes > 1 {
                                    t.push_str(&format!(", {} votes", s.votes));
                                }
                                t.push(')');
                            } else if s.votes > 1 {
                                t.push_str(&format!(" ({} votes)", s.votes));
                            }
                            t
                        })
                        .collect();
                    detail.push(format!("{}: {}", p.title, ids.join(", ")));
                }
                if detail.len() > PHASES_SHOWN {
                    let more = detail.len() - PHASES_SHOWN;
                    detail.truncate(PHASES_SHOWN);
                    detail.push(format!("+{more} more"));
                }
            }
            (summary, detail)
        }
    }
}

const FORMAT: &str = "\
Format of `inline` (TOML): description; max_parallel (default 4, at most 8); max_agents (default 12, at most \
50); max_tokens (default 400000). Then [[phase]] with title, each with [[phase.step]]: id (a-z 0-9 - _), \
prompt, agent (optional kind), returns = \"list\" (the report ends with a JSON array of strings), \
each = \"<id of a step of an earlier phase that returns a list>\" (one sub-agent per item), votes = 1 to 5 \
(independent copies). Phases run in order, the steps of a phase together. In a prompt: {{args}}, {{item}} \
(with each), {{steps.<id>.report}} (a step of an earlier phase).";

#[async_trait]
impl Tool for Workflow {
    fn name(&self) -> &str {
        "workflow"
    }

    fn spec(&self) -> ToolSpec {
        let all = saved(&self.cwd);
        let mut listed = String::new();
        for s in all.iter().take(LISTED) {
            let d = file::parse(&s.name, &s.text).map(|w| w.description).unwrap_or_default();
            let line = if d.is_empty() { s.name.clone() } else { format!("{}: {d}", s.name) };
            listed.push_str(&format!("\n- {}", cut_line(&line, LINE)));
        }
        if all.len() > LISTED {
            listed.push_str(&format!("\n- ... and {} more", all.len() - LISTED));
        }
        let saved_text = if listed.is_empty() { " none".to_string() } else { listed };
        ToolSpec {
            name: "workflow".into(),
            description: format!(
                "Run several sub-agents as one job: phases in order, the steps of a phase together, a step once per \
                 item of an earlier list, copies of a step that vote. Only when the person asks for a workflow or \
                 the job needs that width; the person is asked first. You get one answer: a line per step, then \
                 the reports of the last phase. Give `name` of a saved workflow, or `inline`; `args` is the text \
                 the prompts get as {{{{args}}}}.\nSaved workflows:{saved_text}\n{FORMAT}"
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "A saved workflow."},
                    "inline": {"type": "string", "description": "A workflow written out, in TOML."},
                    "args": {"type": "string", "description": "What the prompts get as {{args}}."}
                }
            }),
        }
    }

    fn compact(&self) -> Option<Compact> {
        Some(Compact {
            description:
                "Run a saved or inline (TOML) workflow of sub-agents in phases; one answer. Asks the person first.",
            params: &[("name", "A saved workflow"), ("inline", "A workflow in TOML"), ("args", "Text for {{args}}")],
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn handles_cancel(&self) -> bool {
        true
    }

    fn summary(&self, args: &Value) -> String {
        question(args, &self.cwd).0
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let (wf, _) = match load(&args, &ctx.cwd) {
            Ok(x) => x,
            Err(e) => return ToolOutput::error(e),
        };
        let given = match args.get("args") {
            None | Some(Value::Null) => "",
            Some(Value::String(s)) => s.as_str(),
            Some(_) => return ToolOutput::error("`args` must be text"),
        };
        if given.len() > MAX_PROMPT {
            return ToolOutput::error(format!(
                "`args` is {} KiB; the most is {} KiB",
                given.len() / 1024,
                MAX_PROMPT / 1024
            ));
        }
        let problems = wf.check_agents(&self.sub.kinds());
        if !problems.is_empty() {
            return ToolOutput::error(refused(&wf.name, &problems));
        }
        let out = run::run(&self.sub, &wf, given, ctx).await;
        let text = render(&wf, &out);
        if out.steps.iter().any(|s| s.state.usable()) {
            ToolOutput::ok(text)
        } else {
            ToolOutput::error(text)
        }
    }
}

fn state_text(s: &StepOutcome) -> String {
    match &s.state {
        State::Ok => "ok".into(),
        State::Partial { missing } => format!("partial, {missing} without a report"),
        State::Failed(e) => format!("failed: {}", cut_line(e, 160)),
        State::Skipped(e) => format!("skipped: {}", cut_line(e, 160)),
        State::NotRun(e) => format!("not run: {}", cut_line(e, 160)),
    }
}

/// The one answer: a line per step, the notes, and the reports of the last phase that ran.
fn render(wf: &Wf, out: &Outcome) -> String {
    let mut t =
        format!("workflow `{}`: {} sub-agents started, {} tokens, {} s.", wf.name, out.agents, out.tokens, out.secs);
    if let Some(stop) = out.stop {
        t.push_str(&format!(" Stopped: {}; no new sub-agent started after that.", stop.text()));
    }
    if out.cancelled {
        t.push_str(" Interrupted by the person.");
    }
    if let Some(why) = &out.ended {
        t.push_str(&format!(" Ended early: {why}."));
    }
    t.push_str("\n\nSteps:");
    for s in &out.steps {
        let counts = if s.agents > 0 {
            format!(
                ", {} sub-agent{}, {} tokens, {} s",
                s.agents,
                if s.agents == 1 { "" } else { "s" },
                s.tokens,
                s.secs
            )
        } else {
            String::new()
        };
        t.push_str(&format!("\n- {}: {}{counts}", s.id, state_text(s)));
        for n in &s.notes {
            t.push_str(&format!("\n  note: {n}"));
        }
    }
    let last = wf.phases.len().saturating_sub(1);
    let shown = (0..wf.phases.len())
        .rev()
        .find(|i| out.steps.iter().any(|s| s.phase == *i && s.state.usable() && s.has_report()));
    if let Some(i) = shown {
        if i != last {
            t.push_str(&format!(
                "\n\nThe last phase did not run; these are the reports of phase \"{}\" instead.",
                wf.phases[i].title
            ));
        }
        // every report of the phase has a share of the group cap: none comes first, none vanishes
        let steps: Vec<&StepOutcome> =
            out.steps.iter().filter(|s| s.phase == i && s.state.usable() && s.has_report()).collect();
        let each = share(steps.iter().map(|s| s.parts.len()).sum());
        for s in steps {
            t.push_str(&format!("\n\n== {} ==\n{}", s.id, s.report(each)));
        }
    }
    t
}
