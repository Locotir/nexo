//! Questions to the person: `ask_user` (pick an option or write an answer)
//! and `exit_plan` (approve the plan made in plan mode). The question goes out
//! as an event, the answer comes back as a command, like a permission ask.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nexo_core::agent::{AgentEvent, EventTx};
use nexo_core::{PermissionMode, Question, QuestionOption, Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};
use tokio::sync::oneshot;

/// Options a question offers at most (the person can always write their own).
pub const MAX_OPTIONS: usize = 6;

pub const APPROVE_AUTO: &str = "Yes, auto-accept edits";
pub const APPROVE_ASK: &str = "Yes, approve each edit";
pub const KEEP_PLANNING: &str = "No, keep planning";

pub struct ChannelAsker {
    events: EventTx,
    pending: Mutex<HashMap<String, oneshot::Sender<Option<String>>>>,
}

impl ChannelAsker {
    pub fn new(events: EventTx) -> Arc<ChannelAsker> {
        Arc::new(ChannelAsker { events, pending: Mutex::new(HashMap::new()) })
    }

    /// Puts the question and waits; None when it was closed or dropped.
    pub async fn ask(&self, q: Question) -> Option<String> {
        let id = q.id.clone();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), tx);
        if self.events.send(AgentEvent::Question { question: q }).is_err() {
            self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            return None;
        }
        rx.await.unwrap_or(None)
    }

    /// Answers the question with this id; false when none waits for it.
    pub fn resolve(&self, id: &str, answer: Option<String>) -> bool {
        let tx = self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
        let Some(tx) = tx else { return false };
        let _ = self.events.send(AgentEvent::QuestionResolved { id: id.to_string(), answer: answer.clone() });
        tx.send(answer).is_ok()
    }

    /// Drops every question still waiting (on interrupt and shutdown).
    pub fn drop_all(&self) {
        let all: Vec<_> = self.pending.lock().unwrap_or_else(|e| e.into_inner()).drain().collect();
        for (id, tx) in all {
            let _ = self.events.send(AgentEvent::QuestionResolved { id, answer: None });
            let _ = tx.send(None);
        }
    }

    pub fn waiting(&self) -> usize {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// The options of a call: strings or `{label, description}`; an error names what is wrong.
fn parse_options(v: Option<&Value>) -> Result<Vec<QuestionOption>, String> {
    let Some(v) = v else { return Ok(Vec::new()) };
    let arr = v.as_array().ok_or("`options` must be a list of {label, description}")?;
    if arr.len() > MAX_OPTIONS {
        return Err(format!(
            "at most {MAX_OPTIONS} options (got {}); the person can always write their own",
            arr.len()
        ));
    }
    let mut out: Vec<QuestionOption> = Vec::with_capacity(arr.len());
    for (i, o) in arr.iter().enumerate() {
        let (label, description) = match o {
            Value::String(s) => (s.trim().to_string(), String::new()),
            Value::Object(m) => (
                m.get("label").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string(),
                m.get("description").and_then(Value::as_str).unwrap_or_default().trim().to_string(),
            ),
            _ => return Err(format!("option {} is neither text nor {{label, description}}", i + 1)),
        };
        if label.is_empty() {
            return Err(format!("option {} has no `label`", i + 1));
        }
        if out.iter().any(|x| x.label == label) {
            return Err(format!("option `{label}` is given twice"));
        }
        out.push(QuestionOption { label, description });
    }
    Ok(out)
}

/// `ask_user`: the model asks the person to choose.
pub struct AskUser {
    asker: Arc<ChannelAsker>,
}

impl AskUser {
    pub fn new(asker: Arc<ChannelAsker>) -> AskUser {
        AskUser { asker }
    }
}

#[async_trait]
impl Tool for AskUser {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ask_user".into(),
            description: "Ask the person a question with a few options to pick from (they can also write their own \
                answer). Use it when a choice is theirs to make: an approach, a name, which of several things to do. \
                Do not use it for what you can find out yourself."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "question": {"type": "string", "description": "The question, one sentence."},
                    "header": {"type": "string", "description": "A word or two for the title, like `Approach`."},
                    "options": {
                        "type": "array",
                        "description": format!("2 to {MAX_OPTIONS} choices; the first is the one you recommend."),
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": {"type": "string", "description": "A few words."},
                                "description": {"type": "string", "description": "What picking it means."}
                            },
                            "required": ["label"]
                        }
                    },
                    "multi": {"type": "boolean", "description": "Several options may be picked."}
                },
                "required": ["question", "options"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn summary(&self, args: &Value) -> String {
        let q = args.get("question").and_then(Value::as_str).unwrap_or("a question");
        format!("ask: {q}")
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let question = args.get("question").and_then(Value::as_str).map(str::trim).unwrap_or_default();
        if question.is_empty() {
            return ToolOutput::error("`question` is required");
        }
        let options = match parse_options(args.get("options")) {
            Ok(o) => o,
            Err(e) => return ToolOutput::error(e),
        };
        let header = args.get("header").and_then(Value::as_str).map(str::trim).filter(|h| !h.is_empty());
        let q = Question {
            id: ctx.call_id.clone(),
            header: header.unwrap_or("Question").to_string(),
            question: question.to_string(),
            options,
            multi: args.get("multi").and_then(Value::as_bool).unwrap_or(false),
            plan: None,
        };
        match self.asker.ask(q).await {
            Some(a) => ToolOutput::ok(format!("The person answered: {a}")),
            None => ToolOutput::ok(
                "The person closed the question without answering. Do not ask it again; go on with your best \
                 judgement or end your turn.",
            ),
        }
    }
}

/// What plan mode knows: the mode now, the one before plan mode, and the
/// switch the approval asks for (the loop applies it after the call).
#[derive(Debug, Clone, Copy, Default)]
pub struct PlanState {
    pub mode: PermissionMode,
    pub before: PermissionMode,
    pub switch: Option<PermissionMode>,
}

pub type PlanCell = Arc<Mutex<PlanState>>;

impl PlanState {
    /// Records a change of mode (keeps the one to go back to after the plan).
    pub fn set(&mut self, mode: PermissionMode) {
        if mode == PermissionMode::Plan && self.mode != PermissionMode::Plan {
            self.before = self.mode;
        }
        self.mode = mode;
    }
}

/// `exit_plan`: the model shows its plan; the person approves it (plan mode
/// ends) or sends it back.
pub struct ExitPlan {
    /// None without an interface to ask (headless): the plan is approved.
    asker: Option<Arc<ChannelAsker>>,
    plan: PlanCell,
}

impl ExitPlan {
    pub fn new(asker: Option<Arc<ChannelAsker>>, plan: PlanCell) -> ExitPlan {
        ExitPlan { asker, plan }
    }
}

/// The mode the plan goes ahead in when approved without asking (a run nobody watches):
/// there nobody can answer an ask, so the plan goes ahead with its edits allowed, or in the
/// mode the run had before if that allowed more.
fn after_plan(before: PermissionMode) -> PermissionMode {
    match before {
        PermissionMode::Auto => PermissionMode::Auto,
        _ => PermissionMode::AcceptEdits,
    }
}

#[async_trait]
impl Tool for ExitPlan {
    fn name(&self) -> &str {
        "exit_plan"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "exit_plan".into(),
            description: "In plan mode, when the plan is ready: show it to the person for approval. On approval plan \
                mode ends and you carry the plan out; otherwise you get their feedback. Only in plan mode."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "plan": {
                        "type": "string",
                        "description": "The plan in markdown: the steps, the files each touches, how it will be checked."
                    }
                },
                "required": ["plan"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn summary(&self, _args: &Value) -> String {
        "plan ready for approval".into()
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let plan = args.get("plan").and_then(Value::as_str).map(str::trim).unwrap_or_default();
        if plan.is_empty() {
            return ToolOutput::error("`plan` is required: the plan in markdown");
        }
        let before = {
            let st = self.plan.lock().unwrap_or_else(|e| e.into_inner());
            if st.mode != PermissionMode::Plan {
                return ToolOutput::error("not in plan mode: carry on with the work");
            }
            st.before
        };
        let switch = |mode: PermissionMode| {
            self.plan.lock().unwrap_or_else(|e| e.into_inner()).switch = Some(mode);
        };
        let Some(asker) = &self.asker else {
            let mode = after_plan(before);
            switch(mode);
            return ToolOutput::ok(format!(
                "The plan is approved ({} mode). Carry it out now, step by step.",
                mode.as_str()
            ));
        };
        let q = Question {
            id: ctx.call_id.clone(),
            header: "Plan".into(),
            question: "Go ahead with this plan?".into(),
            options: vec![
                QuestionOption {
                    label: APPROVE_AUTO.into(),
                    description: "plan mode ends; file edits go through".into(),
                },
                QuestionOption {
                    label: APPROVE_ASK.into(),
                    description: "plan mode ends; you are asked before each edit".into(),
                },
                QuestionOption { label: KEEP_PLANNING.into(), description: "stay in plan mode".into() },
            ],
            multi: false,
            plan: Some(plan.to_string()),
        };
        match asker.ask(q).await.as_deref() {
            Some(APPROVE_AUTO) => {
                switch(PermissionMode::AcceptEdits);
                ToolOutput::ok(
                    "The person approved the plan; edits go through without asking (accept-edits mode). Carry it out \
                     now, step by step, keeping the todo list.",
                )
            }
            Some(APPROVE_ASK) => {
                switch(PermissionMode::Ask);
                ToolOutput::ok(
                    "The person approved the plan; they approve each edit (ask mode). Carry it out now, step by \
                     step, keeping the todo list.",
                )
            }
            Some(KEEP_PLANNING) => ToolOutput::ok(
                "The person wants to keep planning. Ask what to change (ask_user) or refine the plan, then call \
                 exit_plan again.",
            ),
            Some(feedback) => ToolOutput::ok(format!(
                "The person did not approve the plan yet. Their feedback: {feedback}\nRevise the plan and call \
                 exit_plan again."
            )),
            None => {
                ToolOutput::ok("The person closed the plan without answering. Stay in plan mode and end your turn.")
            }
        }
    }
}

/// What the model is told while plan mode is on.
pub const PLAN_DIRECTIVE: &str = "Plan mode is on. Explore and think; change nothing: only reads, searches and page \
fetches run (writes and commands are refused). Find out what the work needs, ask the person about choices that are \
theirs (ask_user), then call exit_plan with the plan in markdown: the steps, the files each one touches, how the \
result will be checked. Do not start the work before the plan is approved.";

/// Plan mode in a run nobody watches: nobody can answer questions, so the model decides.
pub const PLAN_DIRECTIVE_HEADLESS: &str = "Plan mode is on. Explore and think; change nothing: only reads, searches \
and page fetches run (writes and commands are refused). Nobody can answer questions in this run: where a choice is \
open, take the most reasonable one and say so in the plan. Then call exit_plan with the plan in markdown: the steps, \
the files each one touches, how the result will be checked. Do not start the work before the plan is approved.";

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn ctx(id: &str) -> ToolContext {
        let mut c = ToolContext::new(std::env::temp_dir());
        c.call_id = id.into();
        c
    }

    #[test]
    fn options_are_text_or_label_and_description() {
        let o = parse_options(Some(&json!(["a", {"label": "b", "description": "the b"}]))).unwrap();
        assert_eq!(o[0], QuestionOption { label: "a".into(), description: String::new() });
        assert_eq!(o[1].description, "the b");
        assert!(parse_options(Some(&json!([{"description": "x"}]))).unwrap_err().contains("label"));
        assert!(parse_options(Some(&json!(["a", "a"]))).unwrap_err().contains("twice"));
        assert!(parse_options(Some(&json!(["1", "2", "3", "4", "5", "6", "7"]))).unwrap_err().contains("at most"));
        assert!(parse_options(Some(&json!("a"))).is_err());
    }

    #[test]
    fn plan_mode_remembers_the_mode_to_go_back_to() {
        let mut p = PlanState { mode: PermissionMode::Auto, ..Default::default() };
        p.set(PermissionMode::Plan);
        assert_eq!((p.mode, p.before), (PermissionMode::Plan, PermissionMode::Auto));
        p.set(PermissionMode::Plan);
        assert_eq!(p.before, PermissionMode::Auto, "entering twice keeps the first");
        assert_eq!(after_plan(PermissionMode::ReadOnly), PermissionMode::AcceptEdits);
        assert_eq!(after_plan(PermissionMode::Ask), PermissionMode::AcceptEdits);
        assert_eq!(after_plan(PermissionMode::Auto), PermissionMode::Auto);
    }

    #[tokio::test]
    async fn the_answer_comes_back_and_the_question_is_closed() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let asker = ChannelAsker::new(tx);
        let tool = AskUser::new(asker.clone());
        let a = asker.clone();
        let answer = tokio::spawn(async move {
            let Some(AgentEvent::Question { question }) = rx.recv().await else { panic!("no question") };
            assert_eq!(question.options.len(), 2);
            assert!(a.resolve(&question.id, Some("tabs".into())));
            rx.recv().await
        });
        let args = json!({"question": "Tabs or spaces?", "options": ["tabs", "spaces"]});
        let out = tool.run(args, &ctx("q1")).await;
        assert_eq!(out.content, "The person answered: tabs");
        let closed = answer.await.unwrap();
        assert!(matches!(closed, Some(AgentEvent::QuestionResolved { id, answer: Some(_) }) if id == "q1"));
        assert_eq!(asker.waiting(), 0);
    }

    #[tokio::test]
    async fn exit_plan_switches_mode_or_brings_back_the_feedback() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let asker = ChannelAsker::new(tx);
        let cell: PlanCell = Arc::default();
        let tool = ExitPlan::new(Some(asker.clone()), cell.clone());
        let args = json!({"plan": "1. do it"});
        assert!(tool.run(args.clone(), &ctx("p0")).await.is_error, "only in plan mode");
        cell.lock().unwrap().set(PermissionMode::Plan);
        for (answer, switch, says) in [
            (APPROVE_AUTO, Some(PermissionMode::AcceptEdits), "approved"),
            (APPROVE_ASK, Some(PermissionMode::Ask), "approved"),
            (KEEP_PLANNING, None, "keep planning"),
            ("smaller steps", None, "feedback: smaller steps"),
        ] {
            cell.lock().unwrap().switch = None;
            let a = asker.clone();
            let reply = tokio::spawn({
                let answer = answer.to_string();
                async move {
                    let ev = rx.recv().await;
                    let Some(AgentEvent::Question { question }) = ev else { panic!("no question") };
                    assert_eq!(question.plan.as_deref(), Some("1. do it"));
                    a.resolve(&question.id, Some(answer));
                    let _ = rx.recv().await;
                    rx
                }
            });
            let out = tool.run(args.clone(), &ctx("p1")).await;
            rx = reply.await.unwrap();
            assert!(out.content.contains(says), "{answer}: {}", out.content);
            assert_eq!(cell.lock().unwrap().switch, switch, "{answer}");
        }
        // without an interface the plan goes ahead with its edits allowed
        let headless = ExitPlan::new(None, cell.clone());
        cell.lock().unwrap().switch = None;
        let out = headless.run(args, &ctx("p2")).await;
        assert!(out.content.contains("approved (accept-edits mode)"), "{}", out.content);
        assert_eq!(cell.lock().unwrap().switch, Some(PermissionMode::AcceptEdits));
    }
}
