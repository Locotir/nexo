use super::*;

/// The `bash` of these tests: not slow, answers with the command, so the line nexo adds is the only difference.
fn shell_tools() -> ToolSet {
    ToolSet::new().with(Arc::new(Fake { name: "bash", kind: ToolKind::Exec, slow: false }))
}

/// Every tool result text of a request, in order.
fn result_texts(req: &ModelRequest) -> Vec<String> {
    req.messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|p| match p {
            Part::ToolResult { content, .. } => Some(text_of_parts(content)),
            _ => None,
        })
        .collect()
}

/// Runs scripted `bash` commands one per request, the next one after the reply to the last; the requests sent.
async fn run_commands(commands: &[&str], cwd: &std::path::Path, resume: Option<String>) -> (Vec<ModelRequest>, String) {
    run_with(commands, cwd, resume, shell_tools()).await
}

async fn run_commands_with(commands: &[&str], cwd: &std::path::Path, tools: ToolSet) -> (Vec<ModelRequest>, String) {
    run_with(commands, cwd, None, tools).await
}

async fn run_with(
    commands: &[&str],
    cwd: &std::path::Path,
    resume: Option<String>,
    tools: ToolSet,
) -> (Vec<ModelRequest>, String) {
    let mut script = Vec::new();
    for (i, c) in commands.iter().enumerate() {
        script.push(Reply::Events(call(&format!("b{i}"), "bash", json!({"command": c}))));
    }
    script.push(Reply::Events(text("done")));
    let client = Scripted::new(script);
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), resume, cwd, tools).await;
    let sid = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        e => panic!("{e:?}"),
    };
    r.send(AgentCommand::Submit { text: "look at it".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    (client.requests(), sid)
}

#[tokio::test]
async fn the_first_time_a_compiled_program_is_read_by_hand_the_result_says_nexo_bin_exists_once() {
    let d = test_tmpdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let (first, second) = (format!("objdump -d '{}' | head", exe.display()), format!("strings '{}'", exe.display()));
    let (reqs, sid) = run_commands(&["ls", &first, &second, "ls"], d.path(), None).await;
    // no `bin` tool in this session: the line names the same call through the shell
    let line = format!(
        "[nexo: nexo bin reads this file in pages, with names and references: nexo bin overview {}, then fn|funcs|xref|decomp|strings]",
        exe.display()
    );
    let line = line.as_str();
    let last = result_texts(reqs.last().unwrap());
    assert_eq!(last.len(), 4, "{last:?}");
    assert_eq!(last[0], "bash:ls");
    assert_eq!(last[1], format!("bash:{first}\n\n{line}"), "the first command that reads a binary ends with it");
    assert_eq!(last[2], format!("bash:{second}"), "the second does not");
    assert_eq!(last[3], "bash:ls");
    // the line is sent from the round after it was added, and from then on every request carries it once
    assert_eq!(
        reqs.iter().map(|r| result_texts(r).iter().filter(|t| t.contains(line)).count()).collect::<Vec<_>>(),
        [0, 0, 1, 1, 1]
    );
    // a session resumed from disk has it already: the next command on a binary does not repeat it
    let again = format!("xxd '{}'", exe.display());
    let (reqs, _) = run_commands(&[&again], d.path(), Some(sid)).await;
    let texts = result_texts(reqs.last().unwrap());
    assert_eq!(texts.iter().filter(|t| t.contains(line)).count(), 1, "{texts:?}");
    assert_eq!(texts.last().unwrap(), &format!("bash:{again}"));
}

#[tokio::test]
async fn the_line_is_not_added_for_text_a_missing_file_or_nexo_bin_itself() {
    let d = test_tmpdir().unwrap();
    std::fs::write(d.path().join("notes.txt"), "plain words\n").unwrap();
    let exe = std::env::current_exe().unwrap();
    let nb = format!("nexo bin fn '{}' main", exe.display());
    let cmds = ["strings notes.txt", "objdump -d missing.out", nb.as_str(), "cat notes.txt"];
    let (reqs, _) = run_commands(&cmds, d.path(), None).await;
    let last = result_texts(reqs.last().unwrap());
    assert_eq!(last, cmds.map(|c| format!("bash:{c}")));
}

#[tokio::test]
async fn a_session_that_never_touches_a_binary_sends_what_it_sent_before() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b0", "bash", json!({"command": "ls -la"}))),
        Reply::Events(text("done")),
    ]);
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), shell_tools()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "list".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    for req in client.requests() {
        let all = serde_json::to_string(&req).unwrap();
        assert!(!all.contains("nexo bin") && !all.contains("compiled"), "no hint, no system line");
        // the tools are the ones nexo always sends: no tool for binaries rides in every request
        let names = req.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names, ["ask_user", "bash"]);
    }
    assert_eq!(result_texts(client.requests().last().unwrap()), ["bash:ls -la"]);
}

/// The tools of a session that offers the kit: the shell, `bin` and `debug` (the fakes only answer with the call).
fn kit_tools() -> ToolSet {
    shell_tools().with(Arc::new(Fake { name: "bin", kind: ToolKind::Read, slow: false })).with(Arc::new(Fake {
        name: "debug",
        kind: ToolKind::Exec,
        slow: false,
    }))
}

/// Every user message text of a request, in order.
fn user_texts(req: &ModelRequest) -> Vec<String> {
    req.messages
        .iter()
        .filter(|m| m.role == nexo_core::message::Role::User)
        .map(|m| m.content.iter().filter_map(Part::as_text).collect::<Vec<_>>().join("\n"))
        .collect()
}

#[tokio::test]
async fn a_message_that_names_a_compiled_program_says_where_the_kit_is_once_and_a_plain_one_says_nothing() {
    let d = test_tmpdir().unwrap();
    std::fs::copy(std::env::current_exe().unwrap(), d.path().join("c-lcrypt")).unwrap();
    let client =
        Scripted::new(vec![Reply::Events(text("one")), Reply::Events(text("two")), Reply::Events(text("three"))]);
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), kit_tools()).await;
    r.next().await;
    for words in ["fix the readme", "try nexo against c-lcrypt, please", "now c-lcrypt again"] {
        r.send(AgentCommand::Submit { text: words.into(), attachments: vec![] });
        r.turn().await;
    }
    r.stop().await;
    let reqs = client.requests();
    let head = "compiled programs, with nexo's own tools:";
    let said = |req: &ModelRequest| user_texts(req).iter().filter(|t| t.contains(head)).count();
    // the first message is about a readme, the second names the file, the third finds the line already there
    assert_eq!(reqs.iter().map(said).collect::<Vec<_>>(), [0, 1, 1]);
    let second = user_texts(&reqs[1]);
    let note = second.iter().find(|t| t.contains(head)).unwrap();
    assert!(note.contains("bin {\"command\": \"overview c-lcrypt\"}"), "{note}");
    assert!(note.contains("debug {\"command\": \"run c-lcrypt; cont\"}"), "{note}");
    assert!(note.contains("try nexo against c-lcrypt, please"), "the words stay: {note}");
    // nothing about it rides in the system prompt or in the tool list of a request
    for req in &reqs {
        let system = &req.system;
        assert!(!system.contains(head) && !system.contains("objdump"), "no system text for it");
    }
}

#[tokio::test]
async fn the_gdb_paste_gets_the_debug_call_and_objdump_the_bin_call_once_each() {
    let d = test_tmpdir().unwrap();
    std::fs::copy(std::env::current_exe().unwrap(), d.path().join("c-lcrypt")).unwrap();
    let gdb = "cat > g1.gdb <<'EOF'\nset pagination off\nbreak *(main+0x170)\ncommands\nbt 3\ncontinue\nend\nrun -e t.bin -p 0 -P test123\nEOF\ngdb -q -batch -x g1.gdb c-lcrypt 2>&1 | tail -40";
    let (reqs, _) =
        run_commands_with(&["objdump -d c-lcrypt | head -50", gdb, "objdump -t c-lcrypt"], d.path(), kit_tools()).await;
    let last = result_texts(reqs.last().unwrap());
    assert_eq!(last.len(), 3, "{last:?}");
    assert!(last[0].contains("bin {\"command\": \"overview c-lcrypt\"}"), "{}", last[0]);
    assert!(last[1].contains("debug {\"command\": \"run c-lcrypt -e t.bin -p 0 -P test123"), "{}", last[1]);
    assert!(last[1].contains("break *(main+0x170)") || last[1].contains("break *main+0x170"), "{}", last[1]);
    assert_eq!(last[2], "bash:objdump -t c-lcrypt", "the bin line was said once");
}
