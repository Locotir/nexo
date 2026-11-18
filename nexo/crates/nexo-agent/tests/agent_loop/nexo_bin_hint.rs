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
    // on Windows a call writes the path with `/`, which every program and the shell there take
    let exe = std::env::current_exe().unwrap().display().to_string().replace('\\', "/");
    let (first, second) = (format!("objdump -d '{exe}' | head"), format!("strings '{exe}'"));
    let (reqs, sid) = run_commands(&["ls", &first, &second, "ls"], d.path(), None).await;
    // no `bin` tool in this session: the line names the same call through the shell, after what the file is
    let path = std::env::current_exe().unwrap();
    let facts = nexo_bin::kind_at(&path).unwrap().facts(Some(std::fs::metadata(&path).unwrap().len()));
    let line = format!(
        "[nexo: {exe}: {facts}; nexo bin overview {exe} reads it in pages, with names and references, then \
         fn|funcs|xref|decomp|strings]"
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
    let again = format!("xxd '{exe}'");
    let (reqs, _) = run_commands(&[&again], d.path(), Some(sid)).await;
    let texts = result_texts(reqs.last().unwrap());
    assert_eq!(texts.iter().filter(|t| t.contains(line)).count(), 1, "{texts:?}");
    assert_eq!(texts.last().unwrap(), &format!("bash:{again}"));
}

#[tokio::test]
async fn the_line_is_not_added_for_text_a_missing_file_or_nexo_bin_itself() {
    let d = test_tmpdir().unwrap();
    std::fs::write(d.path().join("notes.txt"), "plain words\n").unwrap();
    let exe = std::env::current_exe().unwrap().display().to_string().replace('\\', "/");
    let nb = format!("nexo bin fn '{exe}' main");
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
        // The ordinary tools include shared memory, but no binary tool rides in every request.
        let names = req.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names, ["ask_user", "bash", "shared_memory_read", "shared_memory_write"]);
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

#[tokio::test]
async fn an_attached_program_is_told_by_what_it_is_and_the_call_that_reads_it() {
    let d = test_tmpdir().unwrap();
    let bytes = nexo_bin::fixtures::elf64(false).bytes;
    std::fs::write(d.path().join("prog"), &bytes).unwrap();
    let client = Scripted::new(vec![Reply::Events(text("one"))]);
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), kit_tools()).await;
    r.next().await;
    r.send(AgentCommand::Submit {
        text: "what does @prog print".into(),
        attachments: vec![nexo_core::agent::Attachment { path: PathBuf::from("prog") }],
    });
    r.turn().await;
    r.stop().await;
    let sent = user_texts(&client.requests()[0]).join("\n");
    let size = nexo_bin::size_words(bytes.len() as u64);
    let told = format!(
        "@prog: not attached: ELF64 x86-64 executable, {size}, with symbols; bin {{\"command\": \"overview prog\"}} reads it"
    );
    assert!(sent.contains(&told), "{sent}");
    // the `@` of the mention names the file for the start note too
    assert!(sent.contains("compiled programs, with nexo's own tools: bin {\"command\": \"overview prog\"}"), "{sent}");
}

/// A compaction keeps the person's words and leaves out what nexo put before them, the line about compiled programs
/// with it: the next message of theirs carries it again, and only that one.
#[tokio::test]
async fn the_line_comes_again_with_the_next_message_after_a_compaction_took_it_out() {
    let d = test_tmpdir().unwrap();
    std::fs::copy(std::env::current_exe().unwrap(), d.path().join("c-lcrypt")).unwrap();
    let client = Scripted::new(vec![
        Reply::Events(text("one")),
        Reply::Events(text("two")),
        // plain policy (the test config): a context error is answered with a compaction, which is this test's point
        Reply::Err(ProviderError::new(ErrorKind::ContextLength, "too long")),
        Reply::Events(text("SUMMARY OF ALL")),
        Reply::Events(text("three")),
        Reply::Events(text("four")),
        Reply::Events(text("five")),
    ]);
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), kit_tools()).await;
    r.next().await;
    for words in ["try nexo against c-lcrypt, please", "two", "three", "four", "five"] {
        r.send(AgentCommand::Submit { text: words.into(), attachments: vec![] });
        r.turn().await;
    }
    r.stop().await;
    let reqs = client.requests();
    let head = "compiled programs, with nexo's own tools:";
    let said = |req: &ModelRequest| user_texts(req).iter().map(|t| t.matches(head).count()).sum::<usize>();
    // one, two, three cut by the window, the summary, three again, four, five
    assert_eq!(reqs.len(), 7);
    let counts: Vec<usize> = [0, 1, 2, 4, 5, 6].iter().map(|&i| said(&reqs[i])).collect();
    assert_eq!(counts, [1, 1, 1, 0, 1, 1], "after the summary, the fourth message carries it and the fifth keeps it");
    let four = user_texts(&reqs[5]);
    let note = four.iter().find(|t| t.contains(head)).unwrap();
    assert!(note.contains("bin {\"command\": \"overview c-lcrypt\"}") && note.ends_with("four"), "{note}");
}

/// A sub-agent sees none of the main agent's start notes: a task about a compiled program starts with the line,
/// naming only calls that run can make. A read-only kind (`explore`, the default) gets a `debug` that only steers a
/// session another agent started, and no shell: its line names the `bin` call alone, and without `bin` there is no
/// line. A kind that runs programs gets the `debug` run too.
#[tokio::test]
async fn a_sub_agent_whose_task_names_a_compiled_program_starts_with_the_line_its_tools_can_take() {
    let d = test_tmpdir().unwrap();
    std::fs::copy(std::env::current_exe().unwrap(), d.path().join("c-lcrypt")).unwrap();
    let head = "compiled programs, with nexo's own tools:";
    // `task` comes with `read` and `grep`: the usual tools, then `bin` and `debug`
    let kit = || {
        tools().with(Arc::new(Fake { name: "bin", kind: ToolKind::Read, slow: false })).with(Arc::new(Fake {
            name: "debug",
            kind: ToolKind::Exec,
            slow: false,
        }))
    };
    // the first message of the sub-agent a task of `kind` (none: the default) starts, in a session with `tools`
    let first = |kind: Option<&'static str>, tools: ToolSet| {
        let d = d.path().to_path_buf();
        async move {
            let mut args = json!({"description": "read it", "prompt": "what does c-lcrypt print?"});
            if let Some(k) = kind {
                args["agent"] = json!(k);
            }
            let client = Scripted::new(vec![
                Reply::Events(call("t1", "task", args)),
                Reply::Events(text("it prints a key")),
                Reply::Events(text("done")),
            ]);
            let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, &d, tools).await;
            r.next().await;
            r.send(AgentCommand::Submit { text: "find out what it prints".into(), attachments: vec![] });
            r.turn().await;
            r.stop().await;
            let reqs = client.requests();
            assert!(!user_texts(&reqs[0]).iter().any(|t| t.contains(head)), "the person's words name no program");
            assert!(reqs[1].system.starts_with("You are a sub-agent"), "{}", reqs[1].system);
            user_texts(&reqs[1]).join("\n")
        }
    };
    let explore = first(Some("explore"), kit()).await;
    assert!(explore.contains(head) && explore.contains("bin {\"command\": \"overview c-lcrypt\"}"), "{explore}");
    assert!(!explore.contains("debug {"), "a read-only kind's debug runs nothing: {explore}");
    // no `bin` and no shell to call `nexo bin` in
    let bare = first(Some("explore"), tools()).await;
    assert!(!bare.contains(head), "{bare}");
    let general = first(Some("general"), kit()).await;
    assert!(general.contains("debug {\"command\": \"run c-lcrypt; cont\"}"), "{general}");
    // a kind with a shell and no `bin` gets the shell form
    let shell = first(Some("general"), tools()).await;
    assert!(shell.contains("nexo bin overview c-lcrypt") && !shell.contains("debug {"), "{shell}");
}

/// With a window of 32k tokens or less neither `bin` nor `debug` is offered: the lines name the shell form that
/// works there, and none names a `debug` call the model cannot make.
#[tokio::test]
async fn in_a_small_window_the_lines_name_the_shell_form_and_no_debug_call() {
    let d = test_tmpdir().unwrap();
    std::fs::copy(std::env::current_exe().unwrap(), d.path().join("c-lcrypt")).unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b0", "bash", json!({"command": "objdump -d c-lcrypt | head"}))),
        Reply::Events(call("b1", "bash", json!({"command": "gdb -batch -ex run ./c-lcrypt"}))),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.providers.get_mut("mock").unwrap().context_window.insert("m1".into(), 32_768);
    cfg.context.policy = "auto".into();
    let mut r = start_with(client.clone(), cfg, None, d.path(), kit_tools()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "look at c-lcrypt".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let reqs = client.requests();
    let names: Vec<&str> = reqs[0].tools.iter().map(|t| t.name.as_str()).collect();
    assert!(!names.contains(&"bin") && !names.contains(&"debug"), "{names:?}");
    let note = user_texts(&reqs[0]).join("\n");
    assert!(note.contains("nexo bin overview c-lcrypt reads one") && !note.contains("debug {"), "{note}");
    let last = result_texts(reqs.last().unwrap());
    assert_eq!(last.len(), 2, "{last:?}");
    assert!(last[0].contains("; nexo bin overview c-lcrypt reads it") && !last[0].contains("bin {"), "{}", last[0]);
    assert_eq!(last[1], "bash:gdb -batch -ex run ./c-lcrypt", "no debug line, and the bin line was said");
}
