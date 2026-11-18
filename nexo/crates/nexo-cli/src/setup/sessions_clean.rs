//! `nexo sessions size` and `nexo sessions prune`: what the sessions folder holds, and the way to let old
//! sessions go without ever losing one by accident.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use nexo_agent::session;

/// How many names a list prints before it says how many more there are.
const SHOWN: usize = 20;

/// `nexo sessions size`.
pub fn size_sessions() -> i32 {
    match size_in(&session::dir(), &mut std::io::stdout().lock()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("cannot print the sizes: {e}");
            1
        }
    }
}

/// `nexo sessions prune --older-than AGE [--headless | --trash] [--yes]`.
pub fn prune_sessions(older_than: &str, headless: bool, trash: bool, yes: bool) -> i32 {
    let age = match parse_age(older_than) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let plan = Prune { age, headless_only: headless, empty_trash: trash, yes };
    match prune_in(&session::dir(), &plan, &mut std::io::stdout().lock()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cannot print the result: {e}");
            1
        }
    }
}

/// What `prune` was asked for.
struct Prune {
    /// A session untouched for longer than this is let go.
    age: Duration,
    headless_only: bool,
    /// Empty the trash instead of filling it.
    empty_trash: bool,
    /// Without it nothing is changed: the list is printed and that is all.
    yes: bool,
}

/// `90d` or `36h`: a whole number and the unit, in days or hours.
fn parse_age(text: &str) -> Result<Duration, String> {
    let bad = || format!("{text:?} is not an age: write a number of days or hours, as 90d or 36h");
    let text = text.trim();
    let (digits, hours) = match (text.strip_suffix('d'), text.strip_suffix('h')) {
        (Some(d), _) => (d, 24),
        (_, Some(h)) => (h, 1),
        _ => return Err(bad()),
    };
    let n: u64 = digits.parse().map_err(|_| bad())?;
    if n == 0 {
        // an age of nothing would take every session, which is what `nexo sessions delete` is for
        return Err(format!("{text:?} is no age to prune by: it would take every session"));
    }
    n.checked_mul(hours * 3600).map(Duration::from_secs).ok_or_else(bad)
}

/// How long ago the file was last written, where the system says.
fn idle(path: &Path) -> Option<Duration> {
    SystemTime::now().duration_since(std::fs::metadata(path).and_then(|m| m.modified()).ok()?).ok()
}

fn bytes_text(n: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

/// The files of `dir` itself (not the folders in it) with their sizes, by name.
fn files_of(dir: &Path) -> Vec<(String, u64)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.flatten()
        .filter_map(|e| {
            let md = e.metadata().ok().filter(|m| m.is_file())?;
            Some((e.file_name().into_string().ok()?, md.len()))
        })
        .collect()
}

/// The three kinds of file a sessions folder holds, as the size report groups them.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Session,
    State,
    Other,
}

fn kind_of(name: &str) -> Kind {
    if name.ends_with(".jsonl") {
        Kind::Session
    } else if name.ends_with(".jsonl.curator") {
        Kind::State
    } else {
        // the temporary files of a cut write, the locks and whatever else got in
        Kind::Other
    }
}

/// One folder of the size report: how many files of each kind and how much room they take.
fn size_rows(out: &mut impl Write, label: &str, files: &[(String, u64)]) -> std::io::Result<()> {
    writeln!(out, "{label}")?;
    for (title, kind) in [("sessions", Kind::Session), ("curator state", Kind::State), ("other files", Kind::Other)] {
        let (count, bytes) =
            files.iter().filter(|(n, _)| kind_of(n) == kind).fold((0, 0), |(c, b), (_, len)| (c + 1, b + len));
        writeln!(out, "{:<14}{:>6}  {}", title, count, bytes_text(bytes))?;
    }
    Ok(())
}

/// The size of the file called `name` among `files`, 0 when there is none.
fn len_of(files: &[(String, u64)], name: &str) -> u64 {
    files.iter().find(|(n, _)| n == name).map_or(0, |(_, l)| *l)
}

fn size_in(dir: &Path, out: &mut impl Write) -> std::io::Result<()> {
    let live = files_of(dir);
    let trash_dir = session::trash_dir_in(dir);
    let trashed = files_of(&trash_dir);
    writeln!(out, "{}", dir.display())?;
    size_rows(out, "in use", &live)?;
    writeln!(out)?;
    writeln!(out, "{}", trash_dir.display())?;
    size_rows(out, "in the trash", &trashed)?;
    // the sessions that take the most, each with the state saved beside it
    let mut biggest: Vec<(u64, String, String)> = session::list_in(dir)
        .into_iter()
        .map(|m| {
            let total = len_of(&live, &format!("{}.jsonl", m.id)) + len_of(&live, &format!("{}.jsonl.curator", m.id));
            (total, m.id, m.title)
        })
        .collect();
    biggest.sort_by(|a, b| b.0.cmp(&a.0));
    if !biggest.is_empty() {
        writeln!(out)?;
        writeln!(out, "largest sessions (with their curator state)")?;
        for (total, id, title) in biggest.into_iter().take(5) {
            writeln!(out, "{:<22} {:>9}  {}", id, bytes_text(total), title)?;
        }
    }
    Ok(())
}

/// The names printed for a list: the first few, then how many more.
fn print_list(out: &mut impl Write, lines: &[String]) -> std::io::Result<()> {
    for l in lines.iter().take(SHOWN) {
        writeln!(out, "  {l}")?;
    }
    if lines.len() > SHOWN {
        writeln!(out, "  and {} more", lines.len() - SHOWN)?;
    }
    Ok(())
}

fn prune_in(dir: &Path, plan: &Prune, out: &mut impl Write) -> std::io::Result<i32> {
    if plan.empty_trash {
        empty_trash_in(dir, plan, out)
    } else {
        move_to_trash_in(dir, plan, out)
    }
}

/// The sessions not written for the age, to the trash (where `nexo sessions restore` finds them).
fn move_to_trash_in(dir: &Path, plan: &Prune, out: &mut impl Write) -> std::io::Result<i32> {
    let sizes = files_of(dir);
    let size_of = |id: &str| -> u64 {
        sizes
            .iter()
            .filter(|(n, _)| *n == format!("{id}.jsonl") || *n == format!("{id}.jsonl.curator"))
            .map(|(_, l)| *l)
            .sum()
    };
    // the age is the file's last write, which is what the list shows as `updated`
    let old: Vec<session::SessionMeta> = session::list_in(dir)
        .into_iter()
        .filter(|m| !plan.headless_only || m.headless)
        .filter(|m| idle(&dir.join(format!("{}.jsonl", m.id))).is_some_and(|a| a > plan.age))
        .collect();
    if old.is_empty() {
        writeln!(out, "no session to move to the trash")?;
        return Ok(0);
    }
    let total: u64 = old.iter().map(|m| size_of(&m.id)).sum();
    let lines: Vec<String> =
        old.iter().map(|m| format!("{:<22} {:>9}  {}", m.id, bytes_text(size_of(&m.id)), m.title)).collect();
    if !plan.yes {
        writeln!(out, "{} sessions ({}) would go to the trash:", old.len(), bytes_text(total))?;
        print_list(out, &lines)?;
        writeln!(out, "nothing was moved: add --yes to move them (`nexo sessions restore <id>` brings one back)")?;
        return Ok(0);
    }
    let (mut moved, mut kept) = (0, 0);
    let mut freed = 0;
    for m in &old {
        match session::trash_in(dir, &m.id) {
            Ok(_) => {
                moved += 1;
                freed += size_of(&m.id);
            }
            Err(e) => {
                // one a running nexo holds is left where it is
                kept += 1;
                writeln!(out, "{}: left in place: {e}", m.id)?;
            }
        }
    }
    writeln!(out, "{moved} sessions moved to the trash ({}), {kept} left in place", bytes_text(freed))?;
    Ok(0)
}

/// The files in the trash that belong to a session not written for the age, deleted for good. The age of a
/// file in the trash counts from the day it was moved there (`trash_in` sets that time), so a session that was
/// deleted yesterday is not lost by a prune of what is older than 90 days. Files trashed by an older nexo still
/// carry the time of their last write.
fn empty_trash_in(dir: &Path, plan: &Prune, out: &mut impl Write) -> std::io::Result<i32> {
    let bin = session::trash_dir_in(dir);
    let files = files_of(&bin);
    let old: Vec<(PathBuf, u64)> = files
        .iter()
        .filter(|(name, _)| {
            // a saved state is judged by its session, which is the file that tells when it was last written
            let owner = name.find(".jsonl").map(|i| format!("{}.jsonl", &name[..i]));
            let judged = owner.filter(|o| files.iter().any(|(n, _)| n == o)).unwrap_or_else(|| name.clone());
            idle(&bin.join(judged)).is_some_and(|a| a > plan.age)
        })
        .map(|(name, len)| (bin.join(name), *len))
        .collect();
    if old.is_empty() {
        writeln!(out, "nothing in the trash is that old")?;
        return Ok(0);
    }
    let total: u64 = old.iter().map(|(_, l)| *l).sum();
    if !plan.yes {
        writeln!(out, "{} files ({}) would be deleted for good from {}:", old.len(), bytes_text(total), bin.display())?;
        let lines: Vec<String> = old
            .iter()
            .map(|(p, l)| format!("{:<44} {:>9}", p.file_name().unwrap_or_default().to_string_lossy(), bytes_text(*l)))
            .collect();
        print_list(out, &lines)?;
        writeln!(out, "nothing was deleted: add --yes to delete them")?;
        return Ok(0);
    }
    let (mut gone, mut freed) = (0, 0);
    for (path, len) in &old {
        match std::fs::remove_file(path) {
            Ok(()) => {
                gone += 1;
                freed += len;
            }
            Err(e) => writeln!(out, "{}: not deleted: {e}", path.display())?,
        }
    }
    writeln!(out, "{gone} files deleted from the trash ({})", bytes_text(freed))?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 3600);

    /// A session file of just its first line, last written `age` ago, with a curator state of `state` bytes.
    fn session_file(dir: &Path, id: &str, age: Duration, headless: bool, parent: Option<&str>, state: usize) {
        let mut meta = serde_json::json!({
            "type": "meta", "id": id, "cwd": "/p", "created": "2026-01-01T00:00:00Z",
            "provider": "mock", "model": "m1", "headless": headless,
        });
        if let Some(p) = parent {
            meta["parent"] = p.into();
        }
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{id}.jsonl"));
        std::fs::write(&path, format!("{meta}\n")).unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(SystemTime::now() - age).unwrap();
        if state > 0 {
            std::fs::write(dir.join(format!("{id}.jsonl.curator")), vec![b'x'; state]).unwrap();
        }
    }

    fn run(dir: &Path, plan: &Prune) -> String {
        let mut out = Vec::new();
        prune_in(dir, plan, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn plan(days: u64) -> Prune {
        Prune { age: DAY * days as u32, headless_only: false, empty_trash: false, yes: false }
    }

    #[test]
    fn an_age_is_a_whole_number_of_days_or_hours_and_nothing_else() {
        assert_eq!(parse_age("90d"), Ok(DAY * 90));
        assert_eq!(parse_age(" 36h "), Ok(Duration::from_secs(36 * 3600)));
        for bad in ["", "d", "90", "0d", "1.5d", "-3d", "3w", "ten days"] {
            assert!(parse_age(bad).is_err(), "{bad:?} is not an age");
        }
    }

    #[test]
    fn the_size_report_counts_sessions_curator_state_and_other_files_in_the_folder_and_the_trash() {
        let d = tempfile::tempdir().unwrap();
        session_file(d.path(), "20260101-000001-aaaa", DAY, false, None, 2048);
        session_file(d.path(), "20260101-000002-bbbb", DAY, false, None, 2048);
        std::fs::write(d.path().join("20260101-000002-bbbb.jsonl.1234.tmp"), vec![b'x'; 10]).unwrap();
        session_file(&session::trash_dir_in(d.path()), "20260101-000003-cccc", DAY, false, None, 0);
        let mut out = Vec::new();
        size_in(d.path(), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        let row = |title: &str, n: usize, size: &str| format!("{title:<14}{n:>6}  {size}");
        assert!(text.contains(&row("sessions", 2, "")), "two sessions in use:\n{text}");
        assert!(text.contains(&row("curator state", 2, "4.0 KB")), "their saved state:\n{text}");
        assert!(text.contains(&row("other files", 1, "10 B")), "the leftover of a cut write:\n{text}");
        assert!(text.contains(&row("curator state", 0, "0 B")), "none in the trash:\n{text}");
        assert!(text.contains("20260101-000001-aaaa"), "the largest are named:\n{text}");
    }

    #[test]
    fn prune_lists_what_it_would_move_and_moves_nothing_without_yes() {
        let d = tempfile::tempdir().unwrap();
        session_file(d.path(), "20260101-000001-aaaa", 100 * DAY, false, None, 0);
        let text = run(d.path(), &plan(90));
        assert!(text.contains("20260101-000001-aaaa") && text.contains("nothing was moved"), "{text}");
        assert!(d.path().join("20260101-000001-aaaa.jsonl").is_file());
        assert!(!session::trash_dir_in(d.path()).exists());
    }

    #[test]
    fn prune_moves_the_old_sessions_and_their_state_to_the_trash_and_keeps_the_young_ones() {
        let d = tempfile::tempdir().unwrap();
        session_file(d.path(), "20260101-000001-aaaa", 100 * DAY, false, None, 100);
        session_file(d.path(), "20260101-000002-bbbb", 10 * DAY, false, None, 100);
        // a fork whose parent goes: it stays, and the list shows it as a root
        session_file(d.path(), "20260101-000003-cccc", DAY, false, Some("20260101-000001-aaaa"), 0);
        let text = run(d.path(), &Prune { yes: true, ..plan(90) });
        assert!(text.contains("1 sessions moved to the trash"), "{text}");
        let bin = session::trash_dir_in(d.path());
        assert!(bin.join("20260101-000001-aaaa.jsonl").is_file(), "moved, not deleted");
        assert!(bin.join("20260101-000001-aaaa.jsonl.curator").is_file(), "its state goes along");
        assert!(d.path().join("20260101-000002-bbbb.jsonl").is_file());
        let tree = session::tree(&session::list_in(d.path()));
        assert!(tree.iter().all(|(depth, _)| *depth == 0), "a fork of a pruned session is a root");
        assert_eq!(tree.len(), 2);
        assert!(session::restore_in(d.path(), "20260101-000001-aaaa").is_ok(), "restore brings it back");
    }

    #[test]
    fn prune_with_headless_takes_only_the_sessions_a_program_started() {
        let d = tempfile::tempdir().unwrap();
        session_file(d.path(), "20260101-000001-aaaa", 100 * DAY, true, None, 0);
        session_file(d.path(), "20260101-000002-bbbb", 100 * DAY, false, None, 0);
        run(d.path(), &Prune { yes: true, headless_only: true, ..plan(90) });
        assert!(!d.path().join("20260101-000001-aaaa.jsonl").exists());
        assert!(d.path().join("20260101-000002-bbbb.jsonl").is_file(), "the person's own session stays");
    }

    #[cfg(unix)]
    #[test]
    fn prune_leaves_a_session_that_another_running_nexo_holds() {
        let d = tempfile::tempdir().unwrap();
        session_file(d.path(), "20260101-000001-aaaa", 100 * DAY, false, None, 0);
        let mut other = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        std::fs::write(d.path().join("20260101-000001-aaaa.jsonl.lock"), other.id().to_string()).unwrap();
        let text = run(d.path(), &Prune { yes: true, ..plan(90) });
        other.kill().unwrap();
        other.wait().unwrap();
        assert!(d.path().join("20260101-000001-aaaa.jsonl").is_file(), "{text}");
        assert!(text.contains("left in place") && text.contains("0 sessions moved"), "{text}");
    }

    #[test]
    fn a_session_deleted_today_is_not_lost_to_a_prune_of_what_is_old_in_the_trash() {
        let d = tempfile::tempdir().unwrap();
        session_file(d.path(), "20260101-000001-aaaa", 200 * DAY, false, None, 100);
        session::trash_in(d.path(), "20260101-000001-aaaa").unwrap();
        let said = run(d.path(), &Prune { empty_trash: true, yes: true, ..plan(90) });
        let bin = session::trash_dir_in(d.path());
        assert!(said.contains("nothing in the trash is that old"), "{said}");
        assert!(
            bin.join("20260101-000001-aaaa.jsonl").is_file()
                && bin.join("20260101-000001-aaaa.jsonl.curator").is_file()
        );
    }

    #[test]
    fn the_trash_is_emptied_only_with_the_trash_flag_and_only_of_what_is_old_enough() {
        let d = tempfile::tempdir().unwrap();
        let bin = session::trash_dir_in(d.path());
        session_file(&bin, "20260101-000001-aaaa", 200 * DAY, false, None, 100);
        session_file(&bin, "20260101-000002-bbbb", 20 * DAY, false, None, 100);
        // without --trash the trash is never looked into
        run(d.path(), &Prune { yes: true, ..plan(90) });
        assert!(bin.join("20260101-000001-aaaa.jsonl").is_file());
        // with it and no --yes it only says what it would delete
        let said = run(d.path(), &Prune { empty_trash: true, ..plan(90) });
        assert!(said.contains("nothing was deleted") && bin.join("20260101-000001-aaaa.jsonl").is_file(), "{said}");
        run(d.path(), &Prune { empty_trash: true, yes: true, ..plan(90) });
        assert!(
            !bin.join("20260101-000001-aaaa.jsonl").exists()
                && !bin.join("20260101-000001-aaaa.jsonl.curator").exists()
        );
        assert!(bin.join("20260101-000002-bbbb.jsonl").is_file(), "a young one stays");
        assert!(bin.join("20260101-000002-bbbb.jsonl.curator").is_file());
    }
}
