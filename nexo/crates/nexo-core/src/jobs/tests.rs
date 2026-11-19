use super::*;

#[test]
fn the_tails_show_the_end_of_each_output_without_marking_it_read() {
    let jobs = Jobs::new();
    let h = jobs.start(JobKind::Shell, "make");
    for i in 0..(TAIL_LINES + 5) {
        h.line(&format!("l{i}"));
    }
    let t = jobs.tails();
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].ended_ago_ms, None);
    assert_eq!(t[0].text.lines().count(), TAIL_LINES);
    assert!(t[0].text.ends_with(&format!("l{}", TAIL_LINES + 4)));
    h.finish(true, "exit 0");
    assert!(jobs.tails()[0].ended_ago_ms.is_some());
    assert_eq!(jobs.untold().len(), 1, "a look is not a read");
}

#[tokio::test]
async fn a_job_is_read_once_waited_for_and_told_once() {
    let jobs = Jobs::new();
    let mut changes = jobs.changes();
    let h = jobs.start(JobKind::Shell, "make");
    assert_eq!(changes.recv().await.unwrap().0.status, JobStatus::Running);
    h.line("one");
    h.line("two");
    let c = CancellationToken::new();
    let r = jobs.read(1, Duration::ZERO, &c).await.unwrap();
    assert_eq!(r.new, "one\ntwo\n");
    assert!(r.info.status.running());
    assert_eq!(jobs.read(1, Duration::ZERO, &c).await.unwrap().new, "", "read once");
    assert!(jobs.take_ended().is_empty());
    let t = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        h.line("three");
        h.finish(true, "exit 0");
    });
    let t0 = Instant::now();
    let r = jobs.read(1, Duration::from_secs(10), &c).await.unwrap();
    assert!(t0.elapsed() < Duration::from_secs(5), "returns when the job ends");
    assert_eq!(r.new, "three\n");
    assert_eq!(r.info.status, JobStatus::Done { ok: true, how: "exit 0".into() });
    t.await.unwrap();
    assert!(jobs.take_ended().is_empty(), "the read told the model");
    let h2 = jobs.start(JobKind::Agent, "find the parser");
    h2.finish(false, "error");
    let ended = jobs.take_ended();
    assert_eq!(ended.len(), 1);
    assert!(ended_note(&ended).contains("job 2 sub-agent \"find the parser\" failed (error)"));
    assert!(jobs.take_ended().is_empty(), "told once");
    assert!(jobs.read(9, Duration::ZERO, &c).await.is_none());
}

#[test]
fn the_ended_note_says_done_for_a_job_that_went_well_and_failed_for_one_that_did_not() {
    let jobs = Jobs::new();
    jobs.start(JobKind::Agent, "look").finish(true, "reported");
    jobs.start(JobKind::Agent, "wf").finish(false, "reported an error");
    jobs.start(JobKind::Shell, "make").finish(false, "exit 2");
    let note = ended_note(&jobs.take_ended());
    assert!(note.contains("sub-agent \"look\" done (reported) after"), "{note}");
    assert!(note.contains("sub-agent \"wf\" failed (reported an error) after"), "{note}");
    assert!(note.contains("`make` failed (exit 2) after"), "{note}");
}

#[tokio::test]
async fn killing_cancels_and_the_end_of_the_session_stops_everything() {
    let jobs = Jobs::new();
    let h = jobs.start(JobKind::Shell, "sleep");
    let cancel = h.cancel();
    assert!(jobs.kill(1));
    assert!(cancel.is_cancelled());
    h.finish(true, "exit 0");
    assert_eq!(jobs.get(1).unwrap().status, JobStatus::Killed, "a killed job stays killed");
    assert!(!jobs.kill(1));
    assert!(jobs.take_ended().is_empty(), "the model killed it: no notice");

    let h = jobs.start(JobKind::Shell, "server");
    let cancel = h.cancel();
    drop(jobs);
    assert!(cancel.is_cancelled(), "the session ended");
    drop(h);
}

#[test]
fn long_output_keeps_the_end() {
    let jobs = Jobs::new();
    let h = jobs.start(JobKind::Shell, "yes");
    for i in 0..50_000 {
        h.line(&format!("line {i}"));
    }
    let info = jobs.get(1).unwrap();
    assert_eq!(info.lines, 50_000);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let c = CancellationToken::new();
    let r = rt.block_on(jobs.read(1, Duration::ZERO, &c)).unwrap();
    assert!(r.new.starts_with('['), "{}", &r.new[..60]);
    assert!(
        r.new.contains("unread, the last of them: line 49999;"),
        "the first read says how it ends: {}",
        &r.new[r.new.len() - 200..]
    );
    assert!(r.new.chars().count() < READ_CHARS + 300);
    // the tail arrives in full on the last read
    let mut last = r.new;
    while last.contains(" more lines (") {
        last = rt.block_on(jobs.read(1, Duration::ZERO, &c)).unwrap().new;
    }
    assert!(last.trim_end().ends_with("line 49999"), "{}", &last[last.len().saturating_sub(100)..]);
    drop(h);
    assert_eq!(jobs.get(1).unwrap().status, JobStatus::Killed, "a dropped runner ends the job");
    assert_eq!(duration(65_000), "1 min 5 s");
}

#[test]
fn a_long_output_is_read_in_pages_and_nothing_is_skipped() {
    let jobs = Jobs::new();
    let h = jobs.start(JobKind::Shell, "make");
    // 3 000 lines of 40 characters: 120 000 characters, under KEEP_CHARS
    for i in 1..=3000 {
        if i == 1500 {
            h.line(&format!("FIRST ERROR{}", "x".repeat(40 - 11)));
        } else {
            h.line(&format!("{i:0>5}{}", "-".repeat(35)));
        }
    }
    h.finish(true, "exit 0");
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let c = CancellationToken::new();
    let first = rt.block_on(jobs.read(1, Duration::ZERO, &c)).unwrap();
    assert!(first.new.contains(" more lines ("), "{}", &first.new[first.new.len() - 250..]);
    assert!(first.new.contains(" chars) unread"));
    assert!(first.new.contains("the last of them: 03000"), "{}", &first.new[first.new.len() - 250..]);
    assert!(!first.new.contains("characters omitted"));
    assert_eq!(jobs.untold().len(), 1, "a job read in part is not yet told");
    let mut all = String::new();
    let mut read = first;
    loop {
        let mut text = read.new;
        let more = text.contains(" more lines (");
        if more {
            let at = text.rfind('[').unwrap();
            text.truncate(at);
        }
        all.push_str(&text);
        if !more {
            break;
        }
        read = rt.block_on(jobs.read(1, Duration::ZERO, &c)).unwrap();
    }
    let lines: Vec<&str> = all.lines().collect();
    assert_eq!(lines.len(), 3000, "every line once");
    assert!(lines[1499].starts_with("FIRST ERROR"));
    assert!(lines[0].starts_with("00001") && lines[2999].starts_with("03000"));
    assert!(jobs.untold().is_empty() && jobs.take_ended().is_empty(), "told once all of it was read");
}

#[test]
fn one_huge_line_is_kept_bounded() {
    let jobs = Jobs::new();
    let h = jobs.start(JobKind::Shell, "yes | tr -d '\\n'");
    h.line(&"y".repeat(LINE_CHARS * 10));
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let r = rt.block_on(jobs.read(1, Duration::ZERO, &CancellationToken::new())).unwrap();
    assert!(r.new.starts_with(&"y".repeat(100)));
    assert!(r.new.trim_end().ends_with(&format!("[{} characters of this line cut]", LINE_CHARS * 9)));
    assert!(r.new.chars().count() < LINE_CHARS + 100);
    drop(h);
}

#[test]
fn a_run_that_ended_before_the_call_let_go_leaves_nothing_marked() {
    let d = Detach::default();
    d.begin("t1#1", false);
    assert_eq!(d.attached(), 1);
    // it ends in the same moment the person writes: the call must take the report, not the "moved" text
    d.end("t1#1");
    assert!(!d.to_background("t1#1"), "an ended run cannot be let go of");
    assert!(!d.is_gone("t1#1.s1"));
    assert_eq!(d.attached(), 0);
}

#[test]
fn a_question_is_in_the_background_only_for_its_own_run() {
    let d = Detach::default();
    // two runs of calls that share a provider id
    d.begin("call_0#1", true);
    d.begin("call_0#2", false);
    assert!(d.is_gone("call_0#1.s1"));
    assert!(!d.is_gone("call_0#2.s1"), "the run a call still waits for is not in the background");
    assert!(!d.is_gone("call_0#12.s1"), "a longer id is another run");
    assert_eq!(d.attached(), 1);
    assert!(d.to_background("call_0#2"));
    assert!(d.is_gone("call_0#2.s1"));
    assert_eq!(d.attached(), 0);
    d.end("call_0#1");
    assert!(!d.is_gone("call_0#1.s1"));
    assert!(d.is_gone("call_0#2.s1"));
}

#[test]
fn a_report_in_the_ended_note_is_cut_to_the_room_of_a_small_window_and_says_how_to_read_it_whole() {
    let report = "found it in the parser\n".repeat(1_300);
    let ended = |jobs: &Jobs| {
        let h = jobs.start(JobKind::Agent, "look");
        h.set_report(&report);
        h.finish(true, "reported");
        jobs.take_ended()
    };
    let room = crate::tool::ResultRoom { chars: 4_000, window: 8_000 };
    let jobs = Jobs::new();
    let note = jobs.note_ended_within(&ended(&jobs), Some(room));
    assert!(note.chars().count() < 4_000 + 400, "the note takes {} characters", note.chars().count());
    assert!(
        note.contains(&format!(
            "cut to 3700 of {} characters for a 8000-token window; read the whole report with the `jobs` tool (action \"output\", job 1)",
            report.chars().count()
        )),
        "{note}"
    );
    // a room as large as the report keeps it whole, and no room is the note as before
    let jobs = Jobs::new();
    let large = crate::tool::ResultRoom { chars: 200_000, window: 200_000 };
    let whole = jobs.note_ended_within(&ended(&jobs), Some(large));
    let jobs = Jobs::new();
    assert_eq!(whole, jobs.note_ended(&ended(&jobs)));
    assert!(!whole.contains("-token window"));
}

#[tokio::test]
async fn a_kill_racing_the_finish_never_leaves_a_label_that_contradicts_it() {
    let jobs = Jobs::new();
    for _ in 0..200 {
        let h = jobs.start(JobKind::Shell, "race");
        let id = h.id();
        let j = jobs.clone();
        let killer = std::thread::spawn(move || j.kill(id));
        h.finish(true, "exit 0");
        let killed = killer.join().unwrap();
        let status = jobs.get(id).unwrap().status;
        assert_eq!(killed, status == JobStatus::Killed, "{status:?}");
    }
}
