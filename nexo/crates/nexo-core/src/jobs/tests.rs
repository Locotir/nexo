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
    let r = rt.block_on(jobs.read(1, Duration::ZERO, &CancellationToken::new())).unwrap();
    assert!(r.new.starts_with('['), "{}", &r.new[..60]);
    assert!(r.new.trim_end().ends_with("line 49999"));
    assert!(r.new.chars().count() < READ_CHARS + 200);
    drop(h);
    assert_eq!(jobs.get(1).unwrap().status, JobStatus::Killed, "a dropped runner ends the job");
    assert_eq!(duration(65_000), "1 min 5 s");
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
