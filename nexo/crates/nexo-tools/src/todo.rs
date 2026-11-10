//! `todo`: the task list the model keeps for work with several steps. Each
//! call replaces the whole list; a call without `items` shows it. The list
//! lives in the tool for the session and in `<sessions>/<id>.todo.json`, so
//! a resumed session gets it back.

use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use nexo_core::{paths, Compact, TodoItem, TodoStatus, Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

/// Items at most.
pub const MAX_ITEMS: usize = 50;
/// Characters of an item's text at most.
pub const MAX_TEXT: usize = 200;

pub struct Todo {
    items: Mutex<Vec<TodoItem>>,
    /// Where the per-session file goes (None: the sessions directory).
    dir: Option<PathBuf>,
}

impl Default for Todo {
    fn default() -> Todo {
        Todo::new()
    }
}

impl Todo {
    pub fn new() -> Todo {
        Todo { items: Mutex::new(Vec::new()), dir: None }
    }

    /// Keeps the per-session file under `dir` instead of the sessions directory.
    pub fn with_dir(dir: PathBuf) -> Todo {
        Todo { items: Mutex::new(Vec::new()), dir: Some(dir) }
    }

    /// The list as it stands.
    pub fn items(&self) -> Vec<TodoItem> {
        self.items.lock().map(|g| g.clone()).unwrap_or_default()
    }

    fn file(&self, session_id: &str) -> Option<PathBuf> {
        if session_id.is_empty() {
            return None;
        }
        let dir = self.dir.clone().unwrap_or_else(paths::sessions_dir);
        Some(dir.join(format!("{session_id}.todo.json")))
    }

    fn load(&self, session_id: &str) -> Option<Vec<TodoItem>> {
        let text = std::fs::read_to_string(self.file(session_id)?).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn save(&self, session_id: &str, items: &[TodoItem]) {
        if let Some(f) = self.file(session_id) {
            if let Some(d) = f.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            if let Ok(text) = serde_json::to_string(items) {
                let _ = std::fs::write(f, text);
            }
        }
    }
}

/// Reads the `items` argument; an error names what is wrong.
fn parse_items(v: &Value) -> Result<Vec<TodoItem>, String> {
    let arr = v.as_array().ok_or("`items` must be a list of {text, status}")?;
    if arr.len() > MAX_ITEMS {
        return Err(format!("at most {MAX_ITEMS} items (got {}); fold the rest into fewer steps", arr.len()));
    }
    let mut out = Vec::with_capacity(arr.len());
    for (i, it) in arr.iter().enumerate() {
        let text = match it {
            Value::String(s) => s.as_str(),
            Value::Object(o) => o.get("text").and_then(Value::as_str).ok_or(format!("item {} has no `text`", i + 1))?,
            _ => return Err(format!("item {} must be an object with `text` and `status`", i + 1)),
        };
        let text = text.trim();
        if text.is_empty() {
            return Err(format!("item {} has an empty text", i + 1));
        }
        if text.chars().count() > MAX_TEXT {
            return Err(format!("item {} is longer than {MAX_TEXT} characters; keep each step short", i + 1));
        }
        let status =
            match it.get("status").and_then(Value::as_str).unwrap_or("pending").trim().to_ascii_lowercase().as_str() {
                "pending" | "todo" | "" => TodoStatus::Pending,
                "in_progress" | "in-progress" | "doing" | "active" => TodoStatus::InProgress,
                "done" | "completed" | "complete" => TodoStatus::Done,
                other => return Err(format!("item {}: unknown status `{other}` (pending, in_progress, done)", i + 1)),
            };
        out.push(TodoItem { text: text.to_string(), status });
    }
    Ok(out)
}

/// The list as plain text, one line per item, and a count.
pub fn render(items: &[TodoItem]) -> String {
    if items.is_empty() {
        return "(no tasks)".into();
    }
    let mut out: Vec<String> = items
        .iter()
        .map(|it| {
            let mark = match it.status {
                TodoStatus::Pending => "[ ]",
                TodoStatus::InProgress => "[>]",
                TodoStatus::Done => "[x]",
            };
            format!("{mark} {}", it.text)
        })
        .collect();
    out.push(format!("{} done", counts(items)));
    out.join("\n")
}

/// `done/total`.
pub fn counts(items: &[TodoItem]) -> String {
    let done = items.iter().filter(|i| i.status == TodoStatus::Done).count();
    format!("{done}/{}", items.len())
}

#[async_trait]
impl Tool for Todo {
    fn name(&self) -> &str {
        "todo"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "todo".into(),
            description: "Keep the task list for long work in several parts (over about eight tool \
                          calls); skip it for short tasks. Send it in the same response as real work, never as a response of its own. \
                          Each call replaces the whole list, so send every item each time; write the plan once, \
                          then change it only when the plan changes or a step is finished alongside other work. \
                          Keep items short. A call without `items` shows the list."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "description": "The whole list, in order.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "text": {"type": "string", "description": "One step, under 200 characters."},
                                "status": {"type": "string", "enum": ["pending", "in_progress", "done"], "description": "Default: pending."}
                            },
                            "required": ["text"]
                        }
                    }
                }
            }),
        }
    }

    fn compact(&self) -> Option<Compact> {
        Some(Compact {
            description:
                "Task list for long work; each call replaces the whole list. Send it with real work, never alone.",
            params: &[("items", "The whole list, in order")],
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn summary(&self, args: &Value) -> String {
        match args.get("items").map(parse_items) {
            Some(Ok(items)) => format!("todo: {} done", counts(&items)),
            Some(Err(_)) => "todo: (invalid list)".into(),
            None => "todo: show".into(),
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let items = match args.get("items") {
            Some(v) => match parse_items(v) {
                Ok(items) => items,
                Err(e) => return ToolOutput::error(format!("todo: {e}")),
            },
            None => {
                let mut cur = self.items();
                if cur.is_empty() {
                    if let Some(saved) = self.load(&ctx.session_id) {
                        cur = saved;
                    }
                }
                cur
            }
        };
        if let Ok(mut g) = self.items.lock() {
            *g = items.clone();
        }
        self.save(&ctx.session_id, &items);
        ToolOutput::ok(render(&items)).with_display(ToolDisplay::Todo { items })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &std::path::Path, session: &str) -> ToolContext {
        let mut c = ToolContext::new(dir.to_path_buf());
        c.session_id = session.into();
        c
    }

    #[tokio::test]
    async fn replaces_the_list_renders_it_and_keeps_it_for_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let t = Todo::with_dir(dir.path().to_path_buf());
        let out = t
            .run(
                json!({"items": [{"text": "read the test"}, {"text": "fix it", "status": "in_progress"}, {"text": "run", "status": "done"}]}),
                &ctx(dir.path(), "s1"),
            )
            .await;
        assert!(!out.is_error);
        assert_eq!(out.content, "[ ] read the test\n[>] fix it\n[x] run\n1/3 done");
        assert!(matches!(&out.display, ToolDisplay::Todo { items } if items.len() == 3));
        assert_eq!(t.summary(&json!({"items": [{"text": "a", "status": "done"}]})), "todo: 1/1 done");
        assert_eq!(t.summary(&json!({})), "todo: show");
        // a fresh tool for the same session (a resume) shows the saved list
        let again = Todo::with_dir(dir.path().to_path_buf());
        let shown = again.run(json!({}), &ctx(dir.path(), "s1")).await;
        assert_eq!(shown.content, out.content);
        assert!(dir.path().join("s1.todo.json").is_file());
    }

    #[tokio::test]
    async fn bad_lists_are_refused_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let t = Todo::with_dir(dir.path().to_path_buf());
        let c = ctx(dir.path(), "");
        assert!(t.run(json!({"items": "no"}), &c).await.content.contains("must be a list"));
        assert!(t.run(json!({"items": [{"status": "done"}]}), &c).await.content.contains("no `text`"));
        assert!(t
            .run(json!({"items": [{"text": "x", "status": "later"}]}), &c)
            .await
            .content
            .contains("unknown status"));
        let long = "x".repeat(MAX_TEXT + 1);
        assert!(t.run(json!({"items": [{"text": long}]}), &c).await.content.contains("longer than"));
        let many: Vec<Value> = (0..MAX_ITEMS + 1).map(|i| json!({"text": format!("step {i}")})).collect();
        assert!(t.run(json!({"items": many}), &c).await.content.contains("at most"));
        assert_eq!(t.run(json!({}), &c).await.content, "(no tasks)");
    }
}
