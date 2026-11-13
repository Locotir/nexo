use super::*;

#[test]
fn the_read_only_sandbox_sees_home_paths_and_links_leaving_the_project() {
    let d = tempfile::tempdir().unwrap();
    let cwd = d.path().join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let args = |p: &str| serde_json::json!({ "path": p });
    assert!(outside(&args("src/a.rs"), &cwd).is_none());
    assert!(outside(&args("../x"), &cwd).is_some());
    assert!(outside(&args("~/.ssh/id_rsa"), &cwd).is_some());
    assert!(outside(&args("~"), &cwd).is_some());
    // a read of several files escapes if any one of them does
    let many = |l: serde_json::Value| serde_json::json!({ "paths": l });
    assert!(outside(&many(serde_json::json!(["src/a.rs", "src/b.rs"])), &cwd).is_none());
    assert_eq!(outside(&many(serde_json::json!(["src/a.rs", "../x"])), &cwd).as_deref(), Some("../x"));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(d.path(), cwd.join("link")).unwrap();
        assert!(outside(&args("link/secret"), &cwd).is_some());
    }
}

#[test]
fn a_custom_agent_reads_its_front_matter() {
    let p = parse(
        "---\nname: reviewer\ndescription: reviews a diff\ntools: [read, grep, \"bash\"]\nmodel: small-model\nrounds: 8\n---\nLook for bugs.\n",
        "file",
    )
    .unwrap();
    assert_eq!(p.name, "reviewer");
    assert_eq!(p.description, "reviews a diff");
    assert_eq!(p.tools, Some(vec!["read".into(), "grep".into(), "bash".into()]));
    assert_eq!(p.model.as_deref(), Some("small-model"));
    assert_eq!(p.rounds, 8);
    assert_eq!(p.prompt, "Look for bugs.");
    let bare = parse("Just instructions.", "helper").unwrap();
    assert_eq!((bare.name.as_str(), bare.prompt.as_str(), bare.tools.clone()), ("helper", "Just instructions.", None));
    assert!(parse("---\nname: bad name\n---\nx", "f").is_none());
}

#[test]
fn custom_agents_load_from_the_project_and_replace_by_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".nexo/agents")).unwrap();
    std::fs::write(dir.path().join(".nexo/agents/docs.md"), "---\ndescription: writes docs\n---\nWrite docs.").unwrap();
    std::fs::write(dir.path().join(".nexo/agents/explore.md"), "---\nname: explore\ndescription: mine\n---\nX")
        .unwrap();
    let home = tempfile::tempdir().unwrap();
    let trust = home.path().join("trusted-projects");
    // not trusted yet: only the built-in kinds
    let untrusted = load_from(dir.path(), home.path(), &trust, None);
    assert!(!untrusted.iter().any(|p| p.name == "docs"));
    assert!(untrusted.iter().any(|p| p.name == "explore" && p.source.is_none()));
    nexo_core::config::trust_project(dir.path(), &trust).unwrap();
    let all = load_from(dir.path(), home.path(), &trust, None);
    assert!(all.iter().any(|p| p.name == "docs" && p.description == "writes docs"));
    let ex: Vec<&Profile> = all.iter().filter(|p| p.name == "explore").collect();
    assert_eq!(ex.len(), 1);
    assert_eq!(ex[0].description, "mine");
    assert!(all.iter().any(|p| p.name == "general"));
}

#[test]
fn claude_agents_map_onto_nexo_and_nexo_files_win() {
    let proj = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(proj.path().join(".git")).unwrap();
    let (cfg, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = proj.path().join(".claude/agents");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::write(
        a.join("rev.md"),
        "---\nname: rev\ndescription: reviews\ntools: Read, Grep, Bash, Task\nmodel: sonnet\n---\nReview.",
    )
    .unwrap();
    std::fs::write(a.join("odd.md"), "---\nname: odd\ntools: Foo\nmodel: inherit\n---\nX").unwrap();
    std::fs::write(a.join("docs.md"), "---\nname: docs\n---\nclaude docs").unwrap();
    std::fs::create_dir_all(cfg.path().join("agents")).unwrap();
    std::fs::write(cfg.path().join("agents/docs.md"), "---\nname: docs\n---\nnexo docs").unwrap();
    let trust = cfg.path().join("trusted-projects");
    nexo_core::config::trust_project(proj.path(), &trust).unwrap();
    let all = load_from(proj.path(), cfg.path(), &trust, Some(home.path()));
    let get = |n: &str| all.iter().find(|p| p.name == n).unwrap();
    assert_eq!(get("rev").tools, Some(vec!["read".into(), "grep".into(), "bash".into()]));
    assert!(get("rev").soft_model && get("rev").model.as_deref() == Some("sonnet"));
    assert_eq!(get("odd").tools, Some(vec!["read".into(), "grep".into(), "glob".into(), "ls".into()]));
    assert_eq!(get("odd").model, None);
    assert_eq!(get("docs").prompt, "nexo docs");
}

#[test]
fn a_tool_list_that_names_only_task_and_workflow_gives_no_tool_not_all() {
    for list in ["task", "workflow", "[task, workflow]", "\"task\", 'workflow'"] {
        let p = parse(&format!("---\nname: x\ntools: {list}\n---\nB"), "x").unwrap();
        assert_eq!(p.tools, Some(vec![]), "{list}");
    }
    // the two names are dropped from a longer list, and an empty list still means every tool
    let p = parse("---\nname: x\ntools: [read, task, workflow, grep]\n---\nB", "x").unwrap();
    assert_eq!(p.tools, Some(vec!["read".to_string(), "grep".to_string()]));
    assert_eq!(parse("---\nname: x\ntools:\n---\nB", "x").unwrap().tools, None);
    assert_eq!(parse("---\nname: x\ntools: []\n---\nB", "x").unwrap().tools, None);
}

#[test]
fn a_foreign_agent_whose_tools_name_only_task_or_workflow_ends_with_the_four_read_tools() {
    // read-only on purpose: the two names are dropped, and what is left is not every tool
    for list in ["task", "workflow"] {
        let mut p = parse(&format!("---\nname: x\ntools: {list}\n---\nB"), "x").unwrap();
        adopt_foreign(&mut p);
        assert_eq!(p.tools, Some(["read", "grep", "glob", "ls"].map(String::from).to_vec()), "{list}");
    }
}

fn kind(name: &str, description: &str) -> Profile {
    Profile { name: name.into(), description: description.into(), ..builtin()[1].clone() }
}

#[test]
fn a_description_is_cut_to_its_first_clause_and_a_long_one_at_a_comma_or_a_word() {
    assert_eq!(clause("Writes the manual. Use after a change.", 60), "Writes the manual");
    assert_eq!(clause("Performance specialist: measures first", 60), "Performance specialist");
    assert_eq!(clause("Refines UI code - spacing and states", 60), "Refines UI code");
    assert_eq!(clause("Plans migrations (PostgreSQL, SQLite), checks them", 60), "Plans migrations");
    assert_eq!(clause("  writes docs.  ", 60), "writes docs");
    assert_eq!(clause("First line\\nExamples: <example>", 60), "First line");
    assert_eq!(clause("", 60), "");
    // over the cap: the last comma within it, unless that leaves under a third
    assert_eq!(
        clause("Reads an issue report, reproduces it when it can, labels its area", 60),
        "Reads an issue report, reproduces it when it can"
    );
    assert_eq!(
        clause("One, two three four five six seven eight nine ten eleven twelve thirteen", 60),
        "One, two three four five six seven eight nine ten eleven"
    );
    assert_eq!(clause("abcdefghij klmno", 10), "abcdefghij");
    assert_eq!(clause("abcdefghijklmno", 10), "abcdefghij");
    // characters, not bytes
    assert_eq!(clause(&"\u{e9}".repeat(80), 60), "\u{e9}".repeat(60));
}

#[test]
fn the_short_text_names_the_kinds_of_the_person_and_keeps_the_built_in_two_in_few_words() {
    let stock = format!("{SHORT} `explore` (default) is read-only; `general` can edit.");
    assert_eq!(short_text(&builtin()), stock);
    let mut kinds = builtin();
    kinds.extend([kind("docs", "Writes the manual. Use after a change."), kind("bare", "")]);
    assert_eq!(short_text(&kinds), format!("{stock} More kinds for `agent`: `docs`: Writes the manual; `bare`."));
    // their own `explore` in place of the built-in one: said by what their file says of it
    let mut kinds = builtin();
    kinds.remove(0);
    kinds.push(kind("explore", "reads the wiki too"));
    assert_eq!(
        short_text(&kinds),
        format!("{SHORT} `general` can edit. More kinds for `agent`: `explore` (default): reads the wiki too.")
    );
}

#[test]
fn the_kinds_of_the_person_never_take_more_than_their_ceiling() {
    let long = "Reviews every change for the mistakes that reach production ".repeat(4);
    let own = |text: &str| text.split_once(" More kinds for `agent`: ").unwrap().1.to_string();
    // forty: the first ones described, the others by name, none left out
    let mut kinds = builtin();
    kinds.extend((0..40).map(|i| kind(&format!("reviewer-{i:02}"), &long)));
    let list = own(&short_text(&kinds));
    assert!(list.len() <= KINDS_CHARS, "{}: {list}", list.len());
    assert!((0..40).all(|i| list.contains(&format!("`reviewer-{i:02}`"))), "{list}");
    assert!(list.starts_with("`reviewer-00`: Reviews every change for the mistakes that reach production;"), "{list}");
    assert!(list.ends_with("; `reviewer-38`; `reviewer-39`."), "{list}");
    assert!(!list.contains("more"), "{list}");
    // two hundred: the names that fit, and how many there are besides
    let mut kinds = builtin();
    kinds.extend((0..200).map(|i| kind(&format!("reviewer-{i:03}"), &long)));
    let list = own(&short_text(&kinds));
    let (named, tail) = list.split_once("and ").unwrap();
    assert!(named.len() <= KINDS_CHARS, "{}: {named}", named.len());
    let names = named.matches("`reviewer-").count();
    assert!(names > 80 && !named.contains(": "), "{names}: {named}");
    assert_eq!(
        tail,
        format!(
            "{} more: an `agent` that is none of these is answered with every kind and its description.",
            200 - names
        )
    );
}
