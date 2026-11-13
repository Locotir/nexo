use super::*;

#[tokio::test]
async fn a_list_cut_at_fifty_items_and_equal_items_are_said_in_the_result() {
    let d = test_tmpdir().unwrap();
    let many = (0..60).map(|n| format!("\"i{n}\"")).collect::<Vec<_>>().join(", ");
    let client = Mock::new(vec![("FIND", says(&format!("[{many}]")))]);
    let toml = format!(
        "max_agents = 50\nmax_parallel = 8\n{}",
        CHECKED.replace("votes = 2", "votes = 1").replace("{{item}}", "{{item}}!")
    );
    let (out, _) = go(&client, d.path(), &toml, "").await;
    assert!(
        out.content.contains("note: it returned 60 items; the first 50 are used and the rest are left out"),
        "{}",
        out.content
    );
    // 50 items were used (49 could start under max_agents), not 60
    assert!(steps_line(&out, "check").contains("partial, 1 without a report"), "{}", out.content);
    assert_eq!(client.count("CHECK i50!"), 0);
    assert_eq!(client.count("CHECK i48!"), 1);

    let client = Mock::new(vec![("FIND", says("[\"a\", \"a\", \"b\", \"a\"]"))]);
    let toml = CHECKED.replace("votes = 2", "votes = 1").replace("{{item}}", "{{item}}!");
    let (out, _) = go(&client, d.path(), &toml, "").await;
    assert!(out.content.contains("note: 2 equal items merged"), "{}", out.content);
    assert_eq!(client.count("CHECK a!"), 1);
    assert_eq!(client.count("CHECK b!"), 1);
}

#[tokio::test]
async fn what_a_later_prompt_gets_of_a_step_with_each_is_the_joined_reports_under_one_line_headers_and_capped() {
    let d = test_tmpdir().unwrap();
    let big = "y".repeat(7000);
    let client = Mock::new(vec![
        ("FIND", says("[\"alpha\", \"beta\"]")),
        ("CHECK alpha", says("report alpha")),
        ("CHECK beta", says(&big)),
    ]);
    let toml = format!(
        "{}\n[[phase]]\ntitle = \"Sum\"\n[[phase.step]]\nid = \"sum\"\nprompt = \"SUM {{{{steps.check.report}}}}\"\n",
        CHECKED.replace("votes = 2", "votes = 1")
    );
    let (out, _) = go(&client, d.path(), &toml, "").await;
    assert!(!out.is_error, "{}", out.content);
    let sum = client.tasks().into_iter().find(|t| t.starts_with("SUM")).expect("the last step ran");
    let want = format!(
        "SUM --- check #1 (item 1: alpha) ---\nreport alpha\n\n--- check #2 (item 2: beta) ---\n{}\n[cut: 1000 more characters]",
        "y".repeat(6000)
    );
    assert_eq!(sum, want);
}

#[tokio::test]
async fn an_item_longer_than_two_thousand_characters_is_cut_with_a_mark_in_the_prompt() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![("FIND", says(&format!("[\"{}\"]", "z".repeat(3000)))), ("CHECK", says("fine"))]);
    let (out, _) = go(&client, d.path(), &CHECKED.replace("votes = 2", "votes = 1"), "").await;
    assert!(!out.is_error, "{}", out.content);
    let check = client.tasks().into_iter().find(|t| t.starts_with("CHECK")).unwrap();
    assert_eq!(check, format!("CHECK {}\n[cut: 1000 more characters]", "z".repeat(2000)));
}

// ---- the file that was asked about is the file that runs ----

#[tokio::test]
async fn a_saved_file_changed_after_the_question_is_refused_and_nothing_starts() {
    let text = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"OLD\"\n";
    let d = project_with("saved", text);
    let client = Mock::new(vec![]);
    let wf = tool(&client, d.path());
    // the turn stamps the subject of the file as it is when the person is asked
    let mut args = json!({"name": "saved", "pinned": "saved@00000000000000000000000000000000"});
    nexo_agent::workflow::tool::pin(&mut args, d.path());
    assert_eq!(args["pinned"], json!(subject("saved", text)), "what the model put there is replaced");
    // the file is swapped between the question and the run
    std::fs::write(d.path().join(".nexo/workflows/saved.toml"), text.replace("OLD", "NEW")).unwrap();
    let out = wf.run(args.clone(), &ctx(d.path())).await;
    assert!(out.is_error, "{}", out.content);
    assert!(out.content.contains("changed after you were asked about it, so nothing was started"), "{}", out.content);
    assert!(client.tasks().is_empty());
    // asked about again, it runs the new one
    nexo_agent::workflow::tool::pin(&mut args, d.path());
    let out = wf.run(args, &ctx(d.path())).await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(client.count("NEW"), 1);
}

#[tokio::test]
async fn a_pin_made_with_the_old_short_hash_is_refused_so_nothing_starts_on_it() {
    let text = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"P\"\n";
    let d = project_with("saved", text);
    let client = Mock::new(vec![]);
    let wf = tool(&client, d.path());
    let long = subject("saved", text);
    let short = long[.."saved@".len() + 8].to_string();
    let out = wf.run(json!({"name": "saved", "pinned": short}), &ctx(d.path())).await;
    assert!(out.is_error && out.content.contains("changed after you were asked about it"), "{}", out.content);
    assert!(client.tasks().is_empty());
    let out = wf.run(json!({"name": "saved", "pinned": long}), &ctx(d.path())).await;
    assert!(!out.is_error, "{}", out.content);
}

#[tokio::test]
async fn no_subject_for_inline_text_or_a_name_that_does_not_load_and_a_bad_name_is_refused_first() {
    let text = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"P\"\n";
    let d = project_with("saved", text);
    std::fs::write(d.path().join(".nexo/workflows/broken.toml"), "this is = = not toml").unwrap();
    let pinned = |mut a: Value| {
        nexo_agent::workflow::tool::pin(&mut a, d.path());
        a.get("pinned").cloned()
    };
    assert_eq!(pinned(json!({"name": "saved"})), Some(json!(subject("saved", text))));
    assert_eq!(pinned(json!({"inline": text, "pinned": "saved@00000000000000000000000000000000"})), None);
    assert_eq!(pinned(json!({"name": "broken", "pinned": "x"})), None);
    assert_eq!(pinned(json!({"name": "ghost"})), None);
    assert_eq!(pinned(json!({"name": "../saved"})), None);
    let client = Mock::new(vec![]);
    let out = tool(&client, d.path()).run(json!({"name": "../../etc/passwd"}), &ctx(d.path())).await;
    assert!(out.is_error && out.content.contains("is not a workflow name"), "{}", out.content);
    assert!(client.tasks().is_empty());
}

#[tokio::test]
async fn the_unreported_note_counts_only_sub_agents_that_reported_without_usage() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![
        ("BOOM", Say::Fail),
        ("CUT", Say::Hang),
        ("BARE", Say::Bare("no meter".into())),
        ("FINE", says("fine")),
    ]);
    let wf = tool(&client, d.path());
    // a sub-agent whose first call fails, and one the person stops: neither had a report to give usage with
    let toml = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"boom\"\nprompt = \"BOOM\"\n[[phase.step]]\nid = \"cutoff\"\nprompt = \"CUT\"\n[[phase.step]]\nid = \"bare\"\nprompt = \"BARE\"\n[[phase.step]]\nid = \"fine\"\nprompt = \"FINE\"\n";
    let c = ctx(d.path());
    let cutter = async {
        client.until_started(2).await;
        let job = loop {
            if let Some(j) = c.jobs.list().into_iter().find(|j| j.label.contains("cutoff")) {
                break j;
            }
            tokio::task::yield_now().await;
        };
        assert!(c.jobs.kill(job.id));
    };
    let (out, ()) = tokio::join!(wf.run(json!({"inline": toml}), &c), cutter);
    assert!(steps_line(&out, "boom").contains("failed"), "{}", out.content);
    assert!(steps_line(&out, "cutoff").contains("failed"), "{}", out.content);
    // only the one that reported with no usage is named
    assert_eq!(out.content.matches("token use was not reported").count(), 1, "{}", out.content);
    let note_after = |id: &str| {
        let at = out.content.find(&format!("- {id}:")).unwrap();
        let rest = &out.content[at..];
        let end = rest[1..].find("\n- ").map_or(rest.len(), |i| i + 1);
        rest[..end].contains("token use was not reported")
    };
    assert!(note_after("bare"), "{}", out.content);
    for id in ["boom", "cutoff", "fine"] {
        assert!(!note_after(id), "{id}: {}", out.content);
    }
}

#[tokio::test]
async fn a_step_report_of_five_or_more_parts_in_a_later_prompt_holds_to_the_group_cap_part_by_part() {
    let d = test_tmpdir().unwrap();
    let items = (1..=6).map(|n| format!("\"i{n}\"")).collect::<Vec<_>>().join(", ");
    let client = Mock::new(vec![
        ("FIND", says(&format!("[{items}]"))),
        ("AFTER", says("done")),
        ("BIG", says(&"x".repeat(7000))),
    ]);
    let toml = "[[phase]]\ntitle = \"Find\"\n[[phase.step]]\nid = \"find\"\nprompt = \"FIND\"\nreturns = \"list\"\n[[phase]]\ntitle = \"Work\"\n[[phase.step]]\nid = \"work\"\neach = \"find\"\nprompt = \"BIG {{item}}\"\n[[phase]]\ntitle = \"Last\"\n[[phase.step]]\nid = \"last\"\nprompt = \"AFTER {{steps.work.report}}\"\n";
    let (out, _) = go(&client, d.path(), toml, "").await;
    assert!(!out.is_error, "{}", out.content);
    let after = client.tasks().into_iter().find(|t| t.starts_with("AFTER")).expect("the last step started");
    // 6 parts share 24000: 4000 each (the per-report cap of 6000 alone would let 36000 in)
    assert_eq!(after.matches('x').count(), 24000);
    assert_eq!(after.matches("[cut: 3000 more characters]").count(), 6, "{after}");
    for n in 1..=6 {
        assert!(after.contains(&format!("i{n}")), "part {n} vanished");
    }
}

#[tokio::test]
async fn text_of_many_bytes_per_character_is_cut_in_the_answer_on_a_character_boundary_with_the_cut_line() {
    let d = test_tmpdir().unwrap();
    // a sub-agent's own report is held to about 8 KB, so the 6000-character cap is reached only by text that is
    // mostly one byte per character: ASCII first, then characters of 2 and 3 bytes where the cut falls
    for (ch, ascii, multi, kept) in [("é", 5200, 1000, 800), ("日", 5300, 800, 700)] {
        let text = format!("{}{}", "x".repeat(ascii), ch.repeat(multi));
        let client = Mock::new(vec![("BIG", says(&text))]);
        let one = "[[phase]]\ntitle = \"Only\"\n[[phase.step]]\nid = \"s1\"\nprompt = \"BIG 1\"\n";
        let (out, _) = go(&client, d.path(), one, "").await;
        let head: String = out.content.chars().take(300).collect();
        assert_eq!(out.content.matches(ch).count(), kept, "{ch}: {head}");
        assert_eq!(out.content.matches('x').count(), ascii, "{ch}");
        assert!(
            out.content.contains(&format!("{}\n[cut: {} more characters]", ch.repeat(kept), ascii + multi - 6000)),
            "{ch}"
        );
        assert!(!out.content.contains('\u{fffd}'), "{ch}");
    }
}

#[tokio::test]
async fn a_run_reports_each_step_going_waiting_running_ok_in_order_with_its_sub_agents_and_ends() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![("ONE", says("one")), ("TWO", says("two")), ("THREE", says("three"))]);
    let c = ctx(d.path());
    let mut rx = c.jobs.subs().changes();
    let out = tool(&client, d.path()).run(json!({"inline": THREE}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    let seen = flow_events(&mut rx);

    // the first snapshot: every step waits
    let first = &seen[0];
    assert_eq!((first.id.as_str(), first.name.as_str(), first.state), ("wf#1", "inline", FlowState::Running));
    assert_eq!(first.phases.iter().map(|p| p.title.as_str()).collect::<Vec<_>>(), ["Look", "Join"]);
    assert!(first.phases.iter().flat_map(|p| &p.steps).all(|s| s.state == StepState::Waiting && s.subs.is_empty()));
    for id in ["a", "b", "c"] {
        assert_eq!(path_of(&seen, id), [StepState::Waiting, StepState::Running, StepState::Ok], "step {id}");
    }
    // the second phase begins only once the first has ended
    let at = |f: &dyn Fn(&FlowInfo) -> bool| seen.iter().position(f).unwrap();
    let c_runs = at(&|i| running(i, "c"));
    assert!(c_runs > at(&|i| fstep(i, "a").state == StepState::Ok), "c started before a ended");
    assert!(c_runs > at(&|i| fstep(i, "b").state == StepState::Ok), "c started before b ended");
    // a sub-agent's id is in the snapshot that says its step runs, and it is the id the registry lists
    let listed: Vec<String> = c.jobs.subs().list().into_iter().map(|i| i.id).collect();
    for id in ["a", "b", "c"] {
        let s = seen.iter().map(|i| fstep(i, id)).find(|s| s.state == StepState::Running).unwrap();
        assert_eq!(s.subs.len(), 1, "step {id}");
        assert!(listed.contains(&s.subs[0]), "{} is not in {listed:?}", s.subs[0]);
        assert!(s.started_ms.is_some() && s.ended_ms.is_none());
    }
    // the last snapshot says the run ended, and what each step spent
    let last = seen.last().unwrap();
    assert_eq!(last.state, FlowState::Ok);
    assert!(last.ended_ms.is_some());
    assert!(last
        .phases
        .iter()
        .flat_map(|p| &p.steps)
        .all(|s| s.tokens == 15 && s.ended_ms.is_some() && s.why.is_none()));
    assert_eq!(c.jobs.subs().flows(), vec![last.clone()], "the registry keeps the last snapshot");
}
