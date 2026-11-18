//! `/doctor` runs off the event loop: its answer arrives through a channel and becomes a notice.

use super::*;

#[test]
fn a_doctor_answer_that_arrives_becomes_one_info_notice() {
    let (mut a, _rx) = app();
    let base = a.blocks.len();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    tx.send((true, "ok".to_string())).unwrap();
    a.doctor = Some(rx);
    a.dirty = false;
    a.poll_doctor();
    assert!(a.dirty);
    assert!(a.doctor.is_none());
    assert_eq!(a.blocks.len(), base + 1);
    assert!(matches!(&a.blocks[base], Block::Notice { level: Level::Info, text } if text == "ok"));
}

#[test]
fn a_failed_doctor_check_is_a_warning() {
    let (mut a, _rx) = app();
    let base = a.blocks.len();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    tx.send((false, "bad key".to_string())).unwrap();
    a.doctor = Some(rx);
    a.poll_doctor();
    assert!(matches!(&a.blocks[base], Block::Notice { level: Level::Warn, text } if text == "bad key"));
}

#[test]
fn a_doctor_still_waiting_leaves_the_transcript_alone() {
    let (mut a, _rx) = app();
    let base = a.blocks.len();
    let (_tx, rx) = std::sync::mpsc::sync_channel::<(bool, String)>(1);
    a.doctor = Some(rx);
    a.dirty = false;
    a.poll_doctor();
    assert!(!a.dirty);
    assert!(a.doctor.is_some());
    assert_eq!(a.blocks.len(), base);
}

#[test]
fn a_doctor_thread_that_died_gives_the_shell_hint() {
    let (mut a, _rx) = app();
    let base = a.blocks.len();
    let (tx, rx) = std::sync::mpsc::sync_channel::<(bool, String)>(1);
    drop(tx);
    a.doctor = Some(rx);
    a.poll_doctor();
    assert!(a.doctor.is_none());
    assert!(matches!(&a.blocks[base], Block::Notice { level: Level::Warn, text } if text.contains("nexo doctor")));
}

#[test]
fn a_second_doctor_command_while_one_runs_starts_nothing() {
    let (mut a, _rx) = app();
    let (_tx, rx) = std::sync::mpsc::sync_channel::<(bool, String)>(1);
    a.doctor = Some(rx);
    let base = a.blocks.len();
    a.run_doctor();
    assert!(a.doctor.is_some());
    assert!(matches!(&a.blocks[base], Block::Notice { text, .. } if text.contains("still checking")));
}

#[cfg(unix)]
#[test]
fn a_doctor_child_that_never_answers_is_killed_at_the_deadline() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("nexo-doctor-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("hang.sh");
    std::fs::write(&script, "#!/bin/sh\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let t = Instant::now();
    let (ok, text) = super::super::dispatch::doctor_probe(&script, &dir, Duration::from_millis(200));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok);
    assert!(text.contains("no answer"), "{text}");
    assert!(t.elapsed() < Duration::from_secs(5));
}

#[cfg(unix)]
#[test]
fn a_key_command_the_doctor_started_is_killed_with_it_at_the_deadline() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("nexo-doctor-group-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pid_file = dir.join("grandchild.pid");
    let script = dir.join("hang.sh");
    // the sleeper is a grandchild of the probe and holds the output pipe, as a stuck key command does
    let body = format!("#!/bin/sh\nsleep 30 &\necho $! > {}\nwait\n", pid_file.display());
    std::fs::write(&script, body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let t = Instant::now();
    let (ok, text) = super::super::dispatch::doctor_probe(&script, &dir, Duration::from_millis(500));
    assert!(!ok);
    assert!(text.contains("no answer"), "{text}");
    assert!(t.elapsed() < Duration::from_secs(5));
    let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    // init reaps the killed grandchild a moment after the signal
    let gone = (0..100).any(|_| {
        // SAFETY: signal 0 only checks that the process exists.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        if alive {
            std::thread::sleep(Duration::from_millis(50));
        }
        !alive
    });
    if !gone {
        // SAFETY: cleanup of the sleeper this test started, so a failure leaves nothing behind.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    assert!(gone, "the grandchild outlived the probe");
}
