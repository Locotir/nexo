//! Texts nexo writes for the model that hold only in the situation they were written for. Each test builds the
//! situation their first wording did not picture and checks what the request says in it: a model that reads the
//! text literally must not be told something false there.

use super::*;

/// A read of a long file, so that the curator has something to move out.
struct Long;
#[async_trait]
impl Tool for Long {
    fn name(&self) -> &str {
        "read"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "read".into(), description: "fake".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn summary(&self, args: &Value) -> String {
        format!("read {}", args.get("path").and_then(Value::as_str).unwrap_or(""))
    }
    async fn run(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
        let name = args.get("path").and_then(Value::as_str).unwrap_or("");
        ToolOutput::ok((0..300).map(|k| format!("{name} line {k} holds its own value {}\n", k * 7)).collect::<String>())
    }
}

/// The curator keeps the person's lines that read as standing instructions; "quiero que …" is one, and it is also
/// how a task is asked in Spanish. Once that request was answered and moved out, a later request carried it under
/// "Follow them in every step, as if they were part of the current request": a model taking that literally does the
/// old task again in every later request. The block says each line has its turn and a task there is not the
/// current request.
#[tokio::test]
async fn a_task_asked_earlier_is_not_called_part_of_the_current_request() {
    let d = test_tmpdir().unwrap();
    let task = "Quiero que hagas un script que renombre las fotos por fecha.";
    let mut replies = vec![Reply::Events(text("Hola."))];
    for k in 0..4 {
        replies.push(Reply::Events(call(&format!("a{k}"), "read", json!({"path": format!("fotos{k}")}))));
    }
    replies.push(Reply::Events(text("Hecho: rename.sh renombra las fotos por fecha.")));
    for k in 0..8 {
        replies.push(Reply::Events(call(&format!("b{k}"), "read", json!({"path": format!("conf{k}")}))));
    }
    replies.push(Reply::Events(text("La configuración tiene ocho ficheros.")));
    let client = Scripted::new(replies);
    let mut cfg = config(PermissionMode::Auto);
    cfg.context.policy = "curator".into();
    cfg.context.curator_spec = "eoq=0;floor=0.6;target=2500;cap=5000;hard=60000;still=0".into();
    let mut r = start_with(client.clone(), cfg, None, d.path(), ToolSet::new().with(Arc::new(Long))).await;
    r.next().await;
    for said in ["hola", task, "¿Qué hay en la configuración?"] {
        r.send(AgentCommand::Submit { text: said.into(), attachments: vec![] });
        r.turn().await;
    }
    r.stop().await;
    let reqs = client.requests();
    let sent = reqs.last().unwrap().messages.iter().map(Message::text).collect::<Vec<_>>().join("\n");
    let at = sent.find(&format!(": {task}")).unwrap_or_else(|| panic!("the task is kept as a rule:\n{sent}"));
    let head = sent[..at].rfind("Standing instructions").map(|h| &sent[h..at]).expect(&sent);
    assert!(!head.contains("as if they were part of the current request"), "{head}");
    assert!(head.contains("A task among them was asked in its turn, not in the current request"), "{head}");
}

/// A guard's stop turns the tools off until the model answers. The note said they stay off "for the rest of this
/// request" until "the next message of the person"; a goal goes on after that answer with `[goal: continue]`, which
/// comes from the loop, not the person, and with the tools on. Read literally, the note kept the round from calling
/// any tool. It says the tools are on again after the answer.
#[tokio::test]
async fn a_goal_round_after_a_guard_answer_is_not_told_its_tools_are_off() {
    let d = test_tmpdir().unwrap();
    let mut replies: Vec<Reply> =
        (0..4).map(|i| Reply::Events(call(&format!("c{i}"), "read", json!({"path": "a"})))).collect();
    replies.push(Reply::Events(text("a is x")));
    replies.push(Reply::Events(text("Done.\nGOAL DONE")));
    let client = Scripted::new(replies);
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.max_turns = 8;
    cfg.agent.loop_guard = 0;
    cfg.agent.stale_guard = 3;
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::SetGoal { text: Some("find out what a holds".into()) });
    r.turn().await;
    r.turn().await;
    r.stop().await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 6, "a round of reads stopped by the guard, its answer, then a goal round");
    let round = &reqs[5];
    let last = round.messages.last().unwrap().text();
    assert!(last.contains(nexo_core::agent::GOAL_CONTINUE_MARK), "the goal goes on: {last}");
    assert!(!round.tools.is_empty() && !round.no_tool_call, "the round has its tools");
    let sent = serde_json::to_string(&round.messages).unwrap();
    assert!(sent.contains("Tool calls are off until you answer"), "the earlier stop is in the round's request");
    assert!(!sent.contains("The next message of the person turns them on again"), "{sent}");
    assert!(sent.contains("They are on again after your answer"), "{sent}");
}

/// The system text keeps the date the session began, so the cached prefix keeps its bytes; resumed on a later
/// day, "Date:" there is not today, and a model that writes "today" from it writes the wrong day. The first
/// request of that day says which day it is, once.
#[tokio::test]
async fn a_session_resumed_on_a_later_day_is_told_today() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("hi"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "hello".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let today = time::OffsetDateTime::now_utc().date().to_string();
    assert!(!client.requests()[0].messages[0].text().contains("Today is"), "the day the session began says nothing");
    // the session began on an earlier day
    let file = data_dir().join("sessions").join(format!("{id}.jsonl"));
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains(&format!("\"date\":\"{today}\"")), "{saved}");
    std::fs::write(&file, saved.replace(&format!("\"date\":\"{today}\""), "\"date\":\"2020-01-02\"")).unwrap();
    let client = Scripted::new(vec![Reply::Events(text("a")), Reply::Events(text("b"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), Some(id), d.path()).await;
    r.next().await;
    for said in ["what day is it?", "and now?"] {
        r.send(AgentCommand::Submit { text: said.into(), attachments: vec![] });
        r.turn().await;
    }
    r.stop().await;
    let reqs = client.requests();
    assert!(reqs[0].system.contains("Date: 2020-01-02"), "the prefix keeps its bytes");
    let line = format!("Today is {today}; the Date in the system text is the day this session began.");
    let first = reqs[0].messages.last().unwrap().text();
    assert!(first.contains(&line) && first.ends_with("what day is it?"), "{first}");
    let second = reqs[1].messages.last().unwrap().text();
    assert!(!second.contains("Today is"), "said once: {second}");
}
