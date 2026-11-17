use super::*;
use nexo_core::tool::ToolDisplay;

#[test]
fn a_long_path_in_the_first_request_is_titled_by_its_file_name() {
    let shot = "/tmp/claude-1000/project/scratchpad/My Shots/a b.png";
    assert_eq!(title_of(&format!("what is this? '{shot}'")), "what is this? a b.png");
    assert_eq!(title_of("look at @/home/someone/work/project/src/main.rs please"), "look at @main.rs please");
    assert_eq!(title_of("open file:///home/someone/Pictures/screenshots/x.png"), "open x.png");
    assert_eq!(title_of(r"why C:\Users\someone\Pictures\Screenshots\err.png"), "why err.png");
    // short paths and plain quotes stay as written
    assert_eq!(title_of("why is /usr/bin/env missing"), "why is /usr/bin/env missing");
    assert_eq!(title_of("rename 'old name' to new"), "rename 'old name' to new");
    assert_eq!(title_of("an 'unclosed /very/long/path/that/goes/on/and/on.rs"), "an 'unclosed on.rs");
}
#[test]
fn the_prefix_is_saved_kept_by_a_rewrite_and_a_fork_and_an_old_file_has_none() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
    s.append(&Message::user("one")).unwrap();
    assert!(Session::open_path(s.path()).unwrap().prefix().is_none(), "an old file has none");
    let mut p = Prefix { tools: vec!["bash".into(), "diagnostics".into()], ..Prefix::default() };
    p.volatile.environment = "\nGit: x".into();
    s.record_prefix(&p).unwrap();
    p.shown = "tree".into();
    s.record_prefix(&p).unwrap();
    assert_eq!(Session::open_path(s.path()).unwrap().prefix(), Some(&p), "the last one wins");
    s.reset_with(&[Message::user("one")]).unwrap();
    assert_eq!(Session::open_path(s.path()).unwrap().prefix(), Some(&p), "a rewrite keeps it");
    let f = s.fork(1).unwrap();
    assert_eq!(Session::open_path(f.path()).unwrap().prefix(), Some(&p), "a fork goes on with it");
}
#[test]
fn a_stop_is_kept_through_a_resume_and_a_rewrite_until_the_request_that_carries_its_line() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
    s.append(&Message::user("y documenta cada función")).unwrap();
    let line =
        "[16:13:08 the person stopped the request above (Esc) while you waited for your reply; it got no answer]";
    s.record_stop(line).unwrap();
    assert_eq!(Session::open_path(s.path()).unwrap().stop(), Some(line), "a resume reads it back");
    s.reset_with(&[Message::user("y documenta cada función")]).unwrap();
    assert_eq!(Session::open_path(s.path()).unwrap().stop(), Some(line), "a rewrite keeps it");
    // a message that does not carry it (a mail turn, a goal's next step) leaves it for the person's next request
    s.append(&Message::user("[mail] a note")).unwrap();
    assert_eq!(s.stop(), Some(line));
    s.append(&Message::user(format!("{line}\n\nDetente, cambiamos de tema"))).unwrap();
    assert_eq!(s.stop(), None);
    assert_eq!(Session::open_path(s.path()).unwrap().stop(), None, "the request that carried it took it");
    s.record_stop(line).unwrap();
    s.rewind(1).unwrap();
    assert_eq!(Session::open_path(s.path()).unwrap().stop(), None, "a rewind goes back to before the stop");
}
#[test]
fn undoing_the_request_that_carried_a_stop_makes_the_stop_of_the_one_before_it_pending_again() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
    let line =
        "[16:13:08 the person stopped the request above (Esc) while you waited for your reply; it got no answer]";
    s.append(&Message::user("y documenta cada función")).unwrap();
    s.record_stop(line).unwrap();
    s.append(&Message::user(format!("{line}\n\n{}\nDetente, cambiamos de tema", nexo_core::agent::PERSON_MARK)))
        .unwrap();
    s.append(&Message::assistant("Vale.")).unwrap();
    assert_eq!(s.stop(), None);
    assert_eq!(s.fork(1).unwrap().stop(), Some(line), "a fork before that request goes on with it");
    s.rewind(1).unwrap();
    assert_eq!(s.stop(), Some(line), "the stopped request is the last one again");
    assert_eq!(Session::open_path(s.path()).unwrap().stop(), Some(line), "on disk too");
    assert_eq!(s.fork(1).unwrap().stop(), Some(line), "a fork of the whole goes on with the stop in force");
    s.rewind(0).unwrap();
    assert_eq!(s.stop(), None, "a rewind to before the stopped request takes its stop with it");
}
#[test]
fn a_failed_rewind_keeps_the_stop_the_file_still_holds() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    for t in ["one", "two"] {
        s.append(&Message::user(t)).unwrap();
    }
    let line =
        "[16:13:08 the person stopped the request above (Esc) while you waited for your reply; it got no answer]";
    s.record_stop(line).unwrap();
    let tmp = s.path().with_extension(format!("jsonl.{}.tmp", std::process::id()));
    std::fs::create_dir(&tmp).unwrap();
    assert!(s.rewind(1).is_err());
    assert_eq!(s.stop(), Some(line), "memory still matches the file");
    assert_eq!(Session::open_path(s.path()).unwrap().stop(), Some(line));
}
#[test]
fn sessions_born_in_the_same_second_are_all_saved() {
    // 3000 ids drawn in a few seconds from 65536 per second: some draw the same one
    let d = tempfile::tempdir().unwrap();
    let mut ids = std::collections::HashSet::new();
    let mut kept = Vec::new();
    // each open session holds its file: more than the 256 open files macOS gives a process to begin with
    #[cfg(unix)]
    allow_open_files(8192);
    for _ in 0..3000 {
        let s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
        assert!(ids.insert(s.id().to_string()));
        // an empty session is dropped with its file, so these stay open
        kept.push(s);
    }
    let files = std::fs::read_dir(d.path())
        .unwrap()
        .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "jsonl"))
        .count();
    assert_eq!(files, 3000);
}

/// Raises this process's soft limit of open files to `n`, or to its hard limit when that is lower.
#[cfg(unix)]
fn allow_open_files(n: libc::rlim_t) {
    let mut l = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: both calls only read or write the struct passed, which lives for the call
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut l) == 0 && l.rlim_cur < n {
            l.rlim_cur = n.min(l.rlim_max);
            libc::setrlimit(libc::RLIMIT_NOFILE, &l);
        }
    }
}
#[test]
fn a_cut_last_line_does_not_swallow_the_next_message() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
    s.append(&Message::user("one")).unwrap();
    let p = s.path().to_path_buf();
    drop(s);
    // the process died in the middle of the next line
    let mut f = OpenOptions::new().append(true).open(&p).unwrap();
    f.write_all(b"{\"type\":\"message\",\"index\":1,\"ts\":\"x\",\"mess").unwrap();
    drop(f);
    let mut s = Session::open_path(&p).unwrap();
    assert_eq!(s.messages().len(), 1);
    s.append(&Message::user("two")).unwrap();
    drop(s);
    let s = Session::open_path(&p).unwrap();
    assert_eq!(s.messages().len(), 2, "the message after the cut line was lost");
}

#[test]
fn compaction_rewrites_the_file_whole_and_keeps_the_outputs() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
    s.append(&Message::user("a")).unwrap();
    s.append(&Message::assistant("b")).unwrap();
    s.reset_with(&[Message::user("[summary]")]).unwrap();
    let p = s.path().to_path_buf();
    drop(s);
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(!text.contains("\"type\":\"reset\""), "a reset line means a non-atomic rewrite: {text}");
    assert_eq!(text.matches("\"type\":\"message\"").count(), 1);
    assert!(!p.with_extension("jsonl.tmp").exists());
    let s = Session::open_path(&p).unwrap();
    assert_eq!(s.messages().len(), 1);
}

#[test]
fn a_fork_keeps_the_first_messages_and_knows_its_parent() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
    for t in ["one", "two", "three"] {
        s.append(&Message::user(t)).unwrap();
    }
    let f = s.fork(2).unwrap();
    assert_ne!(f.id(), s.id());
    assert_eq!(f.messages().len(), 2);
    assert_eq!(f.meta().parent.as_deref(), Some(s.id()));
    assert_eq!(f.meta().fork_at, Some(2));
    assert_eq!(s.messages().len(), 3);
    // it reloads from disk with the same links
    let g = Session::open_path(f.path()).unwrap();
    assert_eq!(g.messages().len(), 2);
    assert_eq!(g.meta().parent.as_deref(), Some(s.id()));
    // an old meta line (no parent field) still loads
    let m: SessionMeta =
        serde_json::from_str(r#"{"id":"a","cwd":"/p","created":"","provider":"x","model":"y"}"#).unwrap();
    assert!(m.parent.is_none() && m.fork_at.is_none());
}

#[test]
fn a_session_run_by_a_program_says_so_and_old_ones_read_as_the_persons() {
    let d = tempfile::tempdir().unwrap();
    let s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
    let first = std::fs::read_to_string(d.path().join(format!("{}.jsonl", s.meta().id))).unwrap();
    assert!(!first.contains("headless"), "the person's sessions keep the old meta line: {first}");
    let mut m = s.meta().clone();
    m.headless = true;
    let back: SessionMeta = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
    assert!(back.headless);
}

#[test]
fn the_tree_puts_children_under_their_parent() {
    let mk = |id: &str, parent: Option<&str>, updated: &str| SessionMeta {
        id: id.into(),
        cwd: "/p".into(),
        created: String::new(),
        updated: updated.into(),
        provider: "x".into(),
        model: "y".into(),
        title: String::new(),
        messages: 0,
        parent: parent.map(String::from),
        fork_at: None,
        headless: false,
    };
    let metas = vec![mk("a", None, "1"), mk("c", Some("a"), "3"), mk("b", Some("a"), "2"), mk("z", Some("gone"), "9")];
    let t: Vec<(usize, String)> = tree(&metas).into_iter().map(|(d, m)| (d, m.id)).collect();
    assert_eq!(t, vec![(0, "z".into()), (0, "a".into()), (1, "c".into()), (1, "b".into())]);
}

#[test]
fn usage_lines_say_when_whose_and_how_much_was_held_and_old_readers_still_read_them() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), &PathBuf::from("/p"), "mock", "m1").unwrap();
    s.append(&Message::user("hello there")).unwrap();
    let meta = UsageMeta { model: Some("m1".into()), kind: Some(UsageKind::Main), held: Some(42) };
    s.record_usage_meta(Usage { input: 5, ..Default::default() }, 0.5, meta).unwrap();
    s.record_usage(Usage { input: 1, ..Default::default() }, 0.25).unwrap();
    let text = std::fs::read_to_string(s.path()).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .filter(|v: &serde_json::Value| v["type"] == "usage")
        .collect();
    assert_eq!(
        (lines[0]["kind"].as_str(), lines[0]["model"].as_str(), lines[0]["held"].as_u64()),
        (Some("main"), Some("m1"), Some(42))
    );
    assert!(lines[0]["ts"].as_str().is_some_and(|t| t.contains('T')));
    assert!(lines[1].get("kind").is_none() && lines[1].get("held").is_none(), "unknown stays unwritten");
    // a file with none of the new fields (an older nexo) reads as before
    let old = text.replace(",\"kind\":\"main\"", "").replace(",\"held\":42", "");
    std::fs::write(s.path(), old).unwrap();
    let mut back = Session::open_path(s.path()).unwrap();
    assert_eq!(back.usage().input, 6);
    // a rewrite keeps the totals in one line marked as a sum
    back.rewind(1).unwrap();
    let text = std::fs::read_to_string(back.path()).unwrap();
    let sums = text.lines().filter(|l| l.contains("\"type\":\"usage\"") && l.contains("\"kind\":\"sum\"")).count();
    assert_eq!(sums, 1, "{text}");
}

#[test]
fn writes_reads_lists_and_survives_a_cut_line() {
    let d = tempfile::tempdir().unwrap();
    let cwd = PathBuf::from("/p");
    let mut s = Session::create_in(d.path(), &cwd, "mock", "m1").unwrap();
    s.append(&Message::user("Fix the bug in src/lib.rs please, it is urgent and the tests fail on main")).unwrap();
    s.append(&Message::new(Role::Assistant, vec![Part::tool_call("c1", "read", serde_json::json!({"path": "a"}))]))
        .unwrap();
    s.record_output(
        "c1",
        &ToolOutput::ok("text").with_display(ToolDisplay::File {
            path: "a".into(),
            text: "text".into(),
            start_line: 1,
        }),
    )
    .unwrap();
    s.record_usage(Usage { input: 5, output: 2, ..Default::default() }, 0.001).unwrap();
    let id = s.id().to_string();
    let path = s.path().to_path_buf();
    drop(s);
    // a line cut in the middle
    let mut f = OpenOptions::new().append(true).open(&path).unwrap();
    write!(f, "{{\"type\":\"message\",\"index\":2,\"ts\":\"\",\"mess").unwrap();
    drop(f);
    let s = Session::open_path(&path).unwrap();
    assert_eq!(s.id(), id);
    assert_eq!(s.messages().len(), 2);
    assert_eq!(s.outputs()["c1"].content, "text");
    assert_eq!(s.usage().input, 5);
    assert!((s.cost() - 0.001).abs() < 1e-12);
    let want: String = "Fix the bug in src/lib.rs please, it is urgent and the tests fail on main"
        .chars()
        .take(60)
        .collect::<String>()
        + "…";
    assert_eq!(s.meta().title, want);
    let l = list_in(d.path());
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].messages, 2);
    assert_eq!(l[0].cwd, cwd);
}

#[test]
fn a_deleted_session_waits_in_the_trash_and_comes_back() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("keep me")).unwrap();
    let id = s.id().to_string();
    drop(s);
    let to = trash_in(d.path(), &id).unwrap();
    assert!(to.is_file());
    assert!(list_in(d.path()).is_empty(), "the list no longer shows it");
    assert_eq!(list_in(&trash_dir_in(d.path())).len(), 1);
    assert!(trash_in(d.path(), &id).is_err(), "a second delete finds nothing");
    for bad in ["../x", "a/b", "", "..", "x.jsonl"] {
        assert!(trash_in(d.path(), bad).is_err(), "{bad:?} is refused");
    }
    restore_in(d.path(), &id).unwrap();
    assert_eq!(list_in(d.path())[0].messages, 1);
    assert!(restore_in(d.path(), &id).is_err());
}

#[test]
fn a_rewind_rewrites_the_file_with_only_what_is_kept() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("first request")).unwrap();
    s.append(&Message::new(Role::Assistant, vec![Part::tool_call("c1", "read", serde_json::json!({"path": "a"}))]))
        .unwrap();
    s.record_output("c1", &ToolOutput::ok("A")).unwrap();
    s.append(&Message::tool_results(vec![Part::tool_result("c1", "A", false)])).unwrap();
    s.append(&Message::assistant("done")).unwrap();
    s.record_usage(Usage { input: 7, ..Default::default() }, 0.5).unwrap();
    s.rewind(1).unwrap();
    assert_eq!(s.messages().len(), 1);
    assert!(s.outputs().is_empty(), "outputs of dropped calls go with them");
    s.append(&Message::assistant("after the rewind")).unwrap();
    let p = s.path().to_path_buf();
    drop(s);
    assert!(!p.with_extension("jsonl.tmp").exists());
    let s = Session::open_path(&p).unwrap();
    assert_eq!(s.messages().len(), 2);
    assert_eq!(s.messages()[1].text(), "after the rewind");
    assert_eq!(s.meta().title, "first request");
    assert_eq!(s.usage().input, 7, "what was spent stays");
    assert!(s.outputs().is_empty());
    let md = s.export_markdown(|n, a| format!("{n} {}", a["path"].as_str().unwrap_or("")));
    assert!(md.starts_with("# first request"));
    assert!(md.contains("## You\n\nfirst request") && md.contains("## nexo\n\nafter the rewind"));
}

#[test]
fn export_shows_calls_with_their_output_folded() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("look")).unwrap();
    s.append(&Message::new(
        Role::Assistant,
        vec![
            Part::Reasoning { text: "hm".into(), signature: None, encrypted: None, model: None },
            Part::tool_call("c1", "bash", serde_json::json!({"command": "seq 100"})),
        ],
    ))
    .unwrap();
    let long: String = (1..=100).map(|i| format!("{i}\n")).collect();
    s.record_output("c1", &ToolOutput::ok(long.clone())).unwrap();
    s.append(&Message::tool_results(vec![Part::tool_result("c1", long, false)])).unwrap();
    s.append(&Message::assistant("a hundred lines")).unwrap();
    let md = s.export_markdown(|n, a| format!("{n}: {}", a["command"].as_str().unwrap_or("")));
    assert!(md.contains("<details><summary>reasoning</summary>\n\nhm"));
    assert!(md.contains("**bash** bash: seq 100\n\n```\n1\n2\n"));
    assert!(md.contains("… 40 more lines\n```"));
    assert!(md.contains("## nexo\n\na hundred lines"));
    assert!(md.trim_end().ends_with("cost: $0.0000"));
    assert_eq!(s.export_path(), d.path().join(format!("{}.md", s.id())));
}

#[test]
fn a_reset_replaces_the_history() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("a")).unwrap();
    s.append(&Message::assistant("b")).unwrap();
    s.reset_with(&[Message::user("[summary] a then b")]).unwrap();
    let p = s.path().to_path_buf();
    drop(s);
    let s = Session::open_path(&p).unwrap();
    assert_eq!(s.messages().len(), 1);
    assert!(s.messages()[0].text().starts_with("[summary]"));
}
#[cfg(unix)]
#[test]
fn sessions_are_readable_only_by_their_owner() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("sessions");
    let mut s = Session::create_in(&dir, Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("a secret")).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(s.path()), 0o600);
    s.rewind(0).unwrap();
    assert_eq!(mode(s.path()), 0o600, "the rewritten file too");
}

#[test]
fn a_released_session_leaves_no_lock_even_while_still_held() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("hi")).unwrap();
    let lock = lock_path(s.path());
    assert!(lock.exists());
    // the agent stopped while a task still holds the session: the process may exit now
    s.release();
    assert!(!lock.exists());
    assert!(s.path().exists(), "a session with a message stays");
    assert!(s.append(&Message::user("late")).is_err(), "nothing is saved after the release");
    assert!(!lock.exists(), "a late write does not take the lock again");
    s.release();
    drop(s);
    let names: Vec<_> = std::fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names.len(), 1, "{names:?}");
    // one opened and left without a word goes from the list at once
    let mut e = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    e.claim().unwrap();
    let p = e.path().to_path_buf();
    e.release();
    assert!(!p.exists() && !lock_path(&p).exists());
}

#[test]
fn a_utf8_sequence_cut_by_a_crash_keeps_the_session() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("héllo")).unwrap();
    let p = s.path().to_path_buf();
    drop(s);
    let mut f = OpenOptions::new().append(true).open(&p).unwrap();
    f.write_all(b"{\"type\":\"title\",\"title\":\"h\xc3").unwrap();
    drop(f);
    let s = Session::open_path(&p).unwrap();
    assert_eq!(s.messages().len(), 1);
    assert_eq!(s.messages()[0].text(), "héllo");
}

#[test]
fn a_second_writer_is_refused_and_a_stale_lock_taken_over() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("first")).unwrap();
    let p = s.path().to_path_buf();
    let lock = lock_path(&p);
    assert!(lock.is_file());
    drop(s);
    assert!(!lock.exists(), "the lock goes with its session");
    // another live writer holds it
    std::fs::write(&lock, other_live_pid().to_string()).unwrap();
    let mut s = Session::open_path(&p).unwrap();
    let e = s.append(&Message::user("x")).unwrap_err().to_string();
    assert!(e.contains("in use"), "{e}");
    // a lock whose pid cannot be read and that was left a day and more ago is taken over
    std::fs::write(&lock, "").unwrap();
    let f = File::options().write(true).open(&lock).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 3600)).unwrap();
    drop(f);
    s.append(&Message::user("y")).unwrap();
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), stamp());
    assert_eq!(lock_pid(&lock), Some(std::process::id()));
    drop(s);
    if cfg!(target_os = "linux") {
        // a lock whose process is gone is taken over at once
        std::fs::write(&lock, "4000000000").unwrap();
        let mut s = Session::open_path(&p).unwrap();
        s.append(&Message::user("z")).unwrap();
        drop(s);
    }
    // the temporary file of a rewrite is named for its process and does not stay
    let mut s = Session::open_path(&p).unwrap();
    s.rewind(1).unwrap();
    drop(s);
    let left: Vec<_> = std::fs::read_dir(d.path()).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(left.len(), 1, "{left:?}");
}

#[test]
fn continue_skips_empty_sessions_and_matches_the_folder_however_written() {
    let d = tempfile::tempdir().unwrap();
    let work = d.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let dir = d.path().join("sessions");
    let mut s = Session::create_in(&dir, &work, "mock", "m").unwrap();
    s.append(&Message::user("said something")).unwrap();
    let id = s.id().to_string();
    drop(s);
    let empty = Session::create_in(&dir, &work, "mock", "m").unwrap();
    drop(empty);
    let other_form = d.path().join("work").join("..").join("work");
    assert_eq!(latest_for_in(&dir, &other_form).map(|m| m.id), Some(id));
    assert!(Session::open("../x").is_err_and(|e| e.to_string().contains("not a session id")));
}

#[test]
fn continue_in_a_folder_skips_a_scripts_session_unless_it_is_the_only_one() {
    let d = tempfile::tempdir().unwrap();
    let work = d.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let dir = d.path().join("sessions");
    let mut own = Session::create_in(&dir, &work, "mock", "m").unwrap();
    own.append(&Message::user("a real conversation")).unwrap();
    let own_id = own.id().to_string();
    drop(own);
    // a newer session a program started: its meta line says `headless`, as `nexo -p` writes it
    let mut script = Session::create_in(&dir, &work, "mock", "m").unwrap();
    script.append(&Message::user("a script run")).unwrap();
    let script_id = script.id().to_string();
    let script_path = script.path().to_path_buf();
    drop(script);
    let text = std::fs::read_to_string(&script_path).unwrap();
    let (first, rest) = text.split_once('\n').unwrap();
    let Line::Meta(mut m) = serde_json::from_str(first).unwrap() else { panic!("no meta line") };
    m.headless = true;
    std::fs::write(&script_path, format!("{}\n{rest}", serde_json::to_string(&Line::Meta(m)).unwrap())).unwrap();
    // the script run is the newer file, whatever the clock said when both were written
    let f = File::options().write(true).open(&script_path).unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60)).unwrap();
    drop(f);
    assert_eq!(latest_for_in(&dir, &work).map(|m| m.id), Some(script_id));
    assert_eq!(latest_own_for_in(&dir, &work).map(|m| m.id), Some(own_id.clone()));
    std::fs::remove_file(dir.join(format!("{own_id}.jsonl"))).unwrap();
    assert!(
        latest_own_for_in(&dir, &work).is_none(),
        "a folder with only a script's session has none of the person's own"
    );
}

#[test]
fn a_live_writer_keeps_its_lock_however_old() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("first")).unwrap();
    let p = s.path().to_path_buf();
    let lock = lock_path(&p);
    drop(s);
    let two_days_old = |text: &str| {
        std::fs::write(&lock, text).unwrap();
        let f = File::options().write(true).open(&lock).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 3600)).unwrap();
    };
    // another process that always runs; its lock, with when it started, is two days old
    two_days_old(&live_pid_stamp());
    let mut s = Session::open_path(&p).unwrap();
    let r = s.append(&Message::user("x"));
    // Linux, macOS and Windows tell when a process started (`/proc`, `ps`, the process times)
    if proc_start(other_live_pid()).is_some() {
        assert!(r.unwrap_err().to_string().contains("in use"));
    } else {
        r.unwrap(); // no start time here: a pid in use may be another process, the age decides
    }
    drop(s);
    // without its start time, a live pid may have been taken by another process since: the age decides
    two_days_old(&other_live_pid().to_string());
    let mut s = Session::open_path(&p).unwrap();
    s.append(&Message::user("y")).unwrap();
    assert_eq!(lock_pid(&lock), Some(std::process::id()));
}

/// A process other than the test that always runs: init on Unix, the System process on Windows (its pids are
/// multiples of 4, so pid 1 names nothing there).
fn other_live_pid() -> u32 {
    if cfg!(windows) {
        4
    } else {
        1
    }
}

/// The lock that process would write: its pid and, where the system tells, when it started and the boot.
fn live_pid_stamp() -> String {
    let pid = other_live_pid();
    match (proc_start(pid), boot_id()) {
        (Some(start), Some(boot)) => format!("{pid} {start} {boot}"),
        (Some(start), None) => format!("{pid} {start}"),
        _ => pid.to_string(),
    }
}

#[test]
fn a_failed_rewind_changes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    for t in ["one", "two", "three"] {
        s.append(&Message::user(t)).unwrap();
    }
    // the temporary file cannot be made: a folder holds its name
    let tmp = s.path().with_extension(format!("jsonl.{}.tmp", std::process::id()));
    std::fs::create_dir(&tmp).unwrap();
    assert!(s.rewind(1).is_err());
    assert_eq!(s.messages().len(), 3, "memory still matches the file");
    std::fs::remove_dir(&tmp).unwrap();
    s.rewind(2).unwrap();
    let g = Session::open_path(s.path()).unwrap();
    assert_eq!(g.messages().iter().map(|m| m.text()).collect::<Vec<_>>(), ["one", "two"]);
}

#[test]
fn a_failed_reset_keeps_memory_as_the_file() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    for t in ["one", "two", "three"] {
        s.append(&Message::user(t)).unwrap();
    }
    let tmp = s.path().with_extension(format!("jsonl.{}.tmp", std::process::id()));
    std::fs::create_dir(&tmp).unwrap();
    assert!(s.reset_with(&[Message::user("summary")]).is_err());
    assert_eq!(s.messages().iter().map(|m| m.text()).collect::<Vec<_>>(), ["one", "two", "three"]);
    std::fs::remove_dir(&tmp).unwrap();
    s.reset_with(&[Message::user("summary")]).unwrap();
    assert_eq!(s.messages().len(), 1);
}
#[test]
fn a_file_replaced_but_not_reopened_keeps_memory_as_the_file_and_says_so() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    for t in ["one", "two", "three"] {
        s.append(&Message::user(t)).unwrap();
    }
    FAIL_REOPEN.with(|f| f.set(true));
    assert!(s.rewind(1).is_err());
    assert_eq!(s.messages().len(), 1, "the file holds only the kept message now");
    // while it cannot be opened, every write says it was not saved
    assert!(s.append(&Message::user("lost")).is_err());
    FAIL_REOPEN.with(|f| f.set(false));
    s.append(&Message::user("four")).unwrap();
    let on_disk: Vec<String> = Session::open_path(s.path()).unwrap().messages().iter().map(|m| m.text()).collect();
    assert_eq!(on_disk, ["one", "four"]);
}

#[test]
fn a_fork_keeps_the_goal_on_disk() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("one")).unwrap();
    s.record_goal(Some("ship it")).unwrap();
    let f = s.fork(1).unwrap();
    assert_eq!(f.goal(), Some("ship it"));
    let g = Session::open_path(f.path()).unwrap();
    assert_eq!(g.goal(), Some("ship it"));
}

#[test]
fn the_list_counts_past_a_line_cut_inside_a_utf8_sequence() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("one")).unwrap();
    let p = s.path().to_path_buf();
    drop(s);
    let mut f = OpenOptions::new().append(true).open(&p).unwrap();
    f.write_all(b"{\"type\":\"title\",\"title\":\"h\xc3\n").unwrap();
    drop(f);
    let mut s = Session::open_path(&p).unwrap();
    s.append(&Message::user("two")).unwrap();
    s.set_title("later").unwrap();
    drop(s);
    let m = &list_in(d.path())[0];
    assert_eq!((m.messages, m.title.as_str()), (2, "later"));
}

#[test]
fn a_held_session_is_read_but_not_claimed_nor_touched() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("one")).unwrap();
    let p = s.path().to_path_buf();
    drop(s);
    // the other writer is in the middle of a line
    OpenOptions::new().append(true).open(&p).unwrap().write_all(b"{\"type\":\"ti").unwrap();
    let before = std::fs::read(&p).unwrap();
    std::fs::write(lock_path(&p), other_live_pid().to_string()).unwrap();
    let mut s = Session::open_path(&p).unwrap();
    assert_eq!(s.messages().len(), 1);
    assert!(s.claim().unwrap_err().to_string().contains("in use"));
    drop(s);
    assert_eq!(std::fs::read(&p).unwrap(), before);
    std::fs::remove_file(lock_path(&p)).unwrap();
    let mut s = Session::open_path(&p).unwrap();
    s.claim().unwrap();
    s.append(&Message::user("two")).unwrap();
    assert_eq!(Session::open_path(&p).unwrap().messages().len(), 2);
}

#[test]
fn a_lock_taken_over_stops_the_saving_and_is_left_to_its_new_holder() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("one")).unwrap();
    let lock = lock_path(s.path());
    // another nexo judged the lock stale and took it
    std::fs::write(&lock, "1").unwrap();
    let e = s.append(&Message::user("two")).unwrap_err().to_string();
    assert!(e.contains("taken over") && e.contains("pid 1"), "{e}");
    let p = s.path().to_path_buf();
    drop(s);
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), "1", "its lock stays");
    assert_eq!(Session::open_path(&p).unwrap().messages().len(), 1);
    // a live lock deleted by hand is taken again by its writer
    std::fs::remove_file(&lock).unwrap();
    let mut s = Session::open_path(&p).unwrap();
    s.append(&Message::user("two")).unwrap();
    std::fs::remove_file(&lock).unwrap();
    s.append(&Message::user("three")).unwrap();
    assert_eq!(lock_pid(&lock), Some(std::process::id()));
    std::fs::write(&lock, "1").unwrap();
    assert!(s.append(&Message::user("four")).is_err());
    drop(s);
    std::fs::remove_file(&lock).unwrap();
    assert_eq!(Session::open_path(&p).unwrap().messages().len(), 3);
}

#[test]
fn a_stale_lock_is_taken_over_only_while_it_is_the_one_judged() {
    let d = tempfile::tempdir().unwrap();
    let lock = d.path().join("x.jsonl.lock");
    // judged stale, but another nexo took it over first and holds a fresh one
    std::fs::write(&lock, "1").unwrap();
    assert!(!take_over(&lock, "4000000000").unwrap());
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), "1", "the fresh lock stays");
    // still the one judged: it goes
    std::fs::write(&lock, "4000000000").unwrap();
    assert!(take_over(&lock, "4000000000").unwrap());
    assert!(!lock.exists());
    // already gone: nothing to take over
    assert!(!take_over(&lock, "4000000000").unwrap());
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0, "nothing left aside");
}

#[test]
fn without_hard_links_a_fresh_lock_is_put_back_never_deleted() {
    let d = tempfile::tempdir().unwrap();
    let lock = d.path().join("x.jsonl.lock");
    let no_links = |_: &Path, _: &Path| Err(std::io::Error::other("no hard links here"));
    // another nexo took it over first: moved back, the place being empty
    std::fs::write(&lock, "1").unwrap();
    assert!(!take_over_with(&lock, "4000000000", no_links).unwrap());
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), "1", "the fresh lock stays");
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1, "nothing left aside");
    // yet another took the place meanwhile: the one moved aside is kept, the session is in use
    let third = |_: &Path, to: &Path| {
        std::fs::write(to, "2").unwrap();
        Err(std::io::Error::other("no hard links here"))
    };
    let e = take_over_with(&lock, "4000000000", third).unwrap_err().to_string();
    assert!(e.contains("in use") && e.contains("pid 1"), "{e}");
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), "2");
    let aside = lock.with_extension(format!("lock.{}.stale", std::process::id()));
    assert_eq!(std::fs::read_to_string(&aside).unwrap(), "1", "kept, not deleted");
}

#[cfg(target_os = "linux")]
#[test]
fn a_lock_whose_pid_now_names_another_process_is_stale() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("first")).unwrap();
    let p = s.path().to_path_buf();
    let lock = lock_path(&p);
    drop(s);
    let stat = std::fs::read_to_string("/proc/1/stat").unwrap();
    let start: u64 = stat[stat.rfind(')').unwrap() + 1..].split_whitespace().nth(19).unwrap().parse().unwrap();
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap().trim().to_string();
    // pid 1 runs, but it is not the process that wrote these: it started at another time
    // (a reused pid, or a pid of a container), or the machine has booted since
    for other in [format!("1 {} {boot}", start + 12_345), format!("1 {start} not-this-boot")] {
        std::fs::write(&lock, &other).unwrap();
        let mut s = Session::open_path(&p).unwrap();
        s.append(&Message::user("x")).unwrap();
        assert_eq!(lock_pid(&lock), Some(std::process::id()));
        drop(s);
    }
    // the same process, still running: in use
    std::fs::write(&lock, format!("1 {start} {boot}")).unwrap();
    let mut s = Session::open_path(&p).unwrap();
    assert!(s.append(&Message::user("y")).unwrap_err().to_string().contains("in use"));
}

#[test]
fn a_session_in_use_elsewhere_is_not_trashed() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("one")).unwrap();
    let id = s.id().to_string();
    let lock = lock_path(s.path());
    drop(s);
    std::fs::write(&lock, other_live_pid().to_string()).unwrap();
    let e = trash_in(d.path(), &id).unwrap_err().to_string();
    assert!(e.contains("in use"), "{e}");
    std::fs::remove_file(&lock).unwrap();
    trash_in(d.path(), &id).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn a_lock_from_another_host_or_pid_namespace_is_judged_by_its_age() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("first")).unwrap();
    let p = s.path().to_path_buf();
    let lock = lock_path(&p);
    drop(s);
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap().trim().to_string();
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap().trim().to_string();
    let ns = std::fs::read_link("/proc/self/ns/pid").unwrap().to_string_lossy().into_owned();
    // the pids written there say nothing here: no process of theirs runs under them in this one
    for other in
        [format!("4000000000 77 {boot} {host} pid:[1]"), format!("4000000000 77 another-boot another-host {ns}")]
    {
        std::fs::write(&lock, &other).unwrap();
        let mut s = Session::open_path(&p).unwrap();
        assert!(s.append(&Message::user("x")).unwrap_err().to_string().contains("in use"), "{other}");
        drop(s);
        // left a day and more ago: taken over
        let f = File::options().write(true).open(&lock).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 3600)).unwrap();
        drop(f);
        let mut s = Session::open_path(&p).unwrap();
        s.append(&Message::user("y")).unwrap();
        assert_eq!(std::fs::read_to_string(&lock).unwrap(), stamp());
        drop(s);
    }
    // this process's own stamp names its host and namespace
    assert!(stamp().ends_with(&format!(" {host} {ns}")), "{}", stamp());
}

#[test]
fn a_taker_that_ended_before_the_next_write_is_noticed_in_the_file() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("one")).unwrap();
    let lock = lock_path(s.path());
    let p = s.path().to_path_buf();
    // another nexo took the idle lock over, wrote, and ended, removing its lock
    let mut f = OpenOptions::new().append(true).open(&p).unwrap();
    write_line(&mut f, &Line::Message { index: 1, ts: now(), message: Message::user("theirs") }).unwrap();
    std::fs::remove_file(&lock).unwrap();
    let e = s.append(&Message::user("mine")).unwrap_err().to_string();
    assert!(e.contains("taken over"), "{e}");
    assert!(!lock.exists(), "no lock taken again");
    drop(s);
    let on_disk: Vec<String> = Session::open_path(&p).unwrap().messages().iter().map(|m| m.text()).collect();
    assert_eq!(on_disk, ["one", "theirs"]);
    // and one that replaced the file with one of the same length: only Unix tells files apart
    // (device and inode); elsewhere a taker that wrote is caught by the length
    #[cfg(unix)]
    {
        let mut s = Session::open_path(&p).unwrap();
        s.claim().unwrap();
        let other = d.path().join("other");
        std::fs::copy(&p, &other).unwrap();
        std::fs::rename(&other, &p).unwrap();
        std::fs::remove_file(&lock).unwrap();
        assert!(s.append(&Message::user("mine")).unwrap_err().to_string().contains("taken over"));
    }
}

#[test]
fn a_session_taken_over_stays_lost_after_its_taker_is_gone() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("one")).unwrap();
    let lock = lock_path(s.path());
    let p = s.path().to_path_buf();
    // another nexo took it over, wrote, and ended, removing its lock
    std::fs::write(&lock, "1").unwrap();
    assert!(s.append(&Message::user("two")).is_err());
    std::fs::remove_file(&lock).unwrap();
    let e = s.append(&Message::user("three")).unwrap_err().to_string();
    assert!(e.contains("taken over"), "{e}");
    assert!(s.rewind(0).is_err());
    assert!(!lock.exists(), "no lock taken again");
    drop(s);
    assert_eq!(Session::open_path(&p).unwrap().messages().len(), 1);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_paused_holder_is_named_with_the_way_back() {
    let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
    let pid = child.id();
    let lock = Path::new("/x/20260101-000000-abcd.jsonl.lock");
    assert!(!stopped(pid));
    assert!(in_use(lock, Some(pid)).to_string().contains("in use by another nexo"));
    std::process::Command::new("kill").args(["-STOP", &pid.to_string()]).status().unwrap();
    let mut paused = false;
    for _ in 0..200 {
        paused = stopped(pid);
        if paused {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let msg = in_use(lock, Some(pid)).to_string();
    let _ = child.kill();
    let _ = child.wait();
    assert!(paused, "a stopped process reads as paused");
    assert!(msg.contains("paused nexo") && msg.contains("`fg`") && msg.contains(&format!("kill {pid}")), "{msg}");
    assert!(msg.contains("20260101-000000-abcd"), "{msg}");
}

/// A session with several turns, tool calls and results, usage, a model, a prefix: returns its path.
fn busy_session(dir: &Path) -> PathBuf {
    let mut s = Session::create_in(dir, Path::new("/p"), "mock", "m1").unwrap();
    for n in 0..3 {
        s.append(&Message::user(format!("turn {n}: please look at the file héllo ✓"))).unwrap();
        let id = format!("c{n}");
        s.append(&Message::new(
            Role::Assistant,
            vec![Part::text("reading"), Part::tool_call(&id, "read", serde_json::json!({"path": "a"}))],
        ))
        .unwrap();
        s.record_output(&id, &ToolOutput::ok("text of a")).unwrap();
        s.append(&Message::tool_results(vec![Part::tool_result(&id, "text of a", false)])).unwrap();
        s.append(&Message::assistant(format!("done {n}"))).unwrap();
        s.record_usage(Usage { input: 7, output: 3, ..Default::default() }, 0.002).unwrap();
        if n == 0 {
            s.record_prefix(&Prefix { tools: vec!["read".into()], ..Default::default() }).unwrap();
        }
    }
    s.record_model("mock", "m2").unwrap();
    let p = s.path().to_path_buf();
    drop(s);
    p
}

#[test]
fn a_file_cut_at_any_byte_loads_every_whole_record_before_the_cut() {
    let d = tempfile::tempdir().unwrap();
    let full = std::fs::read(busy_session(d.path())).unwrap();
    // end offset (without the newline) of each line, and whether it is a message
    let (mut ends, mut at) = (Vec::new(), 0usize);
    for l in full.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let start = full[at..].windows(l.len()).position(|w| w == l).unwrap() + at;
        at = start + l.len();
        let is_msg = String::from_utf8_lossy(l).starts_with("{\"type\":\"message\"");
        ends.push((at, is_msg));
    }
    let meta_end = ends[0].0;
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("cut.jsonl");
    for len in 0..=full.len() {
        std::fs::write(&p, &full[..len]).unwrap();
        let r = std::panic::catch_unwind(|| Session::open_path(&p));
        let r = r.unwrap_or_else(|_| panic!("panic at {len} bytes"));
        if len < meta_end {
            // not even the first line is whole: there is nothing to load, and it says so
            assert!(r.is_err(), "{len}");
            continue;
        }
        let s = r.unwrap_or_else(|e| panic!("unusable at {len} bytes: {e}"));
        let want = ends.iter().filter(|(e, m)| *m && *e <= len).count();
        assert_eq!(s.messages().len(), want, "at {len} bytes");
    }
    let s = Session::open_path(&{
        std::fs::write(&p, &full).unwrap();
        p.clone()
    })
    .unwrap();
    assert_eq!(s.messages().len(), 12);
    assert_eq!(s.usage().input, 21);
    assert!(s.prefix().is_some());
    assert_eq!(s.meta().model, "m2");
}

#[test]
fn a_session_cut_between_a_call_and_its_result_is_sent_whole() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
    s.append(&Message::user("go")).unwrap();
    s.append(&Message::new(Role::Assistant, vec![Part::tool_call("c1", "read", serde_json::json!({}))])).unwrap();
    let p = s.path().to_path_buf();
    drop(s);
    let mut s = Session::open_path(&p).unwrap();
    assert!(!nexo_core::message::is_well_formed(s.messages()), "the file itself ends on a bare call");
    // what the next request carries: the person writes again after the cut
    s.append(&Message::user("again")).unwrap();
    let (sent, mends) = nexo_core::message::well_formed(s.messages().to_vec());
    assert!(nexo_core::message::is_well_formed(&sent), "{sent:?}");
    assert_eq!(mends.len(), 1, "{mends:?}");
    // and with nothing written after it
    let (sent, _) = nexo_core::message::well_formed(s.messages()[..2].to_vec());
    assert!(nexo_core::message::is_well_formed(&sent));
}

#[test]
fn garbage_lines_are_skipped_and_the_rest_is_kept() {
    let d = tempfile::tempdir().unwrap();
    let p = busy_session(d.path());
    let good = std::fs::read(&p).unwrap();
    let mut lines: Vec<&[u8]> = good.split(|b| *b == b'\n').filter(|l| !l.is_empty()).collect();
    let nul = vec![0u8; 64];
    let unknown = br#"{"type":"from_the_future","x":[1,2,3]}"#.to_vec();
    let bad_utf8 = b"{\"type\":\"title\",\"title\":\"\xff\xfe\xc3\"}".to_vec();
    lines.insert(3, &nul);
    lines.insert(5, &unknown);
    lines.insert(7, &bad_utf8);
    let mut bytes = lines.join(&b'\n');
    bytes.push(b'\n');
    bytes.extend_from_slice(&nul); // a NUL tail with no newline, as after a power cut
    std::fs::write(&p, &bytes).unwrap();
    let s = Session::open_path(&p).unwrap();
    assert_eq!(s.messages().len(), 12);
    assert_eq!(s.usage().input, 21);
    // the list reads it too
    let l = list_in(d.path());
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].messages, 12);
}

#[test]
fn a_write_after_a_cut_line_starts_on_a_line_of_its_own() {
    let d = tempfile::tempdir().unwrap();
    let p = busy_session(d.path());
    let mut b = std::fs::read(&p).unwrap();
    b.truncate(b.len() - 20); // the last line (the model record) is cut
    std::fs::write(&p, &b).unwrap();
    let mut s = Session::open_path(&p).unwrap();
    s.append(&Message::user("after the cut")).unwrap();
    s.record_usage(Usage { input: 1, ..Default::default() }, 0.0).unwrap();
    drop(s);
    let text = String::from_utf8(std::fs::read(&p).unwrap()).unwrap();
    let bad = text.lines().filter(|l| serde_json::from_str::<Line>(l).is_err()).count();
    assert_eq!(bad, 1, "only the cut line is lost, nothing is glued to it");
    let s = Session::open_path(&p).unwrap();
    assert_eq!(s.messages().len(), 13);
    assert_eq!(s.messages()[12].text(), "after the cut");
    assert_eq!(s.usage().input, 22);
}

#[test]
fn one_empty_or_damaged_file_does_not_hide_the_good_sessions() {
    let d = tempfile::tempdir().unwrap();
    busy_session(d.path());
    std::fs::write(d.path().join("empty.jsonl"), b"").unwrap();
    std::fs::write(d.path().join("nul.jsonl"), vec![0u8; 100]).unwrap();
    std::fs::write(d.path().join("cutmeta.jsonl"), b"{\"type\":\"meta\",\"id\":\"x").unwrap();
    std::fs::write(d.path().join("utf8.jsonl"), b"\xff\xfe\xfd\n").unwrap();
    std::fs::write(d.path().join("noop.jsonl"), b"{\"type\":\"reset\"}\n").unwrap();
    std::fs::create_dir(d.path().join("dir.jsonl")).unwrap();
    let l = list_in(d.path());
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].messages, 12);
}

/// Points the open session file at `target`, as a full disk does to every write.
#[cfg(target_os = "linux")]
fn aim_at(s: &mut Session, target: &str) {
    s.file = Some(OpenOptions::new().write(true).open(target).unwrap());
}

#[cfg(target_os = "linux")]
#[test]
fn a_full_disk_fails_each_write_but_tells_the_person_once_and_again_when_it_works() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
    s.append(&Message::user("before")).unwrap();
    assert_eq!(s.take_save_news(), None);
    let good = s.file.take();
    aim_at(&mut s, "/dev/full");
    for n in 0..4 {
        let e = s.append(&Message::assistant(format!("lost {n}"))).unwrap_err();
        assert!(e.to_string().to_lowercase().contains("space"), "the cause is kept: {e}");
    }
    // the conversation goes on in memory
    assert_eq!(s.messages().len(), 5);
    match s.take_save_news() {
        Some(SaveNews::Stopped(why)) => assert!(why.to_lowercase().contains("space"), "{why}"),
        other => panic!("one notice that saving stopped, got {other:?}"),
    }
    assert_eq!(s.take_save_news(), None, "not one per failed line");
    // space is back
    s.file = good;
    s.append(&Message::assistant("after")).unwrap();
    assert_eq!(s.take_save_news(), Some(SaveNews::Resumed));
    s.append(&Message::assistant("again")).unwrap();
    assert_eq!(s.take_save_news(), None);
    // a fix before anyone was told says nothing
    aim_at(&mut s, "/dev/full");
    assert!(s.append(&Message::assistant("x")).is_err());
    s.file = Some(reopen(&s.path).unwrap());
    s.append(&Message::assistant("y")).unwrap();
    assert_eq!(s.take_save_news(), None);
}

#[cfg(target_os = "linux")]
#[test]
fn a_line_cut_by_a_full_disk_does_not_swallow_the_next_good_line() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
    s.append(&Message::user("one")).unwrap();
    // half a line reaches the file
    s.file.as_mut().unwrap().write_all(b"{\"type\":\"mess").unwrap();
    let good = s.file.take();
    aim_at(&mut s, "/dev/full");
    assert!(s.append(&Message::assistant("lost")).is_err());
    s.file = good;
    s.append(&Message::assistant("kept")).unwrap();
    let back = Session::open_path(s.path()).unwrap();
    let texts: Vec<_> = back.messages().iter().map(|m| m.text()).collect();
    assert_eq!(texts, ["one", "kept"]);
    assert_eq!(back.skipped_lines(), 1);
}

#[test]
fn a_file_removed_or_replaced_by_a_folder_is_a_failure_to_save_not_a_silent_success() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
    s.append(&Message::user("one")).unwrap();
    let path = s.path().to_path_buf();
    std::fs::remove_file(&path).unwrap();
    assert!(s.append(&Message::assistant("two")).is_err(), "removed");
    assert!(matches!(s.take_save_news(), Some(SaveNews::Stopped(_))));
    std::fs::create_dir(&path).unwrap();
    assert!(s.append(&Message::assistant("three")).is_err(), "a folder");
    assert!(s.reset_with(&[Message::user("one")]).is_err(), "a rewrite cannot replace a folder");
    assert_eq!(s.take_save_news(), None, "told once");
    assert_eq!(s.messages().len(), 3, "memory keeps the conversation");
}

#[cfg(unix)]
#[test]
fn a_read_only_folder_fails_the_rewrite_and_keeps_the_old_file_and_memory() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let p = busy_session(d.path());
    let mut s = Session::open_path(&p).unwrap();
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    if std::fs::File::create(d.path().join("probe")).is_ok() {
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        return; // running as root: permissions do not stop it
    }
    let before = std::fs::read(&p).unwrap();
    let kept = s.messages().len();
    assert!(s.reset_with(&[Message::user("short")]).is_err());
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), before, "the old file is untouched");
    assert_eq!(s.messages().len(), kept, "memory matches the file");
    assert!(matches!(s.take_save_news(), Some(SaveNews::Stopped(_))));
    s.reset_with(&[Message::user("short")]).unwrap();
    assert_eq!(s.take_save_news(), Some(SaveNews::Resumed));
}

#[test]
fn resuming_a_damaged_file_loads_every_good_line_and_counts_the_skipped_ones() {
    let d = tempfile::tempdir().unwrap();
    let p = busy_session(d.path());
    let mut lines: Vec<Vec<u8>> =
        std::fs::read(&p).unwrap().split(|b| *b == b'\n').filter(|l| !l.is_empty()).map(|l| l.to_vec()).collect();
    lines.insert(3, b"{not json".to_vec());
    lines.insert(6, br#"{"type":"message","index":"x","message":7}"#.to_vec()); // the wrong shape
    lines.insert(9, vec![0u8; 16]);
    let mut bytes = lines.join(&b'\n');
    bytes.extend_from_slice(b"\n{\"type\":\"usage\",\"usage\":{\"inp"); // cut last line
    std::fs::write(&p, &bytes).unwrap();
    let s = Session::open_path(&p).unwrap();
    assert_eq!(s.messages().len(), 12);
    assert_eq!(s.skipped_lines(), 4);
    let clean = Session::open_path(&busy_session(tempfile::tempdir().unwrap().path())).unwrap();
    assert_eq!(clean.skipped_lines(), 0);
}

/// The listing as it was written first: every line of the file parsed, to compare the cheap one with.
fn read_meta_parsing_every_line(path: &Path) -> Option<SessionMeta> {
    let f = File::open(path).ok()?;
    let mut r = BufReader::new(f);
    let mut first = String::new();
    r.read_line(&mut first).ok()?;
    let Line::Meta(mut m) = serde_json::from_str(&first).ok()? else { return None };
    let mut n = 0;
    let mut title = None;
    for l in r.split(b'\n').map_while(Result::ok) {
        let l = String::from_utf8_lossy(&l);
        match serde_json::from_str::<Line>(&l) {
            Ok(Line::Message { .. }) => n += 1,
            Ok(Line::Reset) => n = 0,
            Ok(Line::Title { title: t }) => title = Some(t),
            _ => {}
        }
    }
    m.messages = n;
    if let Some(t) = title {
        m.title = t;
    }
    if let Ok(md) = std::fs::metadata(path) {
        if let Ok(t) = md.modified() {
            let t: time::OffsetDateTime = t.into();
            m.updated = t.format(&time::format_description::well_known::Rfc3339).unwrap_or_default();
        }
    }
    Some(m)
}

/// A session file of `outputs` tool outputs of `big` bytes each, written the way a session writes it.
fn write_big_session(dir: &Path, outputs: usize, big: usize) -> PathBuf {
    let mut s = Session::create_in(dir, Path::new("/p"), "mock", "m1").unwrap();
    for i in 0..outputs {
        s.append(&Message::user(format!("question {i}"))).unwrap();
        let id = format!("c{i}");
        s.append(&Message::new(Role::Assistant, vec![Part::tool_call(&id, "read", serde_json::json!({"path": "a"}))]))
            .unwrap();
        s.record_output(&id, &ToolOutput::ok("x".repeat(big))).unwrap();
        s.record_usage(Usage { input: 5, output: 2, ..Default::default() }, 0.001).unwrap();
    }
    s.path().to_path_buf()
}

#[test]
fn the_list_reads_what_parsing_every_line_gave_on_files_with_huge_outputs_and_damage() {
    let d = tempfile::tempdir().unwrap();
    let plain = write_big_session(d.path(), 3, 200_000);
    // a reset, a later title, a cut line, a blank line, a line with another key order, a CRLF line and a stray line
    let reset = write_big_session(d.path(), 2, 50_000);
    let mut f = OpenOptions::new().append(true).open(&reset).unwrap();
    write!(f, "{{\"type\":\"reset\"}}\n{{\"type\":\"title\",\"title\":\"later title\"}}\n\n").unwrap();
    f.write_all(b"not json at all\n").unwrap();
    f.write_all(b"{\"index\":9,\"type\":\"message\",\"ts\":\"\",\"message\":{\"role\":\"user\",\"content\":[]}}\r\n")
        .unwrap();
    f.write_all(b"{\"type\":\"tool_output\",\"call_id\":\"x\",\"output\":{\"content\":\"cut\n").unwrap();
    f.write_all(b"{\"type\":\"message\",\"index\":1,\"ts\":\"x\",\"mess").unwrap();
    drop(f);
    // a UTF-8 sequence cut in the middle of a message line, with a message after it
    let cut = write_big_session(d.path(), 1, 10);
    let mut f = OpenOptions::new().append(true).open(&cut).unwrap();
    f.write_all(
        b"{\"type\":\"message\",\"index\":5,\"ts\":\"\",\"message\":{\"role\":\"user\",\"content\":\"\xe2\x82\n",
    )
    .unwrap();
    drop(f);
    // an empty file, a file that is not a session, and one that is only its head
    std::fs::write(d.path().join("empty.jsonl"), b"").unwrap();
    std::fs::write(d.path().join("other.jsonl"), b"{\"type\":\"title\",\"title\":\"x\"}\n").unwrap();
    let head = write_big_session(d.path(), 0, 0);
    let mut seen = 0;
    for p in [&plain, &reset, &cut, &head, &d.path().join("empty.jsonl"), &d.path().join("other.jsonl")] {
        assert_eq!(read_meta(p), read_meta_parsing_every_line(p), "{}", p.display());
        seen += 1;
    }
    assert_eq!(seen, 6);
    assert_eq!(read_meta(&plain).unwrap().messages, 6);
    assert_eq!(read_meta(&reset).unwrap().title, "later title");
    assert!(read_meta(&d.path().join("empty.jsonl")).is_none());
}

/// `NEXO_BENCH_DIR=<empty scratch folder> cargo test -p nexo-agent --release listing_bench -- --ignored --nocapture`
#[test]
#[ignore = "builds ~300 MB of files; a measurement, not a check"]
fn listing_bench() {
    let dir = PathBuf::from(std::env::var("NEXO_BENCH_DIR").expect("NEXO_BENCH_DIR names an empty scratch folder"));
    std::fs::create_dir_all(&dir).unwrap();
    if std::fs::read_dir(&dir).unwrap().next().is_none() {
        // 300 sessions of about 1 MB: 8 tool outputs of 120 KB each
        for _ in 0..300 {
            write_big_session(&dir, 8, 120_000);
        }
    }
    let t = std::time::Instant::now();
    let old: Vec<SessionMeta> =
        std::fs::read_dir(&dir).unwrap().flatten().filter_map(|e| read_meta_parsing_every_line(&e.path())).collect();
    let old_ms = t.elapsed().as_millis();
    let t = std::time::Instant::now();
    let new = list_in(&dir);
    let new_ms = t.elapsed().as_millis();
    let bytes: u64 =
        std::fs::read_dir(&dir).unwrap().flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum();
    println!("{} sessions, {} MB: every line parsed {old_ms} ms, by prefix {new_ms} ms", new.len(), bytes / 1_000_000);
    assert_eq!(old.len(), new.len());
}

fn asked_with_a_note(words: &str) -> Message {
    let mark = nexo_core::agent::PERSON_MARK;
    Message::user(format!("{START_NOTE_HEAD}\nGit: 2 files changed\n\n{mark}\n{words}"))
}

#[test]
fn a_new_session_is_titled_by_the_persons_words_never_by_the_start_note() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
    s.append(&asked_with_a_note("fix the parser")).unwrap();
    assert_eq!(s.meta().title, "fix the parser");
    assert_eq!(title_of_message(&format!("{START_NOTE_HEAD}\nGit: clean")), "", "a note alone is no title");
    assert_eq!(title_of_message("plain words"), "plain words", "a message without a note is read whole");
}

#[test]
fn a_request_with_no_words_of_the_persons_own_leaves_the_title_empty_until_there_are_some() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
    s.append(&Message::user(format!("{START_NOTE_HEAD}\nGit: clean\n\n{}\n", nexo_core::agent::PERSON_MARK))).unwrap();
    assert_eq!(s.meta().title, "");
    s.append(&Message::assistant("ok")).unwrap();
    s.append(&asked_with_a_note("now add tests")).unwrap();
    assert_eq!(s.meta().title, "now add tests");
}

#[test]
fn a_session_saved_with_the_start_note_as_its_title_shows_the_first_message_without_a_rewrite() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
    let id = s.id().to_string();
    s.append(&asked_with_a_note("explain the cache")).unwrap();
    s.set_title(START_NOTE_HEAD).unwrap();
    let before = std::fs::read(s.path()).unwrap();
    assert!(is_note_title(&s.meta().title));
    assert_eq!(meta_in(d.path(), &id).unwrap().title, "explain the cache", "the list shows the words");
    assert_eq!(Session::open_path(s.path()).unwrap().meta().title, "explain the cache");
    assert_eq!(std::fs::read(s.path()).unwrap(), before, "the file is left as it was");
    // a title the person chose is never replaced
    s.set_title("my own name").unwrap();
    assert_eq!(meta_in(d.path(), &id).unwrap().title, "my own name");
}

#[cfg(unix)]
#[test]
fn the_start_read_from_ps_is_the_same_on_every_call_and_differs_between_processes() {
    let me = std::process::id();
    let first = ps_start(me).expect("ps lists this process");
    assert_eq!(ps_start(me), Some(first));
    // a process that started later has another start text, unless both began in the same second
    let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
    let other = ps_start(child.id());
    let _ = child.kill();
    let _ = child.wait();
    assert!(other.is_some());
    assert_eq!(ps_state(me).map(|s| s.starts_with('T')), Some(false));
}

#[cfg(target_os = "macos")]
#[test]
fn a_lock_is_judged_by_the_start_time_and_boot_read_from_the_system_on_macos() {
    let me = std::process::id();
    assert_eq!(proc_start(me), ps_start(me));
    assert!(proc_start(me).is_some());
    assert!(boot_id().is_some());
    assert_eq!(boot_id(), boot_id());
}

#[cfg(unix)]
#[test]
fn the_start_read_from_ps_does_not_depend_on_the_locale_or_the_timezone() {
    let me = std::process::id();
    let fnv = |t: &str| {
        t.trim().bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
    };
    let printed = std::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &me.to_string()])
        .env("LC_ALL", "C")
        .env("TZ", "UTC0")
        .output()
        .unwrap();
    // the hash is that of the text in the C locale and UTC, which every nexo gets, whatever its own settings
    assert_eq!(ps_start(me), Some(fnv(&String::from_utf8_lossy(&printed.stdout))));
}

/// The `ts` of every message line in the session file, in order.
fn message_stamps(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|l| match serde_json::from_str::<Line>(l) {
            Ok(Line::Message { ts, .. }) => Some(ts),
            _ => None,
        })
        .collect()
}

#[test]
fn a_rewind_keeps_the_time_each_kept_message_was_said() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append_at(&Message::user("first"), "2026-01-01T10:00:00Z".into()).unwrap();
    s.append_at(&Message::assistant("second"), "2026-01-01T11:00:00Z".into()).unwrap();
    s.rewind(1).unwrap();
    assert_eq!(message_stamps(s.path()), ["2026-01-01T10:00:00Z"]);
    s.append(&Message::assistant("again")).unwrap();
    assert_eq!(message_stamps(s.path())[0], "2026-01-01T10:00:00Z", "a later rewrite still keeps it");
    let p = s.path().to_path_buf();
    drop(s);
    let mut back = Session::open_path(&p).unwrap();
    back.rewind(2).unwrap();
    assert_eq!(message_stamps(&p)[0], "2026-01-01T10:00:00Z", "a reopened session keeps the times it read");
}

#[test]
fn a_compaction_keeps_the_time_of_each_message_it_keeps_and_stamps_the_summary_now() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    for (i, text) in ["one", "two", "three", "four"].iter().enumerate() {
        let m = if i % 2 == 0 { Message::user(*text) } else { Message::assistant(*text) };
        s.append_at(&m, format!("2026-01-01T1{i}:00:00Z")).unwrap();
    }
    // the summary is inserted in front of the kept tail
    s.reset_with(&[Message::user("a summary of one and two"), Message::user("three"), Message::assistant("four")])
        .unwrap();
    let stamps = message_stamps(s.path());
    assert_ne!(stamps[0], "2026-01-01T10:00:00Z", "the summary is new");
    assert_eq!(stamps[1..], ["2026-01-01T12:00:00Z", "2026-01-01T13:00:00Z"]);
    // the summary is spliced into the first kept message: that one changed, the ones after it did not
    s.reset_with(&[Message::user("a summary of three"), Message::assistant("four")]).unwrap();
    let again = message_stamps(s.path());
    assert_ne!(again[0], "2026-01-01T12:00:00Z");
    assert_eq!(again[1], "2026-01-01T13:00:00Z");
    // nothing changed: every time stays
    s.reset_with(&[Message::user("a summary of three"), Message::assistant("four")]).unwrap();
    assert_eq!(message_stamps(s.path()), again);
}

#[test]
fn a_fork_keeps_the_time_each_copied_message_was_said() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append_at(&Message::user("first"), "2026-01-01T10:00:00Z".into()).unwrap();
    s.append_at(&Message::assistant("second"), "2026-01-01T11:00:00Z".into()).unwrap();
    let f = s.fork(2).unwrap();
    assert_eq!(message_stamps(f.path()), ["2026-01-01T10:00:00Z", "2026-01-01T11:00:00Z"]);
}

#[test]
fn a_trashed_session_takes_its_task_list_along_and_a_restore_brings_it_back() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("keep me")).unwrap();
    let id = s.id().to_string();
    drop(s);
    let live = nexo_tools::todo::file_in(d.path(), &id);
    let binned = nexo_tools::todo::file_in(&trash_dir_in(d.path()), &id);
    std::fs::write(&live, "[]").unwrap();
    trash_in(d.path(), &id).unwrap();
    assert!(!live.exists(), "no orphan list stays in the live folder");
    assert!(binned.is_file());
    restore_in(d.path(), &id).unwrap();
    assert!(live.is_file() && !binned.exists());
}

#[test]
fn a_fork_of_the_whole_conversation_copies_the_task_list_and_a_shorter_one_does_not() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    for t in ["one", "two", "three"] {
        s.append(&Message::user(t)).unwrap();
    }
    std::fs::write(nexo_tools::todo::file_in(d.path(), s.id()), r#"[{"text":"a","status":"done"}]"#).unwrap();
    let whole = s.fork(3).unwrap();
    let copied = nexo_tools::todo::file_in(d.path(), whole.id());
    assert_eq!(std::fs::read_to_string(&copied).unwrap(), r#"[{"text":"a","status":"done"}]"#);
    let part = s.fork(2).unwrap();
    assert!(!nexo_tools::todo::file_in(d.path(), part.id()).exists(), "the list may not match fewer messages");
    let leftovers = std::fs::read_dir(d.path())
        .unwrap()
        .filter(|e| e.as_ref().is_ok_and(|e| e.file_name().to_string_lossy().ends_with(".tmp")));
    assert_eq!(leftovers.count(), 0);
}

#[test]
fn a_rewind_past_a_task_list_call_removes_the_list_saved_for_it_and_one_before_the_call_keeps_it() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
    s.append(&Message::user("go")).unwrap();
    s.append(&Message::new(
        Role::Assistant,
        vec![Part::tool_call("c1", "todo", serde_json::json!({"items": [{"text": "a"}]}))],
    ))
    .unwrap();
    let file = nexo_tools::todo::file_in(d.path(), s.id());
    std::fs::write(&file, r#"[{"text":"a","status":"pending"}]"#).unwrap();
    // cutting where no list call goes leaves the file
    s.rewind(2).unwrap();
    assert!(file.is_file());
    s.rewind(1).unwrap();
    assert!(!file.exists(), "the call that made the list is gone");
}

#[test]
fn a_session_that_cannot_be_opened_names_its_file_and_the_cause() {
    let d = tempfile::tempdir().unwrap();
    let missing = d.path().join("abc123.jsonl");
    let e = Session::open_path(&missing).err().expect("a missing file").to_string();
    assert!(e.contains("no session abc123") && e.contains(&missing.display().to_string()), "{e}");
    let bad = d.path().join("broken.jsonl");
    std::fs::write(&bad, "this is not json\n").unwrap();
    let e = Session::open_path(&bad).err().expect("a damaged file").to_string();
    assert!(
        e.contains(&bad.display().to_string()) && e.contains("damaged at line 1") && !e.contains("/new"),
        "the next step belongs to the caller: {e}"
    );
    let dir_as_file = d.path().join("folder.jsonl");
    std::fs::create_dir(&dir_as_file).unwrap();
    let e = Session::open_path(&dir_as_file).err().expect("an unreadable file").to_string();
    assert!(e.contains(&dir_as_file.display().to_string()) && e.contains("cannot be read"), "{e}");
}

#[cfg(windows)]
#[test]
fn a_fresh_lock_of_a_process_that_has_exited_is_stale_on_windows() {
    let d = tempfile::tempdir().unwrap();
    let lock = d.path().join("x.jsonl.lock");
    let mut child = std::process::Command::new("cmd").args(["/C", "exit"]).spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    drop(child);
    // just written, so only the pid can tell that nobody holds it
    std::fs::write(&lock, pid.to_string()).unwrap();
    assert_eq!(holder(&lock), None);
    // this process runs, and its own stamp (with the start time) names it
    std::fs::write(&lock, stamp()).unwrap();
    assert_eq!(holder(&lock), Some(Some(std::process::id())));
    assert!(proc_start(std::process::id()).is_some(), "Windows tells when a process started");
}
