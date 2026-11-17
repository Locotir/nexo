use super::*;

use crate::context::saved::{no_such_process, YOUNG};

/// A session that holds nothing but its first line is only clutter after this long. The person may still be
/// about to speak in a session opened a minute ago; the lock tells a live one apart, the age spares a new one.
const EMPTY_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// How much of a file is read to tell that it holds only its first line: a first line is a few hundred bytes,
/// so a bigger file is a session with content whatever it starts with.
const META_LINE_MAX: u64 = 64 * 1024;

/// Clears what a crashed or killed nexo leaves next to the sessions in `dir` and nothing else would remove:
///
/// - `<id>.jsonl.<pid>.tmp`, a rewrite cut before its rename (a whole copy of the session, tool outputs
///   included): removed when its process is gone, or, where a process cannot be asked for (Windows), when it
///   was untouched for an hour. The files of this process are left alone, a rewrite may be running.
/// - `<id>.jsonl.lock` that no live nexo holds, and `<id>.jsonl.lock.<pid>.stale` left by a takeover that was cut
///   before it removed the lock it had moved aside: removed. A lock that a nexo takes meanwhile is put back.
/// - `<id>.jsonl` that holds only its first line (no message, title, goal or cost), older than a day and with
///   no live lock: moved to the trash, never deleted, so `restore` brings it back.
/// - `<id>.jsonl.curator` with no session beside it, untouched for an hour (a move into or out of the trash
///   puts the two files down one after the other): removed, as the session would rebuild it.
///
/// The trash folder is not looked into. Returns how many files it removed or moved. It reads the names of the
/// folder and opens only the few files the names point at, so it belongs off the render path and off the
/// path that starts a turn: `sweep_orphans_in_background`.
pub fn sweep_orphans(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let names: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    let mut done = 0;
    // the locks go before the sessions are looked at: a session whose lock was stale is free to move
    for name in &names {
        let path = dir.join(name);
        let fixed = if let Some(pid) = pid_in_name(name, ".jsonl", ".tmp") {
            cut_rewrite_gone(&path, pid)
        } else if let Some(pid) = pid_in_name(name, ".jsonl.lock", ".stale") {
            left_aside_gone(&path, pid)
        } else if name.ends_with(".jsonl.lock") {
            free_lock_gone(&path)
        } else {
            false
        };
        done += usize::from(fixed);
    }
    for name in &names {
        let path = dir.join(name);
        if let Some(id) = name.strip_suffix(".jsonl") {
            done += usize::from(empty_session_trashed(dir, &path, id));
        } else if name.ends_with(".jsonl.curator") {
            done += usize::from(orphan_state_gone(dir, &path, name));
        }
    }
    done
}

/// `sweep_orphans` on the sessions folder, on a thread of its own that nothing waits for: a process that ends
/// first just leaves the rest for the next one.
pub fn sweep_orphans_in_background() {
    let _ = std::thread::Builder::new().name("session-sweep".into()).spawn(|| {
        let n = sweep_orphans(&dir());
        if n > 0 {
            tracing::debug!("swept {n} leftovers of a crash from the sessions folder");
        }
    });
}

/// The pid in `<id><stem>.<pid><end>` (`a.jsonl.123.tmp` is 123 for `.jsonl` and `.tmp`), none for any other
/// name, including the curator's `<id>.jsonl.curator.<pid>.<n>.tmp`, which `context::saved::sweep` owns.
fn pid_in_name(name: &str, stem: &str, end: &str) -> Option<u32> {
    let (head, pid) = name.strip_suffix(end)?.rsplit_once('.')?;
    if !head.ends_with(stem) {
        return None;
    }
    pid.parse().ok()
}

/// How long ago the file was last written, where the system says.
fn idle(path: &Path) -> Option<std::time::Duration> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()?.elapsed().ok()
}

/// A file named after `pid` that is not this process's: its process is gone, or it lay untouched for an hour.
fn left_by_a_dead_process(path: &Path, pid: u32) -> bool {
    pid != std::process::id() && (no_such_process(pid) || idle(path).is_some_and(|a| a > YOUNG))
}

fn cut_rewrite_gone(tmp: &Path, pid: u32) -> bool {
    left_by_a_dead_process(tmp, pid) && std::fs::remove_file(tmp).is_ok()
}

/// A lock moved aside by a takeover that did not finish. It holds the text of the lock it replaced, so one
/// still held by a live nexo (a takeover that could not put it back) is left where it is.
fn left_aside_gone(aside: &Path, pid: u32) -> bool {
    pid != std::process::id() && holder(aside).is_none() && std::fs::remove_file(aside).is_ok()
}

/// A lock that is free or stale, taken over as a writer would: the text that was judged must still be there.
fn free_lock_gone(lock: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(lock) else { return false };
    holder(lock).is_none() && take_over(lock, &text).unwrap_or(false)
}

/// Whether the file at `path` is a session's first line and nothing else.
fn holds_only_its_first_line(path: &Path) -> bool {
    if !std::fs::metadata(path).is_ok_and(|m| m.len() <= META_LINE_MAX) {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(path) else { return false };
    let line = text.trim_end_matches(['\n', '\r']);
    // the first line of a fork names its parent, and a session that has a parent is kept as the list shows it
    !line.contains('\n') && matches!(serde_json::from_str::<Line>(line), Ok(Line::Meta(m)) if m.parent.is_none())
}

/// A session of one line, old, with no live lock, moved to the trash. A session whose goal or cost was
/// recorded has more than that line (`Session::release` also keeps those), so it never matches.
fn empty_session_trashed(dir: &Path, path: &Path, id: &str) -> bool {
    if !idle(path).is_some_and(|a| a > EMPTY_AFTER) || !holds_only_its_first_line(path) {
        return false;
    }
    // any holder, this process included: a nexo left open and quiet for a day still owns its session
    if holder(&lock_path(path)).is_some() {
        return false;
    }
    trash_in(dir, id).is_ok()
}

fn orphan_state_gone(dir: &Path, state: &Path, name: &str) -> bool {
    let Some(session) = name.strip_suffix(".curator") else { return false };
    !dir.join(session).exists() && idle(state).is_some_and(|a| a > YOUNG) && std::fs::remove_file(state).is_ok()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A pid that no process has: a child that was run and waited for.
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    fn write_aged(path: &Path, text: &str, age: std::time::Duration) {
        std::fs::write(path, text).unwrap();
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(std::time::SystemTime::now() - age).unwrap();
    }

    const DAY: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

    #[test]
    fn a_rewrite_cut_by_a_dead_process_is_removed_and_one_of_this_process_is_not() {
        let d = tempfile::tempdir().unwrap();
        let dead = d.path().join(format!("20261101-000000-abcd.jsonl.{}.tmp", dead_pid()));
        let ours = d.path().join(format!("20261101-000001-abcd.jsonl.{}.tmp", std::process::id()));
        let session = d.path().join("20261101-000002-abcd.jsonl");
        let curator = d.path().join("20261101-000003-abcd.jsonl.curator.1.2.tmp");
        for p in [&dead, &ours, &session, &curator] {
            std::fs::write(p, "x").unwrap();
        }
        assert_eq!(sweep_orphans(d.path()), 1);
        assert!(!dead.exists(), "the copy of a dead process goes");
        assert!(ours.exists() && session.exists(), "a rewrite that may be running and a session stay");
        assert!(curator.exists(), "the curator's own temporary files are swept by `context::saved`");
    }

    #[test]
    fn a_held_lock_stays_and_a_stale_one_and_a_takeover_left_aside_go() {
        let d = tempfile::tempdir().unwrap();
        let held = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
        let held_lock = lock_path(held.path());
        let stale = d.path().join("20261101-000000-abcd.jsonl.lock");
        let stale_text = format!("{} 1", dead_pid());
        std::fs::write(&stale, &stale_text).unwrap();
        let aside = d.path().join(format!("20261101-000001-abcd.jsonl.lock.{}.stale", dead_pid()));
        std::fs::write(&aside, &stale_text).unwrap();
        // a takeover that could not put a live writer's lock back: it still names a running process
        let live_aside = d.path().join(format!("20261101-000002-abcd.jsonl.lock.{}.stale", dead_pid()));
        std::fs::write(&live_aside, stamp()).unwrap();
        assert_eq!(sweep_orphans(d.path()), 2);
        assert!(held_lock.exists(), "the lock of a live session stays");
        assert!(!stale.exists() && !aside.exists());
        assert!(live_aside.exists(), "a lock that is still held is not removed from where it was put");
    }

    #[test]
    fn a_session_of_one_line_a_day_old_with_no_lock_goes_to_the_trash_with_its_state() {
        let d = tempfile::tempdir().unwrap();
        let made: Vec<SessionMeta> = (0..5)
            .map(|n| SessionMeta {
                id: format!("20261101-00000{n}-abcd"),
                cwd: "/p".into(),
                created: now(),
                updated: now(),
                provider: "mock".into(),
                model: "m1".into(),
                title: String::new(),
                messages: 0,
                parent: None,
                fork_at: None,
                headless: false,
            })
            .collect();
        let line = |m: &SessionMeta| format!("{}\n", serde_json::to_string(&Line::Meta(m.clone())).unwrap());
        let path = |m: &SessionMeta| d.path().join(format!("{}.jsonl", m.id));
        let (old, young, with_message, with_title, forked) = (&made[0], &made[1], &made[2], &made[3], &made[4]);
        write_aged(&path(old), &line(old), 2 * DAY);
        write_aged(&path(young), &line(young), DAY / 2);
        let mut with_message_text = line(with_message);
        with_message_text.push_str(&format!(
            "{}\n",
            serde_json::to_string(&Line::Message { index: 0, ts: now(), message: Message::user("hi") }).unwrap()
        ));
        write_aged(&path(with_message), &with_message_text, 2 * DAY);
        let mut titled = line(with_title);
        titled.push_str("{\"type\":\"title\",\"title\":\"kept\"}\n");
        write_aged(&path(with_title), &titled, 2 * DAY);
        let mut child = forked.clone();
        child.parent = Some("20261101-000009-abcd".into());
        write_aged(&path(forked), &line(&child), 2 * DAY);
        let state = crate::context::saved::path_for(&path(old));
        std::fs::write(&state, "state").unwrap();

        assert_eq!(sweep_orphans(d.path()), 1);
        let trash = trash_dir_in(d.path());
        assert!(!path(old).exists() && trash.join(format!("{}.jsonl", old.id)).is_file(), "moved, not deleted");
        assert!(trash.join(format!("{}.jsonl.curator", old.id)).is_file(), "its saved state goes along");
        for kept in [young, with_message, with_title, forked] {
            assert!(path(kept).is_file(), "{} is kept", kept.id);
        }
    }

    #[test]
    fn an_old_session_of_one_line_that_a_live_nexo_holds_is_kept() {
        let d = tempfile::tempdir().unwrap();
        let s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
        let f = std::fs::File::options().write(true).open(s.path()).unwrap();
        f.set_modified(std::time::SystemTime::now() - 3 * DAY).unwrap();
        assert_eq!(sweep_orphans(d.path()), 0);
        assert!(s.path().is_file(), "its lock is this process's, and a session left open and quiet is still its own");
    }

    #[test]
    fn a_saved_state_with_no_session_beside_it_goes_when_it_is_an_hour_old() {
        let d = tempfile::tempdir().unwrap();
        let hour = YOUNG + std::time::Duration::from_secs(60);
        let lonely = d.path().join("20261101-000000-abcd.jsonl.curator");
        let fresh = d.path().join("20261101-000001-abcd.jsonl.curator");
        let kept = d.path().join("20261101-000002-abcd.jsonl.curator");
        write_aged(&lonely, "x", hour);
        write_aged(&fresh, "x", std::time::Duration::from_secs(10));
        write_aged(&kept, "x", hour);
        write_aged(&d.path().join("20261101-000002-abcd.jsonl"), "", hour);
        assert_eq!(sweep_orphans(d.path()), 1);
        assert!(!lonely.exists() && fresh.exists() && kept.exists());
    }
}
