use super::*;
use serde_json::json;
use std::io::Write;

#[test]
fn the_program_is_named_by_the_hidden_folder_the_logs_sit_in() {
    let name = |p: &str| JsonlSource::new(p, 12).program();
    assert_eq!(name("/home/u/.othertool/projects"), "othertool");
    assert_eq!(name("/data/logs"), "external");
    assert_eq!(name("/home/u/projects"), "external");
}

fn append(path: &Path, lines: &[Value]) {
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

fn setup() -> (tempfile::TempDir, PathBuf, JsonlSource) {
    let dir = tempfile::tempdir().unwrap();
    let proj = dir.path().join("-work-app");
    std::fs::create_dir_all(&proj).unwrap();
    let log = proj.join("sess-1.jsonl");
    let src = JsonlSource::new(dir.path(), 12);
    (dir, log, src)
}

fn now() -> Secs {
    super::super::run::now_secs()
}

fn assistant_tool(id: &str, name: &str, input: Value, ts: &str) -> Value {
    json!({"type":"assistant","timestamp":ts,"cwd":"/work/app","message":{"stop_reason":"tool_use",
        "content":[{"type":"tool_use","id":id,"name":name,"input":input}]}})
}

fn kinds(obs: &[Observation]) -> Vec<ObsKind> {
    obs.iter().map(|o| o.kind.clone()).collect()
}

#[test]
fn it_reads_what_grows_and_only_that() {
    let (_d, log, mut src) = setup();
    append(&log, &[json!({"type":"custom-title","customTitle":"Builder"})]);
    // the first poll reads what is there
    let first = src.poll(now());
    assert!(kinds(&first).iter().any(|k| matches!(k, ObsKind::Meta { name: Some(n), .. } if n == "Builder")));
    assert!(first.iter().all(|o| o.agent_key == "sess-1"));
    assert!(src.poll(now()).is_empty(), "nothing new, nothing returned");
    append(
        &log,
        &[
            json!({"type":"permission-mode","permissionMode":"bypassPermissions"}),
            assistant_tool("t1", "Bash", json!({"command":"cargo test"}), "2026-11-02T11:19:28.322Z"),
        ],
    );
    let k = kinds(&src.poll(now()));
    assert!(k.contains(&ObsKind::ToolPending { name: "Bash".into() }));
    assert!(k.contains(&ObsKind::Step { text: "Bash  cargo test".into() }));
    assert!(k.iter().any(|k| matches!(k, ObsKind::Meta { permission_mode: Some(m), .. } if m == "bypassPermissions")));
    append(
        &log,
        &[json!({"type":"user","timestamp":"2026-11-02T11:19:40Z","message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"boom"}]}})],
    );
    let k = kinds(&src.poll(now()));
    // a failed test run is an event of its own, then the result
    assert_eq!(k.len(), 2, "{k:?}");
    assert!(matches!(&k[0], ObsKind::Ev(e) if e.kind == EvKind::TestsFail));
    assert!(matches!(&k[1], ObsKind::ToolResult { name, is_error: true, .. } if name == "Bash"));
}

#[test]
fn a_split_line_waits_for_its_end() {
    let (_d, log, mut src) = setup();
    append(&log, &[json!({"type":"progress","timestamp":"2026-11-02T11:00:00Z"})]);
    assert_eq!(src.poll(now()).len(), 1);
    let line =
        json!({"type":"user","timestamp":"2026-11-02T11:05:00Z","message":{"role":"user","content":"hi"}}).to_string();
    let (a, b) = line.split_at(20);
    let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
    write!(f, "{a}").unwrap();
    assert!(src.poll(now() + 6).is_empty(), "half a line is not read");
    writeln!(f, "{b}").unwrap();
    let obs = src.poll(now() + 6);
    assert_eq!(kinds(&obs), vec![ObsKind::UserInput]);
    assert_eq!(obs[0].at, 1_793_617_500);
}

#[test]
fn a_shrunk_file_is_read_again_from_its_last_compaction() {
    let (_d, log, mut src) = setup();
    let old: Vec<Value> = (0..30)
        .map(|i| {
            assistant_tool(&format!("o{i}"), "Read", json!({"file_path": format!("/old/{i}")}), "2026-11-02T10:00:00Z")
        })
        .collect();
    append(&log, &old);
    assert!(!src.poll(now()).is_empty());
    // rewritten shorter: a mark, then two new records
    std::fs::write(&log, "").unwrap();
    append(
        &log,
        &[
            assistant_tool("x", "Read", json!({"file_path":"/before-mark"}), "2026-11-02T11:00:00Z"),
            json!({"type":"system","subtype":"compact_boundary","timestamp":"2026-11-02T11:01:00Z"}),
            assistant_tool("n1", "Edit", json!({"file_path":"/after-mark"}), "2026-11-02T11:02:00Z"),
        ],
    );
    // the shrink may need a second poll only when the new file is not shorter; here it is
    let obs = src.poll(now());
    let steps: Vec<String> = kinds(&obs)
        .into_iter()
        .filter_map(|k| if let ObsKind::Step { text } = k { Some(text) } else { None })
        .collect();
    assert_eq!(steps, vec!["Edit  /after-mark".to_string()], "{obs:?}");
}

#[test]
fn sidechains_and_machine_text_are_activity_and_a_person_clears_the_turn() {
    let (_d, log, mut src) = setup();
    append(
        &log,
        &[
            json!({"type":"assistant","isSidechain":true,"timestamp":"2026-11-02T11:00:00Z","message":{"content":[]}}),
            json!({"type":"user","origin":{"kind":"task-notification"},"timestamp":"2026-11-02T11:00:01Z","message":{"content":"x"}}),
            json!({"type":"user","origin":{"kind":"human"},"timestamp":"2026-11-02T11:00:02Z","message":{"content":"hello"}}),
            json!({"type":"assistant","timestamp":"2026-11-02T11:00:03Z","message":{"stop_reason":"end_turn",
                "content":[{"type":"text","text":"All done. Shall I continue?"}]}}),
        ],
    );
    let k = kinds(&src.poll(now()));
    assert_eq!(k[0], ObsKind::Activity);
    assert_eq!(k[1], ObsKind::Activity);
    assert_eq!(k[2], ObsKind::UserInput);
    assert!(k.contains(&ObsKind::TurnEnd { last_text_tail: "All done. Shall I continue?".into() }));
}

#[test]
fn a_scheduled_task_marks_a_loop_and_error_records_count() {
    let (_d, log, mut src) = setup();
    append(
        &log,
        &[
            json!({"type":"system","subtype":"scheduled_task_fire","cron":"7,22,37,52 * * * *","timestamp":"2026-11-02T11:00:00Z"}),
            json!({"type":"system","subtype":"api_error","level":"error","timestamp":"2026-11-02T11:00:01Z"}),
        ],
    );
    let k = kinds(&src.poll(now()));
    assert!(k.contains(&ObsKind::Loop { cron_secs: 900 }));
    assert!(k.contains(&ObsKind::SystemError));
}

fn wrapper(from: &str, name: &str, body: &str) -> String {
    format!(
        "<cross-session-message from=\"{from}\" from-name=\"{name}\" from-mode=\"ask\">{body}</cross-session-message>"
    )
}

#[test]
fn a_message_of_another_session_is_a_mail_also_beside_a_tool_result() {
    let (_d, log, mut src) = setup();
    append(&log, &[assistant_tool("t1", "Bash", json!({"command":"ls"}), "2026-11-02T11:00:00Z")]);
    src.poll(now());
    append(
        &log,
        &[
            // the wrapper sits next to the result of a tool in one record
            json!({"type":"user","timestamp":"2026-11-02T11:00:05Z","message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":"t1","content":"ok"},
                {"type":"text","text": wrapper("pid-77", "api", "the build is red\nsecond line")}]}}),
            // and alone, as a string: not the person
            json!({"type":"user","timestamp":"2026-11-02T11:00:09Z","message":{"role":"user","content":
                wrapper("pid-78", "web", "ping")}}),
        ],
    );
    let obs = src.poll(now());
    let k = kinds(&obs);
    assert!(
        k.contains(&ObsKind::MsgIn { from: "api".into(), mail: None, summary: "the build is red".into(), held: false }),
        "{k:?}"
    );
    assert!(k.contains(&ObsKind::MsgIn { from: "web".into(), mail: None, summary: "ping".into(), held: false }));
    assert!(
        k.iter().any(|k| matches!(k, ObsKind::ToolResult { name, .. } if name == "Bash")),
        "the result still counts"
    );
    assert!(!k.contains(&ObsKind::UserInput), "a mail is not the person");
}

#[test]
fn a_send_message_call_is_a_mail_out_and_secrets_stay_out_of_its_summary() {
    let (_d, log, mut src) = setup();
    let key = format!("{}{}", "s", "k-abcdefghijklmnopqrstuvwx");
    append(
        &log,
        &[assistant_tool(
            "t1",
            "SendMessage",
            json!({"to":"api","summary":format!("use {key} now"),"message":"x".repeat(300)}),
            "2026-11-02T11:00:00Z",
        )],
    );
    let k = kinds(&src.poll(now()));
    let out = k.iter().find_map(|k| {
        if let ObsKind::MsgOut { to, summary, bytes, .. } = k {
            Some((to, summary, bytes))
        } else {
            None
        }
    });
    let (to, summary, bytes) = out.expect("a mail out");
    assert_eq!((to.as_str(), *bytes), ("api", 300));
    assert!(!summary.contains(&key), "{summary}");
}

#[test]
fn tests_commits_todo_and_compaction_become_events() {
    let (_d, log, mut src) = setup();
    let result = |id: &str, text: &str, err: bool, ts: &str| {
        json!({"type":"user","timestamp":ts,"message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":id,"is_error":err,"content":text}]}})
    };
    append(
        &log,
        &[
            assistant_tool("a", "Bash", json!({"command":"cargo test -p x"}), "2026-11-02T11:00:00Z"),
            result("a", "test result: ok. 12 passed; 0 failed", false, "2026-11-02T11:00:01Z"),
            assistant_tool("b", "Bash", json!({"command":"git commit -m wip"}), "2026-11-02T11:00:02Z"),
            result("b", "[main 1234] wip", false, "2026-11-02T11:00:03Z"),
            assistant_tool("c", "Bash", json!({"command":"pytest"}), "2026-11-02T11:00:04Z"),
            result("c", "3 failed, 4 passed", true, "2026-11-02T11:00:05Z"),
            assistant_tool(
                "d",
                "TodoWrite",
                json!({"todos":[{"status":"completed"},{"status":"pending"},{"status":"completed"}]}),
                "2026-11-02T11:00:06Z",
            ),
            json!({"type":"system","subtype":"compact_boundary","timestamp":"2026-11-02T11:00:07Z"}),
        ],
    );
    let evs: Vec<Ev> =
        src.poll(now()).into_iter().filter_map(|o| if let ObsKind::Ev(e) = o.kind { Some(e) } else { None }).collect();
    let want = [
        (EvKind::TestsPass, EvDetail::Tests { pass: 12, fail: 0 }),
        (EvKind::GitCommit, EvDetail::None),
        (EvKind::TestsFail, EvDetail::Tests { pass: 4, fail: 3 }),
        (EvKind::Todo, EvDetail::Progress { done: 2, total: 3 }),
        (EvKind::Compact, EvDetail::None),
    ];
    let got: Vec<(EvKind, EvDetail)> = evs.into_iter().map(|e| (e.kind, e.detail)).collect();
    assert_eq!(got, want);
}

#[test]
fn cron_periods() {
    assert_eq!(cron_period("*/10 * * * *"), 600);
    assert_eq!(cron_period("7,22,37,52 * * * *"), 900);
    assert_eq!(cron_period("41 0,5,10,15,20 * * *"), 5 * 3600);
    assert_eq!(cron_period("nonsense"), 3600);
}

#[test]
fn old_logs_and_other_files_are_ignored_and_a_big_log_is_read_from_its_end() {
    let dir = tempfile::tempdir().unwrap();
    let proj = dir.path().join("p");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("notes.txt"), "x").unwrap();
    let big = proj.join("big.jsonl");
    let mut body = String::new();
    for i in 0..6000 {
        body.push_str(&format!("{}\n", json!({"type":"progress","n":i,"pad":"x".repeat(60)})));
    }
    std::fs::write(&big, body).unwrap();
    let mut src = JsonlSource::new(dir.path(), 12);
    let obs = src.poll(now());
    assert!(!obs.is_empty() && obs.len() < 6000, "{}", obs.len());
    // far in the future the file is older than the window and is never picked up
    let mut late = JsonlSource::new(dir.path(), 12);
    assert!(late.poll(now() + 13 * 3600).is_empty());
}

fn steps(obs: &[Observation]) -> Vec<String> {
    kinds(obs).into_iter().filter_map(|k| if let ObsKind::Step { text } = k { Some(text) } else { None }).collect()
}

#[test]
fn a_half_written_last_line_and_a_call_without_result_leave_the_state_as_it_was() {
    let (_d, log, mut src) = setup();
    append(&log, &[assistant_tool("t1", "Bash", json!({"command":"cargo build"}), "2026-11-02T11:00:00Z")]);
    let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
    // the writer is in the middle of the next record, and of its string
    write!(
        f,
        "{{\"type\":\"user\",\"timestamp\":\"2026-11-02T11:00:09Z\",\"message\":{{\"content\":[{{\"type\":\"tool_res"
    )
    .unwrap();
    let k = kinds(&src.poll(now()));
    assert!(k.contains(&ObsKind::ToolPending { name: "Bash".into() }));
    assert!(!k.iter().any(|k| matches!(k, ObsKind::ToolResult { .. } | ObsKind::UserInput)), "{k:?}");
    // nothing more for as long as the line is open, however often we look
    for _ in 0..3 {
        assert!(src.poll(now() + 6).is_empty());
    }
    // a line that ends but is not JSON is skipped, the next one still reads
    writeln!(f, "ult\" this is broken").unwrap();
    append(&log, &[json!({"type":"progress","timestamp":"2026-11-02T11:00:10Z"})]);
    assert_eq!(kinds(&src.poll(now() + 6)), vec![ObsKind::Activity]);
}

// only unix can tell a replaced file from one that grew (see file_id)
#[cfg(unix)]
#[test]
fn a_log_replaced_by_a_longer_one_is_read_from_its_new_start() {
    let (d, log, mut src) = setup();
    append(&log, &[assistant_tool("a", "Read", json!({"file_path":"/old"}), "2026-11-02T11:00:00Z")]);
    assert_eq!(steps(&src.poll(now())), vec!["Read  /old".to_string()]);
    // a new file takes the path: larger than the old one was, so a length check alone would miss it
    let fresh = d.path().join("fresh.tmp");
    let lines: Vec<Value> = (0..5)
        .map(|i| {
            assistant_tool(&format!("n{i}"), "Edit", json!({"file_path": format!("/new/{i}")}), "2026-11-02T12:00:00Z")
        })
        .collect();
    append(&fresh, &lines);
    std::fs::rename(&fresh, &log).unwrap();
    let got = steps(&src.poll(now()));
    assert_eq!(got.len(), 5, "{got:?}");
    assert_eq!(got[0], "Edit  /new/0");
}

#[cfg(unix)]
#[test]
fn a_rotated_big_log_is_read_from_its_end_not_whole() {
    let (d, log, mut src) = setup();
    append(&log, &[json!({"type":"progress"})]);
    src.poll(now());
    let fresh = d.path().join("fresh.tmp");
    let mut body = String::new();
    for i in 0..8000 {
        body.push_str(&format!(
            "{}\n",
            assistant_tool(&format!("t{i}"), "Read", json!({"file_path": format!("/f/{i}")}), "2026-11-02T12:00:00Z")
        ));
    }
    assert!(body.len() as u64 > 2 * CHUNK);
    std::fs::write(&fresh, body).unwrap();
    std::fs::rename(&fresh, &log).unwrap();
    let got = steps(&src.poll(now()));
    assert!(!got.is_empty() && got.len() < 8000, "{}", got.len());
    assert_eq!(got.last().unwrap(), "Read  /f/7999");
}

#[test]
fn a_record_dated_in_the_future_counts_as_now() {
    let (_d, log, mut src) = setup();
    append(&log, &[assistant_tool("t1", "Bash", json!({"command":"ls"}), "2099-01-01T00:00:00Z")]);
    let t = now();
    let obs = src.poll(t);
    assert!(!obs.is_empty() && obs.iter().all(|o| o.at <= t), "{obs:?}");
}

#[test]
fn a_log_nobody_touched_for_the_whole_window_is_let_go() {
    let (_d, log, mut src) = setup();
    append(&log, &[json!({"type":"progress"})]);
    src.poll(now());
    assert_eq!(src.files.len(), 1);
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(20 * 3600);
    std::fs::File::options().write(true).open(&log).unwrap().set_modified(old).unwrap();
    src.poll(now() + 10);
    assert!(src.files.is_empty());
    // and a file that is deleted is let go too
    append(&log, &[json!({"type":"progress"})]);
    src.poll(now() + 20);
    assert_eq!(src.files.len(), 1);
    std::fs::remove_file(&log).unwrap();
    src.poll(now() + 21);
    assert!(src.files.is_empty());
}

#[test]
fn what_a_step_shows_is_short_plain_and_free_of_secrets() {
    let (_d, log, mut src) = setup();
    let gh = format!("{}{}", "gh", "p_abcdefghijklmnopqrstuvwxyz0123456789");
    let sk = format!("{}{}", "s", "k-abcdefghijklmnopqrstuvwx");
    append(
        &log,
        &[
            assistant_tool(
                "a",
                "Bash",
                json!({"command": format!("git clone https://{gh}@example.test/r.git")}),
                "2026-11-02T11:00:00Z",
            ),
            assistant_tool(
                "b",
                "Bash",
                json!({"command": "curl -H 'Authorization: Bearer abcdef0123456789xyz' https://example.test"}),
                "2026-11-02T11:00:01Z",
            ),
            assistant_tool("c", "Bash", json!({"command": "{\"a\":1,\"b\":[2,3]}"}), "2026-11-02T11:00:02Z"),
            assistant_tool(
                "d",
                "Bash\u{7}\nweird",
                json!({"command": format!("export KEY={sk}")}),
                "2026-11-02T11:00:03Z",
            ),
            assistant_tool("e", "Read", json!({"file_path": "x".repeat(900)}), "2026-11-02T11:00:04Z"),
            json!({"type":"assistant","timestamp":"2026-11-02T11:00:05Z","message":{"stop_reason":"end_turn",
                "content":[{"type":"text","text":format!("{{\"status\":\"ok\",\"token\":\"{sk}\"}}")}]}}),
        ],
    );
    let obs = src.poll(now());
    let got = steps(&obs);
    assert_eq!(got.len(), 5, "{got:?}");
    for s in &got {
        assert!(!s.contains(&gh) && !s.contains(&sk) && !s.contains("abcdef0123456789xyz"), "{s}");
        assert!(!s.chars().any(char::is_control), "{s:?}");
        assert!(s.chars().count() <= 200, "{}", s.chars().count());
    }
    assert_eq!(got[2], "Bash", "a JSON document is not a step: {:?}", got[2]);
    assert!(got[3].starts_with("Bash weird"), "{:?}", got[3]);
    // the end of the turn keeps no key, and no raw JSON as its step
    let tail = kinds(&obs).into_iter().find_map(|k| {
        if let ObsKind::TurnEnd { last_text_tail } = k {
            Some(last_text_tail)
        } else {
            None
        }
    });
    assert!(!tail.unwrap().contains(&sk));
}

#[test]
fn a_secret_cut_by_the_tail_does_not_leave_half_a_key() {
    let (_d, log, mut src) = setup();
    let sk = format!("{}{}", "s", "k-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ");
    // the key starts 20 characters before the 300 character cut
    let text = format!("{} {sk}{}", "w".repeat(10), "z".repeat(TAIL - 20 - 8));
    append(
        &log,
        &[json!({"type":"assistant","timestamp":"2026-11-02T11:00:05Z","message":{"stop_reason":"end_turn",
            "content":[{"type":"text","text":text}]}})],
    );
    let tail = kinds(&src.poll(now()))
        .into_iter()
        .find_map(|k| if let ObsKind::TurnEnd { last_text_tail } = k { Some(last_text_tail) } else { None })
        .unwrap();
    assert!(!tail.contains("abcdefghij") && !tail.contains("k-abc"), "{tail}");
}

#[test]
fn a_cron_line_made_of_nonsense_does_not_panic() {
    for c in [
        "5,70 * * * *",
        "99,1 * * * *",
        "* 30,40 * * *",
        "*/0 * * * *",
        "*/99999999999999999999 * * * *",
        ",, * * * *",
        "1,2,3,4,5,6,7,8,9 99999999999 * * *",
    ] {
        let _ = cron_period(c);
    }
}

#[test]
fn many_sessions_and_a_huge_log_stay_cheap() {
    use std::time::Instant;
    let dir = tempfile::tempdir().unwrap();
    let proj = dir.path().join("p");
    std::fs::create_dir_all(&proj).unwrap();
    // 200 sessions with a few hundred records each
    for i in 0..200 {
        let lines: Vec<Value> = (0..150)
            .map(|n| {
                assistant_tool(
                    &format!("t{n}"),
                    "Read",
                    json!({"file_path": format!("/s{i}/f{n}")}),
                    "2026-11-02T11:00:00Z",
                )
            })
            .collect();
        append(&proj.join(format!("s{i}.jsonl")), &lines);
    }
    // and one log of 100 MB
    let big = proj.join("big.jsonl");
    {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&big).unwrap());
        let block: String = (0..400)
            .map(|n| {
                format!(
                    "{}\n",
                    assistant_tool(
                        &format!("b{n}"),
                        "Edit",
                        json!({"file_path": format!("/big/{n}")}),
                        "2026-11-02T11:00:00Z"
                    )
                )
            })
            .collect();
        let mut written = 0usize;
        while written < 100 * 1024 * 1024 {
            f.write_all(block.as_bytes()).unwrap();
            written += block.len();
        }
    }
    let mut src = JsonlSource::new(dir.path(), 12);
    let t = Instant::now();
    let first = src.poll(now());
    let first_took = t.elapsed();
    assert!(first.len() > 200 * 150, "{}", first.len());
    // nothing happens: a poll only looks at the files' sizes
    let t = Instant::now();
    for _ in 0..5 {
        assert!(src.poll(now()).is_empty());
    }
    let quiet = t.elapsed();
    // one line lands on the big log: the cost is that line, not the file
    append(&big, &[assistant_tool("last", "Edit", json!({"file_path":"/big/last"}), "2026-11-02T11:00:01Z")]);
    let t = Instant::now();
    let got = steps(&src.poll(now()));
    let tail_took = t.elapsed();
    assert_eq!(got, vec!["Edit  /big/last".to_string()]);
    // generous bounds, for a loaded machine and a build without optimisation
    assert!(first_took.as_secs_f64() < 30.0, "first {first_took:?}");
    assert!(quiet.as_secs_f64() < 2.0, "quiet {quiet:?}");
    assert!(tail_took.as_secs_f64() < 1.0, "tail {tail_took:?}");
}
